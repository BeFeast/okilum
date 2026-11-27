//! macOS Quick Look thumbnail FFI bridge (#3238).
pub(crate) use okilum_shell_viewers::reader_thumbnail::*;

#[cfg(target_os = "macos")]
pub(crate) mod native {
    use okilum_shell_viewers::hooks::ThumbnailRequest;
    use std::{
        ffi::{c_char, c_void, CString},
        os::unix::ffi::OsStrExt,
        path::Path,
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

    pub(crate) struct Request(*mut c_void);
    unsafe impl Send for Request {}

    impl ThumbnailRequest for Request {
        fn poll(&self) -> anyhow::Result<Option<Vec<u8>>> {
            let mut bytes = std::ptr::null_mut();
            let mut length = 0;
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
            unsafe { okilum_thumbnail_cancel(self.0) };
        }
    }

    pub(crate) fn start(path: &Path) -> anyhow::Result<Box<dyn ThumbnailRequest>> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        let handle = unsafe { okilum_thumbnail_start(path.as_ptr()) };
        anyhow::ensure!(!handle.is_null(), "Cannot request thumbnail");
        Ok(Box::new(Request(handle)))
    }
}

#[cfg(all(test, target_os = "macos"))]
mod native_tests {
    use super::native;
    use ::core::prelude::v1::test;
    use okilum_shell_viewers::hooks::ThumbnailRequest as _;
    use std::path::Path;
    use std::time::Duration;

    const MAX_PIXELS: u32 = 2048;
    const TIMEOUT: Duration = Duration::from_secs(10);

    fn generate(path: &Path) -> anyhow::Result<Vec<u8>> {
        let request = native::start(path)?;
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
            drop(native::start(&pdf).unwrap());
        }
        assert!(generate(&pdf).is_ok());
        let bad = directory.path().join("broken.pdf");
        std::fs::write(&bad, b"not a PDF").unwrap();
        assert!(generate(&bad).is_err());
    }
}
