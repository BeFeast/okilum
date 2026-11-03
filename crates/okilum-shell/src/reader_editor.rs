//! Reader source mode. No Brain enrollment or changes to the reader protocol.
mod block_context;
mod block_entry;
mod code_comforts;
mod context_menu;
mod external_edit;
pub(crate) mod format;
mod ime_guard;
mod link_complete;
mod lists;
mod live_preview;
mod math_preview;
mod open_note_sync;
mod paste_jobs;
mod recovery;
mod status;
mod tables;
#[cfg(test)]
pub(crate) mod work;
mod writing_modes;
use super::*;
use crate::platform::labels::Os;
use block_context::BlockNote;
pub(crate) use block_entry::OpenRenderedBlock;
pub(crate) use external_edit::{OpenBlockExternally, StopBlockExternally};
use gpui_component::input::clipboard::{ClipboardPasteEvent, ClipboardReadError, PasteMode};
use gpui_component::input::projection::{
    ActiveSource, MarkerClick, MarkerKind, ProjectionProvider, SourceClick, SourceMutation,
    SourceProjection, SourceSnapshot, SourceStamp,
};
use gpui_component::input::{Editor, EditorState, WrappingIndent};
pub(crate) use writing_modes::{toggle_focus_mode, toggle_typewriter_mode};

/// The editor «Open block in …» names (#1993).
pub(crate) fn external_editor_label() -> String {
    external_edit::editor_label()
}

/// What a paste that inserted nothing tells the user (#1602); it is never
/// silent. `None` for a second paste while the first is still reading.
fn paste_failure_text(error: &ClipboardReadError, image: bool) -> Option<&'static str> {
    use ClipboardReadError as E;
    Some(match error {
        E::Unavailable if image => "Images can’t be pasted into a note yet, only text.",
        E::Unavailable => "Nothing to paste: the clipboard has no text or image.",
        E::Busy => return None,
        E::Cancelled => "Paste was cancelled. Click in the note and paste again.",
        E::Timeout => "The clipboard didn’t answer in time. Paste again.",
        E::TooLarge => "The clipboard text is too large to paste.",
        E::InvalidUtf8 => "The clipboard text isn’t valid UTF-8, so it wasn’t pasted.",
        E::ChangedOffer => "The clipboard changed while pasting. Paste again.",
        E::Unsupported | E::AmbiguousSeat => "Paste isn’t available here.",
        E::Io => "The clipboard couldn’t be read.",
    })
}

#[cfg(test)]
fn source_link_at(source: &str, offset: usize) -> Option<okilum_core::document_links::ParsedLink> {
    if !source.is_char_boundary(offset) {
        return None;
    }
    okilum_core::document_links::parse(source)
        .into_iter()
        .find(|link| link.range.contains(&offset))
}

struct ExactSource;
impl ProjectionProvider for ExactSource {
    fn compose(&self, _: &SourceSnapshot, _: &ActiveSource) -> Option<Arc<dyn SourceProjection>> {
        None
    }
}

use okilum_core::file_editor::{DiskStamp, FileEditor, Save};
use okilum_core::source_projection::revision::Text as DraftText;

/// The pause in typing after which an edit is saved (#1353).
const RECOVERY_PAUSE: std::time::Duration = std::time::Duration::from_millis(300);
#[cfg(test)]
thread_local! { static LIVE_PREPARES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
thread_local! { static FAIL_AUTOSAVE_COPY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
const AUTOSAVE_START_NOTICE: &str = "Automatic Save couldn’t start. Your edits are still here. Retry Save or keep this window open.";
const AUTOSAVE_PAUSE: std::time::Duration = std::time::Duration::from_secs(1);
/// How every conflict notice starts, so a conflict that went away takes its
/// notice with it and leaves any other notice alone (#1376).
const CONFLICT_NOTICE: &str = "This file changed outside Okilum.";

fn conflict_copy_rel(current_rel: &str) -> String {
    let path = Path::new(current_rel);
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let extension = path
        .extension()
        .map(|value| format!(".{}", value.to_string_lossy()))
        .unwrap_or_default();
    let id = uuid::Uuid::new_v4();
    // This random discriminator is not a claim about a device identity.
    let suffix: String = id
        .as_bytes()
        .iter()
        .take(7)
        .map(|b| b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"[(b & 31) as usize] as char)
        .collect();
    let name = format!(
        "{stem}.okilum-conflict-{}-{suffix}{extension}",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    // Syncthing prunes *.sync-conflict-*. Even an original named by Syncthing
    // must not pass its retention marker on to an Okilum-owned copy.
    let name = name.replace(".sync-conflict-", ".syncthing-conflict-");
    okilum_core::vault::note_path(&path.with_file_name(name))
}

/// A disk reload keeps the native editor entity, focus, cursor and viewport.
/// set_value deliberately clears undo: the old base must not be undoable over
/// a newly arrived remote version without another revision-checked save.
fn replace_clean_source(
    input: &mut EditorState,
    text: String,
    window: &mut Window,
    cx: &mut Context<EditorState>,
) {
    let selection = input.selected_range();
    let cursor = input.cursor();
    let directed = if cursor == selection.start && !selection.is_empty() {
        selection.end..selection.start
    } else {
        selection
    };
    let scroll = input.scroll_offset();
    input.set_value(text, window, cx);
    input.set_selected_range_preserving_view(directed, cursor, cx);
    input.set_scroll_offset(scroll, cx);
}

/// Paint-only status. Destructive actions keep using source_is_dirty, which
/// reads the current native buffer directly rather than trusting this cache.
#[derive(Clone, Copy, PartialEq, Eq)]
struct SourceStatusKey {
    source: SourceStamp,
    recovery_epoch: u64,
    saves: u64,
    saved_at: Option<time::OffsetDateTime>,
    disk_stamp: Option<DiskStamp>,
    conflict: bool,
    protecting: bool,
}

#[derive(Clone, Copy)]
struct SourceStatus {
    key: SourceStatusKey,
    store_dirty: bool,
    dirty: bool,
}

#[cfg(test)]
thread_local! {
    static SOURCE_STATUS_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn take_source_status_bytes() -> usize {
    SOURCE_STATUS_BYTES.with(|bytes| bytes.replace(0))
}

pub(super) struct Editing {
    store: FileEditor,
    source_status: std::cell::Cell<Option<SourceStatus>>,
    live_preview: live_preview::LivePreview,
    /// The card under an inline formula holding the caret (#1645).
    math_preview: Entity<math_preview::MathPreview>,
    /// One persistent cell grid per editor; cell changes use the source Undo path.
    table_grid: Entity<tables::TableGrid>,
    save_status: Entity<status::SaveStatus>,
    block_context: BlockContext,
    block_cache: block_context::WorkerCache,
    input: Entity<EditorState>,
    conflict: Option<String>,
    conflict_detected: bool,
    conflict_copy: Option<(SourceStamp, String)>,
    compare: bool,
    save_failed: bool,
    protecting: bool,
    recovery_epoch: u64,
    draft_stamp: gpui_component::input::projection::SourceStamp,
    recovery_task: Option<Task<()>>,
    recovery_pending: Option<recovery::Pending>,
    save_pending: bool,
    /// The text an autosave is writing on a worker (#1353).
    autosaving: Option<DraftText>,
    /// A disk refresh came while that autosave was writing; it runs once the
    /// save is back (#1376).
    refresh_deferred: bool,
    /// Opened on a new note in Live Preview: the Source/Live Preview choice to
    /// keep when it is left still in Live Preview (#1352).
    kept_preference: Option<bool>,
    /// Saves made on the UI thread; an autosave result older than one is dropped.
    saves: u64,
    saved_at: Option<time::OffsetDateTime>,
    current_input: Entity<EditorState>,
    /// Snapshot for the editor's context menu, which cannot read the editor.
    menu_facts: reader_reminder::MenuFacts,
    #[cfg(test)]
    paste_outbox: crate::note_paste::PasteOutbox,
    /// A Markdown note, not a code or text file: lists continue (#1635).
    markdown: bool,
    /// Backspace right after the last list continuation (#1635).
    list_revert: Option<lists::PendingRevert>,
    /// Inferred indentation belongs to the active code fence/editor session.
    code_indentation: code_comforts::Indentation,
    /// Code blocks open in an external editor (#1993); dropped with the
    /// editor, which removes their temporary files.
    external: external_edit::Sessions,
    /// The note list under a `[[` being typed (#1921).
    link_complete: link_complete::State,
    _subscriptions: Vec<Subscription>,
}

/// The authored reason an editor could not start, or plain words for an OS
/// error with no authored context. The raw chain stays in stderr.
fn edit_error_text(error: &anyhow::Error) -> String {
    use std::io::ErrorKind;
    // `downcast_ref` looks through context, so inspect only the outermost layer.
    let outermost = error.chain().next();
    let Some(io) = outermost.and_then(|e| e.downcast_ref::<std::io::Error>()) else {
        return error.to_string();
    };
    match io.kind() {
        ErrorKind::PermissionDenied => "Okilum doesn’t have permission to change this note.",
        ErrorKind::NotFound => "This note no longer exists. It may have been moved or deleted.",
        _ => "This note couldn’t be opened for editing.",
    }
    .into()
}

/// The editor's notice for a note that went away (the deleted-note notice of
/// #1986 says more and replaces it).
pub(super) const MOVED_OR_DELETED_NOTICE: &str = "This file was moved or deleted. Your edits are still here. Copy them or close, keeping a draft.";

const DRAFT_WRITE_NOTICE: &str =
    "The recovery copy couldn’t be saved. Keep this window open and retry Save or copy your draft.";

fn missing_source(error: &anyhow::Error) -> bool {
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    }) {
        return true;
    }
    #[cfg(unix)]
    if error
        .chain()
        .any(|cause| cause.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::NOENT))
    {
        return true;
    }
    false
}

fn file_access_notice(error: &anyhow::Error) -> &'static str {
    if let Some(notice) = crate::reader_file_access_notice::publication_notice(error) {
        return notice;
    }
    if error.is::<okilum_core::file_editor::PublishedSaveNeedsAttention>() {
        return "The save was published, but recovery changed during verification. Your draft is preserved. Inspect the file before saving again.";
    }
    #[cfg(unix)]
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<rustix::io::Errno>()
            .is_some_and(|e| okilum_core::unix_publish::unsupported(*e))
            || cause
                .downcast_ref::<std::io::Error>()
                .and_then(|e| e.raw_os_error())
                .is_some_and(|e| {
                    okilum_core::unix_publish::unsupported(rustix::io::Errno::from_raw_os_error(e))
                })
    }) {
        return "This drive does not support the required safe file operation. Your edits are preserved. Copy them or keep a draft.";
    }
    if error.chain().any(|cause| {
        #[cfg(unix)]
        if cause.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::NOENT) {
            return true;
        }
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    }) {
        MOVED_OR_DELETED_NOTICE
    } else {
        "Okilum couldn’t access this file. Your edits are still here. Copy them or close, keeping a draft."
    }
}

/// A revision of the exact saved UTF-8 source, never a native editor generation.
/// Only AGENTS.md has this receipt; callers publish it after confirmed success.
fn saved_agents_revision(path: &Path, changed: bool, text: &str) -> Option<(PathBuf, String)> {
    if !changed || !path.is_absolute() || path.file_name()?.to_str()? != "AGENTS.md" {
        return None;
    }
    use sha2::{Digest, Sha256};
    Some((
        path.to_owned(),
        format!("sha256:{:x}", Sha256::digest(text.as_bytes())),
    ))
}

/// FileEditor returns Saved for an already matching file too. Exclude that
/// no-op before attempting the save; a failed/ambiguous read produces no offer.
fn pending_agents_revision(store: &FileEditor, changed: bool) -> Option<(PathBuf, String)> {
    if !changed || store.path().file_name()?.to_str()? != "AGENTS.md" {
        return None;
    }
    let current = store.current().ok()?;
    saved_agents_revision(store.path(), current != store.text(), store.text())
}

#[cfg(test)]
mod saved_agents_revision_tests {
    use super::saved_agents_revision;
    use ::core::prelude::v1::test;
    use std::path::Path;

    #[test]
    fn receipt_is_exact_source_sha256_and_only_for_a_changed_absolute_agents_file() {
        let path = std::env::temp_dir().join("AGENTS.md");
        let (absolute_path, revision) = saved_agents_revision(&path, true, "abc").unwrap();
        assert_eq!(absolute_path, path);
        assert_eq!(
            revision,
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(saved_agents_revision(&path, false, "abc").is_none());
        assert!(saved_agents_revision(Path::new("AGENTS.md"), true, "abc").is_none());
        assert!(saved_agents_revision(&path.with_file_name("note.md"), true, "abc").is_none());
    }
}

#[derive(Default)]
struct Editors(Vec<(WeakEntity<Reader>, AnyWindowHandle)>);
impl Global for Editors {}

#[cfg(test)]
pub(crate) fn save_all(cx: &mut App) -> bool {
    save_all_outcomes(cx).0
}

/// Explicit Quit keeps the conflict decision, but an access failure must not
/// trap the user when the latest recovery draft is already durable.
pub(crate) fn save_all_for_quit(cx: &mut App) -> bool {
    let editors = cx.default_global::<Editors>().0.clone();
    editors.into_iter().fold(true, |allowed, (editor, _)| {
        let result = editor
            .update(cx, |reader, cx| reader.save_source_for_close(cx))
            .unwrap_or(true);
        allowed && result
    })
}

/// Canonical conflicts are safe to quit with only when the latest draft is durable.
pub(crate) fn protect_all_for_quit(cx: &mut App) -> bool {
    save_all_outcomes(cx).1
}

fn save_all_outcomes(cx: &mut App) -> (bool, bool) {
    let editors = cx.default_global::<Editors>().0.clone();
    // Visit every editor even if an earlier one has a conflict or save error.
    editors
        .into_iter()
        .fold((true, true), |(saved, protected), (editor, _)| {
            let result = editor
                .update(cx, |reader, cx| reader.save_source_outcome(cx))
                .unwrap_or((true, true));
            (saved && result.0, protected && result.1)
        })
}

#[cfg(all(unix, feature = "brain"))]
pub(crate) fn save_window(window: AnyWindowHandle, cx: &mut App) -> bool {
    let editors = cx.default_global::<Editors>().0.clone();
    editors
        .into_iter()
        .filter(|(_, owner)| *owner == window)
        .all(|(editor, _)| {
            editor
                .update(cx, |reader, cx| reader.save_source_for_close(cx))
                .unwrap_or(true)
        })
}

impl Editing {
    fn replace_clean_source(
        &mut self,
        text: String,
        window: &mut Window,
        cx: &mut Context<Reader>,
    ) {
        self.input.update(cx, |input, cx| {
            replace_clean_source(input, text, window, cx)
        });
        let snapshot = self.input.read(cx).source_snapshot();
        self.draft_stamp = snapshot.stamp;
        // The worker has already persisted this exact clean source. Bind the
        // native reset to it now, so its new Rope identity is not queued later
        // as unsaved typing. Genuine pending input keeps its recovery semantics.
        self.store
            .queue_reset_snapshot(DraftText::from_identified_rope(
                snapshot.text.rope().clone(),
                snapshot.text.id(),
            ));
    }

    pub(super) fn input(&self) -> &Entity<EditorState> {
        &self.input
    }

    #[cfg(test)]
    pub(super) fn menu_facts(&self) -> &reader_reminder::MenuFacts {
        &self.menu_facts
    }

    #[cfg(test)]
    pub(super) fn test_input(&self) -> Entity<EditorState> {
        self.input.clone()
    }

    #[cfg(test)]
    pub(super) fn set_value(&self, value: &str, window: &mut Window, cx: &mut Context<Reader>) {
        self.input
            .update(cx, |input, cx| input.set_value(value, window, cx));
    }

    // UI subscriptions stay on the foreground executor; only the locked file
    // store crosses to the worker. Reassembly preserves undo/input state.
    fn park(mut self, cx: &mut App) -> (FileEditor, impl FnOnce(FileEditor) -> Self) {
        self.recovery_pending = None;
        // A second window can still hold its painted input until the next frame.
        // Freeze it synchronously, before moving the store off the UI thread.
        self.live_preview.suspend();
        self.input
            .update(cx, |input, cx| input.set_disabled(true, cx));
        let Self {
            store,
            source_status: _,
            live_preview,
            math_preview,
            table_grid,
            save_status,
            block_context,
            block_cache,
            input,
            conflict,
            conflict_detected,
            conflict_copy,
            compare,
            save_failed,
            protecting,
            recovery_epoch,
            draft_stamp,
            recovery_task,
            recovery_pending,
            save_pending,
            autosaving,
            refresh_deferred,
            kept_preference,
            saves,
            saved_at,
            current_input,
            menu_facts,
            #[cfg(test)]
            paste_outbox,
            markdown,
            list_revert,
            code_indentation,
            external,
            link_complete,
            _subscriptions,
        } = self;
        (store, move |store| Self {
            store,
            source_status: Default::default(),
            live_preview,
            math_preview,
            table_grid,
            save_status,
            block_context,
            block_cache,
            input,
            conflict,
            conflict_detected,
            conflict_copy,
            compare,
            save_failed,
            protecting,
            recovery_epoch,
            draft_stamp,
            recovery_task,
            recovery_pending,
            save_pending,
            autosaving,
            refresh_deferred,
            kept_preference,
            saves,
            saved_at,
            current_input,
            menu_facts,
            #[cfg(test)]
            paste_outbox,
            markdown,
            list_revert,
            code_indentation,
            external,
            link_complete,
            _subscriptions,
        })
    }

    #[cfg(test)]
    fn status(&self) -> &'static str {
        self.status_with_dirty(self.store.has_pending_revision())
    }

    fn status_with_dirty(&self, store_dirty: bool) -> &'static str {
        if self.conflict_detected {
            "Conflict"
        } else if self.save_pending || self.autosaving.is_some() {
            "Saving…"
        } else if self.save_failed {
            "Save failed"
        } else if store_dirty {
            "Edited"
        } else {
            "Saved"
        }
    }
}

impl Reader {
    /// The editor mode on screen now; Find from Live Preview shows Source.
    pub(super) fn live_preview_shown(&self) -> bool {
        self.editing
            .as_ref()
            .is_some_and(|editing| editing.live_preview.enabled)
    }

    /// The source editor's input, while editing.
    pub(super) fn source_input(&self) -> Option<Entity<EditorState>> {
        self.editing.as_ref().map(|editing| editing.input.clone())
    }

    #[cfg(test)]
    pub(crate) fn source_paste_outbox(&self) -> crate::note_paste::PasteOutbox {
        self.editing.as_ref().unwrap().paste_outbox.clone()
    }

    /// The editor's caret line, 1-based, for «Open in ▸» (#873).
    pub(super) fn editing_caret_line(&self, cx: &App) -> Option<usize> {
        self.editing
            .as_ref()
            .map(|editing| editing.input().read(cx).cursor_position().line as usize + 1)
    }

    pub(super) fn source_live_preview(&self) -> bool {
        self.editing.as_ref().is_some_and(|editing| {
            editing.live_preview.enabled || editing.live_preview.restore_after_find
        })
    }

    pub(super) fn open_source_find(&mut self, cx: &mut Context<Self>) {
        if let Some(editing) = &mut self.editing {
            if !editing.input.read(cx).search_session().open {
                editing.live_preview.restore_after_find = editing.live_preview.enabled;
            }
            editing.live_preview.suspend();
            editing.live_preview.enabled = false;
            let sensitive = reader_ui_state::find_case_sensitive(cx);
            editing.input.update(cx, |input, cx| {
                input.set_projection_provider(None, cx);
                input.set_searchable(true, cx);
                let query = input.search_session().query.clone();
                input.set_search_query(query, !sensitive, cx);
                input.open_search(false, cx);
            });
        }
    }

