//! Typed Linux note clipboard over the existing bounded data-control reader,
//! or Okilum's own CLIPBOARD read where GPUI runs on X11 (#2006). GPUI's own
//! connection supplies self-owned and unsupported-compositor offers.
//!
//! #1618: Ctrl+V on the UI thread reads the offer's type list, then only the
//! flavors the platform-neutral router asks for, through one pinned offer.
//! The native MIME types map to the names the router knows from macOS.
use super::{
    exact_wayland_clipboard::{self, NoteSelection, Receive},
    exact_x11_clipboard,
};
use crate::note_paste::{
    self,
    offer::{self, OfferError, PasteOffer, Pasteboard, CHROMIUM_SOURCE_URL, VSCODE_EDITOR_DATA},
    router::{self, RouteContext, Routed},
    NoteClipboard,
};
use gpui::{App, ClipboardEntry};
use gpui_component::input::clipboard::ClipboardReadError as Error;
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
};

pub struct LinuxNoteClipboard {
    socket: Option<PathBuf>,
    seat: Option<String>,
    /// The X11 display GPUI itself runs on and this process's id; never set
    /// beside a Wayland socket, so an Xwayland bridge never asks Okilum's own
    /// UI thread.
    x11: Option<(String, u32)>,
}
impl Default for LinuxNoteClipboard {
    fn default() -> Self {
        Self::new()
    }
}
impl LinuxNoteClipboard {
    pub fn new() -> Self {
        let socket = exact_wayland_clipboard::existing_compositor();
        let x11 = (socket.is_none() && gpui::guess_compositor() == "X11")
            .then(|| std::env::var("DISPLAY").ok())
            .flatten()
            .map(|display| (display, std::process::id()));
        Self {
            socket,
            seat: std::env::var("OKILUM_CLIPBOARD_SEAT").ok(),
            x11,
        }
    }
    fn read(&self) -> Result<NoteSelection, Error> {
        match (&self.socket, &self.x11) {
            (None, Some((display, own_pid))) => {
                exact_x11_clipboard::read_note_in_command(display, *own_pid)
            }
            _ => exact_wayland_clipboard::read_note_in_command(
                self.socket.as_deref(),
                self.seat.clone(),
            ),
        }
    }
}

/// Most bytes one paste transfers, all flavors together.
const MAX_PASTE_BYTES: usize = offer::MAX_IMAGE_BYTES;
const URI_LIST: &str = "text/uri-list";
/// Nautilus, Nemo, Caja, Thunar: `copy` or `cut`, then one `file://` URL per
/// line. A cut pastes as a copy; the source is never removed. Dolphin's cut
/// marker (`application/x-kde-cutselection`) comes with a `text/uri-list`
/// and is not read for the same reason.
const GNOME_COPIED_FILES: &str = "x-special/gnome-copied-files";

/// Native MIME types smart paste reads, each with the router's name for it.
/// Among MIME types sharing a name the first one offered is the one read.
const FLAVORS: [(&str, &str); 15] = [
    ("text/plain;charset=utf-8", offer::UTF8_TEXT),
    ("UTF8_STRING", offer::UTF8_TEXT),
    ("text/plain", offer::UTF8_TEXT),
    ("text/html", "public.html"),
    (URI_LIST, offer::FILE_URL),
    (GNOME_COPIED_FILES, offer::FILE_URL),
    ("image/png", "public.png"),
    ("image/jpeg", "public.jpeg"),
    ("image/webp", "org.webmproject.webp"),
    ("image/gif", "com.compuserve.gif"),
    ("image/tiff", "public.tiff"),
    ("image/bmp", "com.microsoft.bmp"),
    // VS Code, Cursor, Windsurf: the copied code's language (step 3).
    ("vscode-editor-data", VSCODE_EDITOR_DATA),
    // The page a browser copied from, for relative links in its HTML.
    ("chromium/x-source-url", CHROMIUM_SOURCE_URL),
    ("text/x-moz-url-priv", CHROMIUM_SOURCE_URL),
];

/// A note paste's clipboard: the offered MIME types, and each flavor's
/// bytes once the router reads it. A native offer transfers a flavor on its
/// first read; GPUI's own clipboard is already in memory.
#[derive(Default)]
struct Board {
    offered: Vec<String>,
    received: RefCell<Vec<(String, Vec<u8>)>>,
    receive: RefCell<Option<Receive>>,
    used: Cell<usize>,
    /// The first failed transfer. Nothing is read after it, and the paste
    /// built on the flavors read so far is discarded.
    failure: RefCell<Option<Error>>,
}

impl Board {
    /// Flavors already in memory (GPUI's clipboard).
    fn fixed(data: Vec<(String, Vec<u8>)>) -> Self {
        Self {
            offered: data.iter().map(|(mime, _)| mime.clone()).collect(),
            received: RefCell::new(data),
            ..Self::default()
        }
    }
    /// A pinned native offer: its MIME list now, each flavor on demand.
    fn native(offered: Vec<String>, receive: Receive) -> Self {
        Self {
            offered,
            receive: RefCell::new(Some(receive)),
            ..Self::default()
        }
    }
    fn offers(&self, mime: &str) -> bool {
        self.offered.iter().any(|offered| offered == mime)
    }
    /// The offered MIME type the router's `kind` reads.
    fn mime(&self, kind: &str) -> Option<&'static str> {
        FLAVORS
            .iter()
            .find(|(mime, name)| *name == kind && self.offers(mime))
            .map(|(mime, _)| *mime)
    }
    /// One flavor's bytes, transferred on its first read.
    fn fetch(&self, mime: &str) -> Option<Vec<u8>> {
        if let Some((_, bytes)) = self.received.borrow().iter().find(|(m, _)| m == mime) {
            return Some(bytes.clone());
        }
        if self.failure.borrow().is_some() {
            return None;
        }
        let mut receive = self.receive.borrow_mut();
        let receive = receive.as_mut()?;
        let per_flavor = if mime.starts_with("image/") {
            offer::MAX_IMAGE_BYTES
        } else {
            offer::MAX_TEXT_BYTES
        };
        let limit = per_flavor.min(MAX_PASTE_BYTES - self.used.get());
        match receive(mime, limit) {
            Ok(bytes) if bytes.len() <= limit => {
                self.used.set(self.used.get() + bytes.len());
                self.received
                    .borrow_mut()
                    .push((mime.to_owned(), bytes.clone()));
                Some(bytes)
            }
            Ok(_) => self.fail(Error::TooLarge),
            Err(error) => self.fail(error),
        }
    }
    fn fail(&self, error: Error) -> Option<Vec<u8>> {
        self.failure.borrow_mut().get_or_insert(error);
        None
    }
    fn failure(&self) -> Option<Error> {
        self.failure.borrow().clone()
    }
    /// For the paste log: where the board came from, the native MIME types
    /// offered and the ones transferred. Never contents.
    fn log_detail(&self) -> String {
        let source = if self.receive.borrow().is_some() {
            "native"
        } else {
            "gpui"
        };
        let read: Vec<_> = self
            .received
            .borrow()
            .iter()
            .map(|(mime, _)| mime.clone())
            .collect();
        format!(
            "source={source} offered={:?} read={read:?} read_bytes={}",
            self.offered,
            self.received
                .borrow()
                .iter()
                .map(|(_, bytes)| bytes.len())
                .sum::<usize>()
        )
    }
}

