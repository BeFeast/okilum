//! Paste into a note (#1602): text from any text representation, and an image
//! as a file in the vault's attachment folder plus an Obsidian embed.
//!
//! Shared by every platform. A platform that can read its clipboard inside the
//! paste command implements [`NoteClipboard`] (macOS now); elsewhere the
//! note editor keeps the text-only [`ExactClipboardProvider`].
// Until the Linux and Windows lanes implement `NoteClipboard`, the offer reader is macOS-only.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub mod attachments;
pub mod offer;

use gpui::{App, Global, Task};
use gpui_component::input::clipboard::{
    ClipboardReadError, ClipboardReadRequest, ExactClipboardProvider,
};
use offer::{OfferError, PasteOffer};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The platform clipboard as a note sees it, read synchronously inside the
/// paste command: text (rich text already converted), an image, or nothing.
pub trait NoteClipboard: Send + Sync {
    fn read_offer(&self) -> Result<PasteOffer, OfferError>;
    /// Whether anything at all is on the clipboard (enables the Paste item).
    fn has_content(&self) -> bool;
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
    }))
}

struct NotePaste {
    text: Option<Arc<dyn ExactClipboardProvider>>,
    clipboard: Arc<dyn NoteClipboard>,
    vault_root: PathBuf,
    note_rel: Option<String>,
    notice: PasteNotice,
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
        Some(paste_now(
            self.clipboard.read_offer(),
            &self.vault_root,
            self.note_rel.as_deref(),
            &self.notice,
            &attachments::stamp_now(),
        ))
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