    pub(super) fn discover_source_recovery(&mut self, cx: &mut Context<Self>) {
        self.recovery_offer = false;
        self.recovery_checked = false;
        self.recovery_error = false;
        let Some(directory) = self.session_directory.clone() else {
            self.recovery_error = true;
            cx.notify();
            return;
        };
        let path = self.vault_root.join(self.selected_file());
        let generation = self.navigation.preparation_generation;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    FileEditor::has_unsaved_draft(&path, &directory.join("editor-drafts"))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.navigation.preparation_generation != generation || this.editing.is_some() {
                    return;
                }
                match result {
                    Ok(available) => {
                        this.recovery_offer = available;
                        if available {
                            this.recovery_dismissed = false;
                        }
                        this.recovery_checked = true;
                    }
                    Err(error) => {
                        this.recovery_error = true;
                        this.link_notice = Some(
                            format!(
                        "Could not check saved drafts: {error:#}. Draft files have been preserved."
                    )
                            .into(),
                        )
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn install_source_lifecycle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let participant = (cx.weak_entity(), window.window_handle());
        cx.default_global::<Editors>().0.push(participant);
        self._subs
            .push(cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.save_source(cx);
                }
            }));
        self._subs.push(cx.on_app_quit(|this, cx| {
            this.save_source(cx);
            async {}
        }));
    }

    /// ⌘⇧E / Ctrl+Shift+E (#916): Live Preview from any mode. From Reader it
    /// enters editing first; a note that cannot be edited stays as it is.
    pub(super) fn open_live_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.is_none() {
            self.toggle_source(window, cx);
        }
        if self.editing.is_some() {
            self.set_live_preview(true, window, cx);
        }
    }

    pub(super) fn toggle_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.move_applying {
            return;
        }
        if self.note_encoding_damaged && self.file_preview.is_none() {
            self.link_notice = Some(reader_loading::ENCODING_NOTICE.into());
            cx.notify();
            return;
        }
        if self.active_timeline().is_some_and(|t| t.selected.is_some()) {
            self.toggle_timeline_source(window, cx);
            return;
        }
        let plain_file = self
            .file_preview
            .as_ref()
            .is_some_and(|p| reader_delimited::editable(&p.rel));
        // A code file over the size cap stays in its read-only note (#998).
        if self.file_preview.is_some() && (!plain_file || !self.code_file_editable(cx)) {
            return;
        }
        if self.editing.is_some() {
            let file = self.selected_file().to_owned();
            let input = self.editing.as_ref().map(|editing| editing.input.clone());
            // The Reader opens where the editor was, not at the top (#1296).
            let place = input
                .as_ref()
                .and_then(|input| input.read(cx).top_visible_line())
                .and_then(|line| u32::try_from(line + 1).ok());
            if !self.leave_source(cx) {
                return;
            }
            // Explicit Read, not a navigation exit.
            self.ui_state.source_mode = false;
            // Saved: keep its Undo history for a return to Edit (#1095).
            self.retained_edit =
                input.map(|input| (file, input.update(cx, |input, _| input.take_edit_history())));
            if plain_file {
                let rel = self.selected_file().to_owned();
                self.preview_file(&rel, window, cx);
            } else {
                let rel = self.current_rel.clone();
                self.prepare_document_at_place(&rel, place, window, cx);
            }
            self.focus_handle.focus(window, cx);
            return;
        }
        if (self.current_rel.is_empty() && !plain_file)
            || self
                .loading
                .as_ref()
                .is_some_and(|l| l.active && !l.published)
        {
            return;
        }
        // The typed definition view shares this exact editor/recovery namespace.
        // Explicit Source takes ownership only once its Save/selection is idle.
        if let Some(panel) = self
            .file_preview
            .as_ref()
            .and_then(|preview| preview.virtual_log.clone())
        {
            if !panel.update(cx, |panel, cx| panel.release_for_source(cx)) {
                self.link_notice =
                    Some("Stop the virtual log operation before opening Source.".into());
                cx.notify();
                return;
            }
        }
        let result = (|| -> anyhow::Result<FileEditor> {
            let state = self
                .session_directory
                .clone()
                .ok_or_else(|| anyhow::anyhow!("No recovery directory is available"))?;
            FileEditor::open(
                &self.vault_root.join(self.selected_file()),
                &state.join("editor-drafts"),
            )
        })();
        let store = match result {
            Ok(store) => store,
            Err(error) => {
                eprintln!("Cannot edit: {error:#}");
                // A read-only location explains itself and what still works.
                let notice = location_notice(&error)
                    .unwrap_or_else(|| format!("Cannot edit: {}", edit_error_text(&error)));
                self.link_notice = Some(notice.into());
                cx.notify();
                return;
            }
        };
        // The editor opens at the Reader's place, not at the top (#1296). The
        // mapping parses the note (tens of ms for a long one), so it runs off
        // the UI thread; see place_editor_at_reader.
        let reader_place = (!plain_file)
            .then(|| {
                let top = self
                    .navigation
                    .pending_landing
                    .unwrap_or_else(|| self.content.read(cx).list_state().logical_scroll_top())
                    .item_ix;
                Some((
                    top,
                    self.note_canonical_source.clone()?,
                    self.note_source.clone(),
                ))
            })
            .flatten()
            .filter(|(top, _, _)| *top > 0);
        self.close_find(window, cx);
        self.clear_landing_notice();
        // Cancel a pending navigation before exposing an editable snapshot.
        self.navigation.preparation_generation =
            self.navigation.preparation_generation.wrapping_add(1);
        // A note takes images as attachments and embeds; a plain file takes
        // text only (#1602).
        let paste_notice = crate::note_paste::PasteNotice::default();
        // What a smart paste leaves to do: a toast, background work (#1611).
        let paste_outbox = crate::note_paste::PasteOutbox::default();
        let clipboard = crate::note_paste::provider_for_note(
            cx,
            self.vault_root.clone(),
            (!plain_file).then(|| self.selected_file().to_owned()),
            paste_notice.clone(),
            paste_outbox.clone(),
        );
        // Code files keep their grammar, line numbers and wrap preference (#998).
        let code = plain_file
            .then(|| crate::reader_code_file::language(self.selected_file()))
            .flatten();
        let language = code.unwrap_or(if plain_file { "text" } else { "markdown" });
        let wrap = code.is_none() || reader_ui_state::code_soft_wrap(cx);
        // The note's reference definitions and text, for rendered blocks (#1067).
        let block_context: BlockContext = Rc::new(std::cell::RefCell::new(BlockNote::initial(
            store.snapshot(),
        )));
        let blocks = (!plain_file).then(|| {
            (
                self.vault_root.clone(),
                self.current_rel.clone(),
                cx.entity().downgrade(),
                block_context.clone(),
            )
        });
        // Cmd/Ctrl+/ comments code-block lines; elsewhere it falls through.
        code_comforts::ensure_keys(cx);
        tables::ensure_keys(cx);
        let embed_hover = reader_image_actions::source_hover(cx.entity().downgrade());
        let input = cx.new(|cx| {
            let mut input = EditorState::new(window, cx)
                .document_newlines(true)
                // A leading BOM stays at byte zero whatever is typed (#1093).
                .keep_leading_bom(true)
                .language(language)
                .line_number(code.is_some())
                .folding(false)
                .searchable(true)
                .replaceable(false)
                .soft_wrap(wrap)
                .wrapping_indent(WrappingIndent::None)
                // Hebrew paragraphs align right in notes; code and data files
                // keep one alignment (S6c, #1034).
                .paragraph_direction(!plain_file);
            input.set_search_query("", !reader_ui_state::find_case_sensitive(cx), cx);
            // The key that confirmed an IME candidate runs no automation (#1637).
            input.set_ime_settle(ime_guard::IME_SETTLE);
            // Typed autoformat in notes: wrap a selection, close a pair (#1636).
            // Inside a fenced code block: snippet pairing instead (#1992).
            if !plain_file {
                input.set_typed_text_hook(Some(code_comforts::typed_hook(format::typed_hook())));
            }
            input.set_projection_provider(Some(Arc::new(ExactSource)), cx);
            input.set_exact_clipboard_provider(clipboard, cx);
            input.set_value(store.text().to_owned(), window, cx);
            if !plain_file {
                input.lsp_mut().hover_provider = Some(embed_hover);
            }
            input.set_direction_exempt_lines(direction_exempt_lines(store.text()));
            let press = input.block_press_slot();
            input.set_block_renderer(blocks.map(|(root, rel, reader, context)| {
                live_blocks(root, rel, reader, press, context)
            }));
            input.ensure_highlighter_factory(
                gpui_component::highlighter::input_highlighter_factory(),
            );
            input.prepare_highlighting(window, cx);
            input
        });
        let current_input = cx.new(|cx| {
            let mut input = EditorState::new(window, cx)
                .language(language)
                .line_number(false)
                .soft_wrap(true);
            input.set_readonly(true, cx);
            input
        });
        let mut last_find_case = reader_ui_state::find_case_sensitive(cx);
        let highlighting = cx.observe_in(&input, window, move |this, input, window, cx| {
            if this.editing.as_ref().is_some_and(|editing| {
                editing.input == input
                    && editing.live_preview.restore_after_find
                    && !input.read(cx).search_session().open
            }) {
                this.toggle_live_preview(window, cx);
            }
            if input.read(cx).search_session().open {
                let sensitive = !input.read(cx).search_session().case_insensitive;
                if sensitive != last_find_case {
                    last_find_case = sensitive;
                    reader_ui_state::set_find_case_sensitive(sensitive, cx);
                }
            }

            if this.ui_state.source_highlight_pending
                && this
                    .editing
                    .as_ref()
                    .is_some_and(|editing| editing.input == input)
                && !input.read(cx).highlighting_pending()
            {
                this.ui_state.source_highlight_pending = false;
                cx.notify();
            }
            // #1921: the text or the caret moved; follow a `[[` being typed.
            if this
                .editing
                .as_ref()
                .is_some_and(|editing| editing.input == input)
            {
                this.refresh_link_complete(window, cx);
            }
        });
        let changed = cx.subscribe(&input, move |this, input, mutation: &SourceMutation, cx| {
            #[cfg(test)]
            let _work = work::scope();
            let Some(editing) = &mut this.editing else {
                return;
            };
            if editing.input.entity_id() != input.entity_id() {
                return;
            }
            if this.clear_landing_notice() {
                // Typing normally avoids a Reader render. Retiring a visible
                // landing toast must explicitly synchronize its presentation.
                cx.notify();
            }
            let Some(editing) = &mut this.editing else {
                return;
            };
            let snapshot = input.read(cx).source_snapshot();
            let source_stamp = snapshot.stamp;
            if editing.conflict_detected
                && editing
                    .conflict_copy
                    .as_ref()
                    .is_some_and(|(stamp, _)| *stamp != source_stamp)
            {
                this.link_notice = Some(format!("{CONFLICT_NOTICE} Your latest edits are not in the saved copy. Save or Keep both versions to save another copy. Reload will keep your latest edits in a new copy first.").into());
            }
            let value = okilum_core::source_projection::revision::Text::from_identified_rope(
                snapshot.text.rope().clone(),
                snapshot.text.id(),
            );
            // Retain the exact native revision, including silent Undo/Redo/IME.
            editing.draft_stamp = snapshot.stamp;
            let write = if editing.store.snapshot().same(&value) {
                editing.store.recovery_write()
            } else if mutation.reason
                == gpui_component::input::projection::SourceMutationReason::Reset
            {
                editing.store.queue_reset_snapshot(value.clone())
            } else {
                editing.store.queue_snapshot(value.clone())
            };
            *editing.block_context.borrow_mut() = BlockNote::pending();
            editing.recovery_epoch = editing.recovery_epoch.wrapping_add(1);
            let epoch = editing.recovery_epoch;
            let input_id = input.entity_id();
            // A clean reload/reset already has its exact canonical bytes.
            // Preparing its context must not block a move for 300 ms.
            editing.protecting = editing.store.has_pending_revision();
            editing.recovery_pending = if editing.protecting {
                Some(recovery::Pending::new(write))
            } else {
                drop(write);
                None
            };
            let ticket = editing
                .recovery_pending
                .as_ref()
                .map(recovery::Pending::ticket);
            let context = editing.block_context.clone();
            let cache = editing.block_cache.clone();
            #[cfg(test)]
            drop(_work); // GPUI scheduling traces are outside snapshot/persistence work.
            editing.recovery_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(RECOVERY_PAUSE).await;
                let write = ticket.and_then(|ticket| {
                    let write = ticket.borrow_mut().take();
                    write
                });
                let (result, directions, prepared_context) = cx
                    .background_executor()
                    .spawn(async move {
                        // Persist first: presentation preparation must not delay recovery.
                        let result = write.map_or(Ok(()), |write| write.persist());
                        let directions = direction_exempt_lines(&value);
                        let provider =
                            crate::source_presentation::CachedProvider::classify(snapshot.clone());
                        let prepared =
                            BlockNote::prepare(value, provider.rendered_blocks(&snapshot));
                        // The worker retains the last large derived cache until
                        // its replacement. A key can drop its UI reference in O(1)
                        // without destroying a note-wide footnote map on the UI.
                        let mut cache = cache.lock().unwrap_or_else(|p| p.into_inner());
                        // Retain every cache still owned by a UI snapshot. A
                        // newer worker may finish before its UI adoption; the
                        // previous painted cache must stay worker-owned too.
                        cache.retain(|known| Arc::strong_count(known) > 1);
                        cache.push(prepared.2.clone());
                        (result, directions, prepared)
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    let Some(editing) = &mut this.editing else {
                        return;
                    };
                    if editing.input.entity_id() != input_id
                        || editing.input.read(cx).source_stamp() != source_stamp
                    {
                        return;
                    }
                    // Save retires recovery ownership, not same-revision
                    // presentation preparation. New typing invalidates both.
                    *context.borrow_mut() = BlockNote::from_prepared(prepared_context);
                    editing.input.update(cx, |input, cx| {
                        input.set_direction_exempt_lines(directions);
                        cx.notify();
                    });
                    if editing.recovery_epoch != epoch {
                        return;
                    }
                    editing.protecting = false;
                    if let Err(error) = result {
                        editing.save_failed = true;
                        eprintln!("Draft recovery could not be saved: {error:#}");
                        this.link_notice = Some(DRAFT_WRITE_NOTICE.into());
                        cx.notify();
                    } else if this.link_notice.as_deref() == Some(DRAFT_WRITE_NOTICE) {
                        editing.save_failed = false;
                        this.link_notice = None;
                        cx.notify();
                    }
                });
            }));
            this.refresh_draft_outline(cx);
            this.schedule_autosave(input_id, epoch, cx);
            this.schedule_live_preview(cx);
        });
        let clicked = cx.subscribe_in(
            &input,
            window,
            move |this, input, click: &SourceClick, window, cx| {
                if plain_file
                    || this
                        .editing
                        .as_ref()
                        .is_none_or(|editing| editing.input.entity_id() != input.entity_id())
                    || input.read(cx).source_stamp() != click.stamp
                    || click.event.button != MouseButton::Left
                    || !reader_image_actions::open_modifier(
                        platform::labels::Os::CURRENT,
                        click.event.modifiers,
                    )
                {
                    return;
                }
                let source = input.read(cx).value().to_string();
                this.sync_agent_document(cx);
                if let Some((token, link)) = this.agent_document.as_ref().and_then(|current| {
                    current
                        .imports
                        .links()
                        .iter()
                        .find(|link| link.import.line.contains(&click.offset.0))
                        .map(|link| (current.token.clone(), link.clone()))
                }) {
                    this.open_agent_import(token, link, window, cx);
                    return;
                }
                if this.open_source_image_at(&source, click.offset.0, window, cx) {
                    return;
                }
                let Some(link) = okilum_core::document_links::parse_in_vault(
                    &source,
                    &this.vault,
                    &this.current_rel,
                )
                .into_iter()
                .find(|link| link.range.contains(&click.offset.0)) else {
                    return;
                };
                let resolved = okilum_core::document_links::resolve(
                    &link.target,
                    link.wiki,
                    &this.vault,
                    &this.current_rel,
                );
                // Protect the current draft before navigation, including browser links.
                if !this.save_source(cx) {
                    return;
                }
                match resolved.status {
                    "resolved" => this.open_note_at(
                        &resolved.candidates[0],
                        None,
                        resolved.heading.as_deref(),
                        window,
                        cx,
                    ),
                    "external" => cx.open_url(&resolved.url),
                    "attachment" => this.preview_file(&resolved.candidates[0], window, cx),
                    "outside_file" => this.outside_file_menu(&resolved.url, window, cx),
                    "ambiguous" => {
                        this.link_notice = Some(
                            reader_link_navigation::ambiguous_notice(&resolved.candidates).into(),
                        );
                        this.link_choices = resolved
                            .candidates
                            .into_iter()
                            .map(|path| (path, resolved.heading.clone()))
                            .collect();
                    }
                    _ => {
                        this.link_choices.clear();
                        this.link_notice = Some(
                            resolved
                                .reason
                                .unwrap_or("No document matches this link.")
                                .into(),
                        );
                    }
                }
                cx.notify();
            },
        );
        let blur = cx.on_blur(&input.focus_handle(cx), window, |this, _, cx| {
            // History is a read-only detour; preserve a dirty live buffer.
            if this.active_timeline().is_none() {
                this.save_source(cx);
            }
        });
        if store.has_pending_revision() {
            reader_toast::transient("Unsaved changes restored", window, cx);
        }
        // A click on a Live Preview task checkbox is one source edit and one
        // undo step; the caret stays where it was (S5, #1034).
        let toggled = cx.subscribe_in(
            &input,
            window,
            move |_, input, click: &MarkerClick, window, cx| {
                if !matches!(click.kind, MarkerKind::Task { .. })
                    || input.read(cx).source_stamp() != click.stamp
                {
                    return;
                }
                input.update(cx, |state, cx| {
                    let source = state.value().to_string();
                    if let Some((range, text)) =
                        task_toggle(&source, click.range.start.0..click.range.end.0)
                    {
                        state.replace_source_range(range, text, window, cx);
                    }
                });
            },
        );
        // #1602: every paste outcome reaches the log (sizes and kinds only,
        // never clipboard text); one that inserts nothing also says why.
        #[cfg(test)]
        let test_paste_outbox = paste_outbox.clone();
        let pasted = cx.subscribe_in(
            &input,
            window,
            move |this, input, event: &ClipboardPasteEvent, window, cx| {
                // #1611: the paste's notice and background work (file copies,
                // a page title) run whatever was inserted now.
                let pending = paste_outbox
                    .lock()
                    .map(|mut outbox| outbox.take_for_event(event))
                    .unwrap_or_default();
                this.publish_saved_attachments(&pending.saved_files, window, cx);
                let handled = pending.notice.is_some() || pending.follow_up.is_some();
                if let Some(notice) = &pending.notice {
                    paste_jobs::show_notice(notice, window, cx);
                }
                if let Some(follow_up) = pending.follow_up {
                    this.run_paste_follow_up(input.clone(), follow_up, window, cx);
                }
                match event {
                    ClipboardPasteEvent::Applied { bytes, .. } => {
                        if let Some(editing) = this
                            .editing
                            .as_ref()
                            .filter(|editing| editing.input == *input)
                        {
                            let stamp = input.read(cx).source_stamp();
                            editing
                                .table_grid
                                .update(cx, |grid, cx| grid.claim_paste_edit(stamp, window, cx));
                        }
                        this.suppress_pasted_completion(cx);
                        eprintln!("Paste: inserted {bytes} bytes");
                        // A file too large to copy was linked where it is; say so.
                        let notice = paste_notice.lock().ok().and_then(|mut slot| slot.take());
                        if let Some(text) = notice {
                            reader_toast::transient(text, window, cx);
                        }
                    }
                    ClipboardPasteEvent::Failed(error) => {
                        eprintln!("Paste: nothing inserted ({error:?})");
                        if handled && matches!(error, ClipboardReadError::Unavailable) {
                            return;
                        }
                        // An image that couldn't be saved says why itself.
                        let notice = paste_notice.lock().ok().and_then(|mut slot| slot.take());
                        if let Some(text) = notice {
                            reader_toast::error(text, window, cx);
                            return;
                        }
                        let image = matches!(error, ClipboardReadError::Unavailable)
                            && cx.read_from_clipboard().is_some_and(|item| {
                                item.entries()
                                    .iter()
                                    .any(|entry| matches!(entry, ClipboardEntry::Image(_)))
                            });
                        if let Some(text) = paste_failure_text(error, image) {
                            reader_toast::error(text, window, cx);
                        }
                    }
                }
            },
        );
        let (menu_facts, menu_watch) =
            reader_reminder::MenuFacts::watch(self, &input, !plain_file, cx);
        let live_preview =
            live_preview::LivePreview::new(self, cx.entity_id(), &store, &input, !plain_file);
        let reader = cx.entity();
        let math_preview =
            cx.new(|cx| math_preview::MathPreview::new(reader.clone(), input.clone(), cx));
        let block_cache = block_context::WorkerCache::default();
        let save_status = cx.new(|cx| status::SaveStatus::new(reader.clone(), &input, cx));
        let table_grid = cx.new(|cx| tables::TableGrid::new(reader, input.clone(), window, cx));
        self.editing = Some(Editing {
            store,
            source_status: Default::default(),
            live_preview,
            math_preview,
            table_grid,
            save_status,
            block_context,
            block_cache,
            input: input.clone(),
            conflict: None,
            conflict_detected: false,
            conflict_copy: None,
            compare: false,
            save_failed: false,
            protecting: false,
            recovery_epoch: 0,
            draft_stamp: input.read(cx).source_stamp(),
            recovery_task: None,
            recovery_pending: None,
            save_pending: false,
            autosaving: None,
            refresh_deferred: false,
            kept_preference: None,
            saves: 0,
            saved_at: None,
            current_input,
            menu_facts,
            #[cfg(test)]
            paste_outbox: test_paste_outbox,
            markdown: !plain_file,
            list_revert: None,
            code_indentation: Default::default(),
            external: Default::default(),
            link_complete: Default::default(),
            _subscriptions: vec![
                changed,
                blur,
                clicked,
                toggled,
                highlighting,
                menu_watch,
                pasted,
            ],
        });
        // Back from the Reader to unchanged text: Undo continues (#1095).
        if let Some((file, history)) = self.retained_edit.take() {
            if file == self.selected_file() {
                input.update(cx, |input, cx| input.restore_edit_history(history, cx));
            }
        }
        if let Some(place) = reader_place {
            self.place_editor_at_reader(input.clone(), place, window, cx);
        }
        self.refresh_draft_outline(cx);
        self.set_live_preview(self.ui_state.live_preview, window, cx);
        self.ui_state.source_mode = true;
        self.start_editor_layout_diagnostics(input.clone(), cx);
        input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    pub(super) fn request_source_save(&mut self, cx: &mut Context<Self>) {
        if self.active_timeline().is_some_and(|t| t.selected.is_some()) {
            return;
        }
        let Some(editing) = &mut self.editing else {
            return;
        };
        editing.save_pending = true;
        let input_id = editing.input.entity_id();
        cx.notify();
        // Yield one frame so queued explicit Save is visible. Lifecycle saves
        // remain synchronous: quitting/navigation never relies on this task.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(32))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .editing
                    .as_ref()
                    .is_some_and(|e| e.input.entity_id() == input_id && e.save_pending)
                {
                    this.save_source_explicit(cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn render_save_status(&self, _cx: &mut Context<Self>) -> AnyElement {
        self.editing
            .as_ref()
            .map(|e| e.save_status.clone().into_any_element())
            .unwrap_or_else(|| div().into_any_element())
    }
    fn save_status_element(&self, cx: &App) -> AnyElement {
        let Some(editing) = &self.editing else {
            return div().into_any_element();
        };
        let tooltip = match editing.saved_at {
            Some(at) => format!(
                "Last saved at {:02}:{:02}:{:02}. Changes save on their own a second after you stop typing.",
                at.hour(),
                at.minute(),
                at.second()
            ),
            None => format!(
                "Changes save on their own a second after you stop typing, or now with {}.",
                Os::CURRENT.shortcut("secondary-s")
            ),
        };
        let status = self.source_presentation_status(cx).unwrap();
        let tooltip = format!(
            "{} · {}",
            editing.status_with_dirty(status.store_dirty),
            tooltip
        );
        div()
            .id("source-save-status")
            .flex_none()
            .px_2()
            .text_color(if editing.conflict_detected || editing.save_failed {
                cx.theme().warning
            } else {
                brand::palette(cx).text_muted
            })
            .child(if status.dirty { "●" } else { "" })
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            })
            .into_any_element()
    }

    pub(super) fn owns_source_publication(&self, digest: &[u8; 32]) -> bool {
        self.editing
            .as_ref()
            .is_some_and(|e| e.store.owns_publication(digest))
    }

    /// Background watcher refresh. Sampling the canonical Rope is bounded; all
    /// byte comparisons, disk reads and recovery persistence belong to workers.
    pub(super) fn refresh_source_in_background(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editing) = &mut self.editing else {
            return;
        };
        if editing.autosaving.is_some() {
            editing.refresh_deferred = true;
            return;
        }
        let snapshot = editing.input.read(cx).source_snapshot();
        if editing.draft_stamp != snapshot.stamp {
            editing.draft_stamp = snapshot.stamp;
            editing
                .store
                .queue_snapshot(DraftText::from_identified_rope(
                    snapshot.text.rope().clone(),
                    snapshot.text.id(),
                ));
        }
        let copy = match editing.store.publication_copy() {
            Ok(copy) => copy,
            Err(error) => {
                editing.save_failed = true;
                self.link_notice = Some(file_access_notice(&error).into());
                cx.notify();
                return;
            }
        };
        let input_id = editing.input.entity_id();
        let saves = editing.saves;
        let recovery = editing.recovery_epoch;
        let retained = self.deleted_current().is_some();
        let root = self.vault_root.clone();
        let rel = self.current_rel.clone();
        self.navigation.reconciliation_generation =
            self.navigation.reconciliation_generation.wrapping_add(1);
        let generation = self.navigation.reconciliation_generation;
        let task = cx.background_executor().spawn(async move {
            let observed = copy.current();
            let reviewed = retained.then(|| observed.as_ref().ok().cloned()).flatten();
            let refresh = if retained {
                Ok(None)
            } else {
                copy.refresh_publication()
            };
            (observed, reviewed, refresh)
        });
        cx.spawn_in(window, async move |this, cx| {
            let (observed, reviewed, refresh) = task.await;
            let _ = this.update_in_own_window(cx, |this, window, cx| {
                if this.navigation.reconciliation_generation != generation || this.vault_root != root || this.current_rel != rel { return; }
                let Some(editing) = this.editing.as_mut().filter(|e| e.input.entity_id() == input_id) else { return; };
                if editing.autosaving.is_some() { editing.refresh_deferred = true; return; }
                if editing.saves != saves { this.refresh_source_in_background(window, cx); return; }
                if editing.input.read(cx).source_stamp() != snapshot.stamp || editing.recovery_epoch != recovery {

                    this.refresh_source_in_background(window, cx); return;
                }
                let held = match observed {
                    Ok(raw) => this.track_deleted_observation(Some(raw), cx),
                    Err(ref error) if missing_source(error) => this.track_deleted_observation(None, cx),
                    Err(_) => false,
                };
                let retained_cleared = this.deleted_current().is_none();
                let editing = this.editing.as_mut().unwrap();
                if held {
                    if this.deleted_note.as_ref().is_some_and(|note| note.disk == reader_deleted_note::Disk::Different) {
                        editing.conflict_detected = true;
                        editing.conflict = reviewed;
                    } else {
                        editing.save_failed = true;
                    }
                    cx.notify(); return;
                }
                let was_conflict = editing.conflict_detected;
                let mut resume = None;
                match refresh {
                    Ok(Some(okilum_core::file_editor::PublicationRefresh { editor: store, outcome: result, conflict, replacement })) => {
                        editing.store = store;
                        editing.source_status.set(None);
                        editing.conflict = conflict;
                        editing.conflict_detected = result == Save::Conflict;
                        if result == Save::Conflict {
                            this.link_notice = Some(format!("{CONFLICT_NOTICE} Your edits are still here. Reload, keep your version, or compare before saving.").into());
                            this.describe_source_conflict(cx);
                        } else {
                            editing.save_failed = false;
                            editing.compare = false;
                            if was_conflict && editing.store.has_pending_revision() {
                                resume = Some((editing.input.entity_id(), editing.recovery_epoch));
                            }
                            if this.link_notice.as_deref().is_some_and(|n| n.starts_with(CONFLICT_NOTICE)) { this.link_notice = None; }
                            if let Some(updated) = replacement {
                                editing.replace_clean_source(updated, window, cx);
                                editing.saved_at = None;
                            }
                        }
                        cx.notify();
                    }
                    Ok(None) if !retained || retained_cleared => this.refresh_source_in_background(window, cx),
                    Ok(None) => {},
                    Err(error) => {

                        editing.save_failed = true;
                        this.link_notice = Some(file_access_notice(&error).into());
                        cx.notify();
                    }
                }
                if let Some((input_id, epoch)) = resume { this.schedule_autosave(input_id, epoch, cx); }
            });
        }).detach();
    }

    #[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
    pub(super) fn refresh_source_from_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(test)]
        let _work = work::scope();
        // Every refresh entry point (including shared-window publications and
        // deferred autosave refreshes) preserves an observed deletion. Do this
        // before FileEditor can silently refresh a clean buffer on file return.
        let retain_deleted = self.track_deleted_note(&Default::default(), cx);
        let trace = self
            .loading
            .as_ref()
            .and_then(|load| load.opts.diagnostics.clone());
        let Some(editing) = &mut self.editing else {
            return;
        };
        // The file may already hold the text an autosave is writing while the
        // store's base is still the old one: read now, our own save looks like
        // an outside change and the conflict stops autosave (#1376). Look
        // again once the save is back; an outside change made meanwhile is
        // seen by the save itself or by that refresh.
        if editing.autosaving.is_some() {
            editing.refresh_deferred = true;
            return;
        }
        // Sample native state before looking at disk: a queued mutation event
        // cannot make an actually dirty buffer look clean.
        let text = editing.input.read(cx).value().to_string();
        if editing.store.text() != text {
            if let Err(error) = editing.store.set_text(text.clone()) {
                editing.save_failed = true;
                self.link_notice = Some(
                    format!("Could not protect your edits: {error:#}. Your draft remains open.")
                        .into(),
                );
                cx.notify();
                return;
            }
        }
        if retain_deleted {
            // A retained returned version also blocks autosave until an explicit
            // Reload/Keep mine decision. The banner itself is never modal.
            if self
                .deleted_note
                .as_ref()
                .is_some_and(|deleted| deleted.disk == reader_deleted_note::Disk::Different)
            {
                editing.conflict_detected = true;
                editing.conflict = editing.store.current().ok();
            }
            cx.notify();
            return;
        }
        let mut outcome = "unchanged";
        let mut resume_autosave = None;
        // Time on the UI thread, for the keystroke stalls of #1363.
        let started = std::time::Instant::now();
        let refreshed = editing.store.refresh_from_disk();
        let refresh_ms = started.elapsed().as_secs_f64() * 1000.;
        match refreshed {
            Ok(Save::Conflict) => {
                outcome = "conflict";
                editing.conflict_detected = true;
                if editing.conflict.is_none() {
                    editing.conflict = editing.store.current().ok();
                }
                self.link_notice = Some(format!("{CONFLICT_NOTICE} Your edits are still here. Reload, keep your version, or compare before saving.").into());
            }
            Ok(Save::Saved) => {
                if editing.conflict_detected && editing.store.text() == text {
                    // The disk holds the base or this text again: no conflict
                    // is left to resolve, so autosave may go on (#1376).
                    outcome = "conflict_cleared";
                    editing.conflict = None;
                    editing.conflict_detected = false;
                    editing.compare = false;
                    if self
                        .link_notice
                        .as_deref()
                        .is_some_and(|notice| notice.starts_with(CONFLICT_NOTICE))
                    {
                        self.link_notice = None;
                    }
                    if editing.store.dirty() {
                        resume_autosave = Some((editing.input.entity_id(), editing.recovery_epoch));
                    }
                } else if editing.store.text() != text {
                    outcome = "replaced";
                    let updated = editing.store.text().to_owned();
                    editing.replace_clean_source(updated, window, cx);
                    editing.conflict = None;
                    editing.conflict_detected = false;
                    editing.save_failed = false;
                    editing.saved_at = None;
                    reader_toast::transient("Updated from disk", window, cx);
                }
            }
            Err(error) => {
                outcome = "error";
                eprintln!("Could not refresh the edited file: {error:#}");
                editing.save_failed = true;
                self.link_notice = Some(file_access_notice(&error).into());
            }
        }
        if let Some(trace) = trace {
            trace.event(
                "editor_disk_refresh",
                serde_json::json!({"outcome": outcome,
                "refresh_ms": refresh_ms,
                "generation": editing.input.read(cx).source_stamp().generation,
                "presentation_epoch": editing.input.read(cx).presentation_epoch()}),
            );
        }
        if let Some((input_id, epoch)) = resume_autosave {
            self.schedule_autosave(input_id, epoch, cx);
        }
        cx.notify();
    }

    /// The editor's current buffer, while editing.
    pub(super) fn source_text(&self, cx: &App) -> Option<String> {
        self.editing
            .as_ref()
            .map(|editing| editing.input.read(cx).value().to_string())
    }

    /// Drop the editor's «moved or deleted» notice; the deleted-note notice
    /// (#1986) is showing.
    pub(super) fn clear_moved_or_deleted_notice(&mut self) {
        if self.link_notice.as_deref() == Some(MOVED_OR_DELETED_NOTICE) {
            self.link_notice = None;
        }
    }

    /// The edited file is back at its place (#1986). The editor's disk refresh
    /// has already compared it: a different file is a conflict and keeps its
    /// choices; otherwise the earlier «file is gone» failure is over.
    pub(super) fn source_file_returned(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = &mut self.editing else {
            return;
        };
        if !editing.conflict_detected {
            editing.save_failed = false;
            self.clear_moved_or_deleted_notice();
        }
        cx.notify();
    }

    /// The editor's buffer was just written back at the note's own place
    /// (#1986): that file is its base now. False when the editor can no
    /// longer reach the file (its folder was recreated) and must reopen it.
    pub(super) fn source_restored(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(editing) = &mut self.editing else {
            return true;
        };
        let text = editing.input.read(cx).value().to_string();
        if !editing.store.current().is_ok_and(|current| current == text) {
            return false;
        }
        let acknowledged = editing
            .store
            .set_text(text.clone())
            .and_then(|_| editing.store.saved_as(text));
        match acknowledged {
            Ok(()) => {
                editing.conflict = None;
                editing.conflict_detected = false;
                editing.compare = false;
                editing.save_failed = false;
                self.clear_moved_or_deleted_notice();
                cx.notify();
                true
            }
            Err(error) => {
                eprintln!("The restored note could not be reopened in the editor: {error:#}");
                false
            }
        }
    }

    /// The buffer of a deleted note was saved elsewhere (#1986): nothing is
    /// left to protect for the deleted file, so leaving it keeps no draft.
    pub(super) fn source_saved_elsewhere(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = &mut self.editing else {
            return;
        };
        let text = editing.input.read(cx).value().to_string();
        if let Err(error) = editing.store.saved_as(text) {
            eprintln!("Could not mark the saved draft: {error:#}");
        }
        editing.save_failed = false;
        self.clear_moved_or_deleted_notice();
        cx.notify();
    }

    /// Detach only after protecting the latest buffer. Explicit Don't Save
    /// still retains the durable recovery draft, but must not retry a canonical
    /// save (or overwrite a returned file) during the authorized navigation.
    pub(super) fn release_deleted_editor(&mut self, cx: &mut Context<Self>) -> bool {
        if let Some(editing) = &mut self.editing {
            if let Err(error) = editing
                .store
                .set_text(editing.input.read(cx).value().to_string())
            {
                self.link_notice = Some(
                    format!("Cannot protect this draft: {error:#}. Copy the draft before closing.")
                        .into(),
                );
                return false;
            }
        }
        self.remember_source_mode();
        self.editing = None;
        true
    }

    pub(super) fn save_source_for_close(&mut self, cx: &mut Context<Self>) -> bool {
        let (saved, protected) = self.save_source_outcome(cx);
        saved || (protected && self.editing.as_ref().is_some_and(|e| e.save_failed))
    }

    pub(super) fn save_source(&mut self, cx: &mut Context<Self>) -> bool {
        self.save_source_outcome(cx).0
    }

    /// Only a deliberate Save/Keep both may publish a conflict copy. Lifecycle
    /// saves protect the latest recovery draft without adding files to the vault.
    fn save_source_explicit(&mut self, cx: &mut Context<Self>) -> bool {
        if let Some(editing) = &mut self.editing {
            editing.save_pending = false;
        }
        let (saved, protected) = self.save_source_outcome(cx);
        if !saved
            && protected
            && self.deleted_note.is_none()
            && self
                .editing
                .as_ref()
                .is_some_and(|e| e.conflict_detected && e.conflict.is_some())
        {
            self.keep_both_source(cx);
        }
        saved
    }

    fn save_source_outcome(&mut self, cx: &mut Context<Self>) -> (bool, bool) {
        if self.editing.as_ref().is_some_and(|editing| {
            editing
                .table_grid
                .update(cx, |grid, cx| grid.retain_pending_cells(cx))
        }) {
            return (false, false);
        }
        if self.move_applying {
            return (false, false);
        }
        let Some(editing) = &mut self.editing else {
            return (true, true);
        };
        let explicit_pending = std::mem::take(&mut editing.save_pending);
        editing.recovery_pending = None;
        editing.recovery_epoch = editing.recovery_epoch.wrapping_add(1);
        editing.protecting = false;
        editing.saves = editing.saves.wrapping_add(1);
        // An autosave may already have written its text: that is the base now,
        // not an outside change (#1353).
        // This save checks the disk itself, so a deferred refresh is moot.
        editing.refresh_deferred = false;
        editing.autosaving = None;
        // Lifecycle commands may run before SourceMutation subscribers. Capture
        // the canonical native revision directly, even in that same UI turn.
        let snapshot = editing.input.read(cx).source_snapshot();
        let text =
            DraftText::from_identified_rope(snapshot.text.rope().clone(), snapshot.text.id());
        editing.draft_stamp = snapshot.stamp;
        // Lifecycle capture is a bulk equality check, not a new input edit.
        // Retain a clean native revision as clean instead of manufacturing
        // pending input that would require canonical admission during a hold.
        if !editing.store.snapshot().same(&text) {
            editing.store.queue_reset_snapshot(text);
        }
        let source_changed = editing.store.dirty();
        let draft = editing.store.flush_draft();
        let protected = draft.is_ok();
        let rules_revision = protected
            .then(|| pending_agents_revision(&editing.store, editing.store.dirty()))
            .flatten();
        let retained = self.deleted_note.is_some();
        let result = draft.and_then(|_| {
            if retained {
                Ok(Save::Conflict)
            } else {
                editing.store.save()
            }
        });
        let saved = match result {
            Ok(Save::Saved) => {
                editing.saved_at = Some(
                    time::OffsetDateTime::now_local()
                        .unwrap_or_else(|_| time::OffsetDateTime::now_utc()),
                );
                editing.conflict = None;
                editing.conflict_detected = false;
                editing.save_failed = false;
                editing.protecting = false;
                if matches!(
                    self.link_notice.as_deref(),
                    Some(DRAFT_WRITE_NOTICE | AUTOSAVE_START_NOTICE)
                ) {
                    self.link_notice = None;
                }
                if let Some((absolute_path, saved_revision)) = rules_revision {
                    self.agents_rules_saved(&absolute_path, &saved_revision, cx);
                }
                if source_changed && self.file_preview.is_none() {
                    self.queue_saved_source(cx);
                }
                cx.notify();
                true
            }
            Ok(Save::Conflict) => {
                editing.save_failed = false;
                editing.conflict_detected = true;
                if editing.conflict.is_none() {
                    editing.conflict = editing.store.current().ok();
                }
                self.link_notice = Some(
                    format!(
                        "{CONFLICT_NOTICE} Reload, keep your version, or compare before saving."
                    )
                    .into(),
                );
                cx.notify();
                false
            }
            Err(error) => {
                editing.save_failed = true;
                eprintln!("Could not save the edited file: {error:#}");
                self.link_notice = Some(
                    if protected {
                        file_access_notice(&error)
                    } else {
                        DRAFT_WRITE_NOTICE
                    }
                    .into(),
                );
                cx.notify();
                false
            }
        };
        if !saved && explicit_pending && self.editing.as_ref().is_some_and(|e| e.conflict_detected)
        {
            // A blur/deactivate between the Save request and its next-frame
            // callback only protected the draft. It must not consume Save.
            self.editing.as_mut().unwrap().save_pending = true;
        }
        if !saved {
            self.describe_source_conflict(cx);
        }
        (saved, protected)
    }
    /// Autosave (#1353): one second after the last edit, save on a worker so
    /// typing never waits. Each edit schedules one try, so a failed save is
    /// tried again only after the next edit. Not during IME composition, not
    /// while a conflict is shown (it needs a choice), and never over
    /// a change made outside Okilum: the store refuses that and the conflict
    /// view opens.
    fn schedule_autosave(&mut self, input_id: EntityId, epoch: u64, cx: &mut Context<Self>) {
        let handle = self.reader_window;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AUTOSAVE_PAUSE).await;
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |this, cx| this.autosave(input_id, epoch, window, cx));
            });
        })
        .detach();
    }

    fn autosave(
        &mut self,
        input_id: EntityId,
        epoch: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.deleted_current().is_some()
            || self.move_applying
            || self.active_timeline().is_some_and(|t| t.selected.is_some())
        {
            return;
        }
        let Some(editing) = &mut self.editing else {
            return;
        };
        if editing.input.entity_id() != input_id
            || editing.recovery_epoch != epoch
            || editing.conflict_detected
            || editing.autosaving.is_some()
        {
            return;
        }
        // Mid-composition text is not what the person typed yet; the commit
        // is an edit of its own and schedules the next autosave.
        if editing
            .input
            .update(cx, |input, cx| input.marked_text_range(window, cx))
            .is_some()
        {
            return;
        }
        if !editing.store.has_pending_revision() {
            return;
        }
        let text = editing.store.snapshot();
        #[cfg(test)]
        let copy = if FAIL_AUTOSAVE_COPY.with(|fail| fail.replace(false)) {
            Err(anyhow::anyhow!("Injected save-copy failure"))
        } else {
            editing.store.save_copy()
        };
        #[cfg(not(test))]
        let copy = editing.store.save_copy();
        let copy = match copy {
            Ok(copy) => copy,
            Err(error) => {
                editing.save_failed = true;
                self.link_notice = Some(AUTOSAVE_START_NOTICE.into());
                eprintln!("Autosave could not start: {error:#}");
                cx.notify();
                return;
            }
        };
        editing.autosaving = Some(text.clone());
        let saves = editing.saves;
        cx.notify();
        let worker = cx.background_executor().spawn(async move {
            let mut copy = copy;
            let rules_revision = pending_agents_revision(&copy, copy.dirty());
            let result = copy.save();
            // The UI editor takes the stamp, so its next refresh skips the read (#1363).
            let conflict = matches!(result, Ok(Save::Conflict))
                .then(|| copy.current().ok())
                .flatten();
            (result, copy.disk_stamp(), rules_revision, conflict)
        });
        cx.spawn(async move |this, cx| {
            let (result, stamp, rules_revision, conflict) = worker.await;
            let _ = this.update(cx, |this, cx| {
                let accepted = matches!(&result, Ok(Save::Saved))
                    && this.editing.as_ref().is_some_and(|editing| {
                        editing.input.entity_id() == input_id
                            && editing.saves == saves
                            && editing.autosaving.as_ref() == Some(&text)
                    });
                this.autosaved_observed(input_id, saves, text, result, stamp, conflict, cx);
                if accepted {
                    if let Some((absolute_path, saved_revision)) = rules_revision {
                        this.agents_rules_saved(&absolute_path, &saved_revision, cx);
                    }
                }
            });
        })
        .detach();
    }

    #[cfg(test)]
    fn autosaved(
        &mut self,
        input_id: EntityId,
        saves: u64,
        text: DraftText,
        result: anyhow::Result<Save>,
        stamp: Option<DiskStamp>,
        cx: &mut Context<Self>,
    ) {
        self.autosaved_observed(input_id, saves, text, result, stamp, None, cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn autosaved_observed(
        &mut self,
        input_id: EntityId,
        saves: u64,
        text: DraftText,
        result: anyhow::Result<Save>,
        stamp: Option<DiskStamp>,
        conflict: Option<String>,
        cx: &mut Context<Self>,
    ) {
        #[cfg(test)]
        let _work = work::scope();
        let Some(editing) = &mut self.editing else {
            return;
        };
        if editing.input.entity_id() != input_id || editing.autosaving.as_ref() != Some(&text) {
            return;
        }
        editing.autosaving = None;
        if std::mem::take(&mut editing.refresh_deferred) {
            self.refresh_after_autosave(cx);
        }
        let editing = self.editing.as_mut().unwrap();
        // A save on the UI thread came after this one started; it decided.
        editing.source_status.set(None);
        if editing.saves != saves {
            cx.notify();
            return;
        }
        match result {
            Ok(Save::Saved) => {
                // The recovery copy of a large note is written off the UI thread too.
                let write = editing.store.saved_snapshot_queued_at(text, stamp);
                // This journal generation includes the confirmed base and the
                // latest typing. An obsolete pre-save task cannot acknowledge it.
                editing.recovery_epoch = editing.recovery_epoch.wrapping_add(1);
                editing.protecting = true;
                let epoch = editing.recovery_epoch;
                cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move { write.persist() })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        let Some(editing) = this.editing.as_mut().filter(|e| {
                            e.input.entity_id() == input_id && e.recovery_epoch == epoch
                        }) else {
                            return;
                        };
                        editing.protecting = false;
                        editing.input.update(cx, |_, cx| cx.notify());
                        if let Err(error) = result {
                            editing.save_failed = true;
                            eprintln!("Autosaved, but recovery acknowledgement failed: {error:#}");
                            this.link_notice = Some(DRAFT_WRITE_NOTICE.into());
                            cx.notify();
                        }
                    });
                })
                .detach();
                editing.saved_at = Some(
                    time::OffsetDateTime::now_local()
                        .unwrap_or_else(|_| time::OffsetDateTime::now_utc()),
                );
                editing.save_failed = false;
                if matches!(
                    self.link_notice.as_deref(),
                    Some(DRAFT_WRITE_NOTICE | AUTOSAVE_START_NOTICE)
                ) {
                    self.link_notice = None;
                }
                // Typing during the save: its timer found this save running
                // and skipped, so the rest is saved after its own pause.
                let editing = self.editing.as_mut().unwrap();
                if editing.input.read(cx).source_snapshot().text.id()
                    != editing.store.snapshot().id()
                    || editing.store.has_pending_revision()
                {
                    let epoch = editing.recovery_epoch;
                    self.schedule_autosave(input_id, epoch, cx);
                }
                if self.file_preview.is_none() {
                    self.queue_saved_source(cx);
                }
            }
            Ok(Save::Conflict) => {
                editing.conflict_detected = true;
                if editing.conflict.is_none() {
                    editing.conflict = conflict;
                }
                self.link_notice = Some(format!("{CONFLICT_NOTICE} Your edits are still here. Reload, keep your version, or compare before saving.").into());
            }
            Err(error) => {
                editing.save_failed = true;
                eprintln!("Autosave failed: {error:#}");
                self.link_notice = Some(file_access_notice(&error).into());
            }
        }
        self.describe_source_conflict(cx);
        cx.notify();
    }

    /// Run the refresh that came while an autosave was writing, after the
    /// result has updated the store (#1376).
    fn refresh_after_autosave(&mut self, cx: &mut Context<Self>) {
        let handle = self.reader_window;
        cx.spawn(async move |this, cx| {
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |this, cx| this.refresh_source_in_background(window, cx));
            });
        })
        .detach();
    }

    pub(super) fn leave_source(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.save_source_for_navigation(cx) {
            return false;
        }
        self.remember_source_mode();
        self.editing = None;
        self.outline_list
            .reset_with_uniform_height(self.outline.len(), px(OUTLINE_ROW_HEIGHT));
        true
    }

    /// Remember only presentation preference before a successful navigation exit.
    /// Saving and permission to leave remain the caller's existing guards.
    pub(super) fn remember_source_mode(&mut self) {
        // Document preparation can call leave_source again after the editor
        // has already closed. That no-op must retain the last presentation.
        // A text, CSV or code editor has no Live Preview; leaving it must not
        // turn the next note's Live Preview into Source (GitHub #6).
        if let Some(editing) = self.editing.as_ref().filter(|editing| editing.markdown) {
            let live = self.source_live_preview();
            self.ui_state.live_preview = match editing.kept_preference {
                Some(preferred) if live => preferred,
                _ => live,
            };
        }
    }

    /// The window's chosen editing mode carries to an editable text, CSV or
    /// code file, as it already does between notes (GitHub #6). Explicit Read
    /// clears the choice before showing its file; a file that cannot be edited
    /// stays a read-only preview and the choice is kept for the next one.
    pub(super) fn reopen_chosen_source(
        &mut self,
        tree_had_focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(preview) = &self.file_preview else {
            return;
        };
        if !self.ui_state.source_mode
            || self.editing.is_some()
            || !reader_delimited::editable(&preview.rel)
        {
            return;
        }
        if let Some(code) = preview.code.clone().filter(|code| code.read(cx).loading()) {
            // A code file is known to be editable only once it has loaded.
            let pending = Rc::new(std::cell::Cell::new(true));
            cx.observe_in(&code, window, move |this, code, window, cx| {
                if !pending.get() || code.read(cx).loading() {
                    return;
                }
                pending.set(false);
                let current = this
                    .file_preview
                    .as_ref()
                    .and_then(|preview| preview.code.as_ref())
                    == Some(&code);
                if current {
                    this.reopen_chosen_source(tree_had_focus, window, cx);
                }
            })
            .detach();
            return;
        }
        self.toggle_source(window, cx);
        // Tree navigation and rename keys stay in the navigator that chose it.
        if tree_had_focus && self.editing.is_some() {
            self.tree_focus.focus(window, cx);
        }
    }

    /// A vanished source must not trap this window. Keep the latest durable
    /// draft for Recover notes instead; conflicts and failed recovery writes
    /// still block navigation. Explicit Save continues to report failure.
    pub(super) fn save_source_for_navigation(&mut self, cx: &mut Context<Self>) -> bool {
        let (saved, protected) = self.save_source_outcome(cx);
        if saved {
            return true;
        }
        if !protected
            || !self.editing.as_ref().is_some_and(|editing| {
                std::fs::symlink_metadata(editing.store.path())
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            })
        {
            return false;
        }
        let handle = self.reader_window;
        cx.spawn(async move |_, cx| {
            let _ = handle.update(cx, |_, window, cx| {
                reader_toast::transient(
                    "The file was moved or deleted. Draft kept in recovery. Use More → Recover notes… to save your edits to a new location.",
                    window,
                    cx,
                );
            });
        })
        .detach();
        true
    }

    /// Open a note just created: in Live Preview, so a first note reads as a
    /// note and not as raw Markdown, without changing the Source/Live Preview
    /// choice the person made for other notes (#1352). Switching to Source
    /// here is a choice of its own and is kept.
    pub(super) fn edit_created_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let preferred = self.ui_state.live_preview;
        let source_mode = self.ui_state.source_mode;
        self.toggle_source(window, cx);
        // Creating a note opens its body for typing, not a new window mode.
        self.ui_state.source_mode = source_mode;
        self.set_live_preview(true, window, cx);
        if let Some(editing) = &mut self.editing {
            editing.kept_preference = Some(preferred);
        }
    }
    pub(super) fn resolve_source(
        &mut self,
        reload: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Keep both promises preservation. A later Reload must not throw away
        // typing since that copy (even before mutation subscribers have run).
        // Revalidate the copy's bytes too: it may have been changed or removed.
        if reload
            && self
                .editing
                .as_ref()
                .is_some_and(|e| e.conflict_detected && e.conflict_copy.is_some())
        {
            let (_, protected) = self.save_source_outcome(cx);
            if !protected || !self.keep_both_source(cx) {
                return;
            }
        }
        let Some(mut editing) = self.editing.take() else {
            return;
        };
        editing.recovery_epoch = editing.recovery_epoch.wrapping_add(1);
        editing.protecting = false;
        let rules_revision = (!reload)
            .then(|| pending_agents_revision(&editing.store, true))
            .flatten();
        let result = if reload {
            editing.store.reload().map(|_| Save::Saved)
        } else if let Some(current) = &editing.conflict {
            editing.store.keep_mine(current)
        } else {
            Ok(Save::Conflict)
        };
        let changed_rules_saved = matches!(&result, Ok(Save::Saved));
        match result {
            Ok(Save::Saved) => {
                editing.saved_at = Some(
                    time::OffsetDateTime::now_local()
                        .unwrap_or_else(|_| time::OffsetDateTime::now_utc()),
                );
                if reload {
                    editing.input.update(cx, |input, cx| {
                        input.set_value(editing.store.text().to_owned(), window, cx)
                    });
                }
                editing.conflict = None;
                editing.conflict_detected = false;
                editing.compare = false;
                editing.save_failed = false;
                editing.protecting = false;
                self.link_notice = None;
            }
            Ok(Save::Conflict) => {
                editing.conflict_detected = true;
                editing.conflict = editing.store.current().ok();
                if let Some(current) = &editing.conflict {
                    editing
                        .current_input
                        .update(cx, |input, cx| input.set_value(current.clone(), window, cx));
                    editing.compare = true;
                }
                self.link_notice = Some(
                    "The file changed again. Compare the current version before retrying.".into(),
                );
            }
            Err(error) => {
                editing.save_failed = true;
                self.link_notice = Some(format!("Could not resolve: {error:#}").into());
            }
        }
        self.editing = Some(editing);
        if changed_rules_saved {
            self.deleted_note = None;
            if let Some((absolute_path, saved_revision)) = rules_revision {
                self.agents_rules_saved(&absolute_path, &saved_revision, cx);
            }
        }
        cx.notify();
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn describe_source_conflict(&self, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.as_ref().filter(|e| e.conflict_detected) else {
            return;
        };
        let Some(reviewed) = editing.conflict.clone() else {
            return;
        };
        let Some(controller) = reader_settings_sync::active_controller(cx) else {
            return;
        };
        let input = editing.input.entity_id();
        let root = self.vault_root.clone();
        let rel = self.current_rel.clone();
        let target_root = root.clone();
        let target_rel = rel.clone();
        cx.spawn(async move |this, cx| {
            let text = reviewed.clone();
            let name = cx
                .background_executor()
                .spawn(async move {
                    controller
                        .file_change_origin(&target_root, &target_rel, &text)
                        .ok()
                        .flatten()
                })
                .await;
            let Some(name) = name else {
                return;
            };
            let _ = this.update(cx, |reader, cx| {
                if reader.vault_root != root || reader.current_rel != rel {
                    return;
                }
                if reader.editing.as_ref().is_some_and(|e| {
                    e.input.entity_id() == input
                        && e.conflict_detected
                        && e.conflict.as_ref() == Some(&reviewed)
                }) {
                    if let Some(notice) = reader
                        .link_notice
                        .as_deref()
                        .filter(|n| n.starts_with(CONFLICT_NOTICE))
                    {
                        reader.link_notice = Some(
                            notice
                                .replacen(
                                    CONFLICT_NOTICE,
                                    &format!(
                                        "{CONFLICT_NOTICE} A newer version arrived from {name}."
                                    ),
                                    1,
                                )
                                .into(),
                        );
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn describe_source_conflict(&self, _cx: &mut Context<Self>) {}

    /// Explicit Save/Keep both publishes the latest local buffer as a new
    /// conflict note. The incoming canonical note is never replaced. Keep the
    /// conflict open until the person chooses which document to continue with.
    fn keep_both_source(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(editing) = self.editing.as_mut().filter(|e| e.conflict_detected) else {
            return false;
        };
        let stamp = editing.input.read(cx).source_stamp();
        let text = editing.input.read(cx).value().to_string();
        let previous = editing
            .conflict_copy
            .as_ref()
            .filter(|(revision, rel)| {
                *revision == stamp
                    && std::fs::read_to_string(self.vault_root.join(rel))
                        .ok()
                        .as_deref()
                        == Some(text.as_str())
            })
            .map(|(_, rel)| rel.clone());
        let rel = previous.unwrap_or_else(|| conflict_copy_rel(&self.current_rel));
        let saved = editing
            .conflict_copy
            .as_ref()
            .is_some_and(|(revision, old)| *revision == stamp && *old == rel)
            && std::fs::read_to_string(self.vault_root.join(&rel))
                .ok()
                .as_deref()
                == Some(text.as_str());
        let result = if saved {
            Ok(())
        } else {
            okilum_core::note_files::create_attachment_with_source(
                &self.vault_root,
                Path::new(&rel),
                text.as_bytes(),
            )
        };
        let kept = result.is_ok();
        match result {
            Ok(()) => {
                editing.conflict_copy = Some((stamp, rel.clone()));
                editing.save_failed = false;
                self.link_notice = Some(format!("{CONFLICT_NOTICE} Both versions are kept. A local snapshot was saved as {rel}. The incoming file is unchanged. Reload keeps any later edits in another copy before loading the incoming file.").into());
                self.queue_vault_mutation(
                    okilum_core::Changes {
                        changed: [rel].into(),
                        ..Default::default()
                    },
                    cx,
                );
            }
            Err(error) => {
                editing.save_failed = true;
                self.link_notice = Some(format!("{CONFLICT_NOTICE} Could not save both versions: {error:#}. Your draft is still protected; the incoming file is unchanged.").into());
            }
        }
        cx.notify();
        kept
    }

    fn park_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = &mut self.editing else {
            return;
        };
        // Explicit exit preserves the latest buffer even if the note was moved,
        // deleted, or is temporarily unwritable. Failed journal writes never exit.
        match editing
            .store
            .set_text(editing.input.read(cx).value().to_string())
        {
            Ok(()) => {
                self.editing = None;
                self.ui_state.source_mode = false;
                self.link_notice = Some("Draft kept in recovery. Reopen this note and enter source mode to restore it. If it was moved, restore its original path first.".into());
                self.focus_handle.focus(window, cx);
            }
            Err(error) => {
                self.link_notice = Some(
                    format!("Cannot protect this draft: {error:#}. Copy the draft before closing.")
                        .into(),
                );
            }
        }
        cx.notify();
    }
    /// #1296: scroll the new editor to the Reader's top block and start the
    /// caret there, so typing stays in view; a restored selection (#1095)
    /// elsewhere would be off screen. Undo still scrolls to its own edit
    /// (#945). Skipped when the user typed, moved the caret or opened Find.
    fn place_editor_at_reader(
        &mut self,
        input: Entity<EditorState>,
        (top, canonical, rendered): (usize, Arc<str>, String),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let untouched = {
            let input = input.read(cx);
            (
                input.source_stamp(),
                input.selected_range(),
                input.search_activation_revision(),
            )
        };
        cx.spawn_in(window, async move |this, cx| {
            let line = cx
                .background_executor()
                .spawn(
                    async move { okilum_core::deep_link::block_line(&canonical, &rendered, top) },
                )
                .await
                .filter(|line| *line > 1);
            let Some(line) = line else {
                return;
            };
            let _ = this.update_in(cx, |this, _window, cx| {
                if this
                    .editing
                    .as_ref()
                    .is_none_or(|editing| editing.input != input)
                {
                    return;
                }
                input.update(cx, |input, cx| {
                    // Find does not move the editing selection. Its activation
                    // still supersedes an older Reader-to-editor handoff, even
                    // if Find has already closed when this parse completes.
                    if (
                        input.source_stamp(),
                        input.selected_range(),
                        input.search_activation_revision(),
                    ) != untouched
                    {
                        return;
                    }
                    // Place the source selection without changing keyboard
                    // focus (including an IME in a later sidebar action).
                    use gpui_component::input::RopeExt as _;
                    let offset =
                        input
                            .text()
                            .position_to_offset(&gpui_component::input::Position {
                                line: line - 1,
                                character: 0,
                            });
                    input.set_selected_range(offset..offset, cx);
                    input.scroll_line_to_top(line as usize - 1, cx);
                });
            });
        })
        .detach();
    }

    pub(super) fn restore_source_position(
        &self,
        offset: [f32; 2],
        window: &Window,
        _: &mut Context<Self>,
    ) {
        if let Some(editing) = &self.editing {
            let input = editing.input.clone();
            window.on_next_frame(move |_, cx| {
                input.update(cx, |input, cx| {
                    input
                        .set_scroll_offset(point(px(offset[0].min(0.)), px(offset[1].min(0.))), cx);
                });
            });
        }
    }

    pub(super) fn source_highlighting_pending(&self, cx: &App) -> bool {
        self.editing
            .as_ref()
            .is_some_and(|editing| editing.input.read(cx).highlighting_pending())
    }

    pub(super) fn source_scroll_offset(&self, cx: &App) -> Option<Point<Pixels>> {
        self.editing
            .as_ref()
            .map(|editing| editing.input.read(cx).scroll_offset())
    }

    pub(super) fn render_source(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.refresh_live_preview_colors(cx);
        // Room below the last line for toasts, focus and typewriter (#1643).
        self.apply_writing_modes(window, cx);
        let editing = self.editing.as_ref().unwrap();
        editing.table_grid.update(cx, |grid, cx| {
            grid.sync_presentation(editing.live_preview.enabled, window, cx);
        });
        let palette = brand::palette(cx);
        let column = v_flex().key_context("ReaderSource").size_full().min_w_0();
        // #1992: Enter, Tab and the comment key in a code block's body, before
        // the list keys; #1635: Enter, Tab and Backspace in lists, before the
        // input; #1636: the formatting keys.
        // #1921: the link list takes its keys before everything else.
        let column = self.link_complete_actions(column, cx);
        // #1642: Enter on a rendered block Esc returned to, before the code,
        // list and input keys.
        let column = self.block_entry_actions(column, cx);
        let column = self.code_comfort_actions(column, cx);
        // #1993: the caret's code block in an external editor.
        let column = self.external_block_actions(column, cx);
        let column = self.format_actions(column, cx);
        let column = self.table_actions(column, cx);
        self.list_actions(column, cx)
            // #1611: the paste commands beside Cmd+V, and files dropped onto
            // the note, through the same router.
            .on_action(cx.listener(
                |this, _: &crate::note_paste::PasteAsPlainText, window, cx| {
                    this.paste_with_mode(PasteMode::Plain, window, cx)
                },
            ))
            .on_action(cx.listener(
                |this, _: &crate::note_paste::PasteAsTitledLink, window, cx| {
                    this.paste_with_mode(PasteMode::TitledLink, window, cx)
                },
            ))
            .drag_over::<ExternalPaths>(|style, _, _, cx| {
                style.bg(brand::palette(cx).selected.opacity(0.12))
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.drop_on_editor(paths, window, cx)
            }))
            .when(editing.save_failed || editing.conflict_detected, |column| {
                column.child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .px_4()
                        .py_2()
                        .when(editing.save_failed || editing.conflict_detected, |row| {
                            row.child(
                                reader_icon_button(
                                    "source-park",
                                    IconName::ArrowRight,
                                    "Close, keeping draft",
                                    cx,
                                )
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.park_source(window, cx)),
                                ),
                            )
                            .child(
                                reader_icon_button(
                                    "source-copy-draft",
                                    IconName::Copy,
                                    "Copy draft",
                                    cx,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        if let Some(editing) = &this.editing {
                                            let text = editing.input.read(cx).value().to_string();
                                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                                        }
                                    },
                                )),
                            )
                        })
                        .when(editing.conflict_detected, |row| {
                            row.child(
                                reader_icon_button(
                                    "source-reload",
                                    IconName::RotateCw,
                                    "Reload from disk",
                                    cx,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| this.resolve_source(true, window, cx),
                                )),
                            )
                            .child(
                                reader_icon_button(
                                    "source-keep",
                                    IconName::Check,
                                    "Keep my version",
                                    cx,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| this.resolve_source(false, window, cx),
                                )),
                            )
                            .when(self.deleted_note.is_none(), |row| {
                                row.child(
                                    reader_icon_button(
                                        "source-keep-both",
                                        IconName::Copy,
                                        "Keep both versions",
                                        cx,
                                    )
                                    .debug_selector(|| "source-keep-both".into())
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.save_source_explicit(cx);
                                        },
                                    )),
                                )
                            })
                            .child(
                                reader_icon_button(
                                    "source-compare",
                                    IconName::PanelRight,
                                    "Compare versions",
                                    cx,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        if let Some(editing) = &mut this.editing {
                                            editing.compare = !editing.compare;
                                            if let Some(current) = &editing.conflict {
                                                editing.current_input.update(cx, |input, cx| {
                                                    input.set_value(current.clone(), window, cx)
                                                });
                                            }
                                        }
                                        cx.notify();
                                    },
                                )),
                            )
                        }),
                )
            })
            .when(editing.compare, |column| {
                column
                    .child(
                        div()
                            .px_4()
                            .text_color(palette.text_muted)
                            .child("File on disk (above) · Your version (below)"),
                    )
                    .child(
                        Editor::new(&editing.current_input)
                            .appearance(false)
                            .readonly(true)
                            .context_menu(|menu, _, _| {
                                crate::reader_code_file::text_menu(menu, false)
                            })
                            .font_family(reader_fonts::code(cx))
                            .h(px(220.)),
                    )
            })
            .child(
                Editor::new(&editing.input)
                    .appearance(false)
                    .map(|editor| {
                        let (left, right) = self.note_text_margins(window, cx);
                        editor.text_margins(left, right)
                    })
                    .context_menu({
                        let facts = editing.menu_facts.clone();
                        let markdown = editing.markdown;
                        move |menu, _, cx| {
                            let menu = context_menu::editor_menu(menu, &facts, markdown, cx);
                            if markdown && facts.editable() {
                                menu.separator().submenu(
                                    "Table",
                                    gpui_component::native_menu::NativeMenu::from(tables::menu()),
                                )
                            } else {
                                menu
                            }
                        }
                    })
                    .font_family(if editing.live_preview.enabled {
                        reader_fonts::note(cx)
                    } else {
                        reader_fonts::code(cx)
                    })
                    .text_size(px(if editing.live_preview.enabled {
                        reader_ui_state::font_size(cx)
                    } else {
                        13. * reader_ui_state::font_size(cx) / BODY_FONT_SIZE
                    }))
                    .size_full(),
            )
            .children(self.render_link_complete(window, cx))
            .when(editing.live_preview.enabled, |column| {
                column.child(editing.math_preview.clone())
            })
            .into_any_element()
    }
}