/// Text from a Linux browser flavor as UTF-8. Firefox writes UTF-16: with a
/// byte order mark (HTML, on either protocol) or, for its private source
/// URL, without one; a NUL inside the text marks the latter. Trailing NULs
/// (C strings) go.
fn text_utf8(bytes: Vec<u8>) -> Vec<u8> {
    let utf16 = |units: &[u8], little: bool| {
        let units = units.as_chunks::<2>().0.iter().map(|pair| {
            if little {
                u16::from_le_bytes(*pair)
            } else {
                u16::from_be_bytes(*pair)
            }
        });
        char::decode_utf16(units)
            .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect::<String>()
            .trim_end_matches('\0')
            .as_bytes()
            .to_vec()
    };
    match bytes.get(..2) {
        Some([0xFF, 0xFE]) => return utf16(&bytes[2..], true),
        Some([0xFE, 0xFF]) => return utf16(&bytes[2..], false),
        _ => {}
    }
    let end = bytes
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |i| i + 1);
    if bytes[..end].contains(&0) {
        let padded = (end + 1) & !1;
        return utf16(&bytes[..padded.min(bytes.len())], true);
    }
    let mut bytes = bytes;
    bytes.truncate(end);
    bytes
}

impl Pasteboard for Board {
    fn change_count(&self) -> i64 {
        0
    } // One pinned offer: a change fails the read (`failure`), never mixes.
    fn types(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for (mime, name) in FLAVORS {
            if self.offers(mime) && !names.iter().any(|known| known == name) {
                names.push(name.to_owned());
            }
        }
        names
    }
    fn data(&self, kind: &str) -> Option<Vec<u8>> {
        self.fetch(self.mime(kind)?)
    }
    fn string(&self, kind: &str) -> Option<String> {
        let bytes = self.data(kind)?;
        let bytes = if matches!(kind, "public.html" | CHROMIUM_SOURCE_URL) {
            text_utf8(bytes)
        } else {
            bytes
        };
        String::from_utf8(bytes).ok()
    }
    fn rich_text(&self, _: &[u8], _: bool) -> Option<String> {
        None
    }
    fn source_url(&self) -> Option<String> {
        let text = self.string(CHROMIUM_SOURCE_URL)?;
        let url = text.lines().next()?.trim();
        (!url.is_empty()).then(|| url.to_owned())
    }
    /// The file list: `text/uri-list` when offered, else the file manager's
    /// own list. Only one of them is transferred.
    fn file_urls(&self) -> Vec<String> {
        for mime in [URI_LIST, GNOME_COPIED_FILES] {
            if !self.offers(mime) {
                continue;
            }
            let urls: Vec<_> = self
                .fetch(mime)
                .map(|bytes| {
                    String::from_utf8_lossy(&bytes)
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty() && !line.starts_with('#'))
                        .filter_map(|line| url::Url::parse(line).ok())
                        .filter(|url| url.to_file_path().is_ok())
                        .map(|url| url.to_string())
                        .collect()
                })
                .unwrap_or_default();
            if !urls.is_empty() {
                return urls;
            }
        }
        Vec::new()
    }
}

fn settle(
    selection: Result<NoteSelection, Error>,
    platform: impl FnOnce() -> Board,
) -> Result<Board, Error> {
    match selection {
        Ok(NoteSelection::Offer { mimes, receive }) => Ok(Board::native(mimes, receive)),
        Ok(NoteSelection::Empty) => Ok(Board::default()),
        Ok(NoteSelection::Own) | Err(Error::Unsupported | Error::AmbiguousSeat | Error::Io) => {
            Ok(platform())
        }
        Err(error) => Err(error),
    }
}

/// Run one paste over the clipboard. A transfer that failed midway discards
/// the result: a changed, oversized or late offer is the paste's error; a
/// flavor the owner would not convert hands the paste to GPUI's clipboard,
/// as an unreadable offer does.
fn run_on<T>(
    selection: Result<NoteSelection, Error>,
    platform: impl Fn() -> Board,
    paste: impl Fn(&Board) -> Result<T, OfferError>,
) -> Result<(T, String), Error> {
    let board = settle(selection, &platform)?;
    let result = paste(&board);
    match board.failure() {
        None => result
            .map(|value| (value, board.log_detail()))
            .map_err(note_paste::clipboard_error),
        Some(Error::Unsupported | Error::AmbiguousSeat | Error::Io) => {
            let board = platform();
            paste(&board)
                .map(|value| (value, board.log_detail()))
                .map_err(note_paste::clipboard_error)
        }
        Some(error) => Err(error),
    }
}

