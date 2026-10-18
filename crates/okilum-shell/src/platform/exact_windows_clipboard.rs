//! Exact Windows plain text; delayed clipboard rendering is isolated in a child.
use super::clipboard_process::MAX_BYTES;
use gpui_component::input::clipboard::ClipboardReadError as Error;

/// CF_UNICODETEXT is NUL-terminated UTF-16. Padding after the first terminator
/// is allocation slack, not source. Never replace malformed surrogate pairs.
fn decode_text(words: &[u16]) -> Result<String, Error> {
    let end = words.iter().position(|&unit| unit == 0).ok_or(Error::Io)?;
    let mut text = String::new();
    for character in char::decode_utf16(words[..end].iter().copied()) {
        let character = character.map_err(|_| Error::Io)?;
        if text.len() + character.len_utf8() > MAX_BYTES {
            return Err(Error::TooLarge);
        }
        text.push(character);
    }
    Ok(text)
}

#[cfg(windows)]
pub struct WindowsClipboard;

#[cfg(windows)]
impl gpui_component::input::clipboard::ExactClipboardProvider for WindowsClipboard {
    fn read_text(
        &self,
        request: gpui_component::input::clipboard::ClipboardReadRequest,
        cx: &gpui::App,
    ) -> gpui::Task<Result<Option<String>, Error>> {
        use super::clipboard_process::{self, TIMEOUT};
        use std::{os::windows::process::CommandExt, process::Command, time::Instant};
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
        let deadline = Instant::now() + TIMEOUT;
        cx.background_executor().spawn(async move {
            // Unit tests use the ordinary binary built by the same CI checkout,
            // because libtest's executable does not run the application's main.
            // Production has no executable override or shell command surface.
            #[cfg(test)]
            let executable = std::env::var_os("OKILUM_TEST_CLIPBOARD_EXECUTABLE")
                .map(std::path::PathBuf::from)
                .ok_or(Error::Io)?;
            #[cfg(not(test))]
            let executable = std::env::current_exe().map_err(|_| Error::Io)?;
            clipboard_process::read(
                Command::new(executable)
                    .arg(HELPER_ARG)
                    .creation_flags(CREATE_NO_WINDOW),
                deadline,
                || request.is_cancelled(),
            )
        })
    }
}

#[cfg(windows)]
const HELPER_ARG: &str = "--internal-windows-exact-clipboard-read";

/// Called before Velopack, configuration, background threads and GUI startup.
#[cfg(windows)]
pub fn helper_entry() -> Option<i32> {
    use std::io::Write;
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(HELPER_ARG)) {
        return None;
    }
    if args.next().is_some() {
        return Some(2);
    }
    let (status, text) = match native::read() {
        Ok(Some(text)) => (0, text),
        Ok(None) => (1, String::new()),
        Err(Error::TooLarge) => (2, String::new()),
        Err(Error::ChangedOffer) => (4, String::new()),
        Err(Error::Unavailable) => (6, String::new()),
        Err(_) => (5, String::new()),
    };
    let mut stdout = std::io::stdout().lock();
    Some(
        if stdout
            .write_all(&[status])
            .and_then(|_| stdout.write_all(text.as_bytes()))
            .and_then(|_| stdout.flush())
            .is_ok()
        {
            0
        } else {
            1
        },
    )
}

#[cfg(windows)]
mod native {
    use super::*;
    use windows_sys::Win32::{
        Foundation::HGLOBAL,
        System::{
            DataExchange::{
                CloseClipboard, GetClipboardData, GetClipboardOwner, GetClipboardSequenceNumber,
                IsClipboardFormatAvailable, OpenClipboard,
            },
            Memory::{GlobalLock, GlobalSize, GlobalUnlock},
            Ole::CF_UNICODETEXT,
        },
    };

