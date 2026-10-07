//! A selection-owned bitmap: dropping the view cancels decoding or native
//! provider work. Ordinary raster images are decoded in-process, bounded and
//! downscaled off the UI thread; other formats use Quick Look on macOS.
use super::*;
use image::{AnimationDecoder, ImageDecoder};
use std::io::BufReader;

/// Longest edge of a decoded preview: the Reader column on a 2x display.
const MAX_PIXELS: u32 = 2048;
/// Sources beyond these bounds are refused before a full-size frame exists.
const MAX_SOURCE_EDGE: u32 = 20_000;
const MAX_SOURCE_PIXELS: u64 = 100_000_000;
const MAX_DECODE_ALLOC: u64 = 512 * 1024 * 1024;
/// Downscaled animation budget; past it only the first frame is shown.
const MAX_ANIMATION_BYTES: usize = 256 * 1024 * 1024;
const MAX_ANIMATION_FRAMES: usize = 1_000;
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, PartialEq, Eq)]
struct Revision {
    path: PathBuf,
    size: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}
impl Revision {
    fn read(root: &Path, rel: &str) -> anyhow::Result<Self> {
        let path = reader_files::checked_path(root, rel)?;
        let meta = std::fs::metadata(&path)?;
        anyhow::ensure!(meta.is_file(), "Not a regular file");
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec())
        };
        Ok(Self {
            path,
            size: meta.len(),
            modified: meta.modified().ok(),
            #[cfg(unix)]
            identity,
        })
    }
}

/// Raster formats Tessera decodes itself on every platform, so an ordinary
/// photo never depends on a thumbnail provider.
pub(super) fn raster(rel: &str) -> bool {
    let ext = Path::new(rel)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp"
    )
}

pub(super) fn eligible(rel: &str) -> bool {
    if raster(rel) {
        return true;
    }
    let ext = Path::new(rel)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    // Keep vector fidelity in the existing image renderer.
    cfg!(target_os = "macos")
        && !matches!(ext.as_str(), "svg" | "apng")
        && !tessera_core::excalidraw::is_drawing(rel)
}

fn fit(width: u32, height: u32) -> (u32, u32) {
    let long = width.max(height);
    if long <= MAX_PIXELS {
        return (width, height);
    }
    let scale = |side: u32| ((side as u64 * MAX_PIXELS as u64 / long as u64) as u32).max(1);
    (scale(width), scale(height))
}

/// GPUI frames are BGRA; the delay keeps animation timing.
fn frame(image: image::RgbaImage, delay: image::Delay) -> image::Frame {
    let (width, height) = fit(image.width(), image.height());
    let mut image = if (width, height) == image.dimensions() {
        image
    } else {
        image::imageops::thumbnail(&image, width, height)
    };
    for pixel in image.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    image::Frame::from_parts(image, 0, 0, delay)
}

fn limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_EDGE);
    limits.max_image_height = Some(MAX_SOURCE_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    limits
}

fn check_geometry(decoder: &impl ImageDecoder) -> anyhow::Result<()> {
    let (width, height) = decoder.dimensions();
    anyhow::ensure!(
        width > 0 && height > 0 && width as u64 * height as u64 <= MAX_SOURCE_PIXELS,
        "Image dimensions exceed the limit"
    );
    Ok(())
}

fn animation<'a>(decoder: impl AnimationDecoder<'a>) -> anyhow::Result<Vec<image::Frame>> {
    let mut frames = Vec::new();
    let mut bytes = 0;
    for source in decoder.into_frames() {
        let source = source?;
        let delay = source.delay();
        let next = frame(source.into_buffer(), delay);
        bytes += next.buffer().len();
        frames.push(next);
        if bytes > MAX_ANIMATION_BYTES || frames.len() > MAX_ANIMATION_FRAMES {
            frames.truncate(1);
            break;
        }
    }
    anyhow::ensure!(!frames.is_empty(), "Image has no frames");
    Ok(frames)
}

/// Decodes by content, not extension; corrupt or oversized input is an error
/// the caller turns into the file card.
fn decode_raster(path: &Path) -> anyhow::Result<RenderImage> {
    let open = || -> anyhow::Result<BufReader<std::fs::File>> {
        Ok(BufReader::new(std::fs::File::open(path)?))
    };
    let format = image::ImageReader::new(open()?)
        .with_guessed_format()?
        .format()
        .ok_or_else(|| anyhow::anyhow!("Unrecognised image format"))?;
    let frames = match format {
        image::ImageFormat::Gif => {
            let mut decoder = image::codecs::gif::GifDecoder::new(open()?)?;
            decoder.set_limits(limits())?;
            check_geometry(&decoder)?;
            animation(decoder)?
        }
        image::ImageFormat::WebP => {
            let mut decoder = image::codecs::webp::WebPDecoder::new(open()?)?;
            decoder.set_limits(limits())?;
            check_geometry(&decoder)?;
            if decoder.has_animation() {
                animation(decoder)?
            } else {
                still(decoder)?
            }
        }
        format => {
            let mut reader = image::ImageReader::with_format(open()?, format);
            reader.limits(limits());
            let decoder = reader.into_decoder()?;
            check_geometry(&decoder)?;
            still(decoder)?
        }
    };
    Ok(RenderImage::new(frames))
}