fn platform_board(cx: &App) -> Board {
    let mut data = Vec::new();
    if let Some(item) = cx.read_from_clipboard() {
        for entry in item.into_entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    data.push((image.format.mime_type().to_owned(), image.bytes))
                }
                ClipboardEntry::String(text) => {
                    data.push(("text/plain;charset=utf-8".into(), text.text.into_bytes()))
                }
                ClipboardEntry::ExternalPaths(paths) => {
                    let urls = paths
                        .0
                        .iter()
                        .filter_map(|path| url::Url::from_file_path(path).ok())
                        .map(|url| url.to_string())
                        .collect::<Vec<_>>()
                        .join("\r\n");
                    data.push((URI_LIST.into(), urls.into_bytes()));
                }
            }
        }
    }
    Board::fixed(data)
}
fn offer_error(error: Error) -> OfferError {
    match error {
        Error::TooLarge => OfferError::TooLarge,
        Error::InvalidUtf8 => OfferError::InvalidUtf8,
        Error::ChangedOffer => OfferError::Changed,
        _ => OfferError::Io,
    }
}
fn chat_offer(board: &Board) -> Result<PasteOffer, OfferError> {
    // File-manager copies carry an explicit file flavor. Never turn an
    // ordinary pasted path into an upload, or stat a remote mount on the UI.
    if board.types().iter().any(|kind| kind == offer::FILE_URL) {
        let mut seen = std::collections::HashSet::new();
        let paths: Vec<_> = board
            .file_urls()
            .iter()
            .filter_map(|raw| url::Url::parse(raw).ok()?.to_file_path().ok())
            .filter(|path| seen.insert(path.clone()))
            .collect();
        if !paths.is_empty() {
            return Ok(PasteOffer::Files(paths));
        }
    }
    offer::read_offer_with(board, false)
}
/// A routed paste whose read failed midway is dropped whole; an image it
/// already saved must not stay behind as an orphan.
fn route(board: &Board, context: &RouteContext) -> Result<Routed, OfferError> {
    let routed = router::route(board, context)?;
    if board.failure().is_some() {
        for file in &routed.saved_files {
            let _ = std::fs::remove_file(file);
        }
    }
    Ok(routed)
}
impl NoteClipboard for LinuxNoteClipboard {
    fn read_offer(&self) -> Result<PasteOffer, OfferError> {
        run_on(self.read(), Board::default, offer::read_offer)
            .map(|(offer, _)| offer)
            .map_err(offer_error)
    }
    fn read_chat_offer(&self) -> Result<PasteOffer, OfferError> {
        run_on(self.read(), Board::default, chat_offer)
            .map(|(offer, _)| offer)
            .map_err(offer_error)
    }
    fn read_chat_offer_in_command(&self, cx: &App) -> Result<PasteOffer, OfferError> {
        run_on(self.read(), || platform_board(cx), chat_offer)
            .map(|(offer, _)| offer)
            .map_err(offer_error)
    }
    fn has_content(&self) -> bool {
        true
    } // Availability is checked inside the user's command.
    fn route(&self, context: &RouteContext) -> Result<Routed, OfferError> {
        run_on(self.read(), Board::default, |board| route(board, context))
            .map(|(routed, _)| routed)
            .map_err(offer_error)
    }
    fn route_in_command(&self, context: &RouteContext, cx: &App) -> Result<Routed, Error> {
        let (mut routed, detail) = run_on(
            self.read(),
            || platform_board(cx),
            |board| route(board, context),
        )?;
        // The note editor logs one line per paste; this adds what Linux read.
        routed.detail = format!("{} {detail}", routed.detail).trim().to_owned();
        Ok(routed)
    }
    fn read_attachment(&self) -> Option<offer::ClipAttachment> {
        run_on(self.read(), Board::default, |board| {
            Ok(offer::read_attachment(board))
        })
        .ok()
        .and_then(|(attachment, _)| attachment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use exact_wayland_clipboard::{note_selection, receive_selection, Selection};
    use std::rc::Rc;

    type Reads = Rc<RefCell<Vec<String>>>;

    /// A pinned native offer listing `mimes`, served by `serve`, through the
    /// same settle step the real readers use.
    fn serve(
        mimes: &[&str],
        serve: impl FnMut(&str, usize) -> Result<Vec<u8>, Error> + 'static,
    ) -> Board {
        let selection = note_selection(
            mimes.iter().map(|mime| mime.to_string()).collect(),
            Some("pid/okilum-test"),
            Box::new(serve),
        );
        settle(Ok(selection), || {
            panic!("a foreign native offer must not fall back")
        })
        .unwrap()
    }

    /// A native offer of `flavors`; every transfer is recorded, in order.
    fn native(flavors: &[(&str, &[u8])]) -> (Board, Reads) {
        let reads = Reads::default();
        let served: Vec<(String, Vec<u8>)> = flavors
            .iter()
            .map(|(mime, bytes)| (mime.to_string(), bytes.to_vec()))
            .collect();
        let log = reads.clone();
        let mimes: Vec<&str> = flavors.iter().map(|(mime, _)| *mime).collect();
        let board = serve(&mimes, move |mime, _| {
            log.borrow_mut().push(mime.to_owned());
            Ok(served
                .iter()
                .find(|(offered, _)| offered == mime)
                .unwrap_or_else(|| panic!("{mime} was not offered"))
                .1
                .clone())
        });
        (board, reads)
    }

    #[test]
    fn chat_file_flavor_needs_no_stat_and_plain_paths_remain_text() {
        let uri = "file:///definitely-not-present-okilum-2122/report.txt";
        let files = Board::fixed(vec![(
            "text/uri-list".into(),
            format!("{uri}\r\n").into_bytes(),
        )]);
        assert_eq!(
            chat_offer(&files).unwrap(),
            PasteOffer::Files(vec![PathBuf::from(
                "/definitely-not-present-okilum-2122/report.txt"
            )])
        );
        let plain = Board::fixed(vec![("text/plain".into(), uri.as_bytes().to_vec())]);
        assert_eq!(
            chat_offer(&plain).unwrap(),
            PasteOffer::Text(uri.into()),
            "a text URL must never upload the referenced file"
        );
    }

    #[test]
    fn native_gnome_copied_file_flavor_reaches_chat_without_uploading_plain_paths() {
        let (board, reads) = native(&[
            (GNOME_COPIED_FILES, b"copy\nfile:///qa/exact%20image.png\n"),
            ("text/plain", b"exact image.png"),
        ]);
        assert_eq!(
            chat_offer(&board).unwrap(),
            PasteOffer::Files(vec![PathBuf::from("/qa/exact image.png")])
        );
        assert_eq!(*reads.borrow(), [GNOME_COPIED_FILES]);
        // Nautilus offers both lists: only `text/uri-list` is transferred.
        let (both, reads) = native(&[
            (GNOME_COPIED_FILES, b"copy\nfile:///qa/exact%20image.png\n"),
            (URI_LIST, b"file:///qa/exact%20image.png\r\n"),
            ("text/plain", b"exact image.png"),
        ]);
        assert_eq!(
            chat_offer(&both).unwrap(),
            PasteOffer::Files(vec![PathBuf::from("/qa/exact image.png")])
        );
        assert_eq!(*reads.borrow(), [URI_LIST]);
    }

    #[gpui::test]
    fn chat_command_falls_back_to_self_owned_pixels_files_and_text(cx: &mut gpui::TestAppContext) {
        let clipboard = LinuxNoteClipboard {
            socket: None,
            seat: None,
            x11: None,
        };
        let bytes = png();
        cx.update(|cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_image(&gpui::Image {
                id: 0,
                format: gpui::ImageFormat::Png,
                bytes: bytes.clone(),
            }));
            let offer = clipboard.read_chat_offer_in_command(cx).unwrap();
            assert!(matches!(offer, PasteOffer::Image(image) if image.bytes == bytes));
            cx.write_to_clipboard(gpui::ClipboardItem::new_string("Keep exact text".into()));
            assert_eq!(
                clipboard.read_chat_offer_in_command(cx).unwrap(),
                PasteOffer::Text("Keep exact text".into())
            );
        });
        // Unsupported native readers must use the same captured GPUI board;
        // an empty supported selection must not paste stale platform content.
        let board = || {
            Board::fixed(vec![(
                "text/uri-list".into(),
                b"file:///qa/image.png".to_vec(),
            )])
        };
        assert!(
            matches!(chat_offer(&settle(Ok(NoteSelection::Own), board).unwrap()).unwrap(), PasteOffer::Files(paths) if paths == vec![PathBuf::from("/qa/image.png")])
        );
        assert!(matches!(
            chat_offer(&settle(Ok(NoteSelection::Empty), board).unwrap()).unwrap(),
            PasteOffer::Nothing { .. }
        ));
    }

