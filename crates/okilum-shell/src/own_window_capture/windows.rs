use super::*;
use windows_sys::Win32::{
    Foundation::{HWND, RECT},
    Graphics::Gdi::*,
    Storage::Xps::PrintWindow,
    System::Threading::{GetCurrentProcessId, GetCurrentThreadId},
    UI::WindowsAndMessaging::*,
};

pub(super) fn capture() -> Result<PngBytes, Unsupported> {
    unsafe { capture_window(GetForegroundWindow()) }
}
unsafe fn capture_window(hwnd: HWND) -> Result<PngBytes, Unsupported> {
    let mut pid = 0;
    if hwnd.is_null()
        || GetWindowThreadProcessId(hwnd, &mut pid) != GetCurrentThreadId()
        || pid != GetCurrentProcessId()
        || IsWindowVisible(hwnd) == 0
        || IsIconic(hwnd) != 0
    {
        return Err(Unsupported::NoOwnWindow);
    }
    let mut rect: RECT = std::mem::zeroed();
    if GetClientRect(hwnd, &mut rect) == 0 {
        return Err(Unsupported::CaptureFailed);
    }
    let (w, h) = (rect.right as u32, rect.bottom as u32);
    let len = dimensions(w, h)?;
    let dc = GetDC(hwnd);
    if dc.is_null() {
        return Err(Unsupported::CaptureFailed);
    }
    let memory = CreateCompatibleDC(dc);
    if memory.is_null() {
        ReleaseDC(hwnd, dc);
        return Err(Unsupported::CaptureFailed);
    }
    let mut info: BITMAPINFO = std::mem::zeroed();
    info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biWidth = w as i32;
    info.bmiHeader.biHeight = -(h as i32);
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB;
    let mut data = std::ptr::null_mut();
    let bitmap = CreateDIBSection(
        memory,
        &info,
        DIB_RGB_COLORS,
        &mut data,
        std::ptr::null_mut(),
        0,
    );
    if bitmap.is_null() || data.is_null() {
        DeleteDC(memory);
        ReleaseDC(hwnd, dc);
        return Err(Unsupported::CaptureFailed);
    }
    // Distinguish an unpainted buffer from legitimate black content. The alpha
    // byte is deliberately nonstandard too; normal GDI writes replace RGB.
    const UNPAINTED: [u8; 4] = [0xa5, 0x5a, 0xc3, 0x7e];
    for pixel in std::slice::from_raw_parts_mut(data.cast::<u8>(), len)
        .as_chunks_mut::<4>()
        .0
    {
        *pixel = UNPAINTED;
    }
    let old = SelectObject(memory, bitmap);
    // PW_CLIENTONLY | PW_RENDERFULLCONTENT: never reads the desktop DC.
    let mut before = 0;
    GetWindowThreadProcessId(hwnd, &mut before);
    let ok = before == GetCurrentProcessId() && PrintWindow(hwnd, memory, 1 | 2) != 0;
    let pixels = if ok {
        std::slice::from_raw_parts(data.cast::<u8>(), len).to_vec()
    } else {
        Vec::new()
    };
    SelectObject(memory, old);
    DeleteObject(bitmap);
    DeleteDC(memory);
    ReleaseDC(hwnd, dc);
    if !ok {
        return Err(Unsupported::CaptureFailed);
    }
    if pixels.as_chunks::<4>().0.iter().any(|p| *p == UNPAINTED) {
        return Err(Unsupported::CaptureFailed);
    }
    let mut rgba = pixels;
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
    encode(w, h, &rgba)
}

#[cfg(test)]
mod tests {
    use super::*;
    unsafe extern "system" fn procedure(hwnd: HWND, message: u32, w: usize, l: isize) -> isize {
        if message == WM_PRINTCLIENT {
            let mut rect: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut rect);
            let mode = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if mode == 2 {
                return 0;
            }
            let brush = CreateSolidBrush(if mode == 1 { 0 } else { 0x00563412 });
            FillRect(w as _, &rect, brush);
            DeleteObject(brush);
            return 0;
        }
        DefWindowProcW(hwnd, message, w, l)
    }
    #[test]
    fn native_printwindow_pixels_and_no_window_rejection() {
        unsafe {
            let name: Vec<u16> = "OkilumOwnCaptureControl\0".encode_utf16().collect();
            let class = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                lpszClassName: name.as_ptr(),
                ..std::mem::zeroed()
            };
            assert_ne!(RegisterClassW(&class), 0);
            let hwnd = CreateWindowExW(
                0,
                name.as_ptr(),
                name.as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                0,
                0,
                160,
                120,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            assert!(!hwnd.is_null());
            let result = capture_window(hwnd);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 1);
            let black = capture_window(hwnd).expect("legitimate black content must be accepted");
            assert!(image::load_from_memory(&black)
                .unwrap()
                .to_rgb8()
                .pixels()
                .all(|p| p.0 == [0, 0, 0]));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 2);
            assert_eq!(
                capture_window(hwnd),
                Err(Unsupported::CaptureFailed),
                "successful PrintWindow without drawing is not a capture"
            );
            DestroyWindow(hwnd);
            UnregisterClassW(name.as_ptr(), std::ptr::null_mut());
            let decoded =
                image::load_from_memory(&result.expect("native PrintWindow positive control"))
                    .unwrap()
                    .to_rgba8();
            assert_eq!(decoded.get_pixel(10, 10).0, [0x12, 0x34, 0x56, 255]);
            assert_eq!(
                capture_window(std::ptr::null_mut()),
                Err(Unsupported::NoOwnWindow)
            );
            let desktop = GetDesktopWindow();
            assert_eq!(capture_window(desktop), Err(Unsupported::NoOwnWindow));
        }
    }
}
