//! A transient read-only surface: never enters the ordinary file-navigation path.
use super::*;
#[path = "reader_quick_look/media.rs"]
mod media;

// Pure cancellation authority; workers may finish a synchronous decoder call,
// but lose permission to start another stage or publish after retirement.
mod authority {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    #[derive(Clone)]
    pub(super) struct Selection(std::path::PathBuf);
    impl Selection {
        pub(super) fn new(path: std::path::PathBuf) -> Self {
            Self(path)
        }
        pub(super) fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    #[derive(Default)]
    pub(super) struct Authority(Arc<AtomicBool>);
    impl Authority {
        pub(super) fn retire(&self) {
            self.0.store(true, Ordering::Release);
        }
        pub(super) fn ticket(&self) -> Arc<AtomicBool> {
            self.0.clone()
        }
        pub(super) fn permits(ticket: &AtomicBool) -> bool {
            !ticket.load(Ordering::Acquire)
        }
    }
    impl Drop for Authority {
        fn drop(&mut self) {
            self.retire();
        }
    }
}

enum MediaState {
    None,
    Loading,
    Image(Arc<RenderImage>, String),
    Message(String),
}

type DecodeResult = anyhow::Result<(Arc<RenderImage>, String)>;
type Decoder = Rc<
    dyn Fn(
        PathBuf,
        media::Kind,
        Arc<std::sync::atomic::AtomicBool>,
        &mut App,
    ) -> Task<DecodeResult>,
>;

// One matching Tree-created entity consumes the offline decoder seam. Tests
// still enter through the real Tree action, never replace the Reader route.
#[cfg(test)]
thread_local! {
    static NEXT_DECODER: std::cell::RefCell<Option<(PathBuf, Decoder)>> = const {
        std::cell::RefCell::new(None)
    };
}

fn decoder() -> Decoder {
    Rc::new(|path, kind, ticket, cx| {
        cx.background_executor()
            .spawn(async move { load_media(&path, kind, &ticket) })
    })
}

pub(super) struct Preview {
    rel: String,
    selected: authority::Selection,
    authority: authority::Authority,
    pending: Option<Task<()>>,
    decoder: Decoder,
    released: Vec<Arc<RenderImage>>,
    media: MediaState,
    text: Option<Entity<reader_plain_text::PlainTextPreview>>,
}

impl Preview {
    pub(super) fn new(
        root: PathBuf,
        rel: String,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let decoder = decoder();
        #[cfg(test)]
        let decoder = NEXT_DECODER.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.as_ref().is_some_and(|(expected, _)| expected == &path) {
                slot.take().unwrap().1
            } else {
                decoder
            }
        });
        Self::with_decoder(root, rel, path, decoder, window, cx)
    }

    fn with_decoder(
        root: PathBuf,
        rel: String,
        path: PathBuf,
        decoder: Decoder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.on_release_in(window, |this, window, _| {
            this.authority.retire();
            this.pending = None;
            for image in this.released.drain(..) {
                let _ = window.drop_image(image);
            }
            if let MediaState::Image(image, _) = &this.media {
                let _ = window.drop_image(image.clone());
            }
        })
        .detach();
        // Reuse the plain-file codec, including its bounded UTF-8 prefix and
        // binary rejection. Do not instantiate editable table/log/archive views
        // for a transient selection. Media uses separately bounded snapshots.
        let textual = reader_plain_text::eligible(&rel)
            || reader_code_file::eligible(&rel)
            || reader_delimited::eligible(&rel);
        let text = textual.then(|| {
            cx.new(|cx| {
                reader_plain_text::PlainTextPreview::new(root, rel.clone(), path.clone(), cx)
            })
        });
        let kind = media::kind(&rel);

        let mut this = Self {
            rel,
            selected: authority::Selection::new(path),
            authority: Default::default(),
            pending: None,
            decoder,
            released: Vec::new(),
            text,
            media: if kind.is_some() {
                MediaState::Loading
            } else {
                MediaState::None
            },
        };
        this.start_media(cx);
        this
    }

    fn retry_media(&mut self, cx: &mut Context<Self>) {
        if matches!(self.media, MediaState::Message(_)) {
            self.start_media(cx);
        }
    }

    fn start_media(&mut self, cx: &mut Context<Self>) {
        let Some(kind) = media::kind(&self.rel) else {
            return;
        };
        // Replacing this authority retires any previous attempt before spawning.
        self.authority = Default::default();
        self.pending = None;
        if let MediaState::Image(image, _) = std::mem::replace(&mut self.media, MediaState::Loading)
        {
            self.released.push(image);
        }
        let ticket = self.authority.ticket();
        let worker_ticket = ticket.clone();
        let selected = self.selected.clone();
        let task = (self.decoder)(selected.path().to_owned(), kind, worker_ticket, cx);
        self.pending = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            if !authority::Authority::permits(&ticket) {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                if !authority::Authority::permits(&ticket) {
                    return;
                }
                this.media = match result {
                    Ok((image, notice)) => MediaState::Image(image, notice),
                    Err(error) => MediaState::Message(format!("Quick Look unavailable: {error:#}")),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }
}

impl Render for Preview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in self.released.drain(..) {
            let _ = window.drop_image(image);
        }
        let media = match &self.media {
            MediaState::Image(image, notice) => Some(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(div().px_6().child(notice.clone()))
                    .child(
                        img(ImageSource::Render(image.clone()))
                            .when(cfg!(test), |image| {
                                image.debug_selector(|| "quick-look-image".into())
                            })
                            .size_full()
                            .object_fit(ObjectFit::Contain),
                    )
                    .into_any_element(),
            ),
            MediaState::Loading => Some(
                div()
                    .px_6()
                    .child("Opening bounded preview…")
                    .into_any_element(),
            ),
            MediaState::Message(message) => Some(
                v_flex()
                    .px_6()
                    .gap_3()
                    .child(message.clone())
                    .child(
                        gpui_component::button::Button::new("quick-look-retry")
                            .when(cfg!(test), |button| {
                                button.debug_selector(|| "quick-look-retry".into())
                            })
                            .label("Retry preview")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.retry_media(cx);
                            })),
                    )
                    .into_any_element(),
            ),
            MediaState::None => None,
        };
        v_flex()
            .id("tree-readonly-quick-look")
            .when(cfg!(test), |surface| {
                surface.debug_selector(|| "tree-readonly-quick-look".into())
            })
            .size_full()
            .min_h_0()
            .gap_3()
            .child(
                div()
                    .px_6()
                    .py_3()
                    .child(self.rel.clone())
                    .child(" · Quick Look · Space or Escape to close"),
            )
            .child(match (media, &self.text) {
                (Some(media), _) => media,
                (_, Some(text)) => div().flex_1().min_h_0().child(text.clone()).into_any_element(),
                (_, None) => div()
                    .px_6()
                    .text_color(cx.theme().muted_foreground)
                    .child("This file type has no bounded Quick Look preview. Press Enter in the tree to open it in the reader.")
                    .into_any_element(),
            })
    }
}

