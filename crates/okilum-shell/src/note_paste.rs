//! Paste into a note (#1602): text from any text representation, and an image
//! as a file in the vault's attachment folder plus an Obsidian embed.
//!
//! Shared by every platform. A platform that can read its clipboard inside the
//! paste command implements [`NoteClipboard`] (macOS now); elsewhere the
//! note editor keeps the text-only [`ExactClipboardProvider`].
// Until the Linux and Windows lanes implement `NoteClipboard`, the offer reader is macOS-only.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub mod anchor;
pub mod attachments;
pub mod context;
pub mod download;
pub mod fetch;
pub mod files;
pub mod offer;
pub mod rich;
pub mod router;
pub mod title;

use gpui::{App, Global, Task};
use gpui_component::input::clipboard::{
    ClipboardReadError, ClipboardReadRequest, ExactClipboardProvider, PasteContext, PasteMode,
};
use offer::{OfferError, PasteOffer};
use router::{FollowUp, Notice, RouteContext, Routed};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

gpui::actions!(
    okilum_paste,
    [
        /// Option+Shift+Cmd+V on macOS, Ctrl+Shift+V elsewhere (#1611).
        PasteAsPlainText,
        /// Cmd+Alt+V / Ctrl+Alt+V (#1611).
        PasteAsTitledLink
    ]
);

/// The platform clipboard as a note sees it, read synchronously inside the
/// paste command: text (rich text already converted), an image, or nothing.
pub trait NoteClipboard: Send + Sync {
    fn read_offer(&self) -> Result<PasteOffer, OfferError>;
    /// Whether anything at all is on the clipboard (enables the Paste item).
    fn has_content(&self) -> bool;
    /// The clipboard's type names, for the paste log (never contents).
    fn types(&self) -> Vec<String> {
        Vec::new()
    }
    /// Smart paste (#1611): the router run against this clipboard.
    fn route(&self, context: &RouteContext) -> Result<Routed, OfferError>;
    /// Copied image or video files, or image data, for the Feedback window
    /// (#1609): attached, never pasted as a path.
    fn read_attachment(&self) -> Option<offer::ClipAttachment>;
}

/// What a paste left to do after its text went in: a notice for the toast and
/// background work for the editor (#1611). Taken by the editor's paste
/// event handler.
#[derive(Debug, Default)]
pub struct Outbox {
    pub notice: Option<Notice>,
    pub follow_up: Option<FollowUp>,
}
pub type PasteOutbox = Arc<Mutex<Outbox>>;

gpui::actions!(okilum_edit, [PasteFromMenu]);

/// The Edit menu. Paste is the paste command for the focused note editor,
/// for a mouse user and for tools that press the menu item instead of posting
/// Cmd+V (#1602). It has no key equivalent of its own; Cmd+V stays with the
/// editor's binding, and the Cmd+V key monitor finds this item by its title
/// "Paste". Paste as Plain Text and Paste as Titled Link (#1611) act on the
/// focused note editor and show their shortcuts.
pub fn edit_menu() -> gpui::Menu {
    gpui::Menu {
        name: "Edit".into(),
        disabled: false,
        items: vec![
            gpui::MenuItem::action("Paste", PasteFromMenu),
            gpui::MenuItem::action("Paste as Plain Text", PasteAsPlainText),
            gpui::MenuItem::action("Paste as Titled Link", PasteAsTitledLink),
        ],
    }
}

pub fn install_menu(cx: &mut App) {
    cx.on_action(|_: &PasteFromMenu, cx: &mut App| {
        eprintln!("Paste: Edit menu");
        // Dispatch after the menu command releases the window.
        cx.defer(|cx| {
            let Some(window) = cx
                .active_window()
                .or_else(|| cx.windows().into_iter().next())
            else {
                return;
            };
            let _ = window.update(cx, |_, window, cx| {
                window.dispatch_action(Box::new(gpui_component::input::Paste), cx);
            });
        });
    });
}

/// Files dropped onto a note, routed like a paste of their URLs (#1611).
pub struct DroppedFiles(pub Vec<PathBuf>);

impl DroppedFiles {
    fn urls(&self) -> Vec<String> {
        self.0
            .iter()
            .filter_map(|path| url::Url::from_file_path(path).ok())
            .map(|url| url.to_string())
            .collect()
    }
}

impl offer::Pasteboard for DroppedFiles {
    fn change_count(&self) -> i64 {
        0
    }
    fn types(&self) -> Vec<String> {
        vec![offer::FILE_URL.to_string()]
    }
    fn data(&self, kind: &str) -> Option<Vec<u8>> {
        self.string(kind).map(String::into_bytes)
    }
    fn string(&self, kind: &str) -> Option<String> {
        if kind != offer::FILE_URL {
            return None;
        }
        self.urls().into_iter().next()
    }
    fn rich_text(&self, _: &[u8], _: bool) -> Option<String> {
        None
    }
    fn file_urls(&self) -> Vec<String> {
        self.urls()
    }
}

