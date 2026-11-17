//! Windows: the executable's associated icon, read from its resources without
//! launching it, drawn at menu size into a 32-bit DIB.
use gpui::RenderImage;
use std::{os::windows::ffi::OsStrExt, path::Path, sync::Arc};
use windows_sys::Win32::{
    Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, SelectObject,
        BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    },
    UI::{
        Shell::SHDefExtractIconW,
        WindowsAndMessaging::{DestroyIcon, DrawIconEx, DI_NORMAL, HICON},
    },
};

/// 16 px at up to 4× scale.
const SIDE: i32 = 64;

pub(super) fn load(path: &Path) -> Option<Arc<RenderImage>> {
    if !path.is_file() {
        return None;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call; the icon handle
    // is owned here and destroyed exactly once.
    let icon = unsafe {
        let mut icon: HICON = std::ptr::null_mut();
        let size = SIDE as u32 | (16 << 16);
        let hr = SHDefExtractIconW(wide.as_ptr(), 0, 0, &mut icon, std::ptr::null_mut(), size);
        if hr != 0 || icon.is_null() {
            return None;
        }
        icon
    };
    // Drawing over black and over white recovers alpha for 32-bit icons and
    // for legacy AND-mask icons alike.
    let black = unsafe { draw(icon, 0) };
    let white = unsafe { draw(icon, 255) };
    unsafe { DestroyIcon(icon) };
    let pixels = unmix(black?, &white?);
    // DIB memory is BGRA, which is what RenderImage stores.
    let buffer = image::RgbaImage::from_raw(SIDE as u32, SIDE as u32, pixels)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

/// Straight BGRA from the same icon composited over black and over white.
fn unmix(mut black: Vec<u8>, white: &[u8]) -> Vec<u8> {
    for (pixel, white) in black.chunks_exact_mut(4).zip(white.chunks_exact(4)) {
        let alpha = 255u8.saturating_sub(white[1].saturating_sub(pixel[1]));
        for channel in &mut pixel[..3] {
            *channel = if alpha == 0 {
                0
            } else {
                ((u32::from(*channel) * 255) / u32::from(alpha)).min(255) as u8
            };
        }
        pixel[3] = alpha;
    }
    black
}

/// SAFETY: `icon` must be a live icon handle. Every GDI object created here
/// is released, and the original bitmap is selected back before its DC goes.
unsafe fn draw(icon: HICON, background: u8) -> Option<Vec<u8>> {
    unsafe {
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        if dc.is_null() {
            return None;
        }
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = SIDE;
        info.bmiHeader.biHeight = -SIDE; // top-down
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(
            dc,
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        if bitmap.is_null() || bits.is_null() {
            if !bitmap.is_null() {
                DeleteObject(bitmap);
            }
            DeleteDC(dc);
            return None;
        }
        let previous = SelectObject(dc, bitmap);
        let len = (SIDE * SIDE * 4) as usize;
        let pixels = std::slice::from_raw_parts_mut(bits.cast::<u8>(), len);
        pixels.fill(background);
        let ok = DrawIconEx(
            dc,
            0,
            0,
            icon,
            SIDE,
            SIDE,
            0,
            std::ptr::null_mut(),
            DI_NORMAL,
        ) != 0;
        GdiFlush();
        let result = ok.then(|| pixels.to_vec());
        SelectObject(dc, previous);
        DeleteObject(bitmap);
        DeleteDC(dc);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmix_recovers_alpha_and_straight_color() {
        // Opaque red, half-transparent white, fully transparent.
        let black = vec![0, 0, 255, 0, 128, 128, 128, 0, 0, 0, 0, 0];
        let white = [0, 0, 255, 0, 255, 255, 255, 0, 255, 255, 255, 0];
        assert_eq!(
            unmix(black, &white),
            [0, 0, 255, 255, 255, 255, 255, 128, 0, 0, 0, 0]
        );
    }

    #[test]
    fn an_executable_icon_loads_and_a_file_without_one_falls_back() {
        let windir = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        let notepad = Path::new(&windir).join("System32").join("notepad.exe");
        let icon = load(&notepad).expect("positive control: notepad.exe has an icon");
        assert_eq!(u32::from(icon.size(0).width), SIDE as u32);
        assert!(
            icon.as_bytes(0)
                .unwrap()
                .chunks_exact(4)
                .any(|p| p[3] == 255),
            "the icon is drawn, not blank"
        );
        let temp = tempfile::tempdir().unwrap();
        let plain = temp.path().join("Editor.exe");
        std::fs::write(&plain, b"not a PE file").unwrap();
        assert!(load(&plain).is_none(), "no icon resource: neutral fallback");
        assert!(load(&temp.path().join("Missing.exe")).is_none());
    }
}