type BlockContext = Rc<std::cell::RefCell<BlockNote>>;

/// Live Preview's rendered blocks (tables, images; S7, #936), drawn the way
/// Reader draws them: the block's Markdown is prepared like a Reader document
/// (embeds and relative images resolved against the vault), once per block
/// content, and shown through the Reader's plugins. Display only: a click
/// lands on the editor, which puts the caret in the block and reveals its
/// source; links do not open from here.
fn live_blocks(
    root: PathBuf,
    rel: String,
    reader: WeakEntity<Reader>,
    press: gpui_component::input::projection::BlockPressSlot,
    context: BlockContext,
) -> gpui_component::input::projection::BlockRenderer {
    // Prepared Markdown per block, keyed by the exact Markdown it was prepared
    // from, for the vault it was prepared against: a rescanned vault (a new
    // image) prepares again.
    let prepared = Rc::new(std::cell::RefCell::new((
        std::collections::HashMap::<u64, (SharedString, SharedString, (u64, u64), bool)>::new(),
        std::collections::VecDeque::<u64>::new(),
    )));
    Rc::new(move |block, source, _width, window, cx| {
        let Some(this) = reader.upgrade() else {
            return div().into_any_element();
        };
        if let Some(link) = this.read(cx).agent_document.as_ref().and_then(|current| {
            current
                .imports
                .links()
                .iter()
                .find(|link| link.import.line == (block.source.start.0..block.source.end.0))
                .cloned()
        }) {
            return reader_document_decorations::render_line(
                this.read(cx),
                link,
                reader.clone(),
                None,
                cx,
            );
        }
        let table_range = block.source.start.0..block.source.end.0;
        let grid = {
            let reader = this.read(cx);
            reader
                .editing
                .as_ref()
                .filter(|editing| editing.live_preview.enabled)
                .map(|editing| {
                    (
                        editing.table_grid.clone(),
                        editing.input.read(cx).source_stamp(),
                    )
                })
        };
        if let Some((grid, _)) = &grid {
            if grid.read(cx).active_range(cx).as_ref() == Some(&table_range) {
                return grid.clone().into_any_element();
            }
        }
        let (vault, states, identities, code_source, focused) = {
            let this = this.read(cx);
            (
                this.vault.clone(),
                this.link_presentations.clone(),
                this.link_identities.clone(),
                this.editing
                    .as_ref()
                    .and_then(|editing| editing.live_preview.block_markdown(source)),
                // The block Esc returned to: Enter or F2 opens it (#1642).
                this.valid_block_focus(cx)
                    .is_some_and(|focus| focus.block == (block.source.start.0..block.source.end.0)),
            )
        };
        let markdown = {
            let mut prepared = prepared.borrow_mut();
            let note = context.borrow().clone();
            // Footnote references take the whole note's numbers; only a
            // block with footnote syntax pays for the note-wide pass.
            let block_source = code_source
                .map(|source| source.to_string())
                .unwrap_or_else(|| {
                    note.block_footnotes(source, block.source.start.0..block.source.end.0)
                        .unwrap_or_else(|| source.to_owned())
                });
            let input = note.with_definitions(block_source);
            let identity = vault.resolution_identity();
            // Old/deleted block keys cannot grow forever now that autosave no
            // longer clears preparation. Retire one FIFO entry, never walk it.
            if !prepared.0.contains_key(&block.key) {
                if prepared.0.len() >= 256 {
                    if let Some(retired) = prepared.1.pop_front() {
                        prepared.0.remove(&retired);
                    }
                }
                prepared.1.push_back(block.key);
            }
            let entry = prepared.0.entry(block.key).or_default();
            if entry.1.is_empty() || entry.0 != input || (entry.3 && entry.2 != identity) {
                let source = input.to_string();
                #[cfg(test)]
                LIVE_PREPARES.with(|count| count.set(count.get() + 1));
                let dependent = input.contains('[');
                *entry = (
                    input,
                    okilum_core::render::reader_document_from_source(&vault, &rel, &source)
                        .rendered
                        .into(),
                    identity,
                    dependent,
                );
            }
            entry.1.clone()
        };
        // A press on an image lands on its embed's source, not on whatever
        // raw text lies under the pointer (#1642). A table cell's own press
        // (above) is recorded first and kept.
        let on_image = image_embed_start(source).map(|at| {
            let press = press.clone();
            let offset = block.source.start.0 + at;
            move |event: &gpui::MouseDownEvent, _: &mut Window, _: &mut App| {
                if let Ok(mut slot) = press.lock() {
                    if slot.is_none_or(|known| known.position != event.position) {
                        *slot = Some(gpui_component::input::projection::BlockPress {
                            position: event.position,
                            offset: gpui_component::input::projection::SourceByte(offset),
                        });
                    }
                }
            }
        });
        let font_size = px(reader_ui_state::font_size(cx));
        let mut style = reader_text_zoom::document_style(cx);
        style.heading_base_font_size = font_size;
        // The first line keeps two identical blocks apart.
        let id = SharedString::from(format!("live-block-{}-{}", block.key, block.lines.start));
        // A press on a table cell lands on that cell's source (#1067).
        let on_cell = {
            let press = press.clone();
            let base = block.source.start.0;
            let source: Arc<str> = source.into();
            move |cell: gpui_component::text::TableCellPress,
                  event: &gpui::MouseDownEvent,
                  window: &mut Window,
                  cx: &mut App| {
                // A plain cell press opens the revision-bound persistent grid.
                // Double-click and modified presses retain the existing source hooks.
                if event.click_count == 1
                    && !event.modifiers.shift
                    && !event.modifiers.control
                    && !event.modifiers.platform
                    && !event.modifiers.alt
                {
                    if let Some((grid, stamp)) = &grid {
                        let opened = grid.update(cx, |grid, cx| {
                            grid.open_cell(
                                table_range.clone(),
                                *stamp,
                                &source,
                                cell.row,
                                cell.column,
                                window,
                                cx,
                            )
                        });
                        if opened {
                            if let Some(at) = crate::source_presentation::table_cell_offset(
                                &source,
                                cell.row,
                                cell.column,
                            ) {
                                grid.update(cx, |grid, _| {
                                    grid.remember_source_press(*stamp, event.position, base + at)
                                });
                            }
                            cx.stop_propagation();
                            return;
                        }
                    }
                }
                let Some(at) =
                    crate::source_presentation::table_cell_offset(&source, cell.row, cell.column)
                else {
                    return;
                };
                if let Ok(mut slot) = press.lock() {
                    *slot = Some(gpui_component::input::projection::BlockPress {
                        position: event.position,
                        offset: gpui_component::input::projection::SourceByte(base + at),
                    });
                }
            }
        };
        let focus_ring = brand::palette(cx).focus;
        div()
            // At least one line while an image loads: one height change, not
            // a shrink to padding and a grow.
            .min_h(window.line_height())
            .w_full()
            .relative()
            .when_some(on_image, |this, f| this.on_mouse_down(MouseButton::Left, f))
            .child(
                // Each block is its own document: its position in the note
                // keeps identical callouts' fold state apart (#1067).
                reader_plugins_at(
                    MarkdownScope {
                        fold_base: block.source.start.0,
                        live: true,
                    },
                    root.clone(),
                    TextView::markdown(id, markdown),
                    reader.clone(),
                    SelectionFormat::default(),
                    states,
                    &identities,
                )
                .selectable(false)
                .on_table_cell_press(on_cell)
                .on_link_click(|_, _, _, _| {})
                .style(style)
                .font_family(reader_fonts::note(cx))
                .text_size(font_size)
                .w_full(),
            )
            // Drawn over the block, not around it: its height is unchanged.
            .when(focused, |this| {
                this.child(
                    div()
                        .debug_selector(|| "live-block-focus".into())
                        .absolute()
                        .inset_0()
                        .rounded(cx.theme().radius)
                        .border_2()
                        .border_color(focus_ring),
                )
            })
            .into_any_element()
    })
}

