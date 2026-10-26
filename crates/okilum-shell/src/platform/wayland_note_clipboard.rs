//! Typed Linux note clipboard over the existing bounded data-control reader.
//! GPUI's own connection supplies self-owned/X11/unsupported-compositor offers.
use super::exact_wayland_clipboard::{self, Selection};
use crate::note_paste::{
    self,
    offer::{self, OfferError, PasteOffer, Pasteboard},
    router::{self, RouteContext, Routed},
    NoteClipboard,
};
use gpui::{App, ClipboardEntry};
use gpui_component::input::clipboard::ClipboardReadError as Error;
use std::path::PathBuf;

pub struct LinuxNoteClipboard {
    socket: Option<PathBuf>,
    seat: Option<String>,
}
impl Default for LinuxNoteClipboard {
    fn default() -> Self {
        Self::new()
    }
}
impl LinuxNoteClipboard {
    pub fn new() -> Self {
        Self {
            socket: exact_wayland_clipboard::existing_compositor(),
            seat: std::env::var("OKILUM_CLIPBOARD_SEAT").ok(),
        }
    }
    fn read(&self) -> Result<Selection, Error> {
        exact_wayland_clipboard::read_note_in_command(self.socket.as_deref(), self.seat.clone())
    }
}

#[derive(Default)]
struct Board(Vec<(String, Vec<u8>)>);
fn alias(mime: &str) -> &str {
    match mime {
        "image/png" => "public.png",
        "image/jpeg" => "public.jpeg",
        "image/webp" => "org.webmproject.webp",
        "image/gif" => "com.compuserve.gif",
        "image/tiff" => "public.tiff",
        "image/bmp" => "com.microsoft.bmp",
        "text/uri-list" => offer::FILE_URL,
        "text/plain;charset=utf-8" | "UTF8_STRING" | "text/plain" => "public.utf8-plain-text",
        _ => mime,
    }
}
impl Pasteboard for Board {
    fn change_count(&self) -> i64 {
        0
    } // Immutable bytes after the protocol barrier.
    fn types(&self) -> Vec<String> {
        self.0
            .iter()
            .map(|(mime, _)| alias(mime).to_owned())
            .collect()
    }
    fn data(&self, kind: &str) -> Option<Vec<u8>> {
        self.0
            .iter()
            .find(|(mime, _)| alias(mime) == kind)
            .map(|(_, bytes)| bytes.clone())
    }
    fn string(&self, kind: &str) -> Option<String> {
        String::from_utf8(self.data(kind)?).ok()
    }
    fn rich_text(&self, _: &[u8], _: bool) -> Option<String> {
        None
    }
    fn file_urls(&self) -> Vec<String> {
        self.0
            .iter()
            .filter(|(mime, _)| mime == "text/uri-list")
            .flat_map(|(_, bytes)| {
                String::from_utf8_lossy(bytes)
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                    .filter_map(|line| url::Url::parse(line).ok())
                    .filter(|url| url.to_file_path().is_ok())
                    .map(|url| url.to_string())
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}
fn settle(
    selection: Result<Selection, Error>,
    platform: impl FnOnce() -> Board,
) -> Result<Board, Error> {
    match selection {
        Ok(Selection::Data(data)) => Ok(Board(data)),
        Ok(Selection::Empty) => Ok(Board::default()),
        Ok(Selection::Own) | Err(Error::Unsupported | Error::AmbiguousSeat | Error::Io) => {
            Ok(platform())
        }
        Err(error) => Err(error),
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
                    data.push(("text/uri-list".into(), urls.into_bytes()));
                }
            }
        }
    }
    Board(data)
}
fn offer_error(error: Error) -> OfferError {
    match error {
        Error::TooLarge => OfferError::TooLarge,
        Error::InvalidUtf8 => OfferError::InvalidUtf8,
        Error::ChangedOffer => OfferError::Changed,
        _ => OfferError::Io,
    }
}
impl NoteClipboard for LinuxNoteClipboard {
    fn read_offer(&self) -> Result<PasteOffer, OfferError> {
        let board = settle(self.read(), Board::default).map_err(offer_error)?;
        offer::read_offer(&board)
    }
    fn has_content(&self) -> bool {
        true
    } // Availability is checked inside the user's command.
    fn route(&self, context: &RouteContext) -> Result<Routed, OfferError> {
        let board = settle(self.read(), Board::default).map_err(offer_error)?;
        router::route(&board, context)
    }
    fn route_in_command(&self, context: &RouteContext, cx: &App) -> Result<Routed, Error> {
        let board = settle(self.read(), || platform_board(cx))?;
        router::route(&board, context).map_err(note_paste::clipboard_error)
    }
    fn read_attachment(&self) -> Option<offer::ClipAttachment> {
        let board = settle(self.read(), Board::default).ok()?;
        offer::read_attachment(&board)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use exact_wayland_clipboard::receive_selection;

    fn png() -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 2)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }
    fn context(root: &std::path::Path) -> RouteContext<'_> {
        RouteContext {
            mode: router::Mode::Smart,
            source: "",
            selection: 0..0,
            vault_root: root,
            note_rel: Some("note.md"),
            stamp: "20261026000000",
            ask_above: None,
        }
    }

    #[test]
    fn native_png_mime_negotiation_saves_a_note_image() {
        let root = tempfile::tempdir().unwrap();
        let selection = receive_selection(&["image/png".into()], None, true, |mime, limit| {
            assert_eq!(mime, "image/png");
            assert_eq!(limit, offer::MAX_IMAGE_BYTES);
            Ok(png())
        })
        .unwrap_or_else(|_| panic!("MIME1742: native PNG offer was rejected"));
        let board = settle(Ok(selection), || panic!("native image must not fall back")).unwrap();
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
        let selection = receive_selection(&["text/uri-list".into()], None, true, |mime, _| {
            assert_eq!(mime, "text/uri-list");
            Ok(
                format!("# copied files\r\n{uri}\r\nhttps://example.invalid/not-a-file\r\n")
                    .into_bytes(),
            )
        })
        .unwrap_or_else(|_| panic!("MIME1742: native file URI offer was rejected"));
        let board = settle(Ok(selection), || panic!("native files must not fall back")).unwrap();
        assert_eq!(offer::file_paths(&board, &board.types()), vec![source]);
        let routed = router::route(&board, &context(&vault)).unwrap();
        assert!(matches!(
            routed.follow_up,
            Some(router::FollowUp::Files { .. })
        ));
    }

    #[test]
    fn own_image_and_unsupported_transport_use_typed_fallback_without_a_pipe_read() {
        let selection = receive_selection(
            &["pid/test".into(), "image/png".into()],
            Some("pid/test"),
            true,
            |_, _| panic!("own offer must not block reading its own UI thread"),
        )
        .unwrap();
        let board = settle(Ok(selection), || Board(vec![("image/png".into(), png())])).unwrap();
        assert!(
            matches!(offer::read_offer(&board).unwrap(), PasteOffer::Image(_)),
            "MIME1742: own image was discarded as text"
        );
        for error in [Error::Unsupported, Error::Io, Error::AmbiguousSeat] {
            let board = settle(Err(error), || Board(vec![("image/png".into(), png())])).unwrap();
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
        assert!(settle(Ok(Selection::Empty), || panic!(
            "empty selection must stay empty"
        ))
        .unwrap()
        .types()
        .is_empty());
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
}
