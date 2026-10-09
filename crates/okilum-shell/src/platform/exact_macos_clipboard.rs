//! Raw macOS pasteboard access isolated from the GUI and bounded by its parent.
use super::clipboard_process::{self, MAX_BYTES, TIMEOUT};
use gpui::{App, Task};
use gpui_component::input::clipboard::{
    ClipboardReadError as Error, ClipboardReadRequest, ExactClipboardProvider,
};
use objc2::rc::autoreleasepool;
use objc2_app_kit::NSPasteboard;
use objc2_foundation::NSString;
use std::{io::Write, process::Command, time::Instant};

const HELPER_ARG: &str = "--internal-macos-exact-clipboard-read";
pub struct MacClipboard;
impl ExactClipboardProvider for MacClipboard {
    fn read_text(
        &self,
        request: ClipboardReadRequest,
        cx: &App,
    ) -> Task<Result<Option<String>, Error>> {
        let deadline = Instant::now() + TIMEOUT;
        cx.background_executor().spawn(async move {
            let executable = std::env::current_exe().map_err(|_| Error::Io)?;
            clipboard_process::read(Command::new(executable).arg(HELPER_ARG), deadline, || {
                request.is_cancelled()
            })
        })
    }
}

/// Runs before application, configuration, backend or GPUI startup.
pub fn helper_entry() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(HELPER_ARG)) {
        return None;
    }
    if args.next().is_some() {
        return Some(2);
    }
    let result = autoreleasepool(|_| read_board(&NSPasteboard::generalPasteboard(), || {}));
    let (status, bytes) = match result {
        Ok(Some(bytes)) => (0, bytes),
        Ok(None) => (1, Vec::new()),
        Err(Error::TooLarge) => (2, Vec::new()),
        Err(Error::InvalidUtf8) => (3, Vec::new()),
        Err(Error::ChangedOffer) => (4, Vec::new()),
        Err(_) => (5, Vec::new()),
    };
    let mut stdout = std::io::stdout().lock();
    Some(
        if stdout
            .write_all(&[status])
            .and_then(|_| stdout.write_all(&bytes))
            .is_ok()
        {
            0
        } else {
            1
        },
    )
}

fn read_board(board: &NSPasteboard, after_read: impl FnOnce()) -> Result<Option<Vec<u8>>, Error> {
    let before = board.changeCount();
    let kind = NSString::from_str("public.utf8-plain-text");
    // Only a declared UTF-8 text representation is eligible; no rich-text conversion.
    let offered = board
        .types()
        .is_some_and(|types| types.containsObject(&kind));
    let result = if offered {
        match board.dataForType(&kind) {
            Some(data) if data.length() > MAX_BYTES => Err(Error::TooLarge),
            Some(data) => {
                let bytes = data.to_vec();
                match std::str::from_utf8(&bytes) {
                    Ok(_) => Ok(Some(bytes)),
                    Err(_) => Err(Error::InvalidUtf8),
                }
            }
            None => Err(Error::Io),
        }
    } else {
        Ok(None)
    };
    after_read();
    if board.changeCount() != before {
        return Err(Error::ChangedOffer);
    }
    result
}

#[cfg(test)]
mod native_tests {
    use super::*;
    use objc2::rc::Retained;
    use objc2_foundation::NSData;
    struct OwnedBoard(Retained<NSPasteboard>);
    impl Drop for OwnedBoard {
        fn drop(&mut self) {
            self.0.clearContents();
        }
    }
    fn board() -> OwnedBoard {
        let board = OwnedBoard(NSPasteboard::pasteboardWithUniqueName());
        board.0.clearContents();
        board
    }
    fn put(board: &NSPasteboard, bytes: &[u8]) {
        board.clearContents();
        assert!(board.setData_forType(
            Some(&NSData::with_bytes(bytes)),
            &NSString::from_str("public.utf8-plain-text")
        ));
    }
    #[test]
    fn named_pasteboard_preserves_raw_bytes_and_distinguishes_empty_from_absent() {
        autoreleasepool(|_| {
            let board = board();
            assert_eq!(read_board(&board.0, || {}), Ok(None));
            for bytes in [
                b"one\r\ntwo\r\n".as_slice(),
                "Привет 🧠 e\u{301}\n".as_bytes(),
                b"no final newline",
                b"",
            ] {
                put(&board.0, bytes);
                assert_eq!(read_board(&board.0, || {}).unwrap().as_deref(), Some(bytes));
            }
        });
    }
    #[test]
    fn named_pasteboard_rejects_invalid_oversize_and_changed_offer() {
        autoreleasepool(|_| {
            let board = board();
            put(&board.0, &[255]);
            assert_eq!(read_board(&board.0, || {}), Err(Error::InvalidUtf8));
            put(&board.0, &vec![b'x'; MAX_BYTES + 1]);
            assert_eq!(read_board(&board.0, || {}), Err(Error::TooLarge));
            put(&board.0, b"before\r\n");
            assert_eq!(
                read_board(&board.0, || put(&board.0, b"after\n")),
                Err(Error::ChangedOffer)
            );
            assert_eq!(
                read_board(&board.0, || {}).unwrap(),
                Some(b"after\n".to_vec())
            );
        });
    }
}
