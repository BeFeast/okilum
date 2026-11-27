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

#[cfg(windows)]
const NOTE_HELPER_ARG: &str = "--internal-windows-note-clipboard-read";

#[cfg(windows)]
pub(super) fn read_note(
    request: &gpui_component::input::clipboard::ClipboardReadRequest,
    deadline: std::time::Instant,
) -> Result<super::windows_note_clipboard::Snapshot, Error> {
    use std::{os::windows::process::CommandExt, process::Command};
    #[cfg(test)]
    let executable = std::env::var_os("OKILUM_TEST_CLIPBOARD_EXECUTABLE")
        .map(std::path::PathBuf::from)
        .ok_or(Error::Io)?;
    #[cfg(not(test))]
    let executable = std::env::current_exe().map_err(|_| Error::Io)?;
    let frame = super::clipboard_process::read_with_limit(
        Command::new(executable)
            .arg(NOTE_HELPER_ARG)
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW),
        deadline,
        || request.is_cancelled(),
        super::windows_note_clipboard::MAX_SNAPSHOT_BYTES,
    )?
    .ok_or(Error::Io)?;
    super::windows_note_clipboard::Snapshot::decode(&frame)
}

#[cfg(windows)]
const FLAVOR_HELPER_ARG: &str = "--internal-windows-note-flavor-read";

#[cfg(windows)]
pub(super) fn read_note_flavor(
    request: &gpui_component::input::clipboard::ClipboardReadRequest,
    deadline: std::time::Instant,
    sequence: &std::sync::atomic::AtomicU32,
    kind: &str,
) -> Result<Option<Vec<u8>>, Error> {
    use base64::Engine as _;
    use std::{os::windows::process::CommandExt, process::Command};
    #[cfg(test)]
    let executable = std::env::var_os("OKILUM_TEST_CLIPBOARD_EXECUTABLE")
        .map(std::path::PathBuf::from)
        .ok_or(Error::Io)?;
    #[cfg(not(test))]
    let executable = std::env::current_exe().map_err(|_| Error::Io)?;
    let frame = super::clipboard_process::read_with_limit(
        Command::new(executable)
            .arg(FLAVOR_HELPER_ARG)
            .arg(
                sequence
                    .load(std::sync::atomic::Ordering::Relaxed)
                    .to_string(),
            )
            .arg(kind)
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW),
        deadline,
        || request.is_cancelled(),
        (super::windows_note_clipboard::formats::MAX_IMAGE + 14).div_ceil(3) * 4 + 32,
    )?;
    let frame = frame.ok_or(Error::Io)?;
    let (rendered, payload) = frame.split_once(':').ok_or(Error::Io)?;
    let rendered = rendered.parse::<u32>().map_err(|_| Error::Io)?;
    if rendered == 0 {
        return Err(Error::Io);
    }
    sequence.store(rendered, std::sync::atomic::Ordering::Relaxed);
    if payload == "-" {
        return Ok(None);
    }
    base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map(Some)
        .map_err(|_| Error::Io)
}