fn still(mut decoder: impl ImageDecoder) -> anyhow::Result<Vec<image::Frame>> {
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut image = image::DynamicImage::from_decoder(decoder)?;
    // Shrink before converting so a large photo never exists twice at full size.
    let (width, height) = fit(image.width(), image.height());
    if (width, height) != (image.width(), image.height()) {
        image = image.thumbnail(width, height);
    }
    // Phone photos store rotation as EXIF; show them the way they were taken.
    image.apply_orientation(orientation);
    Ok(vec![frame(
        image.into_rgba8(),
        image::Delay::from_numer_denom_ms(0, 1),
    )])
}

enum State {
    Loading,
    Ready(Arc<RenderImage>),
    Unavailable,
}

pub(super) struct Thumbnail {
    state: State,
    card: reader_files::FileCard,
    _task: Option<Task<()>>,
}
impl Thumbnail {
    pub(super) fn new(
        root: PathBuf,
        rel: String,
        card: reader_files::FileCard,
        cx: &mut Context<Self>,
    ) -> Self {
        let task = if raster(&rel) {
            Self::decode(root, rel, cx)
        } else {
            Self::native(root, rel, cx)
        };
        Self {
            state: State::Loading,
            card,
            _task: Some(task),
        }
    }

    fn finish(this: WeakEntity<Self>, result: anyhow::Result<Arc<RenderImage>>, cx: &mut AsyncApp) {
        let _ = this.update(cx, |this, cx| {
            this.state = match result {
                Ok(image) => State::Ready(image),
                Err(_) => State::Unavailable,
            };
            cx.notify();
        });
    }

    fn decode(root: PathBuf, rel: String, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let before = Revision::read(&root, &rel)?;
                    let image = decode_raster(&before.path)?;
                    // A file replaced mid-decode must not show a mix of revisions.
                    anyhow::ensure!(before == Revision::read(&root, &rel)?, "File changed");
                    Ok(Arc::new(image))
                })
                .await;
            Self::finish(this, result, cx);
        })
    }

    fn native(root: PathBuf, rel: String, cx: &mut Context<Self>) -> Task<()> {
        let renderer = cx.svg_renderer();
        cx.spawn(async move |this, cx| {
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
            Self::finish(this, result, cx);
        })
    }
}
impl Render for Thumbnail {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.state {
            State::Ready(image) => v_flex()
                .gap_3()
                .child(reader_image::ReaderImage::new(image.clone().into()))
                .child(self.card.render_details(cx))
                .into_any_element(),
            State::Loading => div()
                .id("file-preview-loading")
                .py_6()
                .text_color(cx.theme().muted_foreground)
                .child("Loading preview…")
                .into_any_element(),
            State::Unavailable => self.card.render(cx),
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
        fn tessera_thumbnail_start(path: *const c_char) -> *mut c_void;
        fn tessera_thumbnail_poll(
            handle: *mut c_void,
            bytes: *mut *mut u8,
            length: *mut usize,
        ) -> i32;
        fn tessera_thumbnail_free_bytes(bytes: *mut u8);
        fn tessera_thumbnail_cancel(handle: *mut c_void);
    }