/// Where a rendered block made of one image embed (`![[x.png]]` or
/// `![alt](x.png)`) starts its embed, past any indentation; None for any
/// other block.
fn image_embed_start(source: &str) -> Option<usize> {
    let body = source.trim();
    (body.starts_with("![") && !body.contains('\n'))
        .then(|| source.len() - source.trim_start().len())
}

/// Lines that keep one alignment when paragraphs take their own direction:
/// fenced code blocks, fences included, and table rows (S6c, #1034). A
/// right-to-left comment in code or a Hebrew table cell must not move its line.
fn direction_exempt_lines(source: &str) -> Vec<std::ops::Range<usize>> {
    #[cfg(test)]
    work::inspect(source.len());
    let mut lines: Vec<std::ops::Range<usize>> = Vec::new();
    let mut push = |line: std::ops::Range<usize>| match lines.last_mut() {
        Some(last) if last.end >= line.start => last.end = last.end.max(line.end),
        _ => lines.push(line),
    };
    let mut fence: Option<(char, usize, usize)> = None;
    let mut count = 0;
    for (ix, line) in source.split('\n').enumerate() {
        count = ix + 1;
        let indent = line.len() - line.trim_start_matches(' ').len();
        let body = line
            .trim_start_matches(' ')
            .trim_end_matches(['\r', ' ', '\t']);
        let run = |ch: char| body.chars().take_while(|&c| c == ch).count();
        if let Some((ch, len, start)) = fence {
            if indent < 4 && run(ch) >= len && body.chars().all(|c| c == ch) {
                push(start..ix + 1);
                fence = None;
            }
            continue;
        }
        if indent < 4 {
            if let Some(ch) = ['`', '~'].into_iter().find(|&ch| run(ch) >= 3) {
                fence = Some((ch, run(ch), ix));
                continue;
            }
            if body.starts_with('|') {
                push(ix..ix + 1);
            }
        }
    }
    if let Some((_, _, start)) = fence {
        push(start..count);
    }
    lines
}

/// The task toggle for the line holding `caret`: a bullet or ordered task,
/// possibly indented or quoted (#1051). The same edit as a click on its box.
fn caret_task_toggle(source: &str, caret: usize) -> Option<(std::ops::Range<usize>, &'static str)> {
    let caret = caret.min(source.len());
    let start = source.get(..caret)?.rfind('\n').map_or(0, |at| at + 1);
    let end = source[start..]
        .find('\n')
        .map_or(source.len(), |at| start + at);
    let line = &source[start..end];
    let mut at = 0;
    let bytes = line.as_bytes();
    // Indentation and quote markers.
    while at < bytes.len() && matches!(bytes[at], b' ' | b'\t' | b'>') {
        at += 1;
    }
    let marker = at;
    let digits = bytes[at..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    at += match bytes.get(at) {
        Some(b'-' | b'+' | b'*') => 1,
        Some(b'0'..=b'9') if matches!(bytes.get(at + digits), Some(b'.' | b')')) => digits + 1,
        _ => return None,
    };
    let gap = bytes[at..]
        .iter()
        .take_while(|b| matches!(b, b' ' | b'\t'))
        .count();
    if gap == 0 || bytes.get(at + gap) != Some(&b'[') {
        return None;
    }
    task_toggle(source, start + marker..start + at + gap + 3)
}

impl Reader {
    /// «Toggle task» (⌘L / Ctrl+L): flips the checkbox of the caret's task
    /// line, one source edit and one undo step, like a click on the box.
    pub(super) fn toggle_task_at_caret(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.source_input() else {
            return;
        };
        input.update(cx, |state, cx| {
            let source = state.value().to_string();
            if let Some((range, text)) = caret_task_toggle(&source, state.cursor()) {
                state.replace_source_range(range, text, window, cx);
            }
        });
    }
}

/// The edit that toggles the task checkbox whose marker (`- [ ]`) is at
/// `marker`: the byte between the brackets becomes `x` or a space.
fn task_toggle(
    source: &str,
    marker: std::ops::Range<usize>,
) -> Option<(std::ops::Range<usize>, &'static str)> {
    let text = source.get(marker.clone())?;
    let open = text.find('[')?;
    let at = marker.start + open + 1;
    if source.as_bytes().get(at + 1) != Some(&b']') {
        return None;
    }
    match source.as_bytes()[at] {
        b' ' => Some((at..at + 1, "x")),
        b'x' | b'X' => Some((at..at + 1, " ")),
        _ => None,
    }
}

fn same_move_root(a: &Path, b: &Path) -> bool {
    a == b
        || a.canonicalize()
            .ok()
            .zip(b.canonicalize().ok())
            .is_some_and(|(a, b)| a == b)
}
impl Reader {
    pub(super) fn revert_link_move(
        &mut self,
        path: &Path,
        state: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let operation = okilum_core::link_rewrite::Operation::load(path)?;
        anyhow::ensure!(
            same_move_root(&operation.root, &self.vault_root),
            "Recovery belongs to a different folder"
        );
        let paths: Vec<_> = operation
            .files
            .keys()
            .flat_map(|p| {
                [
                    p.clone(),
                    okilum_core::link_rewrite::moved_path(p, &operation.from, &operation.to),
                ]
            })
            .chain([operation.from.clone(), operation.to.clone()])
            .collect();
        self.check_move_editors(&paths, cx)?;
        let self_affected = paths
            .iter()
            .any(|p| self.current_rel == *p || self.current_rel.starts_with(&format!("{p}/")));
        let self_editing = if self_affected {
            self.editing.take().is_some()
        } else {
            false
        };
        let readers = cx.default_global::<Editors>().0.clone();
        let mut parked = vec![];
        for (reader, owner) in readers {
            if reader.entity_id() == cx.entity_id() {
                continue;
            }
            if let Ok(Some(was_editing)) = reader.update(cx, |r, _| {
                if same_move_root(&r.vault_root, &operation.root)
                    && paths
                        .iter()
                        .any(|p| r.current_rel == *p || r.current_rel.starts_with(&format!("{p}/")))
                {
                    Some(r.editing.take().is_some())
                } else {
                    None
                }
            }) {
                parked.push((reader, owner, was_editing));
            }
        }
        let result = okilum_core::link_rewrite::Operation::revert(path, state);
        for (reader, owner, was_editing) in parked {
            let _ = owner.update(cx, |_, window, cx| {
                reader.update(cx, |r, cx| {
                    if result.is_ok() {
                        r.current_rel = okilum_core::link_rewrite::moved_path(
                            &r.current_rel,
                            &operation.to,
                            &operation.from,
                        );
                        for p in &mut r.navigation.history {
                            *p = okilum_core::link_rewrite::moved_path(
                                p,
                                &operation.to,
                                &operation.from,
                            );
                        }
                    }
                    let rel = r.current_rel.clone();
                    r.prepare_document(&rel, None, None, window, cx);
                    if was_editing {
                        r.toggle_source(window, cx);
                    }
                    cx.notify();
                })
            });
        }
        if self_affected {
            if result.is_ok() {
                self.current_rel = okilum_core::link_rewrite::moved_path(
                    &self.current_rel,
                    &operation.to,
                    &operation.from,
                );
                for p in &mut self.navigation.history {
                    *p = okilum_core::link_rewrite::moved_path(p, &operation.to, &operation.from);
                }
            }
            let rel = self.current_rel.clone();
            self.prepare_document(&rel, None, None, window, cx);
            if self_editing {
                self.toggle_source(window, cx);
            }
        }
        if result.is_ok() && same_move_root(&self.vault_root, &operation.root) {
            self.tree.note_moved(&operation.to, &operation.from);
            self.remap_move_sidebar(&operation.to, &operation.from, cx);
            // Pinned/recent paths also belong to readers displaying unrelated notes.
            for (reader, _) in cx.default_global::<Editors>().0.clone() {
                if reader.entity_id() != cx.entity_id() {
                    let _ = reader.update(cx, |r, cx| {
                        if same_move_root(&r.vault_root, &operation.root) {
                            r.tree.note_moved(&operation.to, &operation.from);
                            r.remap_move_sidebar(&operation.to, &operation.from, cx);
                            cx.notify();
                        }
                    });
                }
            }
            let mut changes = okilum_core::Changes {
                changed: operation.files.keys().cloned().collect(),
                removed: std::collections::BTreeSet::from([operation.to.clone()]),
                ..Default::default()
            };
            if operation.directory.is_some() {
                changes
                    .directories
                    .extend([operation.from.clone(), operation.to.clone()]);
            }
            changes.changed.insert(operation.from);
            self.queue_vault_mutation(changes, cx);
        }
        result
    }
    pub(super) fn sync_move_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(e) = self.editing.as_mut() {
            e.input.update(cx, |input, cx| {
                input.set_value(e.store.text().to_owned(), window, cx)
            });
        }
        self.resume_live_preview(cx);
    }
    pub(super) fn source_has_other_editor(&self, cx: &mut Context<Self>) -> bool {
        let readers = cx.default_global::<Editors>().0.clone();
        readers
            .into_iter()
            .filter(|(r, _)| r.entity_id() != cx.entity_id())
            .any(|(r, _)| {
                r.upgrade().is_some_and(|r| {
                    let r = r.read(cx);
                    same_move_root(&r.vault_root, &self.vault_root)
                        && r.current_rel == self.current_rel
                        && r.editing.is_some()
                })
            })
    }
    /// Reuse exact status facts while only selection/presentation changes.
    /// This cache paints the dot and Save affordance; action guards deliberately
    /// retain their independent current-buffer check in source_is_dirty.
    fn source_presentation_status(&self, cx: &App) -> Option<SourceStatus> {
        let editing = self.editing.as_ref()?;
        let input = editing.input.read(cx);
        let key = SourceStatusKey {
            source: input.source_stamp(),
            recovery_epoch: editing.recovery_epoch,
            saves: editing.saves,
            saved_at: editing.saved_at,
            disk_stamp: editing.store.disk_stamp(),
            conflict: editing.conflict_detected,
            protecting: editing.protecting,
        };
        if let Some(status) = editing
            .source_status
            .get()
            .filter(|status| status.key == key)
        {
            return Some(status);
        }
        // #2404: shared draft revisions make even a cold status lookup O(1).
        // Exact content equality belongs to the worker's save acknowledgement.
        let store_dirty = editing.store.has_pending_revision();
        let status = SourceStatus {
            key,
            store_dirty,
            dirty: store_dirty
                || key.conflict
                || key.protecting
                || input.source_stamp() != editing.draft_stamp,
        };
        editing.source_status.set(Some(status));
        Some(status)
    }

    pub(super) fn source_indicator_dirty(&self, cx: &App) -> bool {
        self.source_presentation_status(cx)
            .is_some_and(|status| status.dirty)
    }

    pub(super) fn source_is_dirty(&self, cx: &App) -> bool {
        self.editing.as_ref().is_some_and(|e| {
            e.store.has_pending_revision()
                || e.input.read(cx).source_stamp() != e.draft_stamp
                || e.conflict_detected
                || e.protecting
        })
    }
    pub(super) fn check_move_editors(
        &self,
        paths: &[String],
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.move_applying,
            "A move is already being applied in this window"
        );
        anyhow::ensure!(
            !paths
                .iter()
                .any(|p| self.current_rel == *p || self.current_rel.starts_with(&format!("{p}/")))
                || !self.source_is_dirty(cx),
            "Save or discard this note's unsaved edits before moving"
        );
        let readers = cx.default_global::<Editors>().0.clone();
        for (reader, _) in readers {
            if reader.entity_id() == cx.entity_id() {
                continue;
            }
            if let Some(reader) = reader.upgrade() {
                let reader = reader.read(cx);
                anyhow::ensure!(
                    !same_move_root(&reader.vault_root, &self.vault_root)
                        || !paths.iter().any(|p| reader.current_rel == *p
                            || reader.current_rel.starts_with(&format!("{p}/")))
                        || (!reader.source_is_dirty(cx) && !reader.move_applying),
                    "{} has unsaved edits in another window; save or discard them first",
                    reader.current_rel
                );
            }
        }
        Ok(())
    }
    pub(super) fn finish_move_editor(&mut self, cx: &mut Context<Self>) {
        self.move_applying = false;
        if let Some(editing) = &self.editing {
            editing
                .input
                .update(cx, |input, cx| input.set_disabled(false, cx));
        }
        self.resume_live_preview(cx);
    }

    pub(super) fn apply_link_move(
        &mut self,
        preview: &okilum_core::link_rewrite::Preview,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Task<anyhow::Result<okilum_core::link_rewrite::Applied>>> {
        let paths = preview.editor_paths();
        self.check_move_editors(&paths, cx)?;
        let state = self
            .session_directory
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No recovery storage"))?;
        let readers = cx.default_global::<Editors>().0.clone();
        let mut parked = vec![];
        for (reader, window) in readers {
            if reader.entity_id() == cx.entity_id() {
                continue;
            }
            if let Ok(Some((path, editing))) = reader.update(cx, |r, cx| {
                if same_move_root(&r.vault_root, &self.vault_root)
                    && (paths.contains(&r.current_rel)
                        || okilum_core::link_rewrite::moved_path(
                            r.selected_file(),
                            &preview.from,
                            &preview.to,
                        ) != r.selected_file())
                {
                    r.move_applying = true;
                    r.navigation.preparation_generation =
                        r.navigation.preparation_generation.wrapping_add(1);
                    cx.notify();
                    Some((r.current_rel.clone(), r.editing.take()))
                } else {
                    None
                }
            }) {
                parked.push((reader, window, path, editing));
            }
        }
        let root = self.vault_root.clone();
        let preview = preview.clone();
        let park_own = paths.contains(&self.current_rel);
        let editing = if park_own { self.editing.take() } else { None };
        let (own_store, own_ui) = match editing {
            Some(editing) => {
                let (store, ui) = editing.park(cx);
                (Some(store), Some(ui))
            }
            None => (None, None),
        };
        let mut stores = vec![(self.current_rel.clone(), own_store)];
        let parked: Vec<_> = parked
            .into_iter()
            .map(|(reader, window, path, editing)| {
                let ui = editing.map(|editing| {
                    let (store, ui) = editing.park(cx);
                    stores.push((path.clone(), Some(store)));
                    ui
                });
                if ui.is_none() {
                    stores.push((path.clone(), None));
                }
                (reader, window, path, ui)
            })
            .collect();
        self.move_applying = true;
        self.navigation.preparation_generation =
            self.navigation.preparation_generation.wrapping_add(1);
        let worker_preview = preview.clone();
        let worker = cx.background_executor().spawn(async move {
            let result = (|| -> anyhow::Result<_> {
                let mut open = std::collections::BTreeMap::new();
                for (path, store) in &mut stores {
                    if let Some(store) = store {
                        anyhow::ensure!(open.insert(path.clone(), store).is_none(), "{path} is open in multiple source editors; close duplicate editors before moving");
                    }
                }
                open.values_mut().try_for_each(|store| store.refresh_from_disk().map(|_| ()))?;
                worker_preview.apply(&root, &state, &mut open)
            })();
            (result, stores)
        });
        Ok(cx.spawn_in(window, async move |this, cx| {
            let (result, stores) = worker.await;
            let mut stores = stores.into_iter();
            let own_editing = own_ui
                .zip(stores.next().and_then(|(_, s)| s))
                .map(|(ui, store)| ui(store));
            let moved = result.as_ref().is_ok_and(|a| a.moved);
            let _ = this.update_in_own_window(cx, |this, _, cx| {
                // Keep the initiating window frozen until complete_move has
                // replaced the old source path and synchronized its input.
                if park_own {
                    this.editing = own_editing;
                }
                cx.notify();
            });
            for ((reader, window, path, ui), (_, store)) in parked.into_iter().zip(stores) {
                let editing = ui.zip(store).map(|(ui, store)| ui(store));
                let _ = window.update(cx, |_, window, cx| {
                    reader.update(cx, |r, cx| {
                        r.move_applying = false;
                        let was_editing = editing.is_some();
                        if let Some(editing) = &editing {
                            editing
                                .input
                                .update(cx, |input, cx| input.set_disabled(false, cx));
                        }
                        r.editing = editing;
                        if moved {
                            r.tree.note_moved(&preview.from, &preview.to);
                            r.remap_move_sidebar(&preview.from, &preview.to, cx);
                        }
                        let next_path = okilum_core::link_rewrite::moved_path(
                            &path,
                            &preview.from,
                            &preview.to,
                        );
                        if moved && next_path != path {
                            r.editing = None;
                            r.current_rel = next_path;
                            for p in &mut r.navigation.history {
                                *p = okilum_core::link_rewrite::moved_path(
                                    p,
                                    &preview.from,
                                    &preview.to,
                                );
                            }
                        }
                        if let Some(file) = &r.file_preview {
                            let next = okilum_core::link_rewrite::moved_path(
                                &file.rel,
                                &preview.from,
                                &preview.to,
                            );
                            if moved && next != file.rel {
                                r.preview_file(&next, window, cx);
                            }
                        } else if let Some(e) = r.editing.as_mut() {
                            e.input.update(cx, |input, cx| {
                                input.set_value(e.store.text().to_owned(), window, cx)
                            });
                        } else {
                            let rel = r.current_rel.clone();
                            r.prepare_document(&rel, None, None, window, cx);
                            if moved
                                && okilum_core::link_rewrite::moved_path(
                                    &path,
                                    &preview.from,
                                    &preview.to,
                                ) != path
                                && was_editing
                                && result.as_ref().is_ok_and(|a| a.warning.is_none())
                            {
                                r.toggle_source(window, cx);
                            }
                        }
                        r.resume_live_preview(cx);
                        cx.notify();
                    })
                });
            }
            result
        }))
    }
}

impl Reader {
    pub(super) fn restore_source_version(
        &mut self,
        reviewed: &str,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.source_is_dirty(cx),
            "Save or discard unsaved edits before restoring a version"
        );
        self.check_move_editors(std::slice::from_ref(&self.current_rel), cx)?;
        let state = self
            .session_directory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No recovery storage"))?;
        let result = if let Some(editing) = self.editing.as_mut() {
            okilum_core::source_history::restore(&mut editing.store, reviewed, text)
        } else {
            let mut editor = FileEditor::open(
                &self.vault_root.join(&self.current_rel),
                &state.join("editor-drafts"),
            )?;
            okilum_core::source_history::restore(&mut editor, reviewed, text)
        };
        // On a racing write the recovered version remains dirty and visible.
        self.sync_move_input(window, cx);
        if result.is_ok() && self.editing.is_none() {
            let rel = self.current_rel.clone();
            self.prepare_document(&rel, None, None, window, cx);
        }
        result
    }
}

#[cfg(windows)]
fn location_notice(error: &anyhow::Error) -> Option<String> {
    error
        .downcast_ref::<okilum_core::windows_files::UnsupportedLocation>()
        .map(ToString::to_string)
}

