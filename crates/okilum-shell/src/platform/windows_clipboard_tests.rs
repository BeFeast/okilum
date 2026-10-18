//! One synthetic native offer batch on the isolated Windows CI runner.
//! The original startup routing fails at Unsupported after native readback passes.
use super::managed_clipboard;
use gpui::TestAppContext;
use gpui_component::input::clipboard::{ClipboardReadError as Error, ClipboardReadRequest};
use windows_sys::Win32::{
    Foundation::{GlobalFree, HWND},
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
            OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
        },
        Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE},
        Ole::CF_UNICODETEXT,
    },
    UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow},
};

struct Open;
impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}
struct Fixture(HWND);
impl Fixture {
    fn new() -> Self {
        let class = "STATIC\0".encode_utf16().collect::<Vec<_>>();
        let title = "Okilum synthetic clipboard fixture\0"
            .encode_utf16()
            .collect::<Vec<_>>();
        // A hidden window owns only this runner's synthetic clipboard offers.
        let window = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!window.is_null(), "native owner window positive control");
        Self(window)
    }
    fn open(&self) -> Open {
        let start = std::time::Instant::now();
        loop {
            if unsafe { OpenClipboard(self.0) } != 0 {
                return Open;
            }
            assert!(
                start.elapsed() < std::time::Duration::from_millis(500),
                "native fixture must be able to open its clipboard: {}",
                std::io::Error::last_os_error()
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    fn put(format: u32, bytes: &[u8]) {
        let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) };
        assert!(!memory.is_null());
        let pointer = unsafe { GlobalLock(memory) };
        assert!(!pointer.is_null());
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast::<u8>(), bytes.len());
            GlobalUnlock(memory);
        }
        if unsafe { SetClipboardData(format, memory) }.is_null() {
            unsafe {
                GlobalFree(memory);
            }
            panic!(
                "native SetClipboardData positive control: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    fn publish(&self, text: Option<&[u16]>, mixed: bool) {
        let _open = self.open();
        assert_ne!(unsafe { EmptyClipboard() }, 0);
        if mixed {
            for name in ["HTML Format", "Rich Text Format"] {
                let wide = name.encode_utf16().chain([0]).collect::<Vec<_>>();
                let format = unsafe { RegisterClipboardFormatW(wide.as_ptr()) };
                assert_ne!(format, 0);
                Self::put(format, b"conflicting rich representation\0");
            }
        }
        if let Some(words) = text {
            let bytes = words
                .iter()
                .flat_map(|w| w.to_ne_bytes())
                .collect::<Vec<_>>();
            Self::put(CF_UNICODETEXT as u32, &bytes);
        }
    }
    fn readback(&self) -> Option<Vec<u16>> {
        let _open = self.open();
        if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT as u32) } == 0 {
            return None;
        }
        let handle = unsafe { GetClipboardData(CF_UNICODETEXT as u32) };
        assert!(!handle.is_null(), "native readback handle positive control");
        let count = unsafe { GlobalSize(handle) } / 2;
        let pointer = unsafe { GlobalLock(handle) };
        assert!(!pointer.is_null());
        let words = unsafe { std::slice::from_raw_parts(pointer.cast::<u16>(), count) };
        let end = words
            .iter()
            .position(|&w| w == 0)
            .expect("fixture terminator");
        let result = words[..end].to_vec();
        unsafe {
            GlobalUnlock(handle);
        }
        Some(result)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if unsafe { OpenClipboard(self.0) } != 0 {
            unsafe {
                EmptyClipboard();
                CloseClipboard();
            }
        }
        unsafe {
            DestroyWindow(self.0);
        }
    }
}

