//! A transient read-only surface: never enters the ordinary file-navigation path.
use super::*;
#[path = "reader_quick_look/media.rs"]
mod media;

enum MediaState {
    None,
    Loading,
    Image(Arc<RenderImage>, String),
    Message(String),
}

pub(super) struct Preview {
    rel: String,
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
        cx.on_release_in(window, |this, window, _| {
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
        if let Some(kind) = kind {
            let task = cx
                .background_executor()
                .spawn(async move { load_media(&path, kind) });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                let _ = this.update(cx, |this, cx| {
                    this.media = match result {
                        Ok((image, notice)) => MediaState::Image(image, notice),
                        Err(error) => {
                            MediaState::Message(format!("Quick Look unavailable: {error:#}"))
                        }
                    };
                    cx.notify();
                });
            })
            .detach();
        }
        Self {
            rel,
            text,
            media: if kind.is_some() {
                MediaState::Loading
            } else {
                MediaState::None
            },
        }
    }
}

impl Render for Preview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let media = match &self.media {
            MediaState::Image(image, notice) => Some(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(div().px_6().child(notice.clone()))
                    .child(
                        img(ImageSource::Render(image.clone()))
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
            MediaState::Message(message) => {
                Some(div().px_6().child(message.clone()).into_any_element())
            }
            MediaState::None => None,
        };
        v_flex()
            .id("tree-readonly-quick-look")
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
fn load_media(path: &Path, kind: media::Kind) -> anyhow::Result<(Arc<RenderImage>, String)> {
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
    Ok((
        Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])),
        notice,
    ))
}

#[cfg(test)]
#[path = "reader_quick_look/tests.rs"]
mod tests;