    pub(super) struct Request(*mut c_void);
    impl Request {
        pub(super) fn start(path: &Path) -> anyhow::Result<Self> {
            let path = CString::new(path.as_os_str().as_bytes())?;
            // The bridge copies the path and returns one owned retain.
            let handle = unsafe { tessera_thumbnail_start(path.as_ptr()) };
            anyhow::ensure!(!handle.is_null(), "Cannot request thumbnail");
            Ok(Self(handle))
        }
        pub(super) fn poll(&self) -> anyhow::Result<Option<Vec<u8>>> {
            let mut bytes = std::ptr::null_mut();
            let mut length = 0;
            // No Rust callback or borrow escapes this call. The bridge lock
            // synchronizes provider completion; a successful poll transfers bytes.
            let status = unsafe { tessera_thumbnail_poll(self.0, &mut bytes, &mut length) };
            match status {
                0 => Ok(None),
                1 => {
                    let png = unsafe { std::slice::from_raw_parts(bytes, length) }.to_vec();
                    unsafe { tessera_thumbnail_free_bytes(bytes) };
                    Ok(Some(png))
                }
                _ => anyhow::bail!("Quick Look did not provide a thumbnail"),
            }
        }
    }
    impl Drop for Request {
        fn drop(&mut self) {
            // Exactly one release, including future cancellation and timeout.
            unsafe { tessera_thumbnail_cancel(self.0) };
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
            preview: Some(cx.new(|cx| {
                Thumbnail::new(PathBuf::from("/missing"), "file.pdf".into(), card(), cx)
            })),
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
            // The image fills the column; one details line follows it.
            let image = px(width * 1600. / 1200.);
            assert!(
                bounds.size.height > image + px(8.) && bounds.size.height < image + px(48.),
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

    fn card() -> reader_files::FileCard {
        reader_files::FileCard {
            kind: "JPG".into(),
            size: 1_900_000,
            modified: None,
        }
    }

    /// Deterministic noise, so the JPEG cannot compress far below ~2 MB.
    fn photo(path: &Path, width: u32, height: u32) {
        let mut seed = 0x2545_f491_u32;
        let image = image::RgbImage::from_fn(width, height, |x, y| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let base = ((x / 40 + y / 40) % 2 * 120) as u8;
            image::Rgb([
                base.wrapping_add(seed as u8 % 40),
                (seed >> 8) as u8 % 40,
                200u8.wrapping_sub((seed >> 16) as u8 % 40),
            ])
        });
        let file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
        image::codecs::jpeg::JpegEncoder::new_with_quality(file, 80)
            .encode_image(&image)
            .unwrap();
    }

    fn load(cx: &mut TestAppContext, root: &Path, rel: &str) -> Entity<Thumbnail> {
        let preview = cx.new(|cx| Thumbnail::new(root.to_path_buf(), rel.into(), card(), cx));
        // Decoding is background work: the entity starts out loading.
        assert!(preview.read_with(cx, |p, _| matches!(p.state, State::Loading)));
        cx.run_until_parked();
        preview
    }

    #[gpui::test]
    fn ordinary_photo_renders_inline_downscaled(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Photo.JPG");
        photo(&path, 3000, 2000);
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(
            (1_500_000..3_000_000).contains(&size),
            "fixture must be a ~2 MB photo, was {size} bytes"
        );
        assert!(eligible("Photo.JPG"));
        let preview = load(cx, dir.path(), "Photo.JPG");
        preview.read_with(cx, |preview, _| match &preview.state {
            State::Ready(image) => {
                let size = image.size(0);
                assert_eq!((size.width.0, size.height.0), (2048, 1365));
                // BGRA order: the dominant blue channel sits at index 0.
                let pixels = image.as_bytes(0).unwrap();
                assert!(pixels[0] > pixels[2], "{:?}", &pixels[..4]);
            }
            State::Loading => panic!("decode never completed"),
            State::Unavailable => panic!("a valid photo fell back to the card"),
        });
    }

    #[gpui::test]
    fn corrupt_photo_falls_back_to_the_card(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = Vec::new();
        photo(&dir.path().join("whole.jpg"), 800, 600);
        bytes.extend_from_slice(&std::fs::read(dir.path().join("whole.jpg")).unwrap()[..64]);
        bytes.extend(std::iter::repeat_n(0x5a, 4096));
        std::fs::write(dir.path().join("broken.jpg"), bytes).unwrap();
        std::fs::write(dir.path().join("text.png"), b"not an image").unwrap();
        for rel in ["broken.jpg", "text.png"] {
            let preview = load(cx, dir.path(), rel);
            preview.read_with(cx, |preview, _| {
                assert!(matches!(preview.state, State::Unavailable), "{rel}");
            });
        }
        // Positive control: the same harness decodes the intact source.
        let preview = load(cx, dir.path(), "whole.jpg");
        preview.read_with(cx, |preview, _| {
            assert!(matches!(preview.state, State::Ready(_)));
        });
    }

    #[test]
    fn animation_keeps_frames_and_still_images_fit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spinner.gif");
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut encoder = image::codecs::gif::GifEncoder::new(file);
            for shade in [0u8, 255] {
                encoder
                    .encode_frame(image::Frame::from_parts(
                        image::RgbaImage::from_pixel(64, 32, image::Rgba([shade, 0, 0, 255])),
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(100, 1),
                    ))
                    .unwrap();
            }
        }
        let image = decode_raster(&path).unwrap();
        assert_eq!(image.frame_count(), 2);
        assert_eq!(image.size(0).width.0, 64);
        assert_eq!(fit(4000, 1000), (2048, 512));
        assert_eq!(fit(1000, 9000), (227, 2048));
        assert_eq!(fit(20_000, 1), (2048, 1));
        assert_eq!(fit(640, 480), (640, 480));
    }

    #[cfg(unix)]
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
        assert_eq!(eligible("report.pdf"), cfg!(target_os = "macos"));
        assert_eq!(eligible("deck.pptx"), cfg!(target_os = "macos"));
        assert!(eligible("animated.GIF"));
        assert!(eligible("photo.jpeg"));
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
            if let Ok(output) = std::env::var("TESSERA_THUMBNAIL_EVIDENCE_DIR") {
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