/// Settings → Editor: ask before copying a file larger than this many MB
/// into the vault (0: never ask).
pub fn ask_above_bytes(cx: &App) -> Option<u64> {
    let mb = crate::reader_ui_state::paste_ask_above_mb(cx);
    (mb > 0).then(|| u64::from(mb) * 1024 * 1024)
}

fn router_mode(mode: PasteMode) -> router::Mode {
    match mode {
        PasteMode::Smart => router::Mode::Smart,
        PasteMode::Plain => router::Mode::Plain,
        PasteMode::TitledLink => router::Mode::TitledLink,
    }
}

pub struct ManagedNoteClipboard(pub Arc<dyn NoteClipboard>);
impl Global for ManagedNoteClipboard {}

/// This platform's note clipboard, if it has one.
pub fn platform_note_clipboard() -> Option<Arc<dyn NoteClipboard>> {
    #[cfg(target_os = "macos")]
    {
        Some(Arc::new(
            crate::platform::exact_macos_clipboard::MacClipboard,
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

pub fn clipboard_error(error: OfferError) -> ClipboardReadError {
    match error {
        OfferError::TooLarge => ClipboardReadError::TooLarge,
        OfferError::InvalidUtf8 => ClipboardReadError::InvalidUtf8,
        OfferError::Changed => ClipboardReadError::ChangedOffer,
        OfferError::Io => ClipboardReadError::Io,
    }
}

/// Why the last paste inserted nothing, when the editor's error kind can't
/// say it (an image that couldn't be saved). Taken by the toast.
pub type PasteNotice = Arc<Mutex<Option<String>>>;

/// The note editor's clipboard provider. `note_rel` is `None` for a plain
/// file (CSV, code), which takes text only.
pub fn provider_for_note(
    cx: &App,
    vault_root: PathBuf,
    note_rel: Option<String>,
    notice: PasteNotice,
    outbox: PasteOutbox,
) -> Option<Arc<dyn ExactClipboardProvider>> {
    let text = cx
        .try_global::<crate::platform::ManagedClipboard>()
        .map(|managed| managed.0.clone());
    let Some(clipboard) = cx
        .try_global::<ManagedNoteClipboard>()
        .map(|managed| managed.0.clone())
    else {
        return text;
    };
    Some(Arc::new(NotePaste {
        text,
        clipboard,
        vault_root,
        note_rel,
        notice,
        outbox,
    }))
}

struct NotePaste {
    text: Option<Arc<dyn ExactClipboardProvider>>,
    clipboard: Arc<dyn NoteClipboard>,
    vault_root: PathBuf,
    note_rel: Option<String>,
    notice: PasteNotice,
    outbox: PasteOutbox,
}

impl ExactClipboardProvider for NotePaste {
    fn read_text(
        &self,
        request: ClipboardReadRequest,
        cx: &App,
    ) -> Task<Result<Option<String>, ClipboardReadError>> {
        match &self.text {
            Some(text) => text.read_text(request, cx),
            None => Task::ready(Err(ClipboardReadError::Unsupported)),
        }
    }

    fn read_text_now(&self, _: &App) -> Option<Result<Option<String>, ClipboardReadError>> {
        let started = std::time::Instant::now();
        let offer = self.clipboard.read_offer();
        let (path, bytes) = describe(&offer);
        let types = self.clipboard.types();
        let result = paste_now(
            offer,
            &self.vault_root,
            self.note_rel.as_deref(),
            &self.notice,
            &attachments::stamp_now(),
        );
        // One line per paste: type names, the path taken, sizes, time. Never contents.
        let outcome = match &result {
            Ok(Some(text)) => format!("inserted={}", text.len()),
            Ok(None) => "inserted=0".into(),
            Err(error) => format!("error={error:?}"),
        };
        eprintln!(
            "Paste: attempt types={types:?} path={path} bytes={bytes} {outcome} ms={}",
            started.elapsed().as_millis()
        );
        Some(result)
    }

    /// Smart paste, Paste as Plain Text and Paste as Titled Link (#1611): the
    /// router decides from the clipboard and the caret; a notice and any
    /// background work go to the outbox for the editor.
    fn read_paste_now(
        &self,
        context: &PasteContext,
        cx: &App,
    ) -> Option<Result<Option<String>, ClipboardReadError>> {
        let started = std::time::Instant::now();
        if let Ok(mut slot) = self.notice.lock() {
            *slot = None;
        }
        if let Ok(mut outbox) = self.outbox.lock() {
            *outbox = Outbox::default();
        }
        let stamp = attachments::stamp_now();
        let route_context = RouteContext {
            mode: router_mode(context.mode),
            source: &context.source,
            selection: context.selection.clone(),
            vault_root: &self.vault_root,
            note_rel: self.note_rel.as_deref(),
            stamp: &stamp,
            ask_above: ask_above_bytes(cx),
        };
        let routed = self.clipboard.route(&route_context);
        let elapsed = started.elapsed();
        Some(match routed {
            Ok(routed) => {
                // Type names, the path taken, sizes and time; never contents.
                eprintln!(
                    "Paste: mode={:?} route={:?} bytes={} inserted={} follow_up={} ms={:.1} {} types={:?}",
                    context.mode,
                    routed.route,
                    routed.bytes,
                    routed.insert.as_ref().map_or(0, String::len),
                    routed.follow_up.is_some(),
                    elapsed.as_secs_f64() * 1000.0,
                    routed.detail,
                    routed.types
                );
                let insert = routed.insert;
                let failed = routed
                    .notice
                    .as_ref()
                    .is_some_and(|notice| notice.error && insert.is_none());
                if failed {
                    if let (Ok(mut slot), Some(notice)) = (self.notice.lock(), &routed.notice) {
                        *slot = Some(notice.text.clone());
                    }
                } else if let Ok(mut outbox) = self.outbox.lock() {
                    outbox.notice = routed.notice;
                    outbox.follow_up = routed.follow_up;
                }
                if failed {
                    Err(ClipboardReadError::Io)
                } else {
                    Ok(insert)
                }
            }
            Err(error) => {
                eprintln!("Paste: clipboard read failed ({error:?})");
                Err(clipboard_error(error))
            }
        })
    }
}

/// The path a paste takes and its size, for the log.
fn describe(offer: &Result<PasteOffer, OfferError>) -> (&'static str, u64) {
    match offer {
        Ok(PasteOffer::Text(text)) => ("text", text.len() as u64),
        Ok(PasteOffer::Image(image)) => ("image", image.bytes.len() as u64),
        Ok(PasteOffer::Files(files)) => (
            "files",
            files
                .iter()
                .filter_map(|f| std::fs::metadata(f).ok())
                .map(|m| m.len())
                .sum(),
        ),
        Ok(PasteOffer::Nothing { .. }) => ("nothing", 0),
        Err(_) => ("error", 0),
    }
}

/// The text the editor inserts at the caret for one clipboard offer. An image
/// is saved first; its embed is returned only once the file is on disk.
fn paste_now(
    offer: Result<PasteOffer, OfferError>,
    vault_root: &std::path::Path,
    note_rel: Option<&str>,
    notice: &PasteNotice,
    stamp: &str,
) -> Result<Option<String>, ClipboardReadError> {
    let tell = |text: String| {
        if let Ok(mut slot) = notice.lock() {
            *slot = Some(text);
        }
    };
    if let Ok(mut slot) = notice.lock() {
        *slot = None;
    }
    match offer {
        Ok(PasteOffer::Text(text)) => Ok(Some(text)),
        Ok(PasteOffer::Image(image)) => {
            let Some(note_rel) = note_rel else {
                eprintln!("Paste: image refused, this file takes text only");
                tell("Images can be pasted into notes, not into this file.".into());
                return Ok(None);
            };
            match attachments::save_pasted_image(vault_root, note_rel, &image, stamp) {
                Ok(saved) => {
                    eprintln!(
                        "Paste: saved image {:?} ({:?}, {} bytes)",
                        saved.file_name,
                        image.kind,
                        image.bytes.len()
                    );
                    Ok(Some(saved.embed))
                }
                Err(error) => {
                    eprintln!("Paste: image not saved ({error:?}), nothing inserted");
                    tell(error.user_text());
                    Err(ClipboardReadError::Io)
                }
            }
        }
        Ok(PasteOffer::Files(files)) => {
            let Some(note_rel) = note_rel else {
                // A plain file takes the paths as text.
                let paths: Vec<String> = files
                    .iter()
                    .map(|f| f.to_string_lossy().into_owned())
                    .collect();
                return Ok(Some(paths.join("\n")));
            };
            match attachments::save_pasted_files(vault_root, note_rel, &files, stamp) {
                Ok(saved) => {
                    eprintln!(
                        "Paste: files={} copied={} ({} bytes) in_vault={} linked_outside={}",
                        files.len(),
                        saved.copied,
                        saved.copied_bytes,
                        saved.in_vault,
                        saved.linked_outside.len()
                    );
                    if !saved.linked_outside.is_empty() {
                        tell(format!(
                            "Linked, not copied (over 100 MB or a folder): {}",
                            saved.linked_outside.join(", ")
                        ));
                    }
                    Ok(Some(saved.text))
                }
                Err(error) => {
                    eprintln!("Paste: files not saved ({error:?}), nothing inserted");
                    tell(error.user_text());
                    Err(ClipboardReadError::Io)
                }
            }
        }
        Ok(PasteOffer::Nothing { types }) => {
            // Type names only, never contents: the next report shows what was there.
            eprintln!("Paste: no text or image on the clipboard; types: {types:?}");
            Ok(None)
        }
        Err(error) => {
            eprintln!("Paste: clipboard read failed ({error:?})");
            Err(clipboard_error(error))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::offer::{ImageKind, PastedImage};
    use super::*;

    fn png() -> PastedImage {
        PastedImage {
            bytes: b"\x89PNG\r\n\x1a\n".to_vec(),
            kind: ImageKind::Png,
            source: None,
        }
    }

    /// #1602: an image on the clipboard is saved, then its embed is inserted.
    #[test]
    fn image_paste_saves_the_file_then_inserts_the_embed() {
        let vault = tempfile::tempdir().unwrap();
        let notice = PasteNotice::default();
        let inserted = paste_now(
            Ok(PasteOffer::Image(png())),
            vault.path(),
            Some("Daily/2026-10-23.md"),
            &notice,
            "20261023180000",
        );
        assert_eq!(
            inserted,
            Ok(Some("![[Pasted image 20261023180000.png]]".into()))
        );
        assert!(vault
            .path()
            .join("attachments/Pasted image 20261023180000.png")
            .is_file());
        assert_eq!(*notice.lock().unwrap(), None);
    }

    /// #1602: a failed save inserts nothing and says why.
    #[test]
    fn image_paste_failure_inserts_nothing_and_explains() {
        let vault = tempfile::tempdir().unwrap();
        std::fs::write(vault.path().join("attachments"), b"a file, not a folder").unwrap();
        let notice = PasteNotice::default();
        let inserted = paste_now(
            Ok(PasteOffer::Image(png())),
            vault.path(),
            Some("n.md"),
            &notice,
            "20261023180000",
        );
        assert_eq!(inserted, Err(ClipboardReadError::Io));
        assert!(notice
            .lock()
            .unwrap()
            .as_deref()
            .is_some_and(|text| text.contains("attachment folder")));
        // A plain file takes text only.
        let inserted = paste_now(
            Ok(PasteOffer::Image(png())),
            vault.path(),
            None,
            &notice,
            "20261023180000",
        );
        assert_eq!(inserted, Ok(None));
        assert!(notice.lock().unwrap().is_some());
    }

    /// #1602 Raycast: a file URL into its cache is copied and embedded.
    #[test]
    fn pasted_files_are_saved_then_linked() {
        let vault = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let doc = outside.path().join("spec.pdf");
        std::fs::write(&doc, b"%PDF").unwrap();
        let notice = PasteNotice::default();
        let inserted = paste_now(
            Ok(PasteOffer::Files(vec![doc.clone()])),
            vault.path(),
            Some("n.md"),
            &notice,
            "20261023180000",
        );
        assert_eq!(inserted, Ok(Some("[[spec.pdf]]".into())));
        assert!(vault.path().join("attachments/spec.pdf").is_file());
        assert_eq!(*notice.lock().unwrap(), None);
        // A plain file takes the path as text.
        let inserted = paste_now(
            Ok(PasteOffer::Files(vec![doc.clone()])),
            vault.path(),
            None,
            &notice,
            "x",
        );
        assert_eq!(inserted, Ok(Some(doc.to_string_lossy().into_owned())));
    }

    #[test]
    fn text_and_nothing_pass_through() {
        let vault = tempfile::tempdir().unwrap();
        let notice = PasteNotice::default();
        *notice.lock().unwrap() = Some("stale".into());
        assert_eq!(
            paste_now(
                Ok(PasteOffer::Text("hi".into())),
                vault.path(),
                Some("n.md"),
                &notice,
                "x"
            ),
            Ok(Some("hi".into()))
        );
        assert_eq!(*notice.lock().unwrap(), None);
        assert_eq!(
            paste_now(
                Ok(PasteOffer::Nothing { types: vec![] }),
                vault.path(),
                Some("n.md"),
                &notice,
                "x"
            ),
            Ok(None)
        );
        assert_eq!(
            paste_now(Err(OfferError::Changed), vault.path(), None, &notice, "x"),
            Err(ClipboardReadError::ChangedOffer)
        );
    }
}
