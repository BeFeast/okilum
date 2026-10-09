//! A selection-owned bitmap: dropping the view cancels native provider work.
use super::*;
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

pub(super) fn eligible(rel: &str) -> bool {
    let ext = Path::new(rel)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    // Keep vector fidelity and animation in the existing image renderer.
    !matches!(ext.as_str(), "svg" | "gif" | "webp" | "apng")
        && !okilum_core::excalidraw::is_drawing(rel)
}

enum State {
    Loading,
    Ready(Arc<RenderImage>),
    Unavailable,
}

pub(super) struct Thumbnail {
    state: State,
    _task: Option<Task<()>>,
}
impl Thumbnail {
    pub(super) fn new(root: PathBuf, rel: String, cx: &mut Context<Self>) -> Self {
        let renderer = cx.svg_renderer();
        let task = cx.spawn(async move |this, cx| {
            let source_root = root.clone();
            let source_rel = rel.clone();
            let revision = cx
                .background_executor()
                .spawn(async move { Revision::read(&source_root, &source_rel) })
                .await;
            let result = async {
                let before = revision?;
                let request = native::Request::start(&before.path)?;
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

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use std::{
        ffi::{c_char, c_void, CString},
        os::unix::ffi::OsStrExt,
    };

    unsafe extern "C" {
        fn okilum_thumbnail_start(path: *const c_char) -> *mut c_void;
        fn okilum_thumbnail_poll(
            handle: *mut c_void,
            bytes: *mut *mut u8,
            length: *mut usize,
        ) -> i32;
        fn okilum_thumbnail_free_bytes(bytes: *mut u8);
        fn okilum_thumbnail_cancel(handle: *mut c_void);
    }

    pub(super) struct Request(*mut c_void);
    impl Request {
        pub(super) fn start(path: &Path) -> anyhow::Result<Self> {
            let path = CString::new(path.as_os_str().as_bytes())?;
            // The bridge copies the path and returns one owned retain.
            let handle = unsafe { okilum_thumbnail_start(path.as_ptr()) };
            anyhow::ensure!(!handle.is_null(), "Cannot request thumbnail");
            Ok(Self(handle))
        }
        pub(super) fn poll(&self) -> anyhow::Result<Option<Vec<u8>>> {
            let mut bytes = std::ptr::null_mut();
            let mut length = 0;
            // No Rust callback or borrow escapes this call. The bridge lock
            // synchronizes provider completion; a successful poll transfers bytes.
            let status = unsafe { okilum_thumbnail_poll(self.0, &mut bytes, &mut length) };
            match status {
                0 => Ok(None),
                1 => {
                    let png = unsafe { std::slice::from_raw_parts(bytes, length) }.to_vec();
                    unsafe { okilum_thumbnail_free_bytes(bytes) };
                    Ok(Some(png))
                }
                _ => anyhow::bail!("Quick Look did not provide a thumbnail"),
            }
        }
    }
    impl Drop for Request {
        fn drop(&mut self) {
            // Exactly one release, including future cancellation and timeout.
            unsafe { okilum_thumbnail_cancel(self.0) };
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod native {
    use super::*;
    pub(super) struct Request;
    impl Drop for Request {
        fn drop(&mut self) {}
    }
    impl Request {
        pub(super) fn start(_: &Path) -> anyhow::Result<Self> {
            anyhow::bail!("macOS only")
        }
        pub(super) fn poll(&self) -> anyhow::Result<Option<Vec<u8>>> {
            unreachable!()
        }
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
        cx.update(gpui_component::init);
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
    }
}

#[cfg(all(test, target_os = "macos"))]
mod native_tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn generate(path: &Path) -> anyhow::Result<Vec<u8>> {
        let request = native::Request::start(path)?;
        let start = std::time::Instant::now();
        loop {
            if let Some(png) = request.poll()? {
                return Ok(png);
            }
            anyhow::ensure!(start.elapsed() < TIMEOUT, "Native thumbnail timed out");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn pdf_office_image_and_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        let pdf = directory.path().join("page.pdf");
        let office = directory.path().join("document.docx");
        let photo = directory.path().join("image.png");
        std::fs::write(&pdf, include_bytes!("../tests/fixtures/thumbnail/page.pdf")).unwrap();
        std::fs::write(
            &office,
            include_bytes!("../tests/fixtures/thumbnail/document.docx"),
        )
        .unwrap();
        image::RgbaImage::from_pixel(1600, 900, image::Rgba([40, 100, 190, 255]))
            .save(&photo)
            .unwrap();
        for path in [&pdf, &office, &photo] {
            let png = generate(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            if let Ok(output) = std::env::var("OKILUM_THUMBNAIL_EVIDENCE_DIR") {
                std::fs::create_dir_all(&output).unwrap();
                std::fs::write(
                    Path::new(&output)
                        .join(path.file_name().unwrap())
                        .with_extension("png"),
                    &png,
                )
                .unwrap();
            }
            let decoded = image::load_from_memory(&png).unwrap();
            assert!(
                decoded.width() > 512 && decoded.height() > 512,
                "real high-resolution thumbnail, not an icon: {}x{}",
                decoded.width(),
                decoded.height()
            );
            assert!(decoded.width() <= MAX_PIXELS && decoded.height() <= MAX_PIXELS);
            if path == &pdf {
                let pixels = decoded.to_rgba8();
                let center = pixels.get_pixel(pixels.width() / 2, pixels.height() / 2);
                assert!(
                    center[2] > center[0].saturating_add(40),
                    "PDF must show its blue first page, not its red second page: {center:?}"
                );
            }
        }
        // Cancellation must not leave a dangling Rust callback or prevent the
        // next selection from succeeding. The last request is the positive control.
        for _ in 0..8 {
            drop(native::Request::start(&pdf).unwrap());
        }
        assert!(generate(&pdf).is_ok());
        let bad = directory.path().join("broken.pdf");
        std::fs::write(&bad, b"not a PDF").unwrap();
        assert!(generate(&bad).is_err());
    }
}
