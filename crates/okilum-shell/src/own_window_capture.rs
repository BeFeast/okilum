//! Capture of one of Okilum's own windows for Feedback (#1502), Windows only.
//!
//! PrintWindow with PW_RENDERFULLCONTENT reads the window's own composed
//! content, the GPUI DirectComposition swap chain included. It never reads the
//! desktop or another application's window, needs no permission prompt and
//! writes nothing: the pixels go to the Feedback screenshot slot, where the
//! user can preview, remove or replace them before an explicit Send.
//! macOS and Linux keep attachment only (Oleg 18.11).

/// Raw pixels of a capture, encoded off the UI thread.
#[derive(Debug)]
pub(crate) struct Captured {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The handle is not a window of this process and UI thread.
    NotOwn,
    Minimized,
    TooLarge,
    /// PrintWindow failed or left the buffer unpainted or black.
    Failed,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotOwn => "The Okilum window is no longer open. Paste or drop a screenshot instead.",
            Self::Minimized => "Restore the Okilum window, then capture it again.",
            Self::TooLarge => "The Okilum window is too large to capture. Make it smaller, or paste a screenshot instead.",
            Self::Failed => "Couldn't capture the Okilum window. Use Win+Shift+S, then paste the screenshot here.",
        })
    }
}

/// The same pixel cap as the Feedback image decoder (16384² and 64 MiB).
const MAX_PIXELS: usize = 16 * 1024 * 1024;

/// Fill value written before PrintWindow. No ordinary write keeps this alpha
/// together with these colour bytes, so finding it means "never painted".
const UNPAINTED: [u8; 4] = [0xa5, 0x5a, 0xc3, 0x7e];

fn byte_len(width: u32, height: u32) -> Result<usize, Failure> {
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or(Failure::TooLarge)?;
    if pixels == 0 {
        return Err(Failure::Failed);
    }
    if pixels > MAX_PIXELS {
        return Err(Failure::TooLarge);
    }
    Ok(pixels * 4)
}

/// Turns the BGRA readback into opaque RGBA. A buffer that still holds the
/// fill value, or is black everywhere (what a failed composition readback
/// returns), is not a capture: an Okilum window always draws something.
fn finish(width: u32, height: u32, mut bgra: Vec<u8>) -> Result<Captured, Failure> {
    if bgra.len() != byte_len(width, height)? {
        return Err(Failure::Failed);
    }
    let (pixels, _) = bgra.as_chunks_mut::<4>();
    if pixels.contains(&UNPAINTED) || pixels.iter().all(|p| p[..3] == [0, 0, 0]) {
        return Err(Failure::Failed);
    }
    for pixel in pixels {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
    Ok(Captured {
        width,
        height,
        rgba: bgra,
    })
}

/// Encodes a capture as PNG; called on a background thread.
pub(crate) fn png(captured: Captured) -> anyhow::Result<Vec<u8>> {
    use image::ImageEncoder;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png).write_image(
        &captured.rgba,
        captured.width,
        captured.height,
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(png)
}

/// Captures the client area of `window`, a live GPUI window of this process.
#[cfg(windows)]
pub(crate) fn capture(window: &gpui::Window) -> Result<Captured, Failure> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return Err(Failure::NotOwn);
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err(Failure::NotOwn);
    };
    // SAFETY: a live handle borrowed on the GPUI UI thread; not retained.
    unsafe { native::capture_hwnd(handle.hwnd.get() as _) }
}

#[cfg(windows)]
mod native {
    use super::*;
    use windows_sys::Win32::{
        Foundation::{HWND, RECT},
        Graphics::Gdi::*,
        Storage::Xps::PrintWindow,
        System::Threading::{GetCurrentProcessId, GetCurrentThreadId},
        UI::WindowsAndMessaging::{
            GetClientRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
        },
    };

    const PW_CLIENTONLY: u32 = 1;
    /// Windows 8.1+: render DirectComposition/DirectX content, not the
    /// (absent, WS_EX_NOREDIRECTIONBITMAP) GDI redirection surface.
    const PW_RENDERFULLCONTENT: u32 = 2;