struct DelayedData {
    words: Vec<u16>,
    rendered: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
struct DelayedOwner {
    window: usize,
    thread: Option<std::thread::JoinHandle<()>>,
    rendered: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl DelayedOwner {
    fn new(text: &str) -> Self {
        use windows_sys::Win32::UI::WindowsAndMessaging::*;
        let rendered = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut data = Box::new(DelayedData {
            words: text.encode_utf16().chain([0]).collect(),
            rendered: rendered.clone(),
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let fixture = Fixture::new();
            unsafe {
                SetWindowLongPtrW(
                    fixture.0,
                    GWLP_USERDATA,
                    (&mut *data as *mut DelayedData) as isize,
                );
                SetWindowLongPtrW(
                    fixture.0,
                    GWLP_WNDPROC,
                    delayed_window as *const () as isize,
                );
            }
            {
                let _open = fixture.open();
                assert_ne!(unsafe { EmptyClipboard() }, 0);
                // A null handle advertises delayed rendering, not empty text.
                unsafe {
                    SetClipboardData(CF_UNICODETEXT as u32, std::ptr::null_mut());
                }
                assert_ne!(
                    unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT as u32) },
                    0
                );
            }
            tx.send(fixture.0 as usize).unwrap();
            let mut message = MSG::default();
            while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
                unsafe {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            drop(fixture);
            drop(data);
        });
        let window = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("delayed owner ready");
        Self {
            window,
            thread: Some(thread),
            rendered,
        }
    }
}
impl Drop for DelayedOwner {
    fn drop(&mut self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
        unsafe {
            PostMessageW(self.window as HWND, WM_CLOSE, 0, 0);
        }
        self.thread
            .take()
            .unwrap()
            .join()
            .expect("owned message pump stopped");
    }
}
unsafe extern "system" fn delayed_window(
    window: HWND,
    message: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    if message == WM_RENDERFORMAT && wparam == CF_UNICODETEXT as usize {
        // The consumer holds OpenClipboard; the owner must not reopen it.
        let data = unsafe { &*(GetWindowLongPtrW(window, GWLP_USERDATA) as *const DelayedData) };
        data.rendered
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        let bytes = data.words.len() * 2;
        let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) };
        if !memory.is_null() {
            let pointer = unsafe { GlobalLock(memory) };
            if !pointer.is_null() {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        data.words.as_ptr(),
                        pointer.cast::<u16>(),
                        data.words.len(),
                    );
                    GlobalUnlock(memory);
                }
                if unsafe { SetClipboardData(CF_UNICODETEXT as u32, memory) }.is_null() {
                    unsafe {
                        GlobalFree(memory);
                    }
                }
            } else {
                unsafe {
                    GlobalFree(memory);
                }
            }
        }
        return 0;
    }
    if message == WM_DESTROY {
        unsafe {
            PostQuitMessage(0);
        }
        return 0;
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

#[gpui::test]
async fn windows_clipboard_startup_provider_uses_native_text(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let exact = "\u{feff}clipboard\r\nשלום 🧠\nПривет\rlast\r\n";
    for (text, rich) in [
        (Some(exact), false),
        (Some(exact), true),
        (Some(""), false),
        (None, false),
        (None, true),
    ] {
        let words = text.map(|s| s.encode_utf16().chain([0]).collect::<Vec<_>>());
        fixture.publish(words.as_deref(), rich);
        assert_eq!(
            fixture.readback(),
            text.map(|s| s.encode_utf16().collect()),
            "independent native fixture readback must pass before provider assertion"
        );
        let result = cx
            .update(|cx| managed_clipboard().read_text(ClipboardReadRequest::default(), cx))
            .await;
        assert_eq!(result, Ok(text.map(str::to_owned)),
            "startup-selected Windows provider; original code returns Err(Unsupported) after native positive control");
    }
    {
        let delayed = DelayedOwner::new(exact);
        let result = cx
            .update(|cx| managed_clipboard().read_text(ClipboardReadRequest::default(), cx))
            .await;
        assert_eq!(
            result,
            Ok(Some(exact.into())),
            "first paste must render a delayed Unicode offer"
        );
        assert_eq!(
            delayed.rendered.load(std::sync::atomic::Ordering::Acquire),
            1,
            "WM_RENDERFORMAT positive control must actually fire"
        );
    }
    fixture.publish(Some(&[0xD800, 0]), false);
    assert_eq!(
        fixture.readback(),
        Some(vec![0xD800]),
        "malformed fixture positive control"
    );
    let result = cx
        .update(|cx| managed_clipboard().read_text(ClipboardReadRequest::default(), cx))
        .await;
    assert_eq!(
        result,
        Err(Error::Io),
        "invalid UTF-16 is never replaced lossily"
    );

    fixture.publish(Some(&[65, 0]), false);
    let _held = fixture.open();
    let result = cx
        .update(|cx| managed_clipboard().read_text(ClipboardReadRequest::default(), cx))
        .await;
    assert_eq!(
        result,
        Err(Error::Unavailable),
        "locked clipboard is a refusal, not empty text"
    );
}

#[gpui::test]
async fn windows_clipboard_native_payload_limit(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    // Real child pipe, production deadline, exact supported limit and overflow.
    for size in [8 * 1024 * 1024, 8 * 1024 * 1024 + 1] {
        let text = "x".repeat(size);
        let words = text.encode_utf16().chain([0]).collect::<Vec<_>>();
        fixture.publish(Some(&words), false);
        assert!(
            fixture.readback() == Some(words[..size].to_vec()),
            "native readback positive control"
        );
        let result = cx
            .update(|cx| managed_clipboard().read_text(ClipboardReadRequest::default(), cx))
            .await;
        if size == 8 * 1024 * 1024 {
            let actual = result
                .expect("supported payload within production deadline")
                .expect("native Unicode format is present");
            assert_eq!(actual.len(), size);
            assert!(actual == text, "supported payload must remain exact");
        } else {
            assert_eq!(result, Err(Error::TooLarge));
        }
    }
}
