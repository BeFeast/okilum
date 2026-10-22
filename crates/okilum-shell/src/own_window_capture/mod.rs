//! Explicit, in-memory capture of the calling application's foreground window.
//!
//! Call on the UI thread, before opening an attachment picker. Never captures a
//! display and crops it. Unsupported means the caller must offer attachments.
//! No permission prompt, clipboard operation, file write or upload is performed.
#[cfg(any(target_os = "linux", windows, test))]
use image::{codecs::png::PngEncoder, ImageEncoder};

pub type PngBytes = Vec<u8>;
pub const MAX_PNG_BYTES: usize = 8 * 1024 * 1024;
#[cfg(any(target_os = "linux", windows, test))]
const MAX_PIXELS: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsupported {
    WrongThread,
    Backend,
    NoOwnWindow,
    CaptureFailed,
    TooLarge,
    /// Screen Recording is never requested by this module.
    PermissionOrUnavailable,
}
impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::WrongThread => "Window capture must be called on the application UI thread.",
            Self::Backend => "Window capture is unavailable on this backend; attach an image instead.",
            Self::NoOwnWindow => "No foreground window belonging to this application is available.",
            Self::CaptureFailed => "The application window could not be captured; attach an image instead.",
            Self::TooLarge => "The window image exceeds the attachment size limit.",
            Self::PermissionOrUnavailable => "Own-window capture is unavailable or requires Screen Recording permission; attach an image instead.",
        })
    }
}
impl std::error::Error for Unsupported {}

#[cfg(any(target_os = "linux", windows, test))]
fn dimensions(width: u32, height: u32) -> Result<usize, Unsupported> {
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or(Unsupported::TooLarge)?;
    if pixels == 0 || pixels > MAX_PIXELS {
        return Err(Unsupported::TooLarge);
    }
    pixels.checked_mul(4).ok_or(Unsupported::TooLarge)
}
#[cfg(any(target_os = "linux", windows, test))]
fn encode(width: u32, height: u32, rgba: &[u8]) -> Result<PngBytes, Unsupported> {
    if dimensions(width, height)? != rgba.len() {
        return Err(Unsupported::CaptureFailed);
    }
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|_| Unsupported::CaptureFailed)?;
    if png.len() > MAX_PNG_BYTES {
        return Err(Unsupported::TooLarge);
    }
    Ok(png)
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

pub fn capture_own_window() -> Result<PngBytes, Unsupported> {
    #[cfg(target_os = "linux")]
    {
        linux::capture()
    }
    #[cfg(target_os = "macos")]
    {
        macos::capture()
    }
    #[cfg(windows)]
    {
        windows::capture()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        Err(Unsupported::Backend)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn positive_png_control_preserves_pixels() {
        let rgba = [240, 12, 33, 255, 7, 199, 81, 255];
        let png = encode(2, 1, &rgba).unwrap();
        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (2, 1));
        assert_eq!(decoded.as_raw(), &rgba);
    }
    #[test]
    fn rejects_invalid_and_unbounded_images() {
        assert_eq!(dimensions(0, 1), Err(Unsupported::TooLarge));
        assert_eq!(dimensions(u32::MAX, u32::MAX), Err(Unsupported::TooLarge));
        assert_eq!(encode(2, 2, &[0; 4]), Err(Unsupported::CaptureFailed));
    }
}