    /// Frees the GDI objects on every return path.
    struct Gdi {
        memory: HDC,
        bitmap: HBITMAP,
        previous: HGDIOBJ,
    }
    impl Drop for Gdi {
        fn drop(&mut self) {
            unsafe {
                if !self.previous.is_null() {
                    SelectObject(self.memory, self.previous);
                }
                if !self.bitmap.is_null() {
                    DeleteObject(self.bitmap);
                }
                DeleteDC(self.memory);
            }
        }
    }

    fn own(hwnd: HWND) -> bool {
        let mut pid = 0;
        !hwnd.is_null()
            && unsafe { GetWindowThreadProcessId(hwnd, &mut pid) }
                == unsafe { GetCurrentThreadId() }
            && pid == unsafe { GetCurrentProcessId() }
    }

    /// # Safety
    /// Call on the thread that owns `hwnd`; `hwnd` may be null or foreign.
    pub(super) unsafe fn capture_hwnd(hwnd: HWND) -> Result<Captured, Failure> {
        if !own(hwnd) {
            return Err(Failure::NotOwn);
        }
        unsafe {
            if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
                return Err(Failure::Minimized);
            }
            let mut rect: RECT = std::mem::zeroed();
            if GetClientRect(hwnd, &mut rect) == 0 {
                return Err(Failure::Failed);
            }
            let width = (rect.right - rect.left).max(0) as u32;
            let height = (rect.bottom - rect.top).max(0) as u32;
            let len = byte_len(width, height)?;
            let memory = CreateCompatibleDC(std::ptr::null_mut());
            if memory.is_null() {
                return Err(Failure::Failed);
            }
            let mut gdi = Gdi {
                memory,
                bitmap: std::ptr::null_mut(),
                previous: std::ptr::null_mut(),
            };
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = width as i32;
            info.bmiHeader.biHeight = -(height as i32); // top-down rows
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB;
            let mut bits = std::ptr::null_mut();
            gdi.bitmap = CreateDIBSection(
                memory,
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                std::ptr::null_mut(),
                0,
            );
            if gdi.bitmap.is_null() || bits.is_null() {
                return Err(Failure::Failed);
            }
            let buffer = std::slice::from_raw_parts_mut(bits.cast::<u8>(), len);
            for pixel in buffer.as_chunks_mut::<4>().0 {
                *pixel = UNPAINTED;
            }
            gdi.previous = SelectObject(memory, gdi.bitmap);
            // Recheck right before the readback: never print a handle that
            // was destroyed and reused meanwhile.
            if !own(hwnd) || PrintWindow(hwnd, memory, PW_CLIENTONLY | PW_RENDERFULLCONTENT) == 0 {
                return Err(Failure::Failed);
            }
            GdiFlush();
            let bgra = buffer.to_vec();
            drop(gdi);
            finish(width, height, bgra)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows_sys::Win32::UI::WindowsAndMessaging::*;

        const GDI_COLOR: u32 = 0x00563412; // COLORREF 0x00BBGGRR: RGB 12 34 56

        unsafe extern "system" fn procedure(hwnd: HWND, message: u32, w: usize, l: isize) -> isize {
            unsafe {
                match message {
                    WM_PAINT => {
                        let mut paint: PAINTSTRUCT = std::mem::zeroed();
                        let dc = BeginPaint(hwnd, &mut paint);
                        fill(hwnd, dc);
                        EndPaint(hwnd, &paint);
                        0
                    }
                    WM_PRINTCLIENT => {
                        fill(hwnd, w as HDC);
                        0
                    }
                    WM_ERASEBKGND => 1,
                    _ => DefWindowProcW(hwnd, message, w, l),
                }
            }
        }
        unsafe fn fill(hwnd: HWND, dc: HDC) {
            unsafe {
                if GetWindowLongPtrW(hwnd, GWLP_USERDATA) != 0 {
                    return; // the DirectComposition control draws nothing in GDI
                }
                let mut rect: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rect);
                let brush = CreateSolidBrush(GDI_COLOR);
                FillRect(dc, &rect, brush);
                DeleteObject(brush);
            }
        }