/// Called before Velopack, configuration, background threads and GUI startup.
#[cfg(windows)]
pub fn helper_entry() -> Option<i32> {
    use std::io::Write;
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    let flavor = first.as_deref() == Some(std::ffi::OsStr::new(FLAVOR_HELPER_ARG));
    let note = first.as_deref() == Some(std::ffi::OsStr::new(NOTE_HELPER_ARG));
    if !flavor && !note && first.as_deref() != Some(std::ffi::OsStr::new(HELPER_ARG)) {
        return None;
    }
    let selected = if flavor {
        let sequence = args
            .next()
            .and_then(|s| s.to_str().and_then(|s| s.parse::<u32>().ok()));
        let kind = args.next().and_then(|s| s.into_string().ok());
        match (sequence, kind) {
            (Some(sequence), Some(kind)) => Some((sequence, kind)),
            _ => return Some(2),
        }
    } else {
        None
    };
    if args.next().is_some() {
        return Some(2);
    }
    let result = if let Some((sequence, kind)) = selected {
        use base64::Engine as _;
        native::read_selected(sequence, &kind).map(|(rendered, data)| {
            Some(format!(
                "{rendered}:{}",
                data.map(|data| base64::engine::general_purpose::STANDARD.encode(data))
                    .unwrap_or_else(|| "-".into())
            ))
        })
    } else if note {
        native::read_note().map(|snapshot| Some(snapshot.encode()))
    } else {
        native::read()
    };
    let (status, text) = match result {
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
                IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
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

    pub(super) fn read_note() -> Result<super::super::windows_note_clipboard::Snapshot, Error> {
        let before = unsafe { GetClipboardSequenceNumber() };
        if before == 0 {
            return Err(Error::Unavailable);
        }
        if unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 {
            return Err(Error::Unavailable);
        }
        let _clipboard = Clipboard;
        if unsafe { GetClipboardSequenceNumber() } != before {
            return Err(Error::ChangedOffer);
        }
        let owner = unsafe { GetClipboardOwner() };
        // Presence only: no GetClipboardData (including delayed rendering).
        let mut types = Vec::new();
        for kind in [
            offer::UTF8_TEXT,
            offer::FILE_URL,
            "public.png",
            "com.microsoft.bmp",
            offer::HTML[0],
            offer::VSCODE_EDITOR_DATA,
            offer::URL,
        ] {
            if format_for(kind).is_some() {
                types.push(kind.to_owned());
            }
        }
        if types.iter().any(|kind| kind == offer::HTML[0]) {
            types.push(offer::CHROMIUM_SOURCE_URL.to_owned());
        }
        if unsafe { GetClipboardSequenceNumber() } != before
            || unsafe { GetClipboardOwner() } != owner
        {
            return Err(Error::ChangedOffer);
        }
        Ok(super::super::windows_note_clipboard::Snapshot {
            native: Some(super::super::windows_note_clipboard::NativeOffer {
                sequence: before,
                types,
            }),
            ..Default::default()
        })
    }

    use super::super::windows_note_clipboard::{formats, VERIFY};
    use crate::note_paste::offer;

    fn registered(name: &str) -> Option<u32> {
        let wide = name.encode_utf16().chain([0]).collect::<Vec<_>>();
        let format = unsafe { RegisterClipboardFormatW(wide.as_ptr()) };
        (format != 0).then_some(format)
    }
    fn format_for(kind: &str) -> Option<u32> {
        let candidates: Vec<u32> = match kind {
            offer::UTF8_TEXT => vec![CF_UNICODETEXT as u32],
            "public.html" => registered("HTML Format").into_iter().collect(),
            "public.png" => registered("PNG").into_iter().collect(),
            // CF_DIBV5 before CF_DIB.
            "com.microsoft.bmp" => vec![17, 8],
            // CF_HDROP before the legacy single-file registered format.
            offer::FILE_URL => std::iter::once(15).chain(registered("FileNameW")).collect(),
            offer::VSCODE_EDITOR_DATA => registered("vscode-editor-data").into_iter().collect(),
            offer::URL => registered("UniformResourceLocatorW").into_iter().collect(),
            _ => return None,
        };
        candidates
            .into_iter()
            .find(|format| unsafe { IsClipboardFormatAvailable(*format) } != 0)
    }

    pub(super) fn read_selected(
        expected: u32,
        kind: &str,
    ) -> Result<(u32, Option<Vec<u8>>), Error> {
        let before = unsafe { GetClipboardSequenceNumber() };
        if expected == 0 || before != expected {
            return Err(Error::ChangedOffer);
        }
        if unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 {
            return Err(Error::Unavailable);
        }
        let _clipboard = Clipboard;
        if unsafe { GetClipboardSequenceNumber() } != expected {
            return Err(Error::ChangedOffer);
        }
        let owner = unsafe { GetClipboardOwner() };
        let mut rendered_sequence = expected;
        let data = if kind == VERIFY {
            Some(Vec::new())
        } else {
            match format_for(kind) {
                None => None,
                Some(format) => {
                    let handle = unsafe { GetClipboardData(format) };
                    rendered_sequence = unsafe { GetClipboardSequenceNumber() };
                    if handle.is_null() {
                        return Err(Error::Unavailable);
                    }
                    if kind == offer::FILE_URL && format == 15 {
                        Some(drop_files(handle)?)
                    } else if kind == offer::HTML[0]
                        && (unsafe { GlobalSize(handle) } == 0
                            || unsafe { GlobalSize(handle) } > MAX_BYTES + 64)
                    {
                        // Invalid/oversized CF_HTML retains the declared plain fallback.
                        None
                    } else {
                        let limit = if matches!(kind, "public.png" | "com.microsoft.bmp") {
                            formats::MAX_IMAGE
                        } else {
                            MAX_BYTES * 2 + 64
                        };
                        let bytes = copy_global(handle, limit)?;
                        Some(match kind {
                            offer::UTF8_TEXT | offer::URL => formats::unicode(&bytes, MAX_BYTES)?,
                            offer::FILE_URL => {
                                let path = String::from_utf8(formats::unicode(&bytes, MAX_BYTES)?)
                                    .map_err(|_| Error::Io)?;
                                let url = url::Url::from_file_path(path).map_err(|_| Error::Io)?;
                                serde_json::to_vec(&vec![url.as_str()]).map_err(|_| Error::Io)?
                            }
                            "com.microsoft.bmp" => formats::dib_to_bmp(&bytes)?,
                            "public.png" => bytes,
                            _ => {
                                let end = bytes
                                    .iter()
                                    .position(|byte| *byte == 0)
                                    .unwrap_or(bytes.len());
                                if end > MAX_BYTES && kind != offer::HTML[0] {
                                    return Err(Error::TooLarge);
                                }
                                bytes[..end].to_vec()
                            }
                        })
                    }
                }
            }
        };
        // Delayed rendering may increment the sequence while this helper holds
        // the clipboard open. Return that sequence to pin subsequent reads.
        if rendered_sequence == 0
            || unsafe { GetClipboardSequenceNumber() } != rendered_sequence
            || unsafe { GetClipboardOwner() } != owner
        {
            return Err(Error::ChangedOffer);
        }
        Ok((rendered_sequence, data))
    }

    fn copy_global(handle: HGLOBAL, limit: usize) -> Result<Vec<u8>, Error> {
        let size = unsafe { GlobalSize(handle) };
        if size > limit {
            return Err(Error::TooLarge);
        }
        if size == 0 {
            return Err(Error::Io);
        }
        let pointer = unsafe { GlobalLock(handle) };
        if pointer.is_null() {
            return Err(Error::Io);
        }
        let _memory = Memory(handle);
        // SAFETY: clipboard-owned global storage is locked for size bytes.
        Ok(unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), size) }.to_vec())
    }

    fn drop_files(handle: HGLOBAL) -> Result<Vec<u8>, Error> {
        use windows_sys::Win32::UI::Shell::DragQueryFileW;
        let count = unsafe { DragQueryFileW(handle, u32::MAX, std::ptr::null_mut(), 0) };
        if count > 4096 {
            return Err(Error::TooLarge);
        }
        let mut urls = Vec::new();
        let mut total = 0;
        for index in 0..count {
            let length = unsafe { DragQueryFileW(handle, index, std::ptr::null_mut(), 0) };
            if length == 0 || length > 32767 {
                return Err(Error::Io);
            }
            total += (length as usize + 1) * 2;
            if total > MAX_BYTES {
                return Err(Error::TooLarge);
            }
            let mut path = vec![0u16; length as usize + 1];
            if unsafe { DragQueryFileW(handle, index, path.as_mut_ptr(), length + 1) } != length {
                return Err(Error::Io);
            }
            let path = String::from_utf16(&path[..length as usize]).map_err(|_| Error::Io)?;
            let url = url::Url::from_file_path(path).map_err(|_| Error::Io)?;
            urls.push(url.to_string());
        }
        serde_json::to_vec(&urls).map_err(|_| Error::Io)
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