    fn png() -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 2)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }
    fn context(root: &std::path::Path) -> RouteContext<'_> {
        RouteContext {
            image_options: crate::note_paste::image_import::Options::disabled(),
            mode: router::Mode::Smart,
            source: "",
            selection: 0..0,
            vault_root: root,
            note_rel: Some("note.md"),
            stamp: "20261026000000",
            ask_above: None,
        }
    }

    /// A browser's HTML offer, with or without a plain flavor. Text flavors
    /// are transferred with the text limit.
    fn html_board(html: &[u8], plain: Option<&[u8]>) -> (Board, Reads) {
        let mut mimes = vec!["text/html"];
        if plain.is_some() {
            mimes.push("text/plain");
        }
        let reads = Reads::default();
        let log = reads.clone();
        let (html, plain) = (html.to_vec(), plain.map(<[u8]>::to_vec));
        let board = serve(&mimes, move |mime, limit| {
            assert_eq!(limit, offer::MAX_TEXT_BYTES);
            log.borrow_mut().push(mime.to_owned());
            Ok(match mime {
                "text/html" => html.clone(),
                "text/plain" => plain.clone().unwrap(),
                other => panic!("unexpected MIME {other}"),
            })
        });
        assert!(board.types().iter().any(|kind| kind == "public.html"));
        assert!(
            reads.borrow().is_empty(),
            "#1618: types alone transfer nothing"
        );
        (board, reads)
    }

    #[test]
    fn native_html_with_plain_fallback_and_html_only_reach_smart_paste() {
        let root = tempfile::tempdir().unwrap();
        let html = b"<p>Hello <strong>world</strong></p>";
        for plain in [Some(b"Hello world".as_slice()), None] {
            let (board, _) = html_board(html, plain);
            let routed = router::route(&board, &context(root.path())).unwrap();
            assert_eq!(routed.route, router::Route::RichText);
            assert_eq!(routed.insert.as_deref(), Some("Hello **world**"));
            assert!(routed.saved_files.is_empty() && routed.follow_up.is_none());
            assert_eq!(board.data("public.html").unwrap(), html);
        }
    }

    /// #1618: rich text that outranks a picture of itself never transfers
    /// the picture; only a paste that is the image reads its pixels.
    #[test]
    fn native_html_and_pixels_keep_the_common_rich_text_and_image_precedence() {
        let fixture = tempfile::tempdir().unwrap();
        let image = png();
        for (html, expected, transferred) in [
            (
                "<p><strong>Keep formatting</strong></p>",
                router::Route::RichText,
                &["text/plain;charset=utf-8", "text/html"][..],
            ),
            (
                "<p><img src=\"https://example.invalid/image.png\"></p>",
                router::Route::Image,
                &["text/plain;charset=utf-8", "text/html", "image/png"][..],
            ),
        ] {
            let vault = fixture.path().join(format!("{expected:?}"));
            std::fs::create_dir(&vault).unwrap();
            let (board, reads) = native(&[
                ("text/html", html.as_bytes()),
                ("image/png", &image),
                (
                    "text/plain;charset=utf-8",
                    b"https://example.invalid/image.png",
                ),
                // Never read: the preferred plain representation wins.
                ("text/plain", b"wrong preference"),
            ]);
            assert!(board.types().iter().any(|kind| kind == "public.html"));
            assert!(board.types().iter().any(|kind| kind == "public.png"));
            let routed = router::route(&board, &context(&vault)).unwrap();
            assert_eq!(routed.route, expected);
            assert_eq!(*reads.borrow(), transferred, "{expected:?}");
            assert!(
                routed.follow_up.is_none(),
                "remote HTML source is not fetched when pixels win"
            );
            if expected == router::Route::RichText {
                assert_eq!(routed.insert.as_deref(), Some("**Keep formatting**"));
                assert!(routed.saved_files.is_empty());
                assert_eq!(std::fs::read_dir(&vault).unwrap().count(), 0);
            } else {
                assert_eq!(routed.saved_files.len(), 1);
                let saved = &routed.saved_files[0];
                assert_eq!(std::fs::read(saved).unwrap(), image);
                assert_eq!(
                    routed.insert.as_deref(),
                    Some(format!("![[{}]]", saved.file_name().unwrap().to_str().unwrap()).as_str())
                );
            }
        }
    }

    #[test]
    fn native_html_keeps_code_blocks_and_plain_files_literal_without_attachments() {
        let vault = tempfile::tempdir().unwrap();
        let plain = "<strong>literal markup</strong>\r\n<img src=\"file:///not-read.png\">";
        let (board, reads) = html_board(
            b"<p><strong>Rich formatting</strong></p>",
            Some(plain.as_bytes()),
        );
        let source = "```html\n\n```\n";
        let mut code = context(vault.path());
        code.source = source;
        code.selection = 8..8;
        let routed = router::route(&board, &code).unwrap();
        assert_eq!(routed.route, router::Route::CodeBlockText);
        // Literal routes retain transport bytes; document newlines normalize in the editor.
        assert_eq!(routed.insert.as_deref(), Some(plain));
        assert!(routed.saved_files.is_empty() && routed.follow_up.is_none());
        assert_eq!(
            *reads.borrow(),
            ["text/plain"],
            "#1618: a paste into a code fence transfers no HTML"
        );
        let mut file = context(vault.path());
        file.note_rel = None;
        let routed = router::route(&board, &file).unwrap();
        assert_eq!(routed.route, router::Route::FileText);
        assert_eq!(routed.insert.as_deref(), Some(plain));
        assert!(routed.saved_files.is_empty() && routed.follow_up.is_none());
        // Same actual offer becomes rich Markdown in an ordinary note.
        let routed = router::route(&board, &context(vault.path())).unwrap();
        assert_eq!(routed.route, router::Route::RichText);
        assert_eq!(routed.insert.as_deref(), Some("**Rich formatting**"));
        assert_eq!(std::fs::read_dir(vault.path()).unwrap().count(), 0);
    }

    #[test]
    fn native_html_local_image_publishes_an_embed_and_preserves_existing_files() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source-noext");
        let original = png();
        std::fs::write(&source, &original).unwrap();
        let vault = fixture.path().join("vault");
        let attachments = vault.join("attachments");
        std::fs::create_dir_all(&attachments).unwrap();
        let old = attachments.join("Existing.png");
        std::fs::write(&old, &original).unwrap();
        let html = format!(
            "<p><img src=\"{}\"></p>",
            url::Url::from_file_path(&source).unwrap()
        );
        // Positive control for the transport diagnosis: genuine plain text
        // containing markup stays literal and never creates an attachment.
        // Smart paste must retain the offered HTML MIME, not guess from text.
        let plain_only = Board::fixed(vec![("text/plain".into(), html.as_bytes().to_vec())]);
        let literal = router::route(&plain_only, &context(&vault)).unwrap();
        assert_eq!(literal.route, router::Route::Text);
        assert_eq!(literal.insert.as_deref(), Some(html.as_str()));
        assert!(literal.saved_files.is_empty() && literal.follow_up.is_none());
        assert_eq!(std::fs::read_dir(&attachments).unwrap().count(), 1);
        let (board, _) = html_board(html.as_bytes(), Some(html.as_bytes()));
        let routed = router::route(&board, &context(&vault)).unwrap();
        assert_eq!(routed.route, router::Route::RichText);
        assert!(routed.insert.is_none() && routed.saved_files.is_empty());
        assert_eq!(std::fs::read_dir(&attachments).unwrap().count(), 1);
        let Some(router::FollowUp::Rich { job, .. }) = routed.follow_up else {
            panic!("local HTML image must use the existing background import");
        };
        let done = router::run_rich_job(job);
        assert_eq!(done.files.len(), 1);
        let saved = &done.files[0];
        assert_eq!(saved.parent(), Some(attachments.as_path()));
        assert_eq!(
            done.text,
            format!("![[{}]]", saved.file_name().unwrap().to_str().unwrap())
        );
        assert_eq!(std::fs::read(saved).unwrap(), original);
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert_eq!(std::fs::read(&old).unwrap(), original);
    }

    /// #2459: a local image whose name carries no known image extension is
    /// named and embedded from its content's magic bytes, on every host. The
    /// fixture lives under the crate, not the temp dir: a `/tmp` path looks
    /// like a clipboard cache, and the cache rename (which carries the
    /// sniffed extension) would hide the decision this pins. The route reads
    /// no system MIME database; the sniff is the bytes alone.
    #[test]
    fn native_html_local_images_without_an_image_extension_embed_by_content() {
        let fixture = tempfile::Builder::new()
            .prefix("okilum-2459-fixture-")
            .tempdir_in(env!("CARGO_MANIFEST_DIR"))
            .unwrap();
        for (name, saved_as) in [
            ("source-noext", "source-noext.png"),
            ("capture.dat", "capture.dat.png"),
        ] {
            let source = fixture.path().join(name);
            let original = png();
            std::fs::write(&source, &original).unwrap();
            let source = std::fs::canonicalize(&source).unwrap();
            assert!(
                !crate::note_paste::attachments::is_clipboard_cache(&source),
                "the fixture must not look like a clipboard manager's cache"
            );
            let vault = tempfile::tempdir().unwrap();
            let attachments = vault.path().join("attachments");
            std::fs::create_dir_all(&attachments).unwrap();
            let html = format!(
                "<p><img src=\"{}\"></p>",
                url::Url::from_file_path(&source).unwrap()
            );
            let (board, _) = html_board(html.as_bytes(), Some(html.as_bytes()));
            let routed = router::route(&board, &context(vault.path())).unwrap();
            let Some(router::FollowUp::Rich { job, .. }) = routed.follow_up else {
                panic!("local HTML image must use the existing background import");
            };
            let done = router::run_rich_job(job);
            assert_eq!(done.files.len(), 1);
            let saved = &done.files[0];
            assert_eq!(saved.parent(), Some(attachments.as_path()));
            assert_eq!(saved.file_name().unwrap(), saved_as);
            assert_eq!(done.text, format!("![[{saved_as}]]"));
            assert_eq!(std::fs::read(saved).unwrap(), original);
            assert_eq!(std::fs::read(&source).unwrap(), original);
        }
    }

    #[test]
    fn native_html_plain_command_and_unformatted_html_keep_plain_text() {
        let root = tempfile::tempdir().unwrap();
        let (board, reads) = html_board(
            b"<p><strong>formatted</strong></p>",
            Some(b"plain\r\nfallback"),
        );
        let mut cx = context(root.path());
        cx.mode = router::Mode::Plain;
        let routed = router::route(&board, &cx).unwrap();
        assert_eq!(routed.route, router::Route::PlainText);
        assert_eq!(routed.insert.as_deref(), Some("plain\nfallback"));
        assert!(routed.saved_files.is_empty() && routed.follow_up.is_none());
        assert_eq!(
            *reads.borrow(),
            ["text/plain"],
            "#1618: Ctrl+Shift+V transfers the plain flavor only"
        );
        let (board, _) = html_board(b"<span>plain words</span>", Some(b"plain fallback"));
        let routed = router::route(&board, &context(root.path())).unwrap();
        assert_eq!(routed.route, router::Route::Text);
        assert_eq!(routed.insert.as_deref(), Some("plain fallback"));
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    /// #1618: Ctrl+Alt+V over a browser's copied link takes its title from
    /// the HTML anchor, with no network.
    #[test]
    fn native_titled_link_takes_the_copied_anchor_title() {
        let root = tempfile::tempdir().unwrap();
        let (board, _) = native(&[
            (
                "text/html",
                b"<a href=\"https://example.invalid/guide\">Install guide</a>",
            ),
            ("UTF8_STRING", b"https://example.invalid/guide"),
        ]);
        let mut cx = context(root.path());
        cx.mode = router::Mode::TitledLink;
        let routed = router::route(&board, &cx).unwrap();
        assert_eq!(routed.route, router::Route::TitledLink);
        assert_eq!(
            routed.insert.as_deref(),
            Some("[Install guide](https://example.invalid/guide)")
        );
        assert!(routed.follow_up.is_none());
    }

    #[test]
    fn native_html_is_bounded_and_offer_change_never_becomes_a_partial_paste() {
        let root = tempfile::tempdir().unwrap();
        let paste = |board| {
            run_on(
                Ok(board),
                || panic!("a refusal must not fall back"),
                |board| route(board, &context(root.path())),
            )
        };
        let selection = |serve: Receive| {
            note_selection(vec!["text/html".into(), "text/plain".into()], None, serve)
        };
        let too_large = paste(selection(Box::new(|mime, limit| {
            assert_eq!(limit, offer::MAX_TEXT_BYTES);
            Ok(vec![b'x'; if mime == "text/html" { limit + 1 } else { 1 }])
        })));
        assert_eq!(too_large.err(), Some(Error::TooLarge));
        // The plain flavor is read first (step 1 looks for a cached file
        // path); the offer changes before the HTML is transferred.
        let read_plain = Rc::new(Cell::new(false));
        let plain_done = read_plain.clone();
        let changed = paste(selection(Box::new(move |mime, _| {
            if mime == "text/plain" {
                plain_done.set(true);
                Ok(b"original offer".to_vec())
            } else {
                Err(Error::ChangedOffer)
            }
        })));
        assert!(read_plain.get(), "positive control: a transfer completed");
        assert_eq!(changed.err(), Some(Error::ChangedOffer));
        let mimes = ["text/html".to_owned(), "text/plain".to_owned()];
        let text_only = receive_selection(&mimes, None, |mime, _| {
            assert_eq!(mime, "text/plain", "text fields never request HTML");
            Ok(b"exact text".to_vec())
        })
        .unwrap();
        assert!(
            matches!(text_only, Selection::Data(data) if data == [("text/plain".to_owned(), b"exact text".to_vec())])
        );
        let own = note_selection(
            vec!["pid/test".into(), "text/html".into()],
            Some("pid/test"),
            Box::new(|_, _| panic!("own HTML cannot be read through a UI-thread pipe")),
        );
        assert!(matches!(own, NoteSelection::Own));
    }

    #[test]
    fn native_png_mime_negotiation_saves_a_note_image() {
        let root = tempfile::tempdir().unwrap();
        let board = serve(&["image/png"], |mime, limit| {
            assert_eq!(mime, "image/png");
            assert_eq!(limit, offer::MAX_IMAGE_BYTES);
            Ok(png())
        });
        let routed = router::route(&board, &context(root.path())).unwrap();
        assert_eq!(routed.route, router::Route::Image);
        assert!(routed.insert.unwrap().contains("![[Pasted image "));
        assert_eq!(routed.saved_files.len(), 1);
        assert_eq!(image::open(&routed.saved_files[0]).unwrap().width(), 2);
    }

    #[test]
    fn native_uri_list_negotiation_reaches_the_file_copy_router() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source image.png");
        std::fs::write(&source, png()).unwrap();
        let vault = temp.path().join("vault");
        std::fs::create_dir(&vault).unwrap();
        let uri = url::Url::from_file_path(&source).unwrap();
        let list = format!("# copied files\r\n{uri}\r\nhttps://example.invalid/not-a-file\r\n");
        let (board, _) = native(&[(URI_LIST, list.as_bytes())]);
        assert_eq!(offer::file_paths(&board, &board.types()), vec![source]);
        let routed = router::route(&board, &context(&vault)).unwrap();
        assert!(matches!(
            routed.follow_up,
            Some(router::FollowUp::Files { .. })
        ));
    }

    /// #1618: a file manager's copy and cut (GNOME's own list, Dolphin's
    /// `text/uri-list` with its cut marker) schedule the same non-destructive
    /// copy job, and a picture or text offered beside the files is never
    /// transferred.
    #[test]
    fn native_file_manager_copy_and_cut_schedule_the_same_copy_job() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("report.txt");
        std::fs::write(&source, "original file").unwrap();
        let uri = url::Url::from_file_path(&source).unwrap();
        let cases = [
            ("gnome-copy", GNOME_COPIED_FILES, format!("copy\n{uri}\n")),
            (
                "gnome-cut",
                GNOME_COPIED_FILES,
                format!("cut\r\n# comment\r\n{uri}\r\n"),
            ),
            ("kde-cut", URI_LIST, format!("{uri}\r\n")),
        ];
        for (name, list, bytes) in cases {
            let vault = fixture.path().join(name);
            std::fs::create_dir(&vault).unwrap();
            let reads = Reads::default();
            let log = reads.clone();
            let offered = [
                list,
                "application/x-kde-cutselection",
                "image/png",
                "UTF8_STRING",
            ];
            let board = serve(&offered, move |mime, _| {
                log.borrow_mut().push(mime.to_owned());
                assert_eq!(mime, list, "#1618: {mime} transferred beside the file list");
                Ok(bytes.clone().into_bytes())
            });
            let routed = router::route(&board, &context(&vault)).unwrap();
            assert_eq!(routed.route, router::Route::Files, "{name}");
            let Some(router::FollowUp::Files { plan, .. }) = routed.follow_up else {
                panic!("{name}: an external file must use the background copy job");
            };
            assert_eq!(plan.copies(), 1);
            assert_eq!(*reads.borrow(), [list]);
            assert_eq!(std::fs::read(&source).unwrap(), b"original file");
        }
    }

    /// #1618: VS Code on Linux offers `vscode-editor-data`; its language
    /// names the fence, as on macOS.
    #[test]
    fn native_vscode_editor_data_fences_code_with_its_language() {
        let root = tempfile::tempdir().unwrap();
        let (board, reads) = native(&[
            ("text/plain", b"let answer = 42;\nprintln!(\"{answer}\");"),
            ("vscode-editor-data", br#"{"version":1,"mode":"rust"}"#),
            ("text/html", b"<div style=\"color:#d4d4d4\">colored</div>"),
        ]);
        let routed = router::route(&board, &context(root.path())).unwrap();
        assert_eq!(routed.route, router::Route::IdeCode);
        assert!(
            routed.insert.as_deref().unwrap().starts_with("```rust\n"),
            "{:?}",
            routed.insert
        );
        assert!(
            !reads.borrow().iter().any(|mime| mime == "text/html"),
            "the editor's colored HTML is not transferred"
        );
    }

    /// #1618: a browser's source page resolves relative links. Chromium
    /// sends it as UTF-8 with a trailing NUL; Firefox as UTF-16 without a
    /// byte order mark, and its HTML can be UTF-16 with one.
    #[test]
    fn native_browser_source_url_resolves_relative_links() {
        let utf16 = |text: &str, bom: bool| {
            bom.then_some([0xFF, 0xFE])
                .into_iter()
                .flatten()
                .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
                .collect::<Vec<u8>>()
        };
        let root = tempfile::tempdir().unwrap();
        let chromium = native(&[
            (
                "text/html",
                b"<p>See <a href=\"/docs/start\">Start here</a></p>",
            ),
            ("chromium/x-source-url", b"https://example.invalid/page\0"),
            ("text/plain", b"See Start here"),
        ])
        .0;
        let html = utf16("<p>См. <a href=\"start\">Начало</a></p>", true);
        let source = utf16("https://example.invalid/docs/page", false);
        let firefox = native(&[
            ("text/html", &html),
            ("text/x-moz-url-priv", &source),
            ("text/plain", "См. Начало".as_bytes()),
        ])
        .0;
        for (board, expected, url) in [
            (
                chromium,
                "See [Start here](https://example.invalid/docs/start)",
                "https://example.invalid/page",
            ),
            (
                firefox,
                "См. [Начало](https://example.invalid/docs/start)",
                "https://example.invalid/docs/page",
            ),
        ] {
            assert_eq!(board.source_url().as_deref(), Some(url));
            let routed = router::route(&board, &context(root.path())).unwrap();
            assert_eq!(routed.route, router::Route::RichText);
            assert_eq!(routed.insert.as_deref(), Some(expected));
        }
    }

    #[test]
    fn browser_text_flavors_decode_to_utf8() {
        let mut le = vec![0xFF, 0xFE];
        le.extend("<b>Привет</b>".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(text_utf8(le), "<b>Привет</b>".as_bytes());
        let mut be = vec![0xFE, 0xFF];
        be.extend("<i>x</i>".encode_utf16().flat_map(u16::to_be_bytes));
        assert_eq!(text_utf8(be), b"<i>x</i>");
        let bare: Vec<u8> = "https://a.invalid/\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(text_utf8(bare), b"https://a.invalid/");
        assert_eq!(
            text_utf8(b"https://a.invalid/\0".to_vec()),
            b"https://a.invalid/"
        );
        assert_eq!(
            text_utf8("<p>я</p>".as_bytes().to_vec()),
            "<p>я</p>".as_bytes()
        );
        assert_eq!(text_utf8(Vec::new()), b"");
    }

    #[test]
    fn own_image_and_unsupported_transport_use_typed_fallback_without_a_pipe_read() {
        let own = note_selection(
            vec!["pid/test".into(), "image/png".into()],
            Some("pid/test"),
            Box::new(|_, _| panic!("own offer must not block reading its own UI thread")),
        );
        let board = settle(Ok(own), || Board::fixed(vec![("image/png".into(), png())])).unwrap();
        assert!(
            matches!(offer::read_offer(&board).unwrap(), PasteOffer::Image(_)),
            "MIME1742: own image was discarded as text"
        );
        for error in [Error::Unsupported, Error::Io, Error::AmbiguousSeat] {
            let board = settle(Err(error), || {
                Board::fixed(vec![("image/png".into(), png())])
            })
            .unwrap();
            assert!(matches!(
                offer::read_offer(&board).unwrap(),
                PasteOffer::Image(_)
            ));
        }
        for error in [
            Error::ChangedOffer,
            Error::Timeout,
            Error::Cancelled,
            Error::TooLarge,
            Error::Unavailable,
        ] {
            assert!(settle(Err(error.clone()), || panic!("refusal must not fall back")).is_err());
        }
        assert!(settle(Ok(NoteSelection::Empty), || panic!(
            "empty selection must stay empty"
        ))
        .unwrap()
        .types()
        .is_empty());
    }

    /// #1618: an owner that refuses a flavor it listed hands the whole paste
    /// to GPUI's clipboard, as an unreadable offer does; nothing read before
    /// the refusal is kept.
    #[test]
    fn a_refused_flavor_reroutes_the_whole_paste_through_gpui() {
        let root = tempfile::tempdir().unwrap();
        let offer = note_selection(
            vec!["text/html".into(), "UTF8_STRING".into()],
            None,
            Box::new(|mime, _| match mime {
                "UTF8_STRING" => Ok(b"native text".to_vec()),
                _ => Err(Error::Io),
            }),
        );
        let (routed, detail) = run_on(
            Ok(offer),
            || {
                Board::fixed(vec![(
                    "text/plain;charset=utf-8".into(),
                    b"gpui text".to_vec(),
                )])
            },
            |board| route(board, &context(root.path())),
        )
        .unwrap();
        assert_eq!(routed.insert.as_deref(), Some("gpui text"));
        assert!(detail.starts_with("source=gpui"), "{detail}");
    }

    #[gpui::test]
    fn real_linux_note_adapter_keeps_gpui_image_fallback(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_image(&gpui::Image::from_bytes(
                gpui::ImageFormat::Png,
                png(),
            )));
            let clipboard = LinuxNoteClipboard {
                socket: None,
                seat: None,
                x11: None,
            };
            let routed = clipboard
                .route_in_command(&context(root.path()), cx)
                .unwrap();
            assert!(
                routed
                    .insert
                    .as_ref()
                    .is_some_and(|text| text.contains("![[Pasted image ")),
                "MIME1742: GPUI image fallback discarded"
            );
            assert_eq!(routed.saved_files.len(), 1);
            assert!(
                note_paste::platform_note_clipboard().is_some(),
                "Linux notes must install the typed provider"
            );
        });
    }

    /// #2006: on X11, a page copied in a browser offers `text/html`; GPUI
    /// 0.3.3 reads only its plain text. The X11 path must reach Markdown.
    /// #1618: the paste log names what was offered and what was read.
    #[gpui::test]
    fn x11_smart_paste_reads_html_and_images_that_gpui_drops(cx: &mut gpui::TestAppContext) {
        use exact_x11_clipboard::tests::{Owner, Xvfb};
        let Some(server) = Xvfb::start() else { return };
        let root = tempfile::tempdir().unwrap();
        let _owner = Owner::serve(
            &server.display,
            vec![
                ("text/html", b"<p>Hello <strong>world</strong></p>".to_vec()),
                ("UTF8_STRING", b"Hello world".to_vec()),
            ],
            usize::MAX,
        );
        cx.update(|cx| {
            // What GPUI's X11 clipboard reads from the same copy.
            cx.write_to_clipboard(gpui::ClipboardItem::new_string("Hello world".into()));
            let route = |x11: Option<(String, u32)>| {
                let clipboard = LinuxNoteClipboard {
                    socket: None,
                    seat: None,
                    x11,
                };
                clipboard
                    .route_in_command(&context(root.path()), cx)
                    .unwrap()
            };
            // Positive control: without the X11 read, GPUI's plain text is all there is.
            let gpui_only = route(None);
            assert_ne!(gpui_only.route, router::Route::RichText);
            assert_eq!(gpui_only.insert.as_deref(), Some("Hello world"));
            // The owner thread stands in for a browser: another process.
            let routed = route(Some((server.display.clone(), u32::MAX)));
            assert_eq!(
                routed.route,
                router::Route::RichText,
                "#2006: X11 smart paste got no text/html"
            );
            assert_eq!(routed.insert.as_deref(), Some("Hello **world**"));
            assert!(
                routed.detail.contains("source=native")
                    && routed
                        .detail
                        .contains(r#"read=["UTF8_STRING", "text/html"]"#),
                "{}",
                routed.detail
            );
        });
        drop(_owner);
        let pixels = png();
        let vault = root.path().join("image vault");
        std::fs::create_dir(&vault).unwrap();
        let _owner = Owner::serve(
            &server.display,
            vec![
                (
                    "text/html",
                    b"<img src=\"https://example.invalid/a.png\">".to_vec(),
                ),
                ("image/png", pixels),
                ("UTF8_STRING", b"https://example.invalid/a.png".to_vec()),
            ],
            usize::MAX,
        );
        cx.update(|cx| {
            let clipboard = LinuxNoteClipboard {
                socket: None,
                seat: None,
                x11: Some((server.display.clone(), u32::MAX)),
            };
            let routed = clipboard.route_in_command(&context(&vault), cx).unwrap();
            assert_eq!(routed.route, router::Route::Image);
            assert_eq!(
                routed.saved_files.len(),
                1,
                "image is embedded in the vault"
            );
            assert_eq!(image::open(&routed.saved_files[0]).unwrap().width(), 2);
        });
    }
}