#[cfg(not(windows))]
fn location_notice(_: &anyhow::Error) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn issue2044_editor_copy_names_round_trip_extensionless_and_hidden_notes() {
        for note in [
            "TODO",
            ".hidden",
            "nested/TODO",
            "nested/.hidden",
            "nested/note.md",
        ] {
            let copy = super::conflict_copy_rel(note);
            #[cfg(target_os = "linux")]
            assert_eq!(
                okilum_sync_controller::conflicts::canonical_path(std::path::Path::new(&copy)),
                Some(std::path::PathBuf::from(note))
            );
            if !note.ends_with(".md") {
                assert!(
                    !copy.ends_with('.'),
                    "extensionless copies cannot gain a trailing dot"
                );
            }
        }
    }

    #[test]
    fn code_blocks_and_tables_keep_one_alignment() {
        let source = "שלום\n```\n# הערה\n```\n| שם | Name |\n|---|---|\nטקסט\n~~~~\nopen";
        assert_eq!(
            super::direction_exempt_lines(source),
            [1..6, 7..9],
            "adjacent ranges merge"
        );
        assert!(
            super::direction_exempt_lines("a\n    ```\nb").is_empty(),
            "indented: not a fence"
        );
        assert_eq!(
            super::direction_exempt_lines("```\nx\n`````\ny"),
            vec![std::ops::Range { start: 0, end: 3 }],
            "longer close fence"
        );
    }

    #[test]
    fn task_toggle_flips_only_the_box_byte() {
        let source = "- [ ] open\n* [x] done\n+\t[X] tab\n- [y] odd\n";
        assert_eq!(super::task_toggle(source, 0..5), Some((3..4, "x")));
        assert_eq!(super::task_toggle(source, 11..16), Some((14..15, " ")));
        assert_eq!(super::task_toggle(source, 22..27), Some((25..26, " ")));
        assert_eq!(super::task_toggle(source, 32..37), None, "not a task box");
        assert_eq!(super::task_toggle(source, 0..99), None, "stale range");
    }

    #[test]
    fn caret_task_toggle_finds_the_box_on_the_caret_line() {
        let source = "# Plan\n- [ ] one\n  1. [x] two\n> 3) [ ] quoted\nplain line\n- [ ]tight\n";
        let at = |needle: &str| source.find(needle).unwrap();
        // Anywhere on the line, including inside the task text.
        assert_eq!(
            super::caret_task_toggle(source, at("one") + 2),
            Some((10..11, "x"))
        );
        assert_eq!(
            super::caret_task_toggle(source, at("- [ ] one")),
            Some((10..11, "x"))
        );
        // Ordered and indented, and inside a quote.
        let two = at("[x] two") + 1;
        assert_eq!(
            super::caret_task_toggle(source, at("two")),
            Some((two..two + 1, " "))
        );
        let quoted = at("[ ] quoted") + 1;
        assert_eq!(
            super::caret_task_toggle(source, at("quoted")),
            Some((quoted..quoted + 1, "x"))
        );
        // Positive controls: not a task line.
        assert_eq!(super::caret_task_toggle(source, at("plain")), None);
        assert_eq!(super::caret_task_toggle(source, at("# Plan")), None);
        assert_eq!(
            super::caret_task_toggle(source, source.len()),
            None,
            "end of text"
        );
    }

    /// #1051: «Toggle task» from the keyboard flips the caret's box as one
    /// undo step, in an ordered list too.
    #[gpui::test]
    fn toggle_task_shortcut_flips_the_caret_line_in_one_undo_step(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        let state = dir.path().join("state");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        let original = "# Tasks\n\n1. [ ] first\n2. [ ] second\n";
        std::fs::write(root.join("note.md"), original).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            reader_ui_state::install(&state, cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("note.md")),
                        session_directory: Some(state.clone()),
                        index_dir: Some(dir.path().join("index")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        let value = |visual: &mut gpui::VisualTestContext| {
            reader.read_with(visual, |r, cx| {
                r.source_input().unwrap().read(cx).value().to_string()
            })
        };
        assert_eq!(
            value(visual),
            original,
            "positive control: editing the note"
        );
        reader.update_in(visual, |r, window, cx| {
            let input = r.source_input().unwrap();
            input.update(cx, |input, cx| {
                input.focus_handle(cx).focus(window, cx);
                input.set_cursor_position(
                    gpui_component::input::Position {
                        line: 3,
                        character: 9,
                    },
                    window,
                    cx,
                );
            });
        });
        let key = "secondary-l";
        visual.simulate_keystrokes(key);
        visual.run_until_parked();
        assert_eq!(value(visual), "# Tasks\n\n1. [ ] first\n2. [x] second\n");
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-z"
        } else {
            "ctrl-z"
        });
        visual.run_until_parked();
        assert_eq!(value(visual), original, "one undo step restores it");
        // The caret on a non-task line changes nothing.
        reader.update_in(visual, |r, window, cx| {
            r.source_input().unwrap().update(cx, |input, cx| {
                input.set_cursor_position(
                    gpui_component::input::Position {
                        line: 0,
                        character: 2,
                    },
                    window,
                    cx,
                )
            });
        });
        visual.simulate_keystrokes(key);
        visual.run_until_parked();
        assert_eq!(value(visual), original);
    }

    /// #873: «Open in ▸» never hands an editor a stale file — a dirty draft
    /// is saved first — and passes the caret line; other notes are left alone.
    #[gpui::test]
    fn open_in_saves_the_dirty_draft_and_reports_the_caret_line(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        let state = dir.path().join("state");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        let original = "# Note\n\nFirst line.\nSecond line.";
        std::fs::write(root.join("note.md"), original).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            reader_ui_state::install(&state, cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("note.md")),
                        session_directory: Some(state.clone()),
                        index_dir: Some(dir.path().join("index")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        let root_dir = reader.read_with(visual, |r, _| r.vault_root.clone());
        // Reader mode: nothing to save, no line.
        let line = reader.update(visual, |r, cx| r.prepare_open_in(&root_dir, "note.md", cx));
        assert_eq!(line.unwrap(), None);
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            let input = r.editing.as_ref().unwrap().input().clone();
            input.update(cx, |input, cx| input.focus_handle(cx).focus(window, cx));
        });
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-down"
        } else {
            "ctrl-end"
        });
        visual.simulate_input(" Unsaved open-in sentinel");
        visual.run_until_parked();
        let path = root.join("note.md");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "positive control: the edit is still only in the draft"
        );
        let line = reader.update(visual, |r, cx| r.prepare_open_in(&root_dir, "note.md", cx));
        assert_eq!(line.unwrap(), Some(4), "caret on the last line");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .ends_with("Second line. Unsaved open-in sentinel"),
            "saved before the editor gets the file"
        );
        // Another note: this Reader has nothing to say about it.
        let other = reader.update(visual, |r, cx| r.prepare_open_in(&root_dir, "other.md", cx));
        assert_eq!(other.unwrap(), None);
    }

    /// #916: the shortcut goes straight to Live Preview from Reader (entering
    /// editing) and from Source; pressed again it stays in Live Preview.
    #[gpui::test]
    fn live_preview_shortcut_from_reader_and_source(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        let state = dir.path().join("state");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(root.join("note.md"), "# Note\n\nSome **bold** text.\n").unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            reader_ui_state::install(&state, cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("note.md")),
                        session_directory: Some(state.clone()),
                        index_dir: Some(dir.path().join("index")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        let keys = if cfg!(target_os = "macos") {
            "cmd-shift-e"
        } else {
            "ctrl-shift-e"
        };
        let mode = |visual: &mut VisualTestContext| {
            reader.read_with(visual, |r, _| {
                r.editing.as_ref().map(|e| e.live_preview.enabled)
            })
        };
        assert_eq!(mode(visual), None, "positive control: starts in Reader");
        reader.update_in(visual, |r, window, cx| r.focus_handle.focus(window, cx));
        visual.simulate_keystrokes(keys);
        visual.run_until_parked();
        assert_eq!(mode(visual), Some(true), "Reader -> Live Preview");
        reader.update_in(visual, |r, window, cx| {
            r.set_live_preview(false, window, cx)
        });
        visual.run_until_parked();
        assert_eq!(mode(visual), Some(false), "positive control: in Source");
        visual.simulate_keystrokes(keys);
        visual.run_until_parked();
        assert_eq!(mode(visual), Some(true), "Source -> Live Preview");
        visual.simulate_keystrokes(keys);
        visual.run_until_parked();
        assert_eq!(mode(visual), Some(true), "not a toggle");
    }

    #[gpui::test]
    fn find_menu_action_matches_case_insensitively_in_preview_and_source(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        let state = dir.path().join("state");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        let text = "# Example\n\nOkilum okilum OKILUM\n\nЗаметка заметка ЗАМЕТКА\n";
        std::fs::write(root.join("note.md"), text).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            reader_ui_state::install(&state, cx);
            assert!(!reader_ui_state::find_case_sensitive(cx));
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("note.md")),
                        session_directory: Some(state.clone()),
                        index_dir: Some(dir.path().join("index")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        for source in [false, true] {
            reader.update_in(visual, |r, window, cx| {
                if source {
                    r.toggle_source(window, cx);
                }
                r.focus_handle.focus(window, cx);
                window.dispatch_action(Box::new(FindInNote), cx);
            });
            visual.run_until_parked();
            reader.read_with(visual, |r, cx| {
                if source {
                    assert!(
                        r.editing
                            .as_ref()
                            .unwrap()
                            .input
                            .read(cx)
                            .search_session()
                            .open
                    );
                } else {
                    assert!(r.find_open);
                }
            });
            visual.simulate_input("okilum");
            visual.run_until_parked();
            reader.update_in(visual, |r, window, cx| {
                if source {
                    let input = r.editing.as_ref().unwrap().input.clone();
                    assert_eq!(input.read(cx).search_session().matcher.len(), 3);
                    input.update(cx, |s, cx| s.set_search_query("заметка", true, cx));
                    assert_eq!(input.read(cx).search_session().matcher.len(), 3);
                    input.update(cx, |s, cx| s.set_search_query("заметка", false, cx));
                    assert_eq!(input.read(cx).search_session().matcher.len(), 1);
                    assert_eq!(input.read(cx).value().as_ref(), text);
                } else {
                    assert_eq!(r.content.read(cx).search_status().1, 3);
                    r.find_input
                        .update(cx, |s, cx| s.set_value("заметка", window, cx));
                    r.run_find(window, cx);
                    assert_eq!(r.content.read(cx).search_status().1, 3);
                    reader_ui_state::set_find_case_sensitive(true, cx);
                    r.run_find(window, cx);
                    assert_eq!(r.content.read(cx).search_status().1, 1);
                    reader_ui_state::set_find_case_sensitive(false, cx);
                }
            });
            visual.run_until_parked();
        }
        reader.read_with(visual, |_, cx| {
            assert!(reader_ui_state::find_case_sensitive(cx))
        });
        assert_eq!(std::fs::read_to_string(root.join("note.md")).unwrap(), text);
    }

    #[gpui::test]
    fn initial_source_highlighting_prepares_without_a_typing_debounce(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let mut input = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut state = EditorState::new(window, cx)
                    .language("markdown")
                    .folding(false);
                // Force the asynchronous path independently of machine speed.
                state.set_value(
                    format!("# Heading\n\n{}", "plain text ".repeat(30_000)),
                    window,
                    cx,
                );
                state.ensure_highlighter_factory(
                    gpui_component::highlighter::input_highlighter_factory(),
                );
                state.prepare_highlighting(window, cx);
                assert!(
                    state.highlighting_pending(),
                    "positive control: initial parse is running"
                );
                state
            });
            input = Some(view.clone());
            Root::new(view, window, cx)
        });
        // No virtual clock advance: initial parsing must not wait for the
        // 150ms typing debounce before the first highlighted presentation.
        visual.run_until_parked();
        input.unwrap().read_with(visual, |input, _| {
            assert!(
                !input.highlighting_pending(),
                "initial syntax must settle without a debounce timer"
            );
        });
    }

    #[gpui::test]
    fn recovery_write_failure_keeps_draft_and_reports_the_recovery_copy(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let state = temp.path().join("state");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("note.md"), "original").unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("note.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(state.clone()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            r.toggle_source(window, cx);
            r.editing
                .as_ref()
                .unwrap()
                .set_value("first draft", window, cx);
        });
        visual.run_until_parked();
        visual
            .executor()
            .advance_clock(RECOVERY_PAUSE + std::time::Duration::from_millis(1));
        visual.run_until_parked();
        let journal = std::fs::read_dir(state.join("editor-drafts"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "json"))
            .unwrap();
        // A directory at the journal destination causes a real write failure
        // on either platform without relying on test-user permission privileges.
        std::fs::remove_file(&journal).unwrap();
        std::fs::create_dir(&journal).unwrap();
        reader.update_in(visual, |r, window, cx| {
            r.editing
                .as_ref()
                .unwrap()
                .set_value("latest draft", window, cx);
        });
        visual.run_until_parked();
        visual
            .executor()
            .advance_clock(RECOVERY_PAUSE + std::time::Duration::from_millis(1));
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert_eq!(
                r.link_notice.as_ref().unwrap().to_string(),
                DRAFT_WRITE_NOTICE
            );
        });
        visual.update(|_, cx| assert!(!save_all_for_quit(cx)));
        reader.read_with(visual, |r, cx| {
            assert_eq!(
                r.link_notice.as_ref().unwrap().to_string(),
                DRAFT_WRITE_NOTICE
            );
            assert_eq!(
                r.editing.as_ref().unwrap().input.read(cx).value().as_ref(),
                "latest draft"
            );
        });
        assert_eq!(
            std::fs::read_to_string(root.join("note.md")).unwrap(),
            "original"
        );
        std::fs::remove_dir(&journal).unwrap();
        visual.update(|_, cx| assert!(save_all_for_quit(cx)));
        assert_eq!(
            std::fs::read_to_string(root.join("note.md")).unwrap(),
            "latest draft"
        );
    }

    #[gpui::test]
    fn update_restart_refuses_unprotected_single_file_then_preserves_latest_draft(
        cx: &mut TestAppContext,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let state = temp.path().join("state");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("note.md"), "original").unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            reader_open::install(cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        single_file: true,
                        note: Some("note.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(state.clone()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            r.toggle_source(window, cx);
            r.editing
                .as_ref()
                .unwrap()
                .set_value("first draft", window, cx);
        });
        visual.run_until_parked();
        visual
            .executor()
            .advance_clock(RECOVERY_PAUSE + std::time::Duration::from_millis(1));
        visual.run_until_parked();
        let journal = std::fs::read_dir(state.join("editor-drafts"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "json"))
            .unwrap();
        // A directory at the journal destination causes a real write failure
        // on either platform without relying on test-user permission privileges.
        std::fs::remove_file(&journal).unwrap();
        std::fs::create_dir(&journal).unwrap();
        reader.update_in(visual, |r, window, cx| {
            r.editing
                .as_ref()
                .unwrap()
                .set_value("latest draft", window, cx);
        });
        visual.run_until_parked();
        visual
            .executor()
            .advance_clock(RECOVERY_PAUSE + std::time::Duration::from_millis(1));
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert_eq!(
                r.link_notice.as_ref().unwrap().to_string(),
                DRAFT_WRITE_NOTICE
            );
        });
        visual.update(|_, cx| assert!(crate::reader_update_restart::prepare(cx).is_err()));
        reader.read_with(visual, |r, cx| {
            assert_eq!(
                r.link_notice.as_ref().unwrap().to_string(),
                DRAFT_WRITE_NOTICE
            );
            assert_eq!(
                r.editing.as_ref().unwrap().input.read(cx).value().as_ref(),
                "latest draft"
            );
        });
        assert_eq!(
            std::fs::read_to_string(root.join("note.md")).unwrap(),
            "original"
        );
        std::fs::remove_dir(&journal).unwrap();
        visual.update(|_, cx| {
            let args = crate::reader_update_restart::prepare(cx).unwrap();
            let reader_open::Command::Launch(opts) =
                reader_open::parse_args(args, Opts::default()).unwrap()
            else {
                panic!("restart must launch the reader");
            };
            assert_eq!(
                opts.restart_windows,
                vec![crate::reader_update_restart::Window::File {
                    path: root.join("note.md")
                }]
            );
        });
        assert_eq!(
            std::fs::read_to_string(root.join("note.md")).unwrap(),
            "latest draft"
        );
    }

    #[gpui::test]
    fn local_writer_review_clean_navigation_and_protected_close_during_hold(
        cx: &mut TestAppContext,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let state = temp.path().join("state");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), "original").unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            reader_recovery::install(&state, None, cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("note.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(state.clone()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx);
            assert!(reader.editing.is_some());
        });
        let authority = okilum_write_authority::Authority::for_root(&root).unwrap();
        let hold = authority.begin_drain("reader-clean-close").unwrap();
        reader.update_in(visual, |reader, window, cx| {
            assert!(reader.save_source_for_navigation(cx));
            assert!(reader.save_source_for_close(cx));
            assert!(reader.leave_source(cx));
            assert!(reader.editing.is_none());
            reader.toggle_source(window, cx);
            assert!(reader.editing.is_some());
            reader
                .editing
                .as_ref()
                .unwrap()
                .set_value("held draft", window, cx);
            assert!(!reader.save_source(cx));
            assert!(reader.save_source_for_close(cx));
        });
        visual.update(|_, cx| assert!(save_all_for_quit(cx)));
        assert_eq!(
            std::fs::read_to_string(root.join("note.md")).unwrap(),
            "original"
        );
        assert!(
            FileEditor::has_unsaved_draft(&root.join("note.md"), &state.join("editor-drafts"))
                .unwrap()
        );
        let token = hold.token().clone();
        hold.release(&token).unwrap();
        reader.update_in(visual, |reader, _, cx| assert!(reader.save_source(cx)));
        assert_eq!(
            std::fs::read_to_string(root.join("note.md")).unwrap(),
            "held draft"
        );
        cx.update(|cx| cx.shutdown());
    }

    #[gpui::test]
    fn local_writer_admission_two_reader_windows_retain_drafts_and_unrelated_vault_saves(
        cx: &mut TestAppContext,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let other = temp.path().join("other");
        let state = temp.path().join("state");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&other).unwrap();
        for (vault, name) in [
            (&root, "first.md"),
            (&root, "second.md"),
            (&other, "other.md"),
        ] {
            std::fs::write(vault.join(name), "original").unwrap();
        }
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            reader_recovery::install(&state, None, cx);
        });
        let mut readers = vec![];
        for (index, (vault, name)) in [
            (&root, "first.md"),
            (&root, "second.md"),
            (&other, "other.md"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut reader = None;
            let (_, visual) = cx.add_window_view(|window, cx| {
                let view = cx.new(|cx| {
                    Reader::new(
                        Opts {
                            vault: Some(vault.clone()),
                            note: Some(name.into()),
                            index_dir: Some(temp.path().join(format!("index-{index}"))),
                            session_directory: Some(state.clone()),
                            ..Default::default()
                        },
                        window,
                        cx,
                    )
                });
                reader = Some(view.clone());
                Root::new(view, window, cx)
            });
            let reader = reader.unwrap();
            visual.run_until_parked();
            reader.update_in(visual, |reader, window, cx| {
                reader.toggle_source(window, cx);
                assert!(
                    reader.editing.is_some(),
                    "real Source editor positive control"
                );
                reader
                    .editing
                    .as_ref()
                    .unwrap()
                    .set_value("window draft", window, cx);
            });
            readers.push(reader);
        }
        let authority = okilum_write_authority::Authority::for_root(&root).unwrap();
        let mut hold = authority.begin_drain("reader-windows").unwrap();
        let token = hold.token().clone();
        assert!(hold.try_hold(&token).unwrap());
        cx.update(|cx| assert!(!save_all(cx)));
        for name in ["first.md", "second.md"] {
            assert_eq!(
                std::fs::read_to_string(root.join(name)).unwrap(),
                "original"
            );
            assert!(
                FileEditor::has_unsaved_draft(&root.join(name), &state.join("editor-drafts"))
                    .unwrap()
            );
        }
        assert_eq!(
            std::fs::read_to_string(other.join("other.md")).unwrap(),
            "window draft"
        );
        for reader in &readers[..2] {
            reader.read_with(cx, |reader, cx| {
                assert!(reader.source_is_dirty(cx));
                assert_eq!(
                    reader
                        .editing
                        .as_ref()
                        .unwrap()
                        .input
                        .read(cx)
                        .value()
                        .as_ref(),
                    "window draft"
                );
            });
        }
        drop(hold); // Losing the caller must not reopen publication.
        let observed = authority.snapshot().unwrap();
        assert_eq!(
            observed.phase,
            okilum_write_authority::Phase::RecoveryPending
        );
        cx.update(|cx| assert!(!save_all(cx)));
        let hold = authority.recover(&observed).unwrap();
        let token = hold.token().clone();
        hold.release(&token).unwrap();
        cx.update(|cx| assert!(save_all(cx)));
        for name in ["first.md", "second.md"] {
            assert_eq!(
                std::fs::read_to_string(root.join(name)).unwrap(),
                "window draft"
            );
        }
        cx.update(|cx| cx.shutdown());
        drop(readers);
    }

    #[gpui::test]
    fn quit_flushes_every_editor_even_after_conflicts_and_save_errors(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let state = temp.path().join("state");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            reader_recovery::install(&state, None, cx);
        });
        let mut readers = vec![];
        for name in ["first.md", "second.md"] {
            std::fs::write(root.join(name), "original").unwrap();
            let mut reader = None;
            let (_, visual) = cx.add_window_view(|window, cx| {
                let view = cx.new(|cx| {
                    Reader::new(
                        Opts {
                            vault: Some(root.clone()),
                            open_path: Some(root.join(name)),
                            index_dir: Some(temp.path().join("index")),
                            session_directory: Some(state.clone()),
                            ..Default::default()
                        },
                        window,
                        cx,
                    )
                });
                reader = Some(view.clone());
                Root::new(view, window, cx)
            });
            let reader = reader.unwrap();
            visual.run_until_parked();
            reader.update_in(visual, |r, window, cx| {
                r.toggle_source(window, cx);
                r.editing
                    .as_ref()
                    .unwrap()
                    .set_value("local edits", window, cx);
            });
            readers.push(reader);
        }
        std::fs::write(root.join("first.md"), "external changes").unwrap();
        cx.update(|cx| assert!(!save_all(cx)));
        cx.update(|cx| assert!(!save_all_for_quit(cx)));
        assert_eq!(
            std::fs::read_to_string(root.join("first.md")).unwrap(),
            "external changes"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("second.md")).unwrap(),
            "local edits"
        );
        // A directory in place of the source produces a real access/type error
        // on both Unix and Windows, independently of elevated test privileges.
        std::fs::remove_file(root.join("first.md")).unwrap();
        std::fs::create_dir(root.join("first.md")).unwrap();
        cx.update(|cx| assert!(save_all_for_quit(cx)));
        assert!(readers[0].read_with(cx, |reader, _| reader.editing.as_ref().unwrap().save_failed));
        cx.update(|cx| cx.shutdown());
        std::fs::remove_dir(root.join("first.md")).unwrap();
        std::fs::write(root.join("first.md"), "external changes").unwrap();
        assert_eq!(
            std::fs::read_dir(state.join("reader-runs"))
                .unwrap()
                .count(),
            0
        );
        assert!(FileEditor::has_unsaved_draft(
            &root.join("first.md"),
            &state.join("editor-drafts")
        )
        .unwrap());
        drop(readers);
    }

    #[cfg(target_os = "macos")]
    #[gpui::test]
    fn update_install_post_verification_protects_latest_edits_and_refuses_failure(
        cx: &mut TestAppContext,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let state = temp.path().join("state");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("note.md"), "before check").unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            reader_open::install(cx);
            cx.set_global(reader_history::TestSessionDirectory(state.clone()));
            reader_recovery::install(&state, None, cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        single_file: true,
                        open_path: Some(root.join("note.md")),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(state.clone()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        // An earlier menu-Quit save cannot stand in for this final guard.
        visual.update(|_, cx| assert!(save_all(cx)));
        reader.update_in(visual, |r, window, cx| {
            r.editing
                .as_ref()
                .unwrap()
                .set_value("after verification", window, cx);
        });
        let committed = std::cell::Cell::new(false);
        let refused = std::cell::Cell::new(false);
        visual.update(|_, cx| {
            crate::updater::protect_verified_install(
                cx,
                true,
                || {
                    assert_eq!(
                        std::fs::read_to_string(root.join("note.md")).unwrap(),
                        "after verification"
                    );
                    committed.set(true);
                    Ok(())
                },
                || refused.set(true),
            )
            .unwrap();
        });
        assert_eq!(
            crate::reader_update_restart::take(&state).unwrap(),
            vec![crate::reader_update_restart::Window::File {
                path: root.join("note.md")
            }]
        );
        assert!(committed.get());
        assert!(!refused.get());
        visual.update(|_, cx| {
            crate::updater::protect_verified_install(
                cx,
                false,
                || Ok(()),
                || panic!("safe ordinary Quit"),
            )
            .unwrap();
        });
        assert!(
            crate::reader_update_restart::take(&state)
                .unwrap()
                .is_empty(),
            "ordinary Quit creates no relaunch handoff"
        );
        let drafts = state.join("editor-drafts");
        let backup = state.join("drafts-backup");
        std::fs::rename(&drafts, &backup).unwrap();
        std::fs::write(&drafts, "not a directory").unwrap();
        reader.update_in(visual, |r, window, cx| {
            r.editing
                .as_ref()
                .unwrap()
                .set_value("unprotected newer edit", window, cx);
        });
        committed.set(false);
        visual.update(|_, cx| {
            assert!(crate::updater::protect_verified_install(
                cx,
                true,
                || {
                    committed.set(true);
                    Ok(())
                },
                || refused.set(true),
            )
            .is_err());
        });
        assert!(!committed.get());
        assert!(refused.get());
        assert_eq!(
            std::fs::read_to_string(root.join("note.md")).unwrap(),
            "after verification"
        );
        std::fs::remove_file(&drafts).unwrap();
        std::fs::rename(backup, drafts).unwrap();
    }

    #[gpui::test]
    fn quit_retains_marker_when_latest_draft_cannot_be_protected(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let state = temp.path().join("state");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            reader_recovery::install(&state, None, cx);
        });
        let mut readers = vec![];
        {
            let name = "first.md";
            std::fs::write(root.join(name), "original").unwrap();
            let mut reader = None;
            let (_, visual) = cx.add_window_view(|window, cx| {
                let view = cx.new(|cx| {
                    Reader::new(
                        Opts {
                            vault: Some(root.clone()),
                            open_path: Some(root.join(name)),
                            index_dir: Some(temp.path().join("index")),
                            session_directory: Some(state.clone()),
                            ..Default::default()
                        },
                        window,
                        cx,
                    )
                });
                reader = Some(view.clone());
                Root::new(view, window, cx)
            });
            let reader = reader.unwrap();
            visual.run_until_parked();
            reader.update_in(visual, |r, window, cx| {
                r.toggle_source(window, cx);
                r.editing
                    .as_ref()
                    .unwrap()
                    .set_value("local edits", window, cx);
            });
            readers.push(reader);
        }
        let drafts = state.join("editor-drafts");
        let backup = state.join("drafts-backup");
        std::fs::rename(&drafts, &backup).unwrap();
        std::fs::write(&drafts, "injected non-directory").unwrap();
        cx.update(|cx| assert!(!save_all_for_quit(cx)));
        cx.update(|cx| cx.shutdown());
        assert_eq!(
            std::fs::read_dir(state.join("reader-runs"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            std::fs::read_to_string(root.join("first.md")).unwrap(),
            "original"
        );
        std::fs::remove_file(&drafts).unwrap();
        std::fs::rename(backup, drafts).unwrap();
        drop(readers);
    }

    #[gpui::test]
    fn native_history_restore_preserves_source_and_refuses_dirty_or_stale_input(
        cx: &mut TestAppContext,
    ) {
        use gpui_component::WindowExt;
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let directory =
            std::env::temp_dir().join(format!("okilum-history-{}", uuid::Uuid::new_v4()));
        let root = directory.join("notes");
        std::fs::create_dir_all(&root).unwrap();
        let original = "\u{feff}---\r\ntitle: Привет e\u{301}\r\n---\r\n[[Заметка 🧠]]\r\n";
        std::fs::write(root.join("note.md"), original).unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("note.md")),
                        index_dir: Some(directory.join("index")),
                        session_directory: Some(directory.join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            r.toggle_source(window, cx);
            let input = r.editing.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| {
                input.replace_text_in_range(Some(0..0), "edit", window, cx)
            });
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert!(r
                .restore_source_version(original, "bad", window, cx)
                .is_err());
            assert!(r.save_source(cx));
        });
        // Typed at byte zero: after the BOM (#1093).
        let edited = original.replacen('\u{feff}', "\u{feff}edit", 1);
        assert_ne!(edited, original);
        reader.update_in(visual, |r, window, cx| {
            r.restore_source_version(&edited, original, window, cx)
                .unwrap();
            assert_eq!(
                r.editing.as_ref().unwrap().input.read(cx).value().as_ref(),
                original
            );
            r.source_history(false, window, cx);
            assert!(!window.has_active_dialog(cx));
            assert!(r.active_timeline().is_some());
        });
        visual.run_until_parked();
        visual.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.close_dialog(cx);
        });
        assert_eq!(
            std::fs::read(root.join("note.md")).unwrap(),
            original.as_bytes()
        );
        let versions =
            okilum_core::source_history::list(&directory.join("state/editor-drafts"), &root)
                .unwrap();
        assert!(versions.versions.iter().any(|v| v.text == edited));
        std::fs::write(root.join("note.md"), "external").unwrap();
        reader.update_in(visual, |r, window, cx| {
            assert!(r
                .restore_source_version(original, &edited, window, cx)
                .is_err());
        });
        assert_eq!(
            std::fs::read_to_string(root.join("note.md")).unwrap(),
            "external"
        );
        std::fs::remove_file(root.join("note.md")).unwrap();
        reader.update_in(visual, |r, window, cx| {
            r.source_history(false, window, cx);
            assert!(
                r.active_timeline().is_some(),
                "missing current file must not hide its retained history"
            );
            assert!(!window.has_active_dialog(cx));
        });
        visual.run_until_parked();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn document_end_space_resizes_in_reader_and_editor(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!("okilum-reader419-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let source = (0..90)
            .map(|i| format!("Paragraph {i}\n\n"))
            .collect::<String>()
            + "LAST LINE";
        std::fs::write(root.join("long.md"), &source).unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("long.md".into()),
                        session_directory: Some(root.join("state")),
                        ..Opts::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        // Establish the no-notification baseline independently of fixture startup notices.
        visual.update(|window, cx| {
            use gpui_component::WindowExt;
            window.clear_notifications(cx);
        });
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        visual.update(|window, cx| {
            use gpui_component::WindowExt;
            assert!(window.notifications(cx).is_empty());
        });
        for height in [700., 1000.] {
            visual.simulate_resize(size(px(1500.), px(height)));
            visual.run_until_parked();
            reader.update_in(visual, |v, _, cx| {
                v.content.read(cx).list_state().scroll_to_end();
                cx.notify();
            });
            visual.run_until_parked();
            visual.update(|w, cx| w.draw(cx).clear(cx));
            reader.read_with(visual, |v, cx| {
                let list = v.content.read(cx).list_state();
                assert!(
                    list.logical_scroll_top().item_ix > 0,
                    "document actually scrolled"
                );
                let last = list.bounds_for_item(list.item_count() - 1).unwrap();
                let gap = list.viewport_bounds().bottom() - last.bottom();
                assert!(
                    (gap - reader_bottom_space(px(height))).abs() < px(2.),
                    "Reader gap {gap:?}"
                );
            });
        }
        visual.update(|window, cx| {
            reader_toast::push(
                gpui_component::notification::Notification::new().content(|_, _, _| {
                    div()
                        .debug_selector(|| "end-space-toast".into())
                        .h(px(180.))
                        .child("Operation result")
                        .into_any_element()
                }),
                None,
                window,
                cx,
            );
        });
        visual.run_until_parked();
        reader.update_in(visual, |v, _, cx| {
            v.content.read(cx).list_state().scroll_to_end();
            cx.notify();
        });
        visual.run_until_parked();
        let toast = visual.debug_bounds("end-space-toast").unwrap();
        reader.read_with(visual, |v, cx| {
            let list = v.content.read(cx).list_state();
            let last = list.bounds_for_item(list.item_count() - 1).unwrap();
            assert!(
                last.bottom() < toast.top(),
                "last Reader line scrolls above the overlay"
            );
            assert!(
                last.top() >= list.viewport_bounds().top(),
                "last line remains in view"
            );
        });
        reader.update_in(visual, |v, window, cx| v.toggle_source(window, cx));
        visual.run_until_parked();
        for height in [700., 1000.] {
            visual.simulate_resize(size(px(1500.), px(height)));
            visual.run_until_parked();
            let input = reader.read_with(visual, |v, _| v.editing.as_ref().unwrap().input.clone());
            let bounds = input.read_with(visual, |v, _| v.input_bounds());
            visual.simulate_event(ScrollWheelEvent {
                position: bounds.center(),
                delta: ScrollDelta::Pixels(point(px(0.), px(-100000.))),
                ..Default::default()
            });
            visual.run_until_parked();
            visual.update(|w, cx| w.draw(cx).clear(cx));
            input.read_with(visual, |v, _| {
                assert!(v.scroll_offset().y < px(-100.), "editor actually scrolled");
                let last = v
                    .range_to_bounds(&(source.len() - 9..source.len()))
                    .unwrap();
                let gap = v.input_bounds().bottom() - last.bottom();
                let expected = reader_bottom_space(px(height)).max(px(height) * 0.65);
                assert!(
                    gap >= expected - px(2.) && gap < expected + px(25.),
                    "Editor gap {gap:?}, expected {expected:?}"
                );
                assert_eq!(
                    v.value().to_string(),
                    source,
                    "spacing does not insert source text"
                );
            });
        }
        assert_eq!(
            std::fs::read_to_string(root.join("long.md")).unwrap(),
            source
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// #1296: Esc from the editor kept the note but opened the Reader at the
    /// top, and entering the editor did the same. Both keep the place now, in
    /// Source and Live Preview, for a short note and a 24k-line one.
    #[gpui::test]
    fn leaving_and_entering_the_editor_keep_the_place(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!("okilum-place1296-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let short: String = (0..40).map(|i| format!("Paragraph {i}\n\n")).collect();
        let long: String = (0..12_000).map(|i| format!("Line {i}\n\n")).collect();
        std::fs::write(root.join("short.md"), &short).unwrap();
        std::fs::write(root.join("long.md"), &long).unwrap();
        for (note, block) in [("short.md", 20), ("long.md", 6_000)] {
            for live_preview in [false, true] {
                let mut reader = None;
                let (_, visual) = cx.add_window_view(|window, cx| {
                    let view = cx.new(|cx| {
                        Reader::new(
                            Opts {
                                vault: Some(root.clone()),
                                note: Some(note.into()),
                                session_directory: Some(root.join("state")),
                                ..Opts::default()
                            },
                            window,
                            cx,
                        )
                    });
                    reader = Some(view.clone());
                    Root::new(view, window, cx)
                });
                let reader = reader.unwrap();
                visual.simulate_resize(size(px(1200.), px(700.)));
                visual.run_until_parked();
                let top = |visual: &mut VisualTestContext| {
                    reader.read_with(visual, |v, cx| {
                        v.content.read(cx).list_state().logical_scroll_top().item_ix
                    })
                };
                // Reader at block `block`: its paragraph starts at file line 2*block+1.
                reader.update_in(visual, |v, _, cx| {
                    v.content.read(cx).list_state().scroll_to(gpui::ListOffset {
                        item_ix: block,
                        offset_in_item: px(0.),
                    });
                });
                visual.run_until_parked();
                visual.update(|w, cx| w.draw(cx).clear(cx));
                assert_eq!(top(visual), block, "positive control: the Reader scrolled");
                reader.update_in(visual, |v, window, cx| {
                    v.toggle_source(window, cx);
                    v.set_live_preview(live_preview, window, cx);
                });
                visual.run_until_parked();
                visual.update(|w, cx| w.draw(cx).clear(cx));
                let input =
                    reader.read_with(visual, |v, _| v.editing.as_ref().unwrap().input.clone());
                let line = input
                    .read_with(visual, |v, _| v.top_visible_line())
                    .unwrap();
                assert_eq!(
                    line,
                    2 * block,
                    "{note} live={live_preview}: editor opens at the place"
                );
                // Move the editor further down, then leave it with Esc.
                input.update(visual, |v, cx| v.scroll_line_to_top(2 * (block + 5), cx));
                visual.run_until_parked();
                visual.update(|w, cx| w.draw(cx).clear(cx));
                // Esc ends in Dismiss → toggle_source once nothing else is
                // open; a toast in the test window would take the key, so
                // leave the way Esc does. The native check sends the key.
                reader.update_in(visual, |v, window, cx| v.toggle_source(window, cx));
                // The landing polls on timers until the list has laid out.
                for _ in 0..20 {
                    visual
                        .executor()
                        .advance_clock(std::time::Duration::from_millis(60));
                    visual.run_until_parked();
                    visual.update(|w, cx| w.draw(cx).clear(cx));
                }
                assert!(
                    reader.read_with(visual, |v, _| v.editing.is_none()),
                    "the editor was left"
                );
                assert_eq!(
                    top(visual),
                    block + 5,
                    "{note} live={live_preview}: Reader opens at the place"
                );
                // Entering again (Undo history retained, #1095): the caret is at
                // the place too, not at the last session's selection.
                reader.update_in(visual, |v, window, cx| {
                    v.toggle_source(window, cx);
                    v.set_live_preview(live_preview, window, cx);
                });
                visual.run_until_parked();
                visual.update(|w, cx| w.draw(cx).clear(cx));
                let input =
                    reader.read_with(visual, |v, _| v.editing.as_ref().unwrap().input.clone());
                let caret_line =
                    input.read_with(visual, |v, _| v.value()[..v.cursor()].matches('\n').count());
                assert_eq!(
                    caret_line,
                    2 * (block + 5),
                    "{note} live={live_preview}: the caret starts at the place"
                );
                // Leave, so the next window can edit the same note.
                reader.update_in(visual, |v, window, cx| v.toggle_source(window, cx));
                visual.run_until_parked();
            }
        }
    }

    /// #945: Undo and Redo of an edit that is out of view scroll the editor
    /// back to the restored caret, in Source and in Live Preview.
    #[gpui::test]
    fn undo_and_redo_scroll_to_the_restored_caret(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!("okilum-undo945-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let source = (0..90)
            .map(|i| format!("Paragraph {i}\n\n"))
            .collect::<String>()
            + "LAST LINE";
        std::fs::write(root.join("long.md"), &source).unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("long.md".into()),
                        session_directory: Some(root.join("state")),
                        ..Opts::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.simulate_resize(size(px(1500.), px(700.)));
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| v.toggle_source(window, cx));
        visual.run_until_parked();
        let input = reader.read_with(visual, |v, _| v.editing.as_ref().unwrap().input.clone());
        let scroll_to_bottom = |visual: &mut VisualTestContext| {
            let bounds = input.read_with(visual, |v, _| v.input_bounds());
            visual.simulate_event(ScrollWheelEvent {
                position: bounds.center(),
                delta: ScrollDelta::Pixels(point(px(0.), px(-100000.))),
                ..Default::default()
            });
            visual.run_until_parked();
            visual.update(|w, cx| w.draw(cx).clear(cx));
            // Positive control: the edit is out of view before Undo/Redo.
            input.read_with(visual, |v, _| {
                assert!(v.scroll_offset().y < px(-500.), "editor actually scrolled");
            });
        };
        let edit_in_view = |visual: &mut VisualTestContext, what: &str| {
            visual.update(|w, cx| w.draw(cx).clear(cx));
            input.read_with(visual, |v, _| {
                let caret = v.range_to_bounds(&(0..1)).unwrap();
                let view = v.input_bounds();
                assert!(
                    caret.top() >= view.top() && caret.bottom() <= view.bottom(),
                    "{what} shows the restored caret: {caret:?} in {view:?}"
                );
            });
        };
        for live_preview in [false, true] {
            reader.update_in(visual, |v, window, cx| {
                v.set_live_preview(live_preview, window, cx)
            });
            visual.run_until_parked();
            visual.update(|window, cx| {
                input.update(cx, |input, cx| {
                    input.replace_text_in_range(Some(0..0), "edit ", window, cx)
                })
            });
            visual.run_until_parked();
            scroll_to_bottom(visual);
            #[cfg(target_os = "macos")]
            visual.simulate_keystrokes("cmd-z");
            #[cfg(not(target_os = "macos"))]
            visual.simulate_keystrokes("ctrl-z");
            visual.run_until_parked();
            assert!(input.read_with(visual, |v, _| v.value().starts_with("Paragraph 0")));
            edit_in_view(visual, "Undo");
            scroll_to_bottom(visual);
            #[cfg(target_os = "macos")]
            visual.simulate_keystrokes("cmd-shift-z");
            #[cfg(not(target_os = "macos"))]
            visual.simulate_keystrokes("ctrl-y");
            visual.run_until_parked();
            assert!(input.read_with(visual, |v, _| v.value().starts_with("edit Paragraph 0")));
            edit_in_view(visual, "Redo");
            // Leave the text as it was for the next mode.
            #[cfg(target_os = "macos")]
            visual.simulate_keystrokes("cmd-z");
            #[cfg(not(target_os = "macos"))]
            visual.simulate_keystrokes("ctrl-z");
            visual.run_until_parked();
        }
        let _ = std::fs::remove_dir_all(root);
    }

    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn links_use_bytes_of_current_unicode_source() {
        for source in [
            "Кириллица 🧠e\u{301} [[Заметка|ссылка]] хвост🦀",
            "🧠e\u{301} [ссылка](target.md) хвост",
        ] {
            let links = okilum_core::document_links::parse(source);
            assert_eq!(links.len(), 1);
            let link = &links[0];
            for (offset, _) in source.char_indices() {
                assert_eq!(
                    source_link_at(source, offset).is_some(),
                    link.range.contains(&offset)
                );
            }
            assert!(source.get(link.range.clone()).is_some());
            let changed = format!("новый 🧠 {source}");
            let offset = changed.find("ссылка").unwrap();
            assert_eq!(
                source_link_at(&changed, offset).unwrap().target,
                link.target
            );
        }
    }

    #[gpui::test]
    fn save_status_clean_refresh_and_dirty_conflict(cx: &mut TestAppContext) {
        use gpui_component::WindowExt;
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let directory =
            std::env::temp_dir().join(format!("okilum-save-state-{}", uuid::Uuid::new_v4()));
        let root = directory.join("notes");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("start.md");
        std::fs::write(&path, "original").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(path.clone()),
                        index_dir: Some(directory.join("index")),
                        session_directory: Some(directory.join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx)
        });
        visual.run_until_parked();
        let input = reader.read_with(visual, |r, _| {
            assert_eq!(r.editing.as_ref().unwrap().status(), "Saved");
            r.editing.as_ref().unwrap().input.clone()
        });
        input.update_in(visual, |input, window, cx| {
            input.replace_text_in_range(Some(0..0), "edited ", window, cx)
        });
        visual.run_until_parked();
        reader.update_in(visual, |reader, _, cx| {
            assert_eq!(reader.editing.as_ref().unwrap().status(), "Edited");
            reader.request_source_save(cx);
            assert_eq!(reader.editing.as_ref().unwrap().status(), "Saving…");
        });
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(40));
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.editing.as_ref().unwrap().status(), "Saved")
        });
        // A clean explicit Save also acknowledges the request and updates time.
        reader.update_in(visual, |reader, _, cx| {
            assert!(reader.save_source(cx));
            assert_eq!(reader.editing.as_ref().unwrap().status(), "Saved");
            assert!(reader.editing.as_ref().unwrap().saved_at.is_some());
        });
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "edited original");
        std::fs::write(&path, "external clean").unwrap();
        reader.update_in(visual, |reader, window, cx| {
            assert!(window.notifications(cx).is_empty());
            reader.refresh_source_from_disk(window, cx);
            assert_eq!(input.read(cx).value().as_ref(), "external clean");
            assert_eq!(
                window.notifications(cx).len(),
                1,
                "clean replacement must announce itself"
            );
            reader.refresh_source_from_disk(window, cx);
            assert_eq!(
                window.notifications(cx).len(),
                1,
                "unchanged/self-write refresh must be quiet"
            );
        });
        visual.run_until_parked();
        input.update_in(visual, |input, window, cx| {
            input.replace_text_in_range(Some(0..0), "unsaved ", window, cx)
        });
        visual.run_until_parked();
        std::fs::write(&path, "external conflict").unwrap();
        reader.update_in(visual, |reader, window, cx| {
            reader.refresh_source_from_disk(window, cx);
            assert_eq!(reader.editing.as_ref().unwrap().status(), "Conflict");
            assert_eq!(input.read(cx).value().as_ref(), "unsaved external clean");
            assert!(!reader.save_source(cx));
            let selected = reader.selected_file().to_owned();
            reader.close_note(window, cx);
            assert_eq!(reader.selected_file(), selected);
            assert_eq!(input.read(cx).value().as_ref(), "unsaved external clean");
        });
        reader.update_in(visual, |reader, _, _| {
            let editing = reader.editing.as_mut().unwrap();
            // A file can disappear between conflict detection and snapshot read.
            editing.conflict = None;
            assert_eq!(editing.status(), "Conflict");
        });
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "external conflict");
        std::fs::remove_file(&path).unwrap();
        reader.update_in(visual, |reader, window, cx| {
            reader.refresh_source_from_disk(window, cx);
            assert!(
                reader.deleted_current().is_some(),
                "persistent deletion notice replaces the generic error"
            );
            assert_eq!(input.read(cx).value().as_ref(), "unsaved external clean");
        });
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn unclean_startup_offers_draft_without_restoring_editor(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            cx.set_global(reader_recovery::RecoveryStartup(true));
        });
        let directory =
            std::env::temp_dir().join(format!("okilum-recovery-{}", uuid::Uuid::new_v4()));
        let root = directory.join("notes");
        let state = directory.join("state");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("start.md");
        std::fs::write(&path, "# Original disk note").unwrap();
        {
            let mut editor = FileEditor::open(&path, &state.join("editor-drafts")).unwrap();
            editor
                .set_text("# Несохранённый 🧠e\u{301} [[target]]".into())
                .unwrap();
        }
        let preferences = directory.join("reader-layout.json");
        let preferences_bytes = br#"{"notes":777,"backlinks":888}"#;
        std::fs::write(&preferences, preferences_bytes).unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(path.clone()),
                        index_dir: Some(directory.join("index")),
                        session_directory: Some(state.clone()),
                        panel_settings_override: Some(preferences.clone()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.recovery_startup);
            assert!(reader.recovery_offer);
            assert!(reader.editing.is_none());
            assert!(!reader.panels.notes && !reader.panels.backlinks);
            assert_eq!(
                reader.panel_widths.notes,
                reader_layout::Widths::default().notes
            );
            assert_eq!(std::fs::read(&preferences).unwrap(), preferences_bytes);
            assert_eq!(reader.current_rel, "start.md");
        });
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# Original disk note"
        );
        assert!(FileEditor::has_unsaved_draft(&path, &state.join("editor-drafts")).unwrap());
        // Dismissing an earlier banner cannot hide a subsequently discovered draft.
        reader.update_in(visual, |reader, _, cx| {
            reader.recovery_dismissed = true;
            reader.discover_source_recovery(cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.recovery_offer);
            assert!(!reader.recovery_dismissed);
        });
        // The Restore unsaved edits button invokes this explicit action.
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx)
        });
        visual.run_until_parked();
        reader.read_with(visual, |reader, cx| {
            assert_eq!(
                reader
                    .editing
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .value()
                    .as_ref(),
                "# Несохранённый 🧠e\u{301} [[target]]"
            );
        });
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "# Original disk note"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn unicode_native_click_places_cursor_and_command_click_navigates(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let directory =
            std::env::temp_dir().join(format!("okilum-unicode-click-{}", uuid::Uuid::new_v4()));
        let root = directory.join("notes");
        std::fs::create_dir_all(&root).unwrap();
        let original = "[[target|ссылка 🧠e\u{301}]] Кириллица хвост 🦀";
        std::fs::write(root.join("start.md"), original).unwrap();
        std::fs::write(root.join("target.md"), "# Цель 🧠e\u{301}").unwrap();
        std::fs::write(
            root.join("backlink.md"),
            format!("{}[[start|сс]] хвост 🧠e\u{301}", "я".repeat(79)),
        )
        .unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("start.md")),
                        index_dir: Some(directory.join("index")),
                        session_directory: Some(directory.join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |reader, _, cx| {
            let backlink = reader
                .backlinks
                .iter()
                .find(|b| b.path == "backlink.md")
                .expect("backlink inventory must be ready");
            let (context, range, _) = backlink_occurrence(backlink);
            assert!(context.ends_with('…'));
            assert_eq!(range, Some(158..160));
            reader.panels.open(reader_layout::Panel::Backlinks);
            cx.notify();
        });
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.panels.visible(
                reader_layout::Panel::Backlinks,
                reader.body_viewport_width.into()
            ));
        });
        // Read mode uses rendered link ranges over Unicode label text.
        let origin = reader.read_with(visual, |reader, cx| {
            crate::reader_scroll_geometry_tests::paragraph_origin(reader, cx, 0)
        });
        visual.simulate_click(origin + point(px(25.), px(10.)), gpui::Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.current_rel, "target.md")
        });
        reader.update_in(visual, |reader, window, cx| {
            reader.open_note_at("start.md", None, None, window, cx)
        });
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx)
        });
        visual.run_until_parked();
        let input = reader.read_with(visual, |reader, _| {
            reader.editing.as_ref().unwrap().input.clone()
        });
        let offset = original.find("ссылка").unwrap();
        let position = input.read_with(visual, |input, _| {
            input
                .range_to_bounds(&(offset..offset + 2))
                .unwrap()
                .center()
        });
        visual.simulate_click(position, gpui::Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.current_rel, "start.md")
        });
        input.read_with(visual, |input, _| {
            let cursor = input.cursor();
            assert!(cursor >= offset && cursor <= offset + 2);
        });
        // Change source and re-layout: the next click must use the new string.
        reader.update_in(visual, |_, window, cx| {
            input.update(cx, |input, cx| {
                input.replace_text_in_range(Some(0..0), "новый 🧠 ", window, cx);
            })
        });
        visual.run_until_parked();
        let position = input.read_with(visual, |input, _| {
            let text = input.value();
            let offset = text.find("ссылка").unwrap();
            input
                .range_to_bounds(&(offset..offset + 2))
                .unwrap()
                .center()
        });
        visual.simulate_click(
            position,
            gpui::Modifiers {
                platform: true,
                ..Default::default()
            },
        );
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.current_rel, "target.md")
        });
        assert_eq!(
            std::fs::read_to_string(root.join("start.md")).unwrap(),
            format!("новый 🧠 {original}")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// #1645: an inline formula is drawn in place until the caret enters
    /// it; then its source shows in the line and a card below it draws the
    /// formula, or its error. Money keeps its dollars and gets no card.
    #[gpui::test]
    fn inline_math_draws_in_place_and_previews_under_the_caret(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let directory =
            std::env::temp_dir().join(format!("okilum-inline-math-{}", uuid::Uuid::new_v4()));
        let root = directory.join("notes");
        std::fs::create_dir_all(&root).unwrap();
        let original =
            "Budget about $1.5k/mo and $2$ tips.\n\nArea $x^2$ here and $\\foo$ there.\n";
        std::fs::write(root.join("start.md"), original).unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("start.md")),
                        index_dir: Some(directory.join("index")),
                        session_directory: Some(directory.join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            reader.open_live_preview(window, cx)
        });
        visual.run_until_parked();
        let input = reader.read_with(visual, |reader, _| {
            reader.editing.as_ref().unwrap().input.clone()
        });
        let shown = reader.read_with(visual, |reader, _| {
            reader
                .editing
                .as_ref()
                .unwrap()
                .live_preview
                .concealed_text()
                .expect("classified")
        });
        assert_eq!(
            shown, "Budget about $1.5k/mo and $2$ tips.\n\nArea x² here and $\\foo$ there.\n",
            "the formula is drawn, money and an unknown command stay as written"
        );
        let click = |offset: usize, visual: &mut gpui::VisualTestContext| {
            let position = input.read_with(visual, |input, _| {
                input.range_to_bounds(&(offset..offset)).unwrap().center()
            });
            visual.simulate_click(position, gpui::Modifiers::default());
            visual.run_until_parked();
        };
        let formula = original.find("$x^2$").unwrap();
        click(formula + 1, visual);
        let cursor = input.read_with(visual, |input, _| input.cursor());
        assert!(
            (formula..=formula + "$x^2$".len()).contains(&cursor),
            "a click on the drawn formula lands on it: {cursor}"
        );
        let card = visual
            .debug_bounds("math-preview-card")
            .expect("the caret in a formula shows its card");
        assert!(visual.debug_bounds("math-preview-formula").is_some());
        assert!(visual.debug_bounds("math-preview-error").is_none());
        let line = input.read_with(visual, |input, _| {
            input.range_to_bounds(&(formula..formula + 5)).unwrap()
        });
        assert!(
            card.top() >= line.bottom(),
            "the card sits under the line: {card:?} {line:?}"
        );
        // An unknown command: the card reports it instead of an empty box.
        let broken = original.find("$\\foo$").unwrap();
        click(broken + 2, visual);
        assert!(visual.debug_bounds("math-preview-card").is_some());
        assert!(visual.debug_bounds("math-preview-error").is_some());
        assert!(visual.debug_bounds("math-preview-formula").is_none());
        // Money: no formula, no card.
        click(original.find("1.5k").unwrap(), visual);
        assert!(visual.debug_bounds("math-preview-card").is_none());
        click(original.find("$2$").unwrap() + 1, visual);
        assert!(visual.debug_bounds("math-preview-card").is_none());
        // Back on the formula, then away from it: the card follows the caret.
        click(formula + 1, visual);
        assert!(visual.debug_bounds("math-preview-card").is_some());
        click(original.find("there").unwrap(), visual);
        assert!(visual.debug_bounds("math-preview-card").is_none());
        input.read_with(visual, |input, _| {
            assert_eq!(input.value().as_ref(), original, "previewing never edits")
        });
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn command_click_hands_an_external_link_to_the_system_in_both_editor_modes(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let directory =
            std::env::temp_dir().join(format!("okilum-external-link-{}", uuid::Uuid::new_v4()));
        let root = directory.join("notes");
        std::fs::create_dir_all(&root).unwrap();
        let url = "https://example.com/okilum-1237?q=a&b=c";
        let original = format!("Plain words, then [the external site]({url}) and more.\n");
        std::fs::write(root.join("start.md"), &original).unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("start.md")),
                        index_dir: Some(directory.join("index")),
                        session_directory: Some(directory.join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        for live in [false, true] {
            reader.update_in(visual, |reader, window, cx| {
                if live {
                    reader.open_live_preview(window, cx);
                } else {
                    reader.toggle_source(window, cx);
                }
            });
            visual.run_until_parked();
            let input = reader.read_with(visual, |reader, _| {
                reader.editing.as_ref().unwrap().input.clone()
            });
            let offset = original.find("external site").unwrap();
            let position = input.read_with(visual, |input, _| {
                input
                    .range_to_bounds(&(offset..offset + 1))
                    .unwrap()
                    .center()
            });
            // A plain click places the caret; it never leaves the app.
            let before = visual.opened_url();
            visual.simulate_click(position, gpui::Modifiers::default());
            visual.run_until_parked();
            assert_eq!(
                visual.opened_url(),
                before,
                "plain click, live preview: {live}"
            );
            visual.simulate_click(
                position,
                gpui::Modifiers {
                    platform: true,
                    ..Default::default()
                },
            );
            visual.run_until_parked();
            assert_eq!(
                visual.opened_url().as_deref(),
                Some(url),
                "live preview: {live}"
            );
            reader.read_with(visual, |reader, _| {
                assert_eq!(reader.current_rel, "start.md")
            });
            assert_eq!(
                std::fs::read_to_string(root.join("start.md")).unwrap(),
                original
            );
            // Reset the recorded URL for the next pass.
            visual.update(|_, cx| cx.open_url(""));
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn command_click_opens_a_link_inside_a_hebrew_paragraph_in_both_editor_modes(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let directory =
            std::env::temp_dir().join(format!("okilum-rtl-link-{}", uuid::Uuid::new_v4()));
        let root = directory.join("notes");
        std::fs::create_dir_all(&root).unwrap();
        let original = "שלום עולם זה טקסט\n\nקישור אל [[target|היעד]] כאן\n";
        std::fs::write(root.join("start.md"), original).unwrap();
        std::fs::write(root.join("target.md"), "# Target page").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("start.md")),
                        index_dir: Some(directory.join("index")),
                        session_directory: Some(directory.join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        for live in [false, true] {
            reader.update_in(visual, |reader, window, cx| {
                // The previous pass navigated while editing: leave it first.
                if reader.editing.is_some() {
                    reader.toggle_source(window, cx);
                }
            });
            visual.run_until_parked();
            reader.update_in(visual, |reader, window, cx| {
                reader.open_note_at("start.md", None, None, window, cx)
            });
            visual.run_until_parked();
            reader.update_in(visual, |reader, window, cx| {
                assert_eq!(reader.current_rel, "start.md");
                if live {
                    reader.open_live_preview(window, cx);
                } else {
                    reader.toggle_source(window, cx);
                }
            });
            visual.run_until_parked();
            let input = reader.read_with(visual, |reader, _| {
                reader.editing.as_ref().unwrap().input.clone()
            });
            let offset = original.find("היעד").unwrap();
            input.read_with(visual, |input, _| assert_eq!(input.value(), original));
            let position = input.read_with(visual, |input, _| {
                input
                    .range_to_bounds(&(offset..offset + "ה".len()))
                    .unwrap()
                    .center()
            });
            visual.simulate_click(
                position,
                gpui::Modifiers {
                    platform: true,
                    ..Default::default()
                },
            );
            visual.run_until_parked();
            reader.read_with(visual, |reader, _| {
                assert_eq!(reader.current_rel, "target.md", "live preview: {live}")
            });
        }
        assert_eq!(
            std::fs::read_to_string(root.join("start.md")).unwrap(),
            original
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn reader_round_trip_keeps_undo_history_for_unchanged_text(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let directory =
            std::env::temp_dir().join(format!("okilum-source-{}", uuid::Uuid::new_v4()));
        let root = directory.join("notes");
        std::fs::create_dir_all(&root).unwrap();
        let original = "one **two** three\n";
        std::fs::write(root.join("note.md"), original).unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("note.md")),
                        index_dir: Some(directory.join("index")),
                        session_directory: Some(directory.join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        let end = original.len() - 1;
        let edited = "one **two** threeX\n";
        let round_trip = |visual: &mut VisualTestContext, external: Option<&str>| {
            reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
            visual.run_until_parked();
            if let Some(text) = external {
                std::fs::write(root.join("note.md"), text).unwrap();
            }
            reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
            visual.run_until_parked();
        };
        let value = |visual: &mut VisualTestContext| {
            reader.update_in(visual, |r, _, cx| {
                r.editing
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .value()
                    .to_string()
            })
        };
        let undo = |visual: &mut VisualTestContext| {
            #[cfg(target_os = "macos")]
            visual.simulate_keystrokes("cmd-z");
            #[cfg(not(target_os = "macos"))]
            visual.simulate_keystrokes("ctrl-z");
            visual.run_until_parked();
        };
        // Edit, type X at the end of the line, save; Reader and back.
        reader.update_in(visual, |r, window, cx| {
            r.toggle_source(window, cx);
            let input = r.editing.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| {
                input.replace_text_in_range(Some(end..end), "X", window, cx)
            });
            assert!(r.save_source(cx));
        });
        visual.run_until_parked();
        round_trip(visual, None);
        assert_eq!(value(visual), edited);
        // The edit made before the Reader still undoes, exactly.
        undo(visual);
        assert_eq!(value(visual), original);
        // Control: a file changed outside while in the Reader starts a new
        // history; Undo has nothing from before to apply to it.
        reader.update_in(visual, |r, window, cx| {
            let input = r.editing.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| {
                input.replace_text_in_range(Some(end..end), "X", window, cx)
            });
            assert!(r.save_source(cx));
        });
        visual.run_until_parked();
        round_trip(visual, Some("external\n"));
        assert_eq!(value(visual), "external\n");
        undo(visual);
        assert_eq!(value(visual), "external\n");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn replacing_vault_saves_dirty_source_and_refuses_conflict(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        for root in [&first, &second] {
            std::fs::create_dir(root).unwrap();
            std::fs::write(root.join("note.md"), "# Original").unwrap();
        }
        // Reader stores canonical roots. On macOS the temporary directory may
        // use /var, a symlink to /private/var; compare the same filesystem identity.
        let first = first.canonicalize().unwrap();
        let second = second.canonicalize().unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            reader_open::install(cx);
            reader_ui_state::install(&temp.path().join("state"), cx);
            reader_ui_state::set_vault_open("current", cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(first.clone()),
                        note: Some("note.md".into()),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader_open::register(view.downgrade(), first.clone(), cx);
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            window.activate_window();
            reader.toggle_source(window, cx);
            let input = reader.editing.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| {
                input.replace_text_in_range(Some(0..0), "mine", window, cx)
            });
        });
        // Replace the on-disk revision before the navigation guard can save.
        std::fs::write(first.join("note.md"), "external").unwrap();
        let requested = Opts {
            vault: Some(second.clone()),
            note: Some("note.md".into()),
            ..Default::default()
        };
        visual.update(|_, cx| reader_open::open_window(requested.clone(), cx).unwrap());
        visual.run_until_parked();
        reader.update_in(visual, |reader, _, cx| {
            assert_eq!(reader.vault_root, first);
            assert!(reader.editing.as_ref().unwrap().conflict.is_some());
            assert!(!reader.save_source(cx));
            assert_eq!(
                std::fs::read_to_string(first.join("note.md")).unwrap(),
                "external"
            );
        });
        reader.update_in(visual, |reader, window, cx| {
            // Resolve with the latest disk version, then make a fresh dirty edit.
            reader.resolve_source(true, window, cx);
            let input = reader.editing.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| {
                input.replace_text_in_range(Some(0..0), "saved-", window, cx)
            });
        });
        visual.update(|_, cx| reader_open::open_window(requested, cx).unwrap());
        visual.run_until_parked();
        reader.update_in(visual, |reader, _, _| {
            assert_eq!(reader.vault_root, second);
            assert!(reader.editing.is_none());
        });
        assert_eq!(
            std::fs::read_to_string(first.join("note.md")).unwrap(),
            "saved-external"
        );
    }

    #[gpui::test]
    fn native_edit_undo_save_and_conflict_navigation(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let directory =
            std::env::temp_dir().join(format!("okilum-source-{}", uuid::Uuid::new_v4()));
        let root = directory.join("notes");
        std::fs::create_dir_all(&root).unwrap();
        let original = "\u{feff}---\r\ntitle: Привет\r\n---\r\nbody 🧠\r\n";
        std::fs::write(root.join("first.md"), original).unwrap();
        std::fs::write(root.join("second.md"), "Second").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("first.md")),
                        index_dir: Some(directory.join("index")),
                        session_directory: Some(directory.join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx);
            let input = reader.editing.as_ref().unwrap().input.clone();
            assert_eq!(input.read(cx).value().as_ref(), original);
            input.update(cx, |input, cx| {
                input.replace_text_in_range(Some(0..0), "edit", window, cx)
            });
        });
        visual.run_until_parked();
        reader.update_in(visual, |reader, _, cx| {
            assert!(reader.editing.as_ref().unwrap().store.dirty());
            assert!(reader.save_source(cx));
        });
        // Typed at byte zero, the text lands after the BOM: the saved file
        // still starts EF BB BF (#1093).
        let saved = std::fs::read(root.join("first.md")).unwrap();
        assert_eq!(&saved[..3], b"\xef\xbb\xbf");
        assert_eq!(
            String::from_utf8(saved).unwrap(),
            original.replacen('\u{feff}', "\u{feff}edit", 1)
        );
        #[cfg(target_os = "macos")]
        visual.simulate_keystrokes("cmd-z");
        #[cfg(not(target_os = "macos"))]
        visual.simulate_keystrokes("ctrl-z");
        visual.run_until_parked();
        visual
            .executor()
            .advance_clock(RECOVERY_PAUSE + std::time::Duration::from_millis(1));
        visual.run_until_parked();
        let journal = std::fs::read_dir(directory.join("state/editor-drafts"))
            .unwrap()
            .flatten()
            .find(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .unwrap()
            .path();
        let recovered: serde_json::Value =
            serde_json::from_slice(&std::fs::read(journal).unwrap()).unwrap();
        assert_eq!(
            recovered["text"].as_str(),
            Some(original),
            "undo is durable before Save"
        );
        reader.update_in(visual, |reader, _, cx| {
            assert_eq!(
                reader
                    .editing
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .value()
                    .as_ref(),
                original
            );
            assert!(reader.save_source(cx));
        });
        reader.update_in(visual, |reader, window, cx| {
            let input = reader.editing.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| {
                input.replace_text_in_range(Some(0..0), "mine", window, cx)
            });
        });
        visual.run_until_parked();
        std::fs::write(root.join("first.md"), "external").unwrap();
        reader.update_in(visual, |reader, window, cx| {
            reader.open_note("second.md", None, window, cx);
            assert_eq!(reader.current_rel, "first.md");
            assert!(reader.editing.as_ref().unwrap().conflict.is_some());
            assert!(!reader.save_source(cx));
            std::fs::write(root.join("first.md"), "external v2").unwrap();
            assert!(!reader.save_source(cx));
            assert_eq!(
                reader.editing.as_ref().unwrap().conflict.as_deref(),
                Some("external")
            );
            reader.resolve_source(false, window, cx);
            assert_eq!(
                std::fs::read_to_string(root.join("first.md")).unwrap(),
                "external v2"
            );
            assert!(reader.editing.as_ref().unwrap().compare);
            assert_eq!(
                reader
                    .editing
                    .as_ref()
                    .unwrap()
                    .current_input
                    .read(cx)
                    .value()
                    .as_ref(),
                "external v2"
            );
            reader.resolve_source(true, window, cx);
            assert_eq!(
                reader
                    .editing
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .value()
                    .as_ref(),
                "external v2"
            );
            reader.open_note("second.md", None, window, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |reader, _, _| {
            assert_eq!(reader.current_rel, "second.md");
            assert!(
                reader.editing.is_some(),
                "successful navigation retains Source"
            );
        });
        assert_eq!(
            std::fs::read_to_string(root.join("first.md")).unwrap(),
            "external v2"
        );
        reader.update_in(visual, |reader, window, cx| {
            reader
                .editing
                .as_ref()
                .unwrap()
                .input
                .clone()
                .update(cx, |input, cx| {
                    input.replace_text_in_range(Some(0..0), "recover me", window, cx);
                });
        });
        visual.run_until_parked();
        std::fs::remove_file(root.join("second.md")).unwrap();
        reader.update_in(visual, |reader, window, cx| {
            assert!(!reader.save_source(cx));
            assert!(reader.editing.as_ref().unwrap().save_failed);
            reader.park_source(window, cx);
            assert!(
                reader.editing.is_none(),
                "failed canonical save can exit with durable recovery"
            );
        });
        assert!(std::fs::read_dir(directory.join("state/editor-drafts"))
            .unwrap()
            .flatten()
            .any(|entry| {
                entry.path().extension().is_some_and(|ext| ext == "json")
                    && std::fs::read_to_string(entry.path())
                        .unwrap()
                        .contains("recover meSecond")
            }));
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[gpui::test]
    async fn move_blocks_dirty_other_window_and_reloads_clean_input(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let directory =
            std::env::temp_dir().join(format!("okilum-move-windows-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(directory.join("notes/New")).unwrap();
        let root = directory.join("notes").canonicalize().unwrap();
        std::fs::write(root.join("start.md"), "# Source").unwrap();
        std::fs::write(root.join("ref.md"), "[[start]]").unwrap();
        let mut reference_cx = cx.clone();
        let mut source = None;
        let (_, source_visual) = cx.add_window_view(|window, cx| {
            let r = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("start.md")),
                        index_dir: Some(directory.join("index")),
                        session_directory: Some(directory.join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            source = Some(r.clone());
            Root::new(r, window, cx)
        });
        let mut reference = None;
        let (_, reference_visual) = reference_cx.add_window_view(|window, cx| {
            let r = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("ref.md")),
                        index_dir: Some(directory.join("index")),
                        session_directory: Some(directory.join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reference = Some(r.clone());
            Root::new(r, window, cx)
        });
        let source = source.unwrap();
        let reference = reference.unwrap();
        source_visual.run_until_parked();
        reference_visual.run_until_parked();
        reference.update_in(reference_visual, |r, window, cx| {
            r.toggle_source(window, cx);
            r.set_live_preview(true, window, cx);
        });
        reference_visual.run_until_parked();
        reference.read_with(reference_visual, |r, _| {
            assert!(!r
                .editing
                .as_ref()
                .unwrap()
                .live_preview
                .completed_navigation_bytes()
                .is_empty());
        });
        let input = reference.read_with(reference_visual, |r, _| {
            r.editing.as_ref().unwrap().input.clone()
        });
        input.update_in(reference_visual, |input, window, cx| {
            input.replace_text_in_range(Some(0..0), "edited ", window, cx)
        });
        reference_visual.run_until_parked();
        let preview =
            okilum_core::link_rewrite::Preview::prepare(&root, "start.md", "New/renamed.md")
                .unwrap();
        source.update_in(source_visual, |r, window, cx| {
            assert!(r
                .apply_link_move(&preview, window, cx)
                .err()
                .unwrap()
                .to_string()
                .contains("unsaved edits"))
        });
        assert!(root.join("start.md").exists());
        assert!(!root.join("New/renamed.md").exists());
        reference.update_in(reference_visual, |r, _, cx| assert!(r.save_source(cx)));
        let preview =
            okilum_core::link_rewrite::Preview::prepare(&root, "start.md", "New/renamed.md")
                .unwrap();
        let old_marker = reference.update_in(reference_visual, |r, window, cx| {
            input.update(cx, |i, cx| i.set_value("edited [[start]] ", window, cx));
            input.update(cx, |i, cx| i.set_value("edited [[start]]", window, cx));
            r.schedule_live_preview(cx);
            r.editing
                .as_ref()
                .unwrap()
                .live_preview
                .pending_marker()
                .unwrap()
        });
        assert!(
            old_marker.get(),
            "real LP classification pending at two-window park"
        );
        // Mutation callbacks mark recovery pending after the update above.
        // A REAL save in the next update settles the current input before move.
        reference.update_in(reference_visual, |r, _, cx| {
            assert!(r.save_source(cx));
            assert!(
                !r.source_is_dirty(cx),
                "the unchanged production move guard admits this input"
            );
        });
        let task = source.update_in(source_visual, |r, window, cx| {
            let task = r.apply_link_move(&preview, window, cx).unwrap();
            assert!(r.move_applying);
            assert!(
                !input.read(cx).is_editable(),
                "Parked windows must refuse input before the next paint"
            );
            assert!(
                !r.save_source(cx),
                "Navigation/quit must wait for the worker"
            );
            task
        });
        assert!(task.await.unwrap().moved);
        assert!(
            !old_marker.get(),
            "old captured marker settled without changing new work"
        );
        source.update_in(source_visual, |r, _, cx| r.finish_move_editor(cx));
        reference_visual.run_until_parked();
        reference.read_with(reference_visual, |r, cx| {
            let editing = r.editing.as_ref().unwrap();
            assert_eq!(
                editing.input.read(cx).value().as_ref(),
                "edited [[renamed]]"
            );
            assert!(!editing.store.dirty());
            assert!(editing.input.read(cx).is_editable());
            assert!(
                editing.live_preview.concealed_text().is_some(),
                "other-window LP resumed"
            );
            assert_eq!(
                *editing
                    .live_preview
                    .completed_navigation_bytes()
                    .last()
                    .unwrap(),
                "edited [[renamed]]".len() as u64
            );
        });
        assert_eq!(
            std::fs::read_to_string(root.join("ref.md")).unwrap(),
            "edited [[renamed]]"
        );
        let state = directory.join("state");
        let operations = okilum_core::link_rewrite::Operation::list(&state, &root).unwrap();
        reference.update_in(reference_visual, |r, _, _| {
            r.sidebar.pinned = vec!["New/renamed.md".into()];
            r.sidebar.recent = vec![("New/renamed.md".into(), 123)];
            r.tree_revealed = "New/renamed.md".into();
        });
        source.update_in(source_visual, |r, window, cx| {
            r.revert_link_move(&operations.operations[0], &state, window, cx)
                .unwrap()
        });
        reference_visual.run_until_parked();
        reference.read_with(reference_visual, |r, cx| {
            let editing = r.editing.as_ref().unwrap();
            assert_eq!(editing.input.read(cx).value().as_ref(), "edited [[start]]");
            assert!(!editing.store.dirty());
            assert_eq!(r.sidebar.pinned, ["start.md"]);
            assert!(r
                .sidebar
                .recent
                .iter()
                .any(|(path, time)| path == "start.md" && *time == 123));
            assert!(!r
                .sidebar
                .recent
                .iter()
                .any(|(path, _)| path == "New/renamed.md"));
            assert_eq!(
                r.tree_revealed, "ref.md",
                "reloaded current note is revealed"
            );
        });
        assert!(root.join("start.md").exists());
        assert!(!root.join("New/renamed.md").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[cfg(test)]
mod edit_error_tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn edit_failures_show_authored_reasons_or_plain_words() {
        use std::io::{Error as Io, ErrorKind};
        let recovery = anyhow::Error::from(Io::from(ErrorKind::PermissionDenied))
            .context(okilum_core::file_editor::RECOVERY_UNAVAILABLE);
        assert_eq!(
            edit_error_text(&recovery),
            okilum_core::file_editor::RECOVERY_UNAVAILABLE
        );
        for (kind, expected) in [
            (
                ErrorKind::PermissionDenied,
                "Okilum doesn’t have permission",
            ),
            (ErrorKind::NotFound, "This note no longer exists"),
            (
                ErrorKind::Other,
                "This note couldn’t be opened for editing.",
            ),
        ] {
            let raw = anyhow::Error::from(Io::new(kind, "Access is denied. (os error 5)"));
            let text = edit_error_text(&raw);
            assert!(text.starts_with(expected), "{text}");
            assert!(!text.contains("os error"), "{text}");
            // Positive control: stderr keeps the OS detail.
            assert!(format!("{raw:#}").contains("os error 5"));
        }
    }

    /// #1602: a paste that inserts nothing is never silent, except a second
    /// paste while the first is still reading.
    #[test]
    fn paste_failures_are_explained_in_plain_words() {
        use ClipboardReadError as E;
        for error in [
            E::Unsupported,
            E::Unavailable,
            E::Cancelled,
            E::Timeout,
            E::TooLarge,
            E::InvalidUtf8,
            E::ChangedOffer,
            E::AmbiguousSeat,
            E::Io,
        ] {
            let text = paste_failure_text(&error, false).expect("explained");
            assert!(
                !text.contains("Unavailable") && !text.contains("Err"),
                "{text}"
            );
        }
        assert_eq!(paste_failure_text(&E::Busy, false), None);
        assert!(paste_failure_text(&E::Unavailable, true)
            .unwrap()
            .starts_with("Images can’t be pasted"));
        assert_eq!(
            paste_failure_text(&E::Unavailable, false),
            Some("Nothing to paste: the clipboard has no text or image.")
        );
    }
}

/// #1353: autosave a second after typing stops, off the UI thread.
#[cfg(test)]
mod autosave_tests {
    use super::*;
    use ::core::prelude::v1::test;
    use std::time::Duration;

    struct Fixture {
        _temp: tempfile::TempDir,
        file: PathBuf,
        reader: Entity<Reader>,
    }

    fn open<'a>(cx: &'a mut TestAppContext, text: &str) -> (Fixture, &'a mut VisualTestContext) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("note.md"), text).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("note.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        (
            Fixture {
                file: root.join("note.md"),
                reader,
                _temp: temp,
            },
            visual,
        )
    }

    fn input(f: &Fixture, visual: &mut VisualTestContext) -> Entity<EditorState> {
        f.reader
            .read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone())
    }

    fn type_at_start(f: &Fixture, visual: &mut VisualTestContext, text: &str) {
        let input = input(f, visual);
        visual.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_text_in_range(Some(0..0), text, window, cx)
            })
        });
        visual.run_until_parked();
    }

    fn wait(visual: &mut VisualTestContext, millis: u64) {
        visual
            .executor()
            .advance_clock(Duration::from_millis(millis));
        visual.run_until_parked();
    }

    fn on_disk(f: &Fixture) -> String {
        std::fs::read_to_string(&f.file).unwrap()
    }

    #[gpui::test]
    fn a_pause_saves_typing_continues_and_undo_reaches_past_the_save(cx: &mut TestAppContext) {
        let (f, visual) = open(cx, "original");
        type_at_start(&f, visual, "edit ");
        wait(visual, 500);
        assert_eq!(on_disk(&f), "original", "nothing is saved mid-pause");
        wait(visual, 700);
        assert_eq!(
            on_disk(&f),
            "edit original",
            "saved a second after typing stopped"
        );
        f.reader.read_with(visual, |r, cx| {
            assert!(!r.source_is_dirty(cx), "no dot once saved");
            assert!(r.link_notice.is_none());
        });
        // A burst of typing is saved once it pauses, not in the middle.
        type_at_start(&f, visual, "a");
        wait(visual, 600);
        type_at_start(&f, visual, "b");
        wait(visual, 600);
        assert_eq!(on_disk(&f), "edit original", "still typing: not saved yet");
        wait(visual, 500);
        assert_eq!(on_disk(&f), "baedit original");
        // Undo goes back past the autosave, and that is saved too.
        #[cfg(target_os = "macos")]
        visual.simulate_keystrokes("cmd-z");
        #[cfg(not(target_os = "macos"))]
        visual.simulate_keystrokes("ctrl-z");
        visual.run_until_parked();
        let undone = input(&f, visual).read_with(visual, |v, _| v.value().to_string());
        assert_ne!(undone, "baedit original", "undo works after an autosave");
        wait(visual, 1200);
        assert_eq!(on_disk(&f), undone);
    }

    #[gpui::test]
    fn issue2405_autosave_publication_work_and_prepared_cache(cx: &mut TestAppContext) {
        let mut violations = Vec::new();
        for shared in [false, true] {
            for bytes in [25 * 1024, 130 * 1024, 1024 * 1024] {
                let mut source =
                    "# Note [[note]]\r\n\r\n```rust\r\nlet value = 1;\r\n```\r\n\r\n".to_owned();
                source
                    .push_str(&"Ordinary prose with unchanged semantics. ".repeat(bytes / 40 + 1));
                source.truncate(bytes);
                let before_preparation = LIVE_PREPARES.with(|n| n.get());
                let mut sibling_cx = cx.clone();
                let (f, visual) = open(cx, &source);
                let original = f.reader.update_in(visual, |r, window, cx| {
                    r.editing
                        .as_ref()
                        .unwrap()
                        .input
                        .update(cx, |i, _| i.enable_layout_diagnostics());
                    r.set_live_preview(true, window, cx);
                    if shared {
                        assert!(r.share_session().is_some());
                    }
                    r.vault.clone()
                });
                let sibling = if shared {
                    let (session, opts) = f.reader.read_with(visual, |r, _| {
                        (
                            r.shared_session.clone().unwrap(),
                            Opts {
                                vault: Some(r.vault_root.clone()),
                                note: Some("note.md".into()),
                                session_directory: Some(f._temp.path().join("other-state")),
                                defer_loading: true,
                                ..Default::default()
                            },
                        )
                    });
                    let mut sibling = None;
                    let (_, other_visual) = sibling_cx.add_window_view(|window, cx| {
                        let view = cx.new(|cx| Reader::new(opts, window, cx));
                        view.update(cx, |r, cx| {
                            r.attach_session(session, "note.md".into(), window, cx)
                        });
                        sibling = Some(view.clone());
                        Root::new(view, window, cx)
                    });
                    other_visual.run_until_parked();
                    Some((sibling.unwrap(), other_visual))
                } else {
                    None
                };
                visual.run_until_parked();
                visual.update(|window, cx| window.draw(cx).clear(cx));
                let prepared = LIVE_PREPARES.with(|n| n.get());
                assert!(
                    prepared > before_preparation,
                    "prepared block positive control must fire for each fixture"
                );
                // Real native edit, debounce, worker save, and both publication paths.
                let native = input(&f, visual);
                native.update_in(visual, |i, window, cx| {
                    i.take_layout_frames();
                    i.replace_text_in_range(Some(0..1), "x", window, cx);
                });
                visual.run_until_parked();
                visual.update(|window, cx| window.draw(cx).clear(cx));
                let prepared = LIVE_PREPARES.with(|n| n.get());
                // Retain the edited frame: an unchanged autosave correctly
                // performs no new native layout. The final stamp must match.
                work::reset();
                wait(visual, 1200);
                f.reader.update_in(visual, |r, window, cx| {
                    if shared {
                        r.poll_shared_session(window, cx);
                    }
                });
                visual.run_until_parked();
                let measured = work::take();
                let mut sibling_same = true;
                if let Some((other, other_visual)) = sibling {
                    other.update_in(other_visual, |r, window, cx| {
                        r.poll_shared_session(window, cx)
                    });
                    other_visual.run_until_parked();
                    sibling_same = other.read_with(other_visual, |r, _| {
                        assert_eq!(
                            r.note_canonical_source.as_deref(),
                            Some(format!("x{}", &source[1..]).as_str()),
                            "duplicate reader receives the actual edited source, not just its unchanged length"
                        );
                        Arc::ptr_eq(&r.vault, &original)
                    });
                    other_visual.update(|window, _| window.remove_window());
                    other_visual.run_until_parked();
                }
                let same = f.reader.read_with(visual, |r, cx| {
                    assert_eq!(on_disk(&f), format!("x{}", &source[1..]));
                    assert!(!r.source_is_dirty(cx));
                    assert!(!r.editing.as_ref().unwrap().conflict_detected);
                    Arc::ptr_eq(&original, &r.vault) && sibling_same
                });
                visual.update(|window, cx| window.draw(cx).clear(cx));
                native.read_with(visual, |i, _| {
                    let frames = i.take_layout_frames();
                    let frame = frames.last().expect("actual edited native frame");
                    assert!(!frame.raw, "autosaved fixture must remain projected");
                    assert_eq!(frame.generation, i.source_stamp().generation);
                    assert_eq!(frame.presentation_epoch, i.presentation_epoch());
                });
                let cache_kept = LIVE_PREPARES.with(|n| n.get()) == prepared;
                println!(
                    "EDITOR_SAVE_WORK bytes={bytes} shared={shared} measured={measured:?} same_vault={same} cache_kept={cache_kept}"
                );
                if !same
                    || !cache_kept
                    || measured.largest >= 16 * 1024
                    || measured.bytes >= 128 * 1024
                    || measured.frees >= 512
                {
                    violations.push((bytes, shared, measured, same, cache_kept));
                }
                // The exact legacy UI refresh must expose the same note-size
                // copies even when its timestamp fast path avoids a disk read.
                work::reset();
                f.reader.update_in(visual, |r, window, cx| {
                    r.refresh_source_from_disk(window, cx)
                });
                let control = work::take();
                assert!(
                    control.largest >= bytes && control.bytes >= 2 * bytes,
                    "unfired refresh control {control:?}"
                );
                println!("EDITOR_SAVE_CONTROL bytes={bytes} shared={shared} measured={control:?}");
                // Each size owns its windows. Old watcher timers must not join
                // the next fixture's measured autosave after its vault is gone.
                visual.update(|window, _| window.remove_window());
                visual.run_until_parked();
            }
        }
        assert!(
            violations.is_empty(),
            "own-save publication regression: {violations:?}"
        );
    }

    #[gpui::test]
    fn issue2405_external_publication_refreshes_clean_and_conflicts_dirty(cx: &mut TestAppContext) {
        for shared in [false, true] {
            let (f, visual) = open(cx, "\u{feff}base\r\n");
            f.reader.update_in(visual, |r, _, _| {
                if shared {
                    assert!(r.share_session().is_some());
                }
            });
            std::fs::write(&f.file, "\u{feff}outside\r\n").unwrap();
            f.reader.update_in(visual, |r, window, cx| {
                r.apply_vault_changes(
                    okilum_core::Changes {
                        changed: ["note.md".into()].into(),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            visual.run_until_parked();
            if shared {
                f.reader
                    .update_in(visual, |r, window, cx| r.poll_shared_session(window, cx));
                visual.run_until_parked();
            }
            assert_eq!(
                input(&f, visual).read_with(visual, |i, _| i.value().to_string()),
                "\u{feff}outside\r\n"
            );
            type_at_start(&f, visual, "mine");
            std::fs::write(&f.file, "\u{feff}external conflict\r\n").unwrap();
            f.reader.update_in(visual, |r, window, cx| {
                r.apply_vault_changes(
                    okilum_core::Changes {
                        changed: ["note.md".into()].into(),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            visual.run_until_parked();
            if shared {
                f.reader
                    .update_in(visual, |r, window, cx| r.poll_shared_session(window, cx));
                visual.run_until_parked();
            }
            f.reader.read_with(visual, |r, _| {
                assert!(r.editing.as_ref().unwrap().conflict_detected)
            });
            assert_eq!(
                input(&f, visual).read_with(visual, |i, _| i.value().to_string()),
                "\u{feff}mineoutside\r\n"
            );
            wait(visual, 1200);
            assert_eq!(on_disk(&f), "\u{feff}external conflict\r\n");
            // Its original timer has exited while the conflict was visible.
            // Returning the confirmed base must restart autosave without a key.
            std::fs::write(&f.file, "\u{feff}outside\r\n").unwrap();
            f.reader.update_in(visual, |r, window, cx| {
                r.apply_vault_changes(
                    okilum_core::Changes {
                        changed: ["note.md".into()].into(),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            visual.run_until_parked();
            if shared {
                f.reader
                    .update_in(visual, |r, window, cx| r.poll_shared_session(window, cx));
                visual.run_until_parked();
            }
            wait(visual, 1200);
            assert_eq!(
                on_disk(&f),
                "\u{feff}mineoutside\r\n",
                "conflict-cleared autosave resumes without new input"
            );
        }
    }

    #[gpui::test]
    fn issue2404_key_has_no_note_sized_foreground_allocation_or_reader_notification(
        cx: &mut TestAppContext,
    ) {
        let mut violations = Vec::new();
        for bytes in [25 * 1024, 130 * 1024, 1024 * 1024] {
            let mut source = "Ordinary prose line.\r\n\r\n".repeat(bytes / 24 + 1);
            source.truncate(bytes);
            let (f, visual) = open(cx, &source);
            let input = input(&f, visual);
            // Warm drawing/observer registrations before measuring a key.
            visual.update(|window, cx| window.draw(cx).clear(cx));
            let notified = Rc::new(std::cell::Cell::new(0));
            let seen = notified.clone();
            let _watch =
                visual.update(|_, cx| cx.observe(&f.reader, move |_, _| seen.set(seen.get() + 1)));
            work::reset();
            input.update_in(visual, |input, window, cx| {
                input.replace_text_in_range(Some(0..1), "x", window, cx);
            });
            let measured = work::take();
            if measured.largest >= 16 * 1024
                || measured.bytes >= 128 * 1024
                || measured.frees >= 512
                || measured.inspected >= 16 * 1024
                || notified.get() != 0
            {
                violations.push((bytes, measured, notified.get()));
            }
            // Positive control runs the legacy value/String snapshot operation
            // under the same allocator; it must exceed the key's bound.
            work::reset();
            let flat = input.read_with(visual, |i, _| {
                let _work = work::scope();
                i.value().to_string()
            });
            let control = work::take();
            assert!(
                control.largest >= bytes && control.bytes >= 2 * bytes && control.frees > 0,
                "unfired positive control: {control:?}"
            );
            assert!(flat.starts_with('x'));
            f.reader.read_with(visual, |r, _| {
                assert_eq!(
                    r.editing.as_ref().unwrap().store.text(),
                    flat,
                    "the measured mutation must reach the real persistence subscription"
                );
            });
            println!("EDITOR_KEY_WORK bytes={bytes} measured={measured:?} control={control:?}");
        }
        assert!(
            violations.is_empty(),
            "bounded per-key UI work failed: {violations:?}"
        );
    }

    #[gpui::test]
    fn issue2404_recovery_debounces_then_restores_latest_bom_crlf(cx: &mut TestAppContext) {
        let (f, visual) = open(cx, "\u{feff}base\r\n");
        type_at_start(&f, visual, "first");
        wait(visual, 150);
        type_at_start(&f, visual, "latest");
        let path = f.reader.read_with(visual, |r, _| {
            r.session_directory.as_ref().unwrap().join("editor-drafts")
        });
        // No recovery write until the pause after the most recent mutation.
        assert!(!FileEditor::has_unsaved_draft(&f.file, &path).unwrap());
        wait(visual, 200);
        assert!(!FileEditor::has_unsaved_draft(&f.file, &path).unwrap());
        wait(visual, 150);
        assert!(FileEditor::has_unsaved_draft(&f.file, &path).unwrap());
        let text = input(&f, visual).read_with(visual, |i, _| i.value().to_string());
        f.reader.update_in(visual, |r, _, _| {
            r.editing = None;
        });
        let recovered = FileEditor::open(&f.file, &path).unwrap();
        assert_eq!(recovered.text(), text);
        assert!(recovered.text().starts_with('\u{feff}'));
        assert!(recovered.text().ends_with("\r\n"));
        drop(recovered);
    }

    #[gpui::test]
    fn issue2404_close_and_quit_flush_before_debounce(cx: &mut TestAppContext) {
        let (f, visual) = open(cx, "\u{feff}base\r\n");
        type_at_start(&f, visual, "latest");
        let text = input(&f, visual).read_with(visual, |i, _| i.value().to_string());
        let path = f.reader.read_with(visual, |r, _| {
            r.session_directory.as_ref().unwrap().join("editor-drafts")
        });
        // Keep Draft closes immediately, before the recovery timer can run.
        f.reader
            .update_in(visual, |r, window, cx| r.park_source(window, cx));
        let recovered = FileEditor::open(&f.file, &path).unwrap();
        assert_eq!(recovered.text(), text);
        assert_eq!(on_disk(&f), "\u{feff}base\r\n");
        drop(recovered);
        f.reader
            .update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        type_at_start(&f, visual, "quit");
        let expected = input(&f, visual).read_with(visual, |i, _| i.value().to_string());
        visual.update(|_, cx| assert!(protect_all_for_quit(cx)));
        assert_eq!(on_disk(&f), expected);
        f.reader.update_in(visual, |r, _, _| {
            r.editing = None;
        });
        assert_eq!(FileEditor::open(&f.file, &path).unwrap().text(), expected);
    }

    #[gpui::test]
    fn issue2404_debounced_worker_failure_is_visible_and_latest_draft_retries(
        cx: &mut TestAppContext,
    ) {
        let (f, visual) = open(cx, "base");
        let state = f.reader.read_with(visual, |r, _| {
            r.session_directory.as_ref().unwrap().join("editor-drafts")
        });
        // The locked state folder exists already. Replacing its journal file
        // with a directory deterministically refuses the worker's rename.
        use sha2::{Digest, Sha256};
        let key = format!(
            "{:x}",
            Sha256::digest(
                f.file
                    .canonicalize()
                    .unwrap()
                    .as_os_str()
                    .as_encoded_bytes()
            )
        );
        let journal = state.join(format!("{key}.json"));
        std::fs::create_dir(&journal).unwrap();
        type_at_start(&f, visual, "latest");
        wait(visual, 350);
        f.reader.read_with(visual, |r, _| {
            assert!(r.editing.as_ref().unwrap().save_failed);
            assert_eq!(r.link_notice.as_deref(), Some(DRAFT_WRITE_NOTICE));
        });
        std::fs::remove_dir(&journal).unwrap();
        f.reader
            .update_in(visual, |r, _, cx| assert!(r.save_source(cx)));
        assert_eq!(on_disk(&f), "latestbase");
        f.reader
            .read_with(visual, |r, _| assert!(r.link_notice.is_none()));
    }

    #[gpui::test]
    fn issue2404_save_worker_start_failure_is_visible_and_explicit_save_retries(
        cx: &mut TestAppContext,
    ) {
        let (f, visual) = open(cx, "base");
        type_at_start(&f, visual, "latest");
        wait(visual, 350);
        FAIL_AUTOSAVE_COPY.with(|fail| fail.set(true));
        wait(visual, 800);
        f.reader.read_with(visual, |r, _| {
            assert!(r.editing.as_ref().unwrap().save_failed);
            assert_eq!(r.link_notice.as_deref(), Some(AUTOSAVE_START_NOTICE));
        });
        assert_eq!(on_disk(&f), "base", "failed worker never published");
        f.reader
            .update_in(visual, |r, _, cx| assert!(r.save_source(cx)));
        assert_eq!(on_disk(&f), "latestbase");
        f.reader
            .read_with(visual, |r, _| assert!(r.link_notice.is_none()));
    }

    #[gpui::test]
    fn issue2404_save_does_not_cancel_same_revision_context_preparation(cx: &mut TestAppContext) {
        let (f, visual) = open(cx, "[id]: https://example.com/old\r\n");
        let input = input(&f, visual);
        input.update_in(visual, |i, window, cx| {
            i.set_value("[id]: https://example.com/new\r\n", window, cx)
        });
        visual.run_until_parked();
        f.reader
            .update_in(visual, |r, _, cx| assert!(r.save_source(cx)));
        wait(visual, 350);
        f.reader.read_with(visual, |r, _| {
            let context = r.editing.as_ref().unwrap().block_context.borrow();
            let source = context.with_definitions("[label][id]".into());
            assert!(source.contains("https://example.com/new"));
            assert!(!source.contains("https://example.com/old"));
        });
    }

    #[gpui::test]
    fn issue2404_save_acknowledgement_owns_protection_of_newer_typing(cx: &mut TestAppContext) {
        let (f, visual) = open(cx, "base");
        type_at_start(&f, visual, "a");
        let saves = autosave_in_flight(&f, visual, "abase");
        type_at_start(&f, visual, "b");
        let path = f.reader.read_with(visual, |r, _| {
            r.session_directory.as_ref().unwrap().join("editor-drafts")
        });
        let input_id = input(&f, visual).entity_id();
        f.reader.update(visual, |r, cx| {
            let before = r.editing.as_ref().unwrap().recovery_epoch;
            r.autosaved(input_id, saves, "abase".into(), Ok(Save::Saved), None, cx);
            let editing = r.editing.as_ref().unwrap();
            assert!(editing.protecting);
            assert!(
                editing.recovery_epoch > before,
                "the previous journal worker lost acknowledgement ownership"
            );
        });
        visual.run_until_parked();
        f.reader.read_with(visual, |r, _| {
            assert!(!r.editing.as_ref().unwrap().protecting)
        });
        // Crash-style release without Save/close flush. The acknowledged draft
        // must contain the typing made after the canonical save's snapshot.
        f.reader.update(visual, |r, _| {
            r.editing = None;
        });
        visual.run_until_parked();
        let recovered = FileEditor::open(&f.file, &path).unwrap();
        assert_eq!(recovered.text(), "babase");
        assert_eq!(on_disk(&f), "abase");
    }

    /// A change made outside Okilum before the autosave is never overwritten:
    /// the conflict view opens, the typing stays, and no save is retried.
    #[gpui::test]
    fn an_outside_change_is_never_overwritten(cx: &mut TestAppContext) {
        let (f, visual) = open(cx, "original");
        type_at_start(&f, visual, "mine ");
        std::fs::write(&f.file, "theirs").unwrap();
        wait(visual, 1200);
        assert_eq!(on_disk(&f), "theirs", "the outside version stays");
        f.reader.read_with(visual, |r, cx| {
            let editing = r.editing.as_ref().unwrap();
            assert!(editing.conflict_detected);
            assert_eq!(editing.input.read(cx).value().as_ref(), "mine original");
        });
        type_at_start(&f, visual, "more ");
        wait(visual, 3000);
        assert_eq!(
            on_disk(&f),
            "theirs",
            "no autosave while the conflict is open"
        );
    }

    /// Our own autosave is not an outside change: the next watcher poll
    /// replaces nothing, opens no conflict and shows no notice.
    #[gpui::test]
    fn the_watcher_does_not_echo_an_autosave(cx: &mut TestAppContext) {
        let (f, visual) = open(cx, "original");
        type_at_start(&f, visual, "edit ");
        wait(visual, 1200);
        assert_eq!(on_disk(&f), "edit original");
        let before = input(&f, visual).read_with(visual, |v, _| v.scroll_offset());
        // Several real watcher and poll intervals.
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(100));
            wait(visual, 300);
        }
        f.reader.update_in(visual, |r, window, cx| {
            r.refresh_source_from_disk(window, cx)
        });
        visual.run_until_parked();
        f.reader.read_with(visual, |r, cx| {
            let editing = r.editing.as_ref().unwrap();
            assert!(!editing.conflict_detected, "no conflict from our own save");
            assert!(r.link_notice.is_none(), "{:?}", r.link_notice);
            assert_eq!(editing.input.read(cx).value().as_ref(), "edit original");
            assert!(!r.source_is_dirty(cx));
        });
        let after = input(&f, visual).read_with(visual, |v, _| v.scroll_offset());
        assert_eq!(before, after, "no reload, no scroll jump");
    }

    /// IME composition is not saved half-way; the commit is.
    #[gpui::test]
    fn composition_is_saved_only_after_the_commit(cx: &mut TestAppContext) {
        let (f, visual) = open(cx, "original");
        let input = input(&f, visual);
        visual.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(Some(0..0), "か", None, window, cx)
            })
        });
        visual.run_until_parked();
        wait(visual, 1500);
        assert_eq!(on_disk(&f), "original", "not during composition");
        visual.update(|window, cx| {
            input.update(cx, |input, cx| {
                let marked = input.marked_text_range(window, cx);
                input.replace_text_in_range(marked, "漢", window, cx)
            })
        });
        visual.run_until_parked();
        wait(visual, 1200);
        assert_eq!(on_disk(&f), "漢original", "the committed text is saved");
    }

    /// Typing while an autosave is still writing: that edit's timer finds the
    /// save running and skips; when the save completes it schedules another,
    /// so the last edit is saved without a further keystroke (review of #1362).
    #[gpui::test]
    fn typing_during_an_autosave_is_saved_after_it(cx: &mut TestAppContext) {
        let (f, visual) = open(cx, "original");
        type_at_start(&f, visual, "a");
        // A save of «aoriginal» is still in flight.
        let input_id = input(&f, visual).entity_id();
        let saves = f.reader.update(visual, |r, _| {
            let editing = r.editing.as_mut().unwrap();
            editing.autosaving = Some("aoriginal".into());
            editing.saves
        });
        type_at_start(&f, visual, "b");
        wait(visual, 1200);
        assert_eq!(on_disk(&f), "original", "the timer found the save running");
        // That save completes (it wrote «aoriginal»).
        std::fs::write(&f.file, "aoriginal").unwrap();
        f.reader.update(visual, |r, cx| {
            r.autosaved(
                input_id,
                saves,
                "aoriginal".into(),
                Ok(Save::Saved),
                None,
                cx,
            )
        });
        visual.run_until_parked();
        wait(visual, 1200);
        assert_eq!(
            on_disk(&f),
            "baoriginal",
            "the edit made during the save is saved"
        );
    }

    /// Start an autosave of `text` by hand and let its worker "write" it, the
    /// way a watcher can see the file before the save is back (#1376).
    fn autosave_in_flight(f: &Fixture, visual: &mut VisualTestContext, text: &str) -> u64 {
        let saves = f.reader.update(visual, |r, _| {
            let editing = r.editing.as_mut().unwrap();
            editing.autosaving = Some(text.into());
            editing.saves
        });
        std::fs::write(&f.file, text).unwrap();
        saves
    }

    fn autosave_back(f: &Fixture, visual: &mut VisualTestContext, saves: u64, text: &str) {
        let input_id = input(f, visual).entity_id();
        f.reader.update(visual, |r, cx| {
            r.autosaved(input_id, saves, text.into(), Ok(Save::Saved), None, cx)
        });
        visual.run_until_parked();
    }

    fn refresh(f: &Fixture, visual: &mut VisualTestContext) {
        f.reader.update_in(visual, |r, window, cx| {
            r.refresh_source_from_disk(window, cx)
        });
        visual.run_until_parked();
    }

    /// #1376: a refresh that lands while an autosave is writing finds that
    /// save's text on disk and newer typing in the buffer. It is our own
    /// write, not a conflict: no banner, and every later edit is saved.
    #[gpui::test]
    fn a_refresh_during_an_autosave_is_not_a_conflict(cx: &mut TestAppContext) {
        let (f, visual) = open(cx, "original");
        type_at_start(&f, visual, "a");
        let saves = autosave_in_flight(&f, visual, "aoriginal");
        type_at_start(&f, visual, "b");
        refresh(&f, visual);
        f.reader.read_with(visual, |r, _| {
            let editing = r.editing.as_ref().unwrap();
            assert!(!editing.conflict_detected, "our own save is not a conflict");
            assert!(r.link_notice.is_none(), "{:?}", r.link_notice);
        });
        autosave_back(&f, visual, saves, "aoriginal");
        wait(visual, 1200);
        assert_eq!(on_disk(&f), "baoriginal", "the edit after it is saved too");
        // Twenty keys, each a pause apart, with a refresh racing every save.
        for _ in 0..20 {
            type_at_start(&f, visual, "x");
            let text = input(&f, visual).read_with(visual, |v, _| v.value().to_string());
            let saves = autosave_in_flight(&f, visual, &text);
            refresh(&f, visual);
            autosave_back(&f, visual, saves, &text);
        }
        // And the real autosave still runs after all of them.
        type_at_start(&f, visual, "end ");
        wait(visual, 1200);
        assert_eq!(on_disk(&f), format!("end {}baoriginal", "x".repeat(20)));
        f.reader.read_with(visual, |r, cx| {
            let editing = r.editing.as_ref().unwrap();
            assert!(!editing.conflict_detected);
            assert!(r.link_notice.is_none(), "{:?}", r.link_notice);
            assert!(!r.source_is_dirty(cx));
        });
    }

    /// A real outside change made while an autosave is writing is still a
    /// conflict once the save is back, and both versions are kept.
    #[gpui::test]
    fn an_outside_change_during_an_autosave_is_still_a_conflict(cx: &mut TestAppContext) {
        let (f, visual) = open(cx, "original");
        type_at_start(&f, visual, "a");
        let saves = autosave_in_flight(&f, visual, "aoriginal");
        type_at_start(&f, visual, "b");
        std::fs::write(&f.file, "theirs").unwrap();
        refresh(&f, visual);
        autosave_back(&f, visual, saves, "aoriginal");
        wait(visual, 1200);
        assert_eq!(on_disk(&f), "theirs", "the outside version stays");
        f.reader.read_with(visual, |r, cx| {
            let editing = r.editing.as_ref().unwrap();
            assert!(editing.conflict_detected, "the deferred refresh saw it");
            assert_eq!(editing.conflict.as_deref(), Some("theirs"));
            assert_eq!(editing.input.read(cx).value().as_ref(), "baoriginal");
            assert!(r.link_notice.is_some());
        });
    }

    /// A conflict whose outside change is gone again (the file holds the base
    /// once more) no longer blocks autosave, and its banner goes (#1376).
    #[gpui::test]
    fn a_conflict_that_went_away_lets_autosave_go_on(cx: &mut TestAppContext) {
        let (f, visual) = open(cx, "original");
        type_at_start(&f, visual, "mine ");
        std::fs::write(&f.file, "theirs").unwrap();
        wait(visual, 1200);
        f.reader.read_with(visual, |r, _| {
            assert!(r.editing.as_ref().unwrap().conflict_detected)
        });
        std::fs::write(&f.file, "original").unwrap();
        refresh(&f, visual);
        f.reader.read_with(visual, |r, _| {
            assert!(!r.editing.as_ref().unwrap().conflict_detected);
            assert!(r.link_notice.is_none(), "{:?}", r.link_notice);
        });
        wait(visual, 1200);
        assert_eq!(on_disk(&f), "mine original", "the waiting edit is saved");
    }

    /// A save that fails keeps the text, says so, and is not retried in a loop.
    #[cfg(unix)]
    #[gpui::test]
    fn a_failed_autosave_keeps_the_text_and_says_so(cx: &mut TestAppContext) {
        use std::os::unix::fs::PermissionsExt;
        let (f, visual) = open(cx, "original");
        let folder = f.file.parent().unwrap().to_owned();
        type_at_start(&f, visual, "mine ");
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
        wait(visual, 1200);
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
        if on_disk(&f) == "mine original" {
            // Running as a user that ignores directory permissions.
            return;
        }
        f.reader.read_with(visual, |r, cx| {
            let editing = r.editing.as_ref().unwrap();
            assert!(editing.save_failed, "the failure is shown");
            assert!(r.link_notice.is_some());
            assert_eq!(editing.input.read(cx).value().as_ref(), "mine original");
        });
        wait(visual, 3000);
        assert_eq!(on_disk(&f), "original", "no retry loop until the next edit");
    }
}

#[cfg(test)]
mod mode_navigation_tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn fixture(
        cx: &mut TestAppContext,
    ) -> (Entity<Reader>, &mut VisualTestContext, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        for (path, text) in [
            ("first.md", "# First\r\n\r\n**one** e\u{301} 🧠\r\n"),
            ("second.md", "# Second\n\n[[first|one]]\n"),
            ("third.md", "# Third\n\n| a | b |\n| - | - |\n| c | d |\n"),
            ("plain.txt", "Plain text follows the chosen editing mode.\n"),
            ("table.csv", "a,b\n1,2\n"),
            ("tool.py", "print('code follows the chosen editing mode')\n"),
        ] {
            std::fs::write(root.join(path), text).unwrap();
        }
        // Neither text nor code: always a read-only preview.
        std::fs::write(root.join("asset.bin"), [0u8, 159, 146, 150]).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("first.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        visual.run_until_parked();
        (reader.unwrap(), visual, temp)
    }

    fn assert_mode(reader: &Reader, expected: &str, live: Option<bool>, cx: &App) {
        assert_eq!(
            reader.selected_file(),
            expected,
            "actual selected file positive control"
        );
        match live {
            None => assert!(reader.editing.is_none(), "explicit Read must remain Read"),
            Some(live) => {
                let editing = reader
                    .editing
                    .as_ref()
                    .expect("chosen editing mode must survive the real file-switch pipeline");
                assert_eq!(reader.source_live_preview(), live);
                assert_eq!(editing.store.path(), reader.vault_root.join(expected));
                assert_eq!(
                    editing.input.read(cx).value().as_ref(),
                    std::fs::read_to_string(editing.store.path()).unwrap()
                );
            }
        }
    }

    #[gpui::test]
    fn new_note_keeps_read_mode_for_later_file_choices(cx: &mut TestAppContext) {
        let (reader, visual, temp) = fixture(cx);
        reader.update_in(visual, |r, window, cx| {
            assert_mode(r, "first.md", None, cx);
            r.new_note(None, window, cx);
        });
        visual.run_until_parked();
        visual.simulate_input("Created from Read");
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert_mode(r, "Created from Read.md", Some(true), cx);
            assert!(r
                .editing
                .as_ref()
                .unwrap()
                .input
                .focus_handle(cx)
                .is_focused(window));
            r.open_note("second.md", None, window, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| assert_mode(r, "second.md", None, cx));
        reader.update_in(visual, |r, window, cx| {
            r.open_note("third.md", None, window, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| assert_mode(r, "third.md", None, cx));
        assert_eq!(
            std::fs::read_to_string(temp.path().join("vault/Created from Read.md")).unwrap(),
            "# Created from Read\n"
        );
    }

    #[gpui::test]
    fn sidebar_mode_reopen_keeps_rename_keys_out_of_editor(cx: &mut TestAppContext) {
        let (reader, visual, temp) = fixture(cx);
        visual.simulate_resize(size(px(1400.), px(860.)));
        reader.update_in(visual, |r, window, cx| {
            r.toggle_source(window, cx);
            r.set_live_preview(false, window, cx);
            r.reveal_in_tree("second.md", window, cx);
        });
        visual.run_until_parked();
        let before = std::fs::read_to_string(temp.path().join("vault/second.md")).unwrap();
        let row = visual.debug_bounds("tree-row-second.md").unwrap();
        visual.simulate_click(
            point(row.left() + px(70.), row.center().y),
            Modifiers::default(),
        );
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert_mode(r, "second.md", Some(false), cx);
            assert!(
                r.tree_focus.is_focused(window),
                "async reopen must retain tree focus"
            );
        });
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            assert!(
                r.renaming.is_some(),
                "Enter reaches the actual rename action"
            );
            assert_eq!(
                r.editing.as_ref().unwrap().input.read(cx).value().as_ref(),
                before
            );
            assert!(!r.source_is_dirty(cx), "tree keys must not edit the note");
        });
        assert_eq!(
            std::fs::read_to_string(temp.path().join("vault/second.md")).unwrap(),
            before
        );
    }

    #[gpui::test]
    fn selected_mode_follows_real_file_choices_and_latest_request(cx: &mut TestAppContext) {
        let (reader, visual, _temp) = fixture(cx);
        reader.read_with(visual, |r, cx| assert_mode(r, "first.md", None, cx));
        reader.update_in(visual, |r, window, cx| {
            r.toggle_source(window, cx); // Actual explicit Source choice.
            r.set_live_preview(false, window, cx);
            assert_mode(r, "first.md", Some(false), cx);
            r.open_note("second.md", None, window, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| assert_mode(r, "second.md", Some(false), cx));
        reader.update_in(visual, |r, window, cx| {
            r.set_live_preview(true, window, cx); // Actual explicit LP choice.
            r.open_note("third.md", None, window, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| assert_mode(r, "third.md", Some(true), cx));
        reader.update_in(visual, |r, window, cx| {
            r.open_note("third.md", None, window, cx)
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| assert_mode(r, "third.md", Some(true), cx));
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx)); // Explicit Read.
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| assert_mode(r, "third.md", None, cx));
        reader.update_in(visual, |r, window, cx| {
            r.open_note("first.md", None, window, cx)
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| assert_mode(r, "first.md", None, cx));
        reader.update_in(visual, |r, window, cx| {
            r.toggle_source(window, cx);
            r.set_live_preview(false, window, cx);
            r.open_note("second.md", None, window, cx);
            r.open_note("third.md", None, window, cx); // Supersedes before the first worker is polled.
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| assert_mode(r, "third.md", Some(false), cx));
    }

    #[gpui::test]
    fn selected_mode_keeps_preview_fallback_and_failed_target_without_forced_editing(
        cx: &mut TestAppContext,
    ) {
        let (reader, visual, temp) = fixture(cx);
        reader.update_in(visual, |r, window, cx| {
            r.open_live_preview(window, cx);
            assert_mode(r, "first.md", Some(true), cx);
            r.open_note("asset.bin", None, window, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            assert_mode(r, "asset.bin", None, cx);
            assert!(
                r.file_preview.is_some(),
                "actual unsupported-Markdown preview fallback"
            );
        });
        reader.update_in(visual, |r, window, cx| {
            r.open_note("second.md", None, window, cx)
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| assert_mode(r, "second.md", Some(true), cx));
        reader.update_in(visual, |r, window, cx| {
            r.set_live_preview(false, window, cx);
            r.open_note("missing.md", None, window, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert_eq!(r.current_rel, "second.md");
            assert!(
                r.link_notice.is_some(),
                "failed prepare reports absence rather than fabricating an editor"
            );
            assert!(r.editing.is_none());
        });
        reader.update_in(visual, |r, window, cx| {
            r.open_note("third.md", None, window, cx)
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| assert_mode(r, "third.md", Some(false), cx));
        assert!(!temp.path().join("vault/missing.md").exists());
    }
    /// GitHub #6: a file chosen from the list kept Read for text, CSV and
    /// code files, so the chosen editing mode was lost on the way.
    #[gpui::test]
    fn chosen_editing_mode_carries_to_editable_files_and_back(cx: &mut TestAppContext) {
        let (reader, visual, _temp) = fixture(cx);
        reader.update_in(visual, |r, window, cx| {
            r.toggle_source(window, cx);
            r.set_live_preview(true, window, cx);
            assert_mode(r, "first.md", Some(true), cx);
            r.open_note("plain.txt", None, window, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            assert!(r.file_preview.is_some(), "positive control: a file view");
            assert_mode(r, "plain.txt", Some(false), cx);
        });
        reader.update_in(visual, |r, window, cx| {
            r.open_note("table.csv", None, window, cx)
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| assert_mode(r, "table.csv", Some(false), cx));
        reader.update_in(visual, |r, window, cx| {
            r.open_note("tool.py", None, window, cx)
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            assert!(
                r.file_preview
                    .as_ref()
                    .is_some_and(|preview| preview.code.is_some()),
                "positive control: the code view, which loads asynchronously"
            );
            assert_mode(r, "tool.py", Some(false), cx);
        });
        // Back to a note: Live Preview, not the code editor's plain Source.
        reader.update_in(visual, |r, window, cx| {
            r.open_note("second.md", None, window, cx)
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| assert_mode(r, "second.md", Some(true), cx));
        reader.update_in(visual, |r, window, cx| {
            r.set_live_preview(false, window, cx);
            r.open_note("plain.txt", None, window, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert_mode(r, "plain.txt", Some(false), cx);
            r.open_note("third.md", None, window, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| assert_mode(r, "third.md", Some(false), cx));
        // Explicit Read on a file is kept for the next files and notes.
        reader.update_in(visual, |r, window, cx| {
            r.open_note("table.csv", None, window, cx)
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert_mode(r, "table.csv", Some(false), cx);
            r.toggle_source(window, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert_mode(r, "table.csv", None, cx);
            r.open_note("plain.txt", None, window, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert_mode(r, "plain.txt", None, cx);
            r.open_note("tool.py", None, window, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert_mode(r, "tool.py", None, cx);
            r.open_note("first.md", None, window, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| assert_mode(r, "first.md", None, cx));
    }

    /// Leaving a text file's editor must not replace the notes' Live Preview
    /// choice with that editor's plain Source.
    #[gpui::test]
    fn text_file_editor_keeps_note_live_preview_choice(cx: &mut TestAppContext) {
        let (reader, visual, _temp) = fixture(cx);
        reader.update_in(visual, |r, window, cx| {
            r.open_live_preview(window, cx);
            assert_mode(r, "first.md", Some(true), cx);
            r.toggle_source(window, cx); // Explicit Read.
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert_mode(r, "first.md", None, cx);
            r.open_note("plain.txt", None, window, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert_mode(r, "plain.txt", None, cx);
            r.toggle_source(window, cx); // Explicit Edit of the text file.
            assert_mode(r, "plain.txt", Some(false), cx);
            r.open_note("second.md", None, window, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| assert_mode(r, "second.md", Some(true), cx));
    }

    #[gpui::test]
    fn tree_choice_of_text_file_keeps_mode_and_tree_focus(cx: &mut TestAppContext) {
        let (reader, visual, temp) = fixture(cx);
        visual.simulate_resize(size(px(1400.), px(860.)));
        reader.update_in(visual, |r, window, cx| {
            r.toggle_source(window, cx);
            r.set_live_preview(false, window, cx);
            r.reveal_in_tree("plain.txt", window, cx);
        });
        visual.run_until_parked();
        let row = visual.debug_bounds("tree-row-plain.txt").unwrap();
        visual.simulate_click(
            point(row.left() + px(70.), row.center().y),
            Modifiers::default(),
        );
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert_mode(r, "plain.txt", Some(false), cx);
            assert!(
                r.tree_focus.is_focused(window),
                "the reopened editor must not take tree keys"
            );
            assert!(!r.source_is_dirty(cx));
        });
        assert_eq!(
            std::fs::read_to_string(temp.path().join("vault/plain.txt")).unwrap(),
            "Plain text follows the chosen editing mode.\n"
        );
    }
}
