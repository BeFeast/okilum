//! Raw macOS pasteboard access, read in the paste command itself (#1602),
//! plus a helper-process reader bounded by its parent for asynchronous callers.
//!
//! Which representation a paste takes is decided in `note_paste::offer`; this
//! file only exposes the general pasteboard to it: type names, bytes, AppKit's
//! own string decoding and NSAttributedString's RTF/RTFD to plain text.
use super::clipboard_process::{self, TIMEOUT};
use crate::note_paste::offer::{self, OfferError, PasteOffer, Pasteboard};
use crate::note_paste::{clipboard_error, NoteClipboard};
use gpui::{App, Task};
use gpui_component::input::clipboard::{
    ClipboardReadError as Error, ClipboardReadRequest, ExactClipboardProvider,
};
use objc2::rc::autoreleasepool;
use objc2::AllocAnyThread;
use objc2_app_kit::{NSAttributedStringAppKitDocumentFormats, NSPasteboard};
use objc2_foundation::{NSAttributedString, NSData, NSString};
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

    /// The editor's paste reads here, on the UI thread, while it handles the
    /// user's Cmd+V or Paste command (#1602). A helper process never received
    /// that command, and its result arrived turns later, after an activation,
    /// layout or focus change could cancel it: pastes into notes were dropped
    /// without a word. The read is the same one AppKit text views make.
    fn read_text_now(&self, _: &App) -> Option<Result<Option<String>, Error>> {
        Some(read_general_text().map_err(clipboard_error))
    }
}

impl NoteClipboard for MacClipboard {
    fn read_offer(&self) -> Result<PasteOffer, OfferError> {
        autoreleasepool(|_| offer::read_offer(&MacBoard(&NSPasteboard::generalPasteboard())))
    }
    fn has_content(&self) -> bool {
        autoreleasepool(|_| {
            !MacBoard(&NSPasteboard::generalPasteboard())
                .types()
                .is_empty()
        })
    }
    fn types(&self) -> Vec<String> {
        autoreleasepool(|_| MacBoard(&NSPasteboard::generalPasteboard()).types())
    }
}

/// The general pasteboard's text in any text representation, in this process.
fn read_general_text() -> Result<Option<String>, OfferError> {
    autoreleasepool(|_| read_text(&MacBoard(&NSPasteboard::generalPasteboard())))
}

fn read_text(board: &impl Pasteboard) -> Result<Option<String>, OfferError> {
    match offer::read_offer_with(board, false)? {
        PasteOffer::Text(text) => Ok(Some(text)),
        PasteOffer::Image(_) => {
            eprintln!("Paste: the clipboard holds an image; this field takes text only");
            Ok(None)
        }
        PasteOffer::Files(_) => {
            eprintln!("Paste: the clipboard holds files; this field takes text only");
            Ok(None)
        }
        PasteOffer::Nothing { types } => {
            eprintln!("Paste: no text on the clipboard; types: {types:?}");
            Ok(None)
        }
    }
}