/// Decode only an in-memory bounded snapshot, never re-open a mutable path in
/// the decoder. Each selection owns its result, including a late failure.
fn load_media(
    path: &Path,
    kind: media::Kind,
    ticket: &std::sync::atomic::AtomicBool,
) -> anyhow::Result<(Arc<RenderImage>, String)> {
    anyhow::ensure!(authority::Authority::permits(ticket), "Preview retired");
    let limit = match kind {
        media::Kind::Image => media::IMAGE_BYTES,
        media::Kind::Pdf => media::PDF_BYTES,
    };
    let file = std::fs::File::open(path)?;
    anyhow::ensure!(file.metadata()?.is_file(), "Choose a regular file");
    anyhow::ensure!(
        file.metadata()?.len() <= limit as u64,
        "File exceeds the Quick Look byte limit ({limit} bytes)"
    );
    let bytes = media::snapshot(file, limit)?;
    anyhow::ensure!(authority::Authority::permits(ticket), "Preview retired");
    let (buffer, notice) = match kind {
        media::Kind::Image => {
            use image::ImageDecoder as _;
            let mut reader =
                image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(media::IMAGE_SIDE);
            limits.max_image_height = Some(media::IMAGE_SIDE);
            limits.max_alloc = Some(media::IMAGE_PIXELS * 4);
            reader.limits(limits);
            let mut decoder = reader.into_decoder()?;
            let (width, height) = decoder.dimensions();
            anyhow::ensure!(
                media::dimensions_fit(width, height),
                "Image exceeds the Quick Look pixel limit"
            );
            let orientation = decoder.orientation()?;
            let mut decoded = image::DynamicImage::from_decoder(decoder)?;
            decoded.apply_orientation(orientation);
            let mut buffer = decoded.into_rgba8();
            // GPUI RenderImage consumes BGRA, as does the existing PDF viewer.
            for pixel in buffer.pixels_mut() {
                pixel.0.swap(0, 2);
            }
            (buffer, "Image preview · first frame".to_owned())
        }
        media::Kind::Pdf => {
            let document =
                crate::pdf_engine::PdfDocument::open(bytes).map_err(|error| match error {
                    crate::pdf_engine::OpenError::Locked => {
                        anyhow::anyhow!("This PDF is password-protected")
                    }
                    crate::pdf_engine::OpenError::Unreadable => {
                        anyhow::anyhow!("This PDF could not be read")
                    }
                })?;
            anyhow::ensure!(authority::Authority::permits(ticket), "Preview retired");
            let bitmap = document
                .renderer()
                .render(0, 1024)
                .ok_or_else(|| anyhow::anyhow!("The first PDF page could not be rendered"))?;
            let buffer = image::RgbaImage::from_raw(bitmap.width, bitmap.height, bitmap.bgra)
                .ok_or_else(|| anyhow::anyhow!("Invalid PDF page bitmap"))?;
            (
                buffer,
                format!("Page 1 of {} · first-page preview", document.sizes().len()),
            )
        }
    };
    anyhow::ensure!(authority::Authority::permits(ticket), "Preview retired");
    Ok((
        Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])),
        notice,
    ))
}

#[cfg(test)]
#[path = "reader_quick_look/tests.rs"]
mod tests;