    struct Clipboard;
    impl Drop for Clipboard {
        fn drop(&mut self) {
            // SAFETY: this thread successfully opened the clipboard.
            unsafe {
                CloseClipboard();
            }
        }
    }
    struct Memory(HGLOBAL);
    impl Drop for Memory {
        fn drop(&mut self) {
            // SAFETY: GlobalLock succeeded for this still clipboard-owned block.
            unsafe {
                GlobalUnlock(self.0);
            }
        }
    }

    pub(super) fn read() -> Result<Option<String>, Error> {
        // SAFETY: these read-only clipboard calls require no borrowed pointers.
        let before = unsafe { GetClipboardSequenceNumber() };
        if before == 0 {
            return Err(Error::Unavailable);
        }
        if unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 {
            // Busy, denied or inaccessible remains a refusal. No alternate
            // process, privilege or rich-format path tries to evade policy.
            return Err(Error::Unavailable);
        }
        let _clipboard = Clipboard;
        if unsafe { GetClipboardSequenceNumber() } != before {
            return Err(Error::ChangedOffer);
        }
        let owner = unsafe { GetClipboardOwner() };
        let (text, sequence) = read_open()?;
        if unsafe { GetClipboardSequenceNumber() } != sequence
            || unsafe { GetClipboardOwner() } != owner
        {
            return Err(Error::ChangedOffer);
        }
        Ok(text)
    }

    fn read_open() -> Result<(Option<String>, u32), Error> {
        // Windows may synthesize Unicode from its supported plain text formats;
        // HTML/RTF are never parsed or substituted for declared plain text.
        if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT as u32) } == 0 {
            return Ok((None, unsafe { GetClipboardSequenceNumber() }));
        }
        let handle = unsafe { GetClipboardData(CF_UNICODETEXT as u32) };
        if handle.is_null() {
            return Err(Error::Unavailable);
        }
        // GetClipboardData can render a delayed format and legitimately bump
        // the sequence. This process holds the clipboard open throughout:
        // another opener cannot replace the offer. Pin the rendered sequence
        // and retain the owner check, then verify both after copying the text.
        let rendered_sequence = unsafe { GetClipboardSequenceNumber() };
        if rendered_sequence == 0 {
            return Err(Error::Unavailable);
        }
        // Delayed GetClipboardData may block: only this disposable child does
        // that call; its parent enforces the total deadline and reaps it.
        let size = unsafe { GlobalSize(handle) };
        // Bound raw UTF-16 plus a small amount of allocator padding. The
        // decoder separately enforces the exact UTF-8 payload limit.
        if size > MAX_BYTES * 2 + 64 {
            return Err(Error::TooLarge);
        }
        if size == 0 || size % 2 != 0 {
            return Err(Error::Io);
        }
        let pointer = unsafe { GlobalLock(handle) };
        if pointer.is_null() {
            return Err(Error::Io);
        }
        let _memory = Memory(handle);
        // SAFETY: GlobalLock returns suitably aligned storage valid for
        // GlobalSize bytes while the clipboard and global block remain locked.
        let words = unsafe { std::slice::from_raw_parts(pointer.cast::<u16>(), size / 2) };
        decode_text(words).map(|text| (Some(text), rendered_sequence))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_clipboard_strict_utf16_preserves_source_and_empty() {
        let text = "\u{feff}first\r\nשלום 🧠\nПривет\rlast\r\n";
        let words = text.encode_utf16().chain([0, 0xD800]).collect::<Vec<_>>();
        assert_eq!(decode_text(&words), Ok(text.into()));
        assert_eq!(decode_text(&[0]), Ok(String::new()));
        assert_eq!(decode_text(&[0xD800, 0]), Err(Error::Io));
        assert_eq!(decode_text(&[0xDC00, 0]), Err(Error::Io));
        assert_eq!(decode_text(&[65]), Err(Error::Io));
        let mut oversized = vec![65; MAX_BYTES + 1];
        oversized.push(0);
        assert_eq!(decode_text(&oversized), Err(Error::TooLarge));
    }
}