struct MacBoard<'a>(&'a NSPasteboard);
impl Pasteboard for MacBoard<'_> {
    fn change_count(&self) -> i64 {
        self.0.changeCount() as i64
    }
    fn types(&self) -> Vec<String> {
        self.0
            .types()
            .map(|types| {
                (0..types.count())
                    .map(|index| types.objectAtIndex(index).to_string())
                    .collect()
            })
            .unwrap_or_default()
    }
    fn data(&self, kind: &str) -> Option<Vec<u8>> {
        self.0
            .dataForType(&NSString::from_str(kind))
            .map(|data| data.to_vec())
    }
    /// Each item's file URL: several copied files are several items, and
    /// `dataForType` on the board reads the first one only.
    fn file_urls(&self) -> Vec<String> {
        let kind = NSString::from_str(offer::FILE_URL);
        let Some(items) = self.0.pasteboardItems() else {
            return Vec::new();
        };
        (0..items.count())
            .filter_map(|index| {
                let item = items.objectAtIndex(index);
                item.stringForType(&kind)
                    .map(|url| url.to_string())
                    .or_else(|| {
                        item.dataForType(&kind)
                            .map(|data| String::from_utf8_lossy(&data.to_vec()).into_owned())
                    })
            })
            .collect()
    }
    fn string(&self, kind: &str) -> Option<String> {
        self.0
            .stringForType(&NSString::from_str(kind))
            .map(|text| text.to_string())
    }
    /// NSAttributedString's own RTF reader; attachments (U+FFFC) are dropped.
    fn rich_text(&self, data: &[u8], rtfd: bool) -> Option<String> {
        let data = NSData::with_bytes(data);
        // SAFETY: no document-attributes dictionary is requested.
        let text = unsafe {
            if rtfd {
                NSAttributedString::initWithRTFD_documentAttributes(
                    NSAttributedString::alloc(),
                    &data,
                    None,
                )
            } else {
                NSAttributedString::initWithRTF_documentAttributes(
                    NSAttributedString::alloc(),
                    &data,
                    None,
                )
            }
        }?;
        Some(text.string().to_string().replace('\u{fffc}', ""))
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
    let result = read_general_text();
    let (status, bytes) = match result {
        Ok(Some(text)) => (0, text.into_bytes()),
        Ok(None) => (1, Vec::new()),
        Err(OfferError::TooLarge) => (2, Vec::new()),
        Err(OfferError::InvalidUtf8) => (3, Vec::new()),
        Err(OfferError::Changed) => (4, Vec::new()),
        Err(OfferError::Io) => (5, Vec::new()),
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

#[cfg(test)]
mod native_tests {
    use super::*;
    use crate::note_paste::offer::{ImageKind, MAX_TEXT_BYTES};
    use objc2::rc::Retained;
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
    fn put(board: &NSPasteboard, kind: &str, bytes: &[u8]) {
        board.clearContents();
        assert!(board.setData_forType(Some(&NSData::with_bytes(bytes)), &NSString::from_str(kind)));
    }
    #[test]
    fn named_pasteboard_preserves_raw_bytes_and_distinguishes_empty_from_absent() {
        autoreleasepool(|_| {
            let board = board();
            assert_eq!(read_text(&MacBoard(&board.0)), Ok(None));
            for bytes in [
                b"one\r\ntwo\r\n".as_slice(),
                "Привет 🧠 e\u{301}\n".as_bytes(),
                b"no final newline",
                b"",
            ] {
                put(&board.0, offer::UTF8_TEXT, bytes);
                assert_eq!(
                    read_text(&MacBoard(&board.0)).unwrap().as_deref(),
                    Some(std::str::from_utf8(bytes).unwrap())
                );
            }
        });
    }
    #[test]
    fn named_pasteboard_rejects_invalid_and_oversize() {
        autoreleasepool(|_| {
            let board = board();
            put(&board.0, offer::UTF8_TEXT, &[255]);
            assert_eq!(read_text(&MacBoard(&board.0)), Err(OfferError::InvalidUtf8));
            put(&board.0, offer::UTF8_TEXT, &vec![b'x'; MAX_TEXT_BYTES + 1]);
            assert_eq!(read_text(&MacBoard(&board.0)), Err(OfferError::TooLarge));
        });
    }
    /// #1602: rich text alone pastes as its plain text.
    #[test]
    fn named_pasteboard_rtf_only_is_plain_text() {
        autoreleasepool(|_| {
            let board = board();
            put(
                &board.0,
                "public.rtf",
                br"{\rtf1\ansi\deff0{\fonttbl{\f0 Helvetica;}}\f0 Hello {\b bold} world}",
            );
            assert_eq!(
                read_text(&MacBoard(&board.0))
                    .unwrap()
                    .as_deref()
                    .map(str::trim),
                Some("Hello bold world")
            );
        });
    }
    /// #1602: an image is offered to a note, not reported as "no text".
    #[test]
    fn named_pasteboard_offers_images() {
        autoreleasepool(|_| {
            let board = board();
            put(&board.0, "public.png", b"\x89PNG\r\n\x1a\n");
            match offer::read_offer(&MacBoard(&board.0)) {
                Ok(PasteOffer::Image(image)) => assert_eq!(image.kind, ImageKind::Png),
                other => panic!("{other:?}"),
            }
        });
    }
}
