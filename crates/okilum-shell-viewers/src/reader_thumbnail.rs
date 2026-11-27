//! A selection-owned bitmap: dropping the view cancels native provider work.
use crate::prelude::*;
use std::os::unix::fs::MetadataExt;

const MAX_PIXELS: u32 = 2048;
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, PartialEq, Eq)]
struct Revision {
    path: PathBuf,
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Revision {
    fn read(root: &Path, rel: &str) -> anyhow::Result<Self> {
        let path = reader_files::checked_path(root, rel)?;
        let meta = std::fs::metadata(&path)?;
        anyhow::ensure!(meta.is_file(), "Not a regular file");
        Ok(Self {
            path,
            device: meta.dev(),
            inode: meta.ino(),
            size: meta.len(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            changed: (meta.ctime(), meta.ctime_nsec()),
        })
    }
}

pub fn eligible(rel: &str) -> bool {
    let ext = Path::new(rel)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    // Keep vector fidelity and animation in the existing image renderer.
    // HEIC/HEIF use the image viewer's bounded decoder on every platform, so
    // its live refresh and decoder messages reach the preview (#3064).
    !matches!(
        ext.as_str(),
        "svg" | "gif" | "webp" | "apng" | "heic" | "heif"
    ) && !okilum_core::excalidraw::is_drawing(rel)
}

enum State {
    Loading,
    Ready(Arc<RenderImage>),
    Unavailable,
}

pub struct Thumbnail {
    state: State,
    _task: Option<Task<()>>,
}
impl Thumbnail {
    pub fn new(root: PathBuf, rel: String, cx: &mut Context<Self>) -> Self {
        let renderer = cx.svg_renderer();
        let start_thumbnail = cx
            .try_global::<crate::hooks::ViewerHooks>()
            .and_then(|h| h.start_thumbnail);
        let task = cx.spawn(async move |this, cx| {
            let source_root = root.clone();
            let source_rel = rel.clone();
            let revision = cx
                .background_executor()
                .spawn(async move { Revision::read(&source_root, &source_rel) })
                .await;
            let result = async {
                let before = revision?;
                let request = match start_thumbnail {
                    Some(start) => start(&before.path)?,
                    None => anyhow::bail!("Thumbnails are only provided on macOS"),
                };
                let started = std::time::Instant::now();
                let png = loop {
                    if let Some(png) = request.poll()? {
                        break png;
                    }
                    anyhow::ensure!(started.elapsed() < TIMEOUT, "Thumbnail timed out");
                    cx.background_executor()
                        .timer(Duration::from_millis(50))
                        .await;
                };
                drop(request);
                cx.background_executor()
                    .spawn(async move {
                        anyhow::ensure!(before == Revision::read(&root, &rel)?, "File changed");
                        // Validate geometry before GPUI allocates a decoded frame.
                        let decoder = image::ImageReader::with_format(
                            std::io::Cursor::new(&png),
                            image::ImageFormat::Png,
                        );
                        let (width, height) = decoder.into_dimensions()?;
                        anyhow::ensure!(
                            width > 0 && height > 0 && width <= MAX_PIXELS && height <= MAX_PIXELS,
                            "Thumbnail dimensions exceed the limit"
                        );
                        let image =
                            Image::from_bytes(ImageFormat::Png, png).to_image_data(renderer)?;
                        anyhow::ensure!(before == Revision::read(&root, &rel)?, "File changed");
                        Ok(image)
                    })
                    .await
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                this.state = match result {
                    Ok(image) => State::Ready(image),
                    Err(_) => State::Unavailable,
                };
                cx.notify();
            });
        });
        Self {
            state: State::Loading,
            _task: Some(task),
        }
    }
}
impl Render for Thumbnail {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.state {
            State::Ready(image) => {
                reader_image::ReaderImage::new(image.clone().into()).into_any_element()
            }
            State::Loading => div()
                .id("file-preview-loading")
                .p_6()
                .text_color(cx.theme().muted_foreground)
                .child("Loading preview…")
                .into_any_element(),
            State::Unavailable => v_flex()
                .id("file-preview-unavailable")
                .p_6()
                .gap_3()
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(IconName::File).size(px(48.)))
                .child("Preview unavailable. Press Space for Quick Look, or open the file.")
                .into_any_element(),
        };
        div().w_full().flex_none().child(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn preview_states_fit_the_column_and_release_on_navigation(cx: &mut TestAppContext) {
        struct Host {
            preview: Option<Entity<Thumbnail>>,
            width: f32,
        }
        impl Render for Host {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
                    .id("thumbnail-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .flex_none()
                            .w(px(self.width))
                            .debug_selector(|| "thumbnail-box".into())
                            .children(self.preview.clone()),
                    )
            }
        }
        cx.update(crate::test_init);
        let (host, visual) = cx.add_window_view(|_, cx| Host {
            preview: Some(
                cx.new(|cx| Thumbnail::new(PathBuf::from("/missing"), "file.pdf".into(), cx)),
            ),
            width: 640.,
        });
        // Cancel the real task; supply controlled completed states to test layout.
        let preview = host.read_with(visual, |host, _| host.preview.clone().unwrap());
        preview.update(visual, |preview, cx| {
            preview._task = None;
            preview.state = State::Loading;
            cx.notify();
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(visual.debug_bounds("thumbnail-box").unwrap().size.height > px(20.));
        let image = Arc::new(RenderImage::new(vec![image::Frame::new(
            image::RgbaImage::from_pixel(1200, 1600, image::Rgba([80, 100, 180, 255])),
        )]));
        preview.update(visual, |preview, cx| {
            preview.state = State::Ready(image);
            cx.notify();
        });
        for width in [640., 280., 920.] {
            host.update(visual, |host, cx| {
                host.width = width;
                cx.notify();
            });
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
            let bounds = visual.debug_bounds("thumbnail-box").unwrap();
            assert!((bounds.size.width - px(width)).abs() < px(1.));
            assert!(
                (bounds.size.height - px(width * 1600. / 1200.)).abs() < px(1.),
                "width={width}, bounds={bounds:?}"
            );
        }
        preview.update(visual, |preview, cx| {
            preview.state = State::Unavailable;
            cx.notify();
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(visual.debug_bounds("thumbnail-box").unwrap().size.height > px(48.));
        let weak = preview.downgrade();
        drop(preview);
        host.update(visual, |host, cx| {
            host.preview = None;
            cx.notify();
        });
        visual.run_until_parked();
        assert!(
            weak.upgrade().is_none(),
            "leaving a file releases its thumbnail/task"
        );
    }

    #[test]
    fn revision_rejects_replace_change_and_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let path = root.join("file.pdf");
        std::fs::write(&path, b"before").unwrap();
        let before = Revision::read(&root, "file.pdf").unwrap();
        assert_eq!(before, Revision::read(&root, "file.pdf").unwrap());
        std::fs::write(root.join("replacement"), b"after!").unwrap();
        std::fs::rename(root.join("replacement"), &path).unwrap();
        assert_ne!(before, Revision::read(&root, "file.pdf").unwrap());
        std::fs::remove_file(&path).unwrap();
        assert!(Revision::read(&root, "file.pdf").is_err());
        std::fs::write(dir.path().join("outside.pdf"), b"outside").unwrap();
        std::os::unix::fs::symlink(dir.path().join("outside.pdf"), &path).unwrap();
        assert!(Revision::read(&root, "file.pdf").is_err());
        assert!(eligible("report.pdf"));
        assert!(eligible("deck.pptx"));
        assert!(!eligible("animated.GIF"));
        assert!(!eligible("diagram.svg"));
        assert!(!eligible("photo.HEIC"));
        assert!(!eligible("photo.heif"));
    }
}