        struct TestWindow(HWND, Vec<u16>);
        impl Drop for TestWindow {
            fn drop(&mut self) {
                unsafe {
                    DestroyWindow(self.0);
                    UnregisterClassW(self.1.as_ptr(), std::ptr::null_mut());
                }
            }
        }
        fn window(class: &str, ex_style: u32, composition: bool) -> TestWindow {
            let name: Vec<u16> = format!("{class}\0").encode_utf16().collect();
            unsafe {
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(procedure),
                    lpszClassName: name.as_ptr(),
                    ..std::mem::zeroed()
                };
                assert_ne!(RegisterClassW(&wc), 0);
                let hwnd = CreateWindowExW(
                    ex_style,
                    name.as_ptr(),
                    name.as_ptr(),
                    WS_OVERLAPPEDWINDOW,
                    40,
                    40,
                    240,
                    180,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                );
                assert!(!hwnd.is_null());
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, composition as isize);
                ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                UpdateWindow(hwnd);
                TestWindow(hwnd, name)
            }
        }
        fn pump() {
            unsafe {
                let mut message: MSG = std::mem::zeroed();
                while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
        fn center(captured: &Captured) -> [u8; 4] {
            let (x, y) = (captured.width / 2, captured.height / 2);
            let at = ((y * captured.width + x) * 4) as usize;
            captured.rgba[at..at + 4].try_into().unwrap()
        }

        #[test]
        fn own_window_capture_reads_gdi_pixels_and_rejects_foreign_windows() {
            let control = window("OkilumOwnCaptureGdi", 0, false);
            // PW_RENDERFULLCONTENT reads what DWM composed, not the GDI
            // redirection bitmap directly: wait until DWM has presented the
            // window's first painted frame, as for the composition control.
            for _ in 0..3 {
                pump();
                let _ = unsafe { windows::Win32::Graphics::Dwm::DwmFlush() };
            }
            let captured = unsafe { capture_hwnd(control.0) }.expect("GDI positive control");
            assert_eq!(center(&captured), [0x12, 0x34, 0x56, 255]);
            assert!(png(captured).unwrap().starts_with(b"\x89PNG"));
            unsafe {
                assert_eq!(
                    capture_hwnd(std::ptr::null_mut()).unwrap_err(),
                    Failure::NotOwn
                );
                // Created by another process: the desktop and the shell tray.
                assert_eq!(
                    capture_hwnd(GetDesktopWindow()).unwrap_err(),
                    Failure::NotOwn
                );
                let tray: Vec<u16> = "Shell_TrayWnd\0".encode_utf16().collect();
                let tray = FindWindowW(tray.as_ptr(), std::ptr::null());
                if !tray.is_null() {
                    assert_eq!(capture_hwnd(tray).unwrap_err(), Failure::NotOwn);
                }
                ShowWindow(control.0, SW_MINIMIZE);
                assert_eq!(capture_hwnd(control.0).unwrap_err(), Failure::Minimized);
            }
            // A window of this process but another thread is not ours to print.
            let hwnd = control.0 as usize;
            let other = std::thread::spawn(move || unsafe { capture_hwnd(hwnd as HWND) })
                .join()
                .unwrap();
            assert_eq!(other.unwrap_err(), Failure::NotOwn);
        }

        /// The path GPUI 0.3.3 takes on Windows: WS_EX_NOREDIRECTIONBITMAP and
        /// a premultiplied B8G8R8A8 flip-sequential swap chain shown through a
        /// topmost DirectComposition target. GDI draws nothing, so a readback
        /// that ignored the composition content would fail or come back black.
        #[test]
        fn own_window_capture_reads_direct_composition_swap_chain() {
            use windows::core::Interface;
            use windows::Win32::{
                Foundation::{HMODULE, HWND as WHWND},
                Graphics::{
                    Direct3D::*,
                    Direct3D11::*,
                    DirectComposition::*,
                    Dwm::DwmFlush,
                    Dxgi::{Common::*, *},
                },
            };
            let control = window(
                "OkilumOwnCaptureComposition",
                WS_EX_NOREDIRECTIONBITMAP,
                true,
            );
            let mut rect: RECT = unsafe { std::mem::zeroed() };
            unsafe { GetClientRect(control.0, &mut rect) };
            let (width, height) = (
                (rect.right - rect.left) as u32,
                (rect.bottom - rect.top) as u32,
            );
            unsafe {
                let mut device: Option<ID3D11Device> = None;
                let mut context: Option<ID3D11DeviceContext> = None;
                let hardware = D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_HARDWARE,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                );
                if hardware.is_err() {
                    // Hosted runners have no GPU; WARP is still DirectX.
                    D3D11CreateDevice(
                        None,
                        D3D_DRIVER_TYPE_WARP,
                        HMODULE::default(),
                        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                        None,
                        D3D11_SDK_VERSION,
                        Some(&mut device),
                        None,
                        Some(&mut context),
                    )
                    .expect("D3D11 device");
                }
                let (device, context) = (device.unwrap(), context.unwrap());
                let dxgi_device: IDXGIDevice = device.cast().unwrap();
                let factory: IDXGIFactory2 =
                    CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).unwrap();
                let desc = DXGI_SWAP_CHAIN_DESC1 {
                    Width: width,
                    Height: height,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    Stereo: false.into(),
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                    BufferCount: 2,
                    Scaling: DXGI_SCALING_STRETCH,
                    SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                    AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                    Flags: 0,
                };
                let swap_chain = factory
                    .CreateSwapChainForComposition(&device, &desc, None)
                    .unwrap();
                let back: ID3D11Texture2D = swap_chain.GetBuffer(0).unwrap();
                let mut view = None;
                device
                    .CreateRenderTargetView(&back, None, Some(&mut view))
                    .unwrap();
                // RGB 0xd2 0x3c 0x1e, opaque.
                context.ClearRenderTargetView(
                    view.as_ref().unwrap(),
                    &[210. / 255., 60. / 255., 30. / 255., 1.],
                );
                swap_chain.Present(0, DXGI_PRESENT(0)).ok().unwrap();
                let composition: IDCompositionDevice =
                    DCompositionCreateDevice(&dxgi_device).unwrap();
                let target = composition
                    .CreateTargetForHwnd(WHWND(control.0), true)
                    .unwrap();
                let visual = composition.CreateVisual().unwrap();
                visual.SetContent(&swap_chain).unwrap();
                target.SetRoot(&visual).unwrap();
                composition.Commit().unwrap();
                composition.WaitForCommitCompletion().unwrap();
                for _ in 0..3 {
                    pump();
                    let _ = DwmFlush();
                }
                let captured = capture_hwnd(control.0).expect("DirectComposition positive control");
                let [r, g, b, _] = center(&captured);
                assert!(
                    r.abs_diff(210) <= 2 && g.abs_diff(60) <= 2 && b.abs_diff(30) <= 2,
                    "composition content missing from the capture: {:?}",
                    [r, g, b]
                );
                drop((visual, target, composition, view, back, swap_chain));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_window_capture_converts_bgra_to_opaque_rgba_and_encodes_png() {
        let captured = finish(2, 1, vec![30, 60, 210, 0, 1, 2, 3, 255]).unwrap();
        assert_eq!(captured.rgba, [210, 60, 30, 255, 3, 2, 1, 255]);
        let decoded = image::load_from_memory(&png(captured).unwrap())
            .unwrap()
            .to_rgba8();
        assert_eq!(decoded.as_raw(), &[210, 60, 30, 255, 3, 2, 1, 255]);
    }

    #[test]
    fn own_window_capture_rejects_unpainted_black_and_unbounded_buffers() {
        let mut partly = vec![9; 8];
        partly[4..].copy_from_slice(&UNPAINTED);
        assert_eq!(finish(2, 1, partly).unwrap_err(), Failure::Failed);
        // Alpha differs, colour is black everywhere: a failed composition read.
        assert_eq!(
            finish(2, 1, vec![0, 0, 0, 255, 0, 0, 0, 0]).unwrap_err(),
            Failure::Failed
        );
        // One dark pixel among black ones is real content.
        assert!(finish(2, 1, vec![0, 0, 0, 255, 0, 0, 1, 255]).is_ok());
        assert_eq!(finish(2, 2, vec![1; 4]).unwrap_err(), Failure::Failed);
        assert_eq!(byte_len(0, 10).unwrap_err(), Failure::Failed);
        assert_eq!(byte_len(16385, 1024).unwrap_err(), Failure::TooLarge);
        assert_eq!(byte_len(u32::MAX, u32::MAX).unwrap_err(), Failure::TooLarge);
        assert_eq!(byte_len(4096, 4096).unwrap(), 4096 * 4096 * 4);
    }
}
