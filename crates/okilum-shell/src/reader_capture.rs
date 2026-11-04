//! Local capture composer and explicit menu entrypoint; all draft/save IO is serial
//! on the background executor. No reader or Brain protocol is extended.
use super::*;
use gpui_component::{
    input::{Textarea, TextareaState},
    menu::PopupMenuItem,
    WindowExt as _,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};
#[cfg(test)]
mod mounted_tests;
mod namespace;
use namespace::{Binding, PreparedNamespace, Snapshot};

type EntryKey = (WindowId, PathBuf);
// A non-owning singleton index, not a second owner/controller. The sole composer
// retains its own entity/tasks until its durable CaptureClosed event.
thread_local! { static ENTRIES: RefCell<HashMap<EntryKey, EntrySlot>> = RefCell::new(HashMap::new()); }
struct EntrySlot {
    reader: WeakEntity<Reader>,
    snapshot: Snapshot,
    generation: String,
    composer: Option<WeakEntity<CaptureComposer>>,
    task: Option<Task<()>>,
}
fn binding(reader: &Reader) -> Binding {
    let mut indexes: Vec<PathBuf> = reader.index_dir.iter().cloned().collect();
    if let Some(lease) = &reader.cache_lease {
        indexes.push(lease.path.clone());
    }
    Binding {
        root: reader.vault_root.clone(),
        vault_root: reader.vault.root.clone(),
        session: reader.session_directory.clone(),
        indexes,
        single_file: reader.single_file,
        ready: !reader.loading.as_ref().is_some_and(|load| load.active)
            && !reader.incremental_initializing
            && reader
                .cache_lease
                .as_ref()
                .is_none_or(|lease| lease.root == reader.vault_root),
    }
}
fn current(reader: &WeakEntity<Reader>, snapshot: &Snapshot, cx: &App) -> anyhow::Result<()> {
    let entity = reader
        .upgrade()
        .ok_or_else(|| anyhow::anyhow!("The captured Reader was closed"))?;
    snapshot.matches(&binding(entity.read(cx)))
}
fn notice(error: impl std::fmt::Display, window: &mut Window, cx: &mut App) {
    reader_toast::transient(format!("Quick capture unavailable: {error}"), window, cx);
}

/// Parent's only menu hook. Snapshot the actual Reader/root/session at MENU OPEN.
/// Namespace provisioning and composer publication require the explicit click.
pub(crate) fn menu_item(
    reader: WeakEntity<Reader>,
    captured_root: PathBuf,
    cx: &App,
) -> PopupMenuItem {
    let snapshot = reader
        .upgrade()
        .ok_or_else(|| anyhow::anyhow!("The captured Reader was closed"))
        .and_then(|entity| Snapshot::capture(captured_root, &binding(entity.read(cx))))
        .map_err(|error| format!("{error:#}"));
    PopupMenuItem::new("Quick capture…").on_click(move |_, window, cx| {
        let snapshot = match &snapshot {
            Ok(snapshot) => snapshot.clone(),
            Err(error) => {
                notice(error, window, cx);
                return;
            }
        };
        if let Err(error) = current(&reader, &snapshot, cx) {
            notice(error, window, cx);
            return;
        }
        let key = (window.window_handle().window_id(), snapshot.root.clone());
        let retained = ENTRIES.with(|entries| {
            let entries = entries.borrow();
            let Some(slot) = entries.get(&key) else {
                return Ok(None);
            };
            anyhow::ensure!(
                slot.reader.entity_id() == reader.entity_id() && slot.snapshot == snapshot,
                "A capture is retained for an earlier Reader/session; it will not be retargeted"
            );
            anyhow::ensure!(
                slot.task.is_none(),
                "Namespace preparation is already running for this window/vault"
            );
            Ok(slot.composer.as_ref().and_then(|view| view.upgrade()))
        });
        let retained: Option<Entity<CaptureComposer>> = match retained {
            Ok(view) => view,
            Err(error) => {
                notice(error, window, cx);
                return;
            }
        };
        if window.has_active_dialog(cx) {
            if let Some(view) = &retained {
                let state = view.read(cx);
                if state.entry_visible.get() && state.entry_focus.contains_focused(window, cx) {
                    Focusable::focus_handle(state.text.read(cx), cx).focus(window, cx);
                    return;
                }
            }
            notice(
                "Close the current dialog first; any existing capture remains retained",
                window,
                cx,
            );
            return;
        }
        let generation = uuid::Uuid::new_v4().to_string();
        ENTRIES.with(|entries| {
            entries.borrow_mut().insert(
                key.clone(),
                EntrySlot {
                    reader: reader.clone(),
                    snapshot: snapshot.clone(),
                    generation: generation.clone(),
                    composer: retained.as_ref().map(Entity::downgrade),
                    task: None,
                },
            );
        });
        let source = snapshot.clone();
        let existing_namespace = retained
            .as_ref()
            .and_then(|view| view.read(cx).entry_namespace.clone());
        let work = cx.background_executor().spawn(async move {
            if let Some(namespace) = existing_namespace {
                namespace.validate()?;
                Ok(namespace)
            } else {
                source.prepare().map(Arc::new)
            }
        });
        let asking_window = window.window_handle();
        let task_key = key.clone();
        let owner = reader.clone();
        let token = generation.clone();
        let task = cx.spawn(async move |cx| {
            let result: anyhow::Result<Arc<PreparedNamespace>> = work.await;
            #[cfg(test)]
            let test_done = mounted_tests::pause_prepared(
                &snapshot.root,
                result.as_ref().err().map(|error| format!("{error:#}")),
            )
            .await;
            let delivered = asking_window.update(cx, |_, window, cx| {
                let task = ENTRIES.with(|entries| {
                    let mut entries = entries.borrow_mut();
                    let slot = entries.get_mut(&task_key)?;
                    if slot.generation != token {
                        return None;
                    }
                    slot.task.take()
                });
                let Some(task) = task else {
                    notice(
                        "Preparation completion is stale; retained capture was not replaced",
                        window,
                        cx,
                    );
                    return;
                };
                let prepared = match result.and_then(|prepared| {
                    current(&owner, &snapshot, cx)?;
                    anyhow::ensure!(
                        prepared.root.as_os_str() == snapshot.root.as_os_str()
                            && prepared.session.as_os_str() == snapshot.session.as_os_str(),
                        "Prepared namespace differs from captured session"
                    );
                    Ok(prepared)
                }) {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        if retained.is_none() {
                            ENTRIES.with(|entries| {
                                entries.borrow_mut().remove(&task_key);
                            });
                        }
                        task.detach();
                        notice(error, window, cx);
                        return;
                    }
                };
                let view = retained.unwrap_or_else(|| {
                    cx.new(|cx| {
                        CaptureComposer::new(
                            owner.clone(),
                            prepared.root.clone(),
                            prepared.drafts.clone(),
                            prepared.records.clone(),
                            window,
                            cx,
                        )
                    })
                });
                view.update(cx, |this, cx| {
                    this.entry_snapshot = Some(snapshot.clone());
                    this.entry_namespace = Some(prepared);
                    this.entry_key = Some(task_key.clone());
                    this.entry_task = Some(task);
                    this.entry_retained = Some(cx.entity());
                    if this.entry_closed.is_none() {
                        this.entry_closed = Some(cx.subscribe_in(
                            &cx.entity(),
                            window,
                            |this, _, _: &CaptureClosed, window, cx| {
                                this.entry_close_ready = true;
                                this.finish_entry_close(window, cx);
                            },
                        ));
                    }
                });
                ENTRIES.with(|entries| {
                    if let Some(slot) = entries.borrow_mut().get_mut(&task_key) {
                        slot.composer = Some(view.downgrade());
                    }
                });
                show_composer(view, window, cx);
            });
            #[cfg(test)]
            if let Some(done) = test_done {
                let _ = done.send(());
            }
            if delivered.is_err() {
                ENTRIES.with(|entries| {
                    let mut entries = entries.borrow_mut();
                    if let Some(slot) = entries.get_mut(&task_key) {
                        if slot.generation == token {
                            if let Some(task) = slot.task.take() {
                                task.detach();
                            }
                            if slot.composer.is_none() {
                                entries.remove(&task_key);
                            }
                        }
                    }
                });
            }
        });
        ENTRIES.with(|entries| {
            if let Some(slot) = entries.borrow_mut().get_mut(&key) {
                if slot.generation == generation {
                    slot.task = Some(task);
                }
            }
        });
    })
}
struct DialogLease(Rc<Cell<bool>>);
impl Drop for DialogLease {
    fn drop(&mut self) {
        self.0.set(false);
    }
}
fn show_composer(view: Entity<CaptureComposer>, window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        notice("Another dialog opened during preparation; capture is retained. Close it and choose Quick capture again", window, cx);
        return;
    }
    let visible = view.read(cx).entry_visible.clone();
    visible.set(true);
    let lease = Rc::new(DialogLease(visible));
    let cancel = view.downgrade();
    let content = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let _lease = lease.clone();
        let cancel = cancel.clone();
        dialog
            .title("Quick capture")
            .width(px(640.))
            .overlay_closable(false)
            .keyboard(false)
            .close_button(false)
            .on_ok(|_, _, _| false)
            .on_cancel(move |_, _, cx| {
                let _ = cancel.update(cx, |this, cx| this.request_close(cx));
                false
            })
            .child(
                div()
                    .id("capture-composer-dialog")
                    .debug_selector(|| "capture-composer-dialog".into())
                    .max_h(px(620.))
                    .overflow_y_scroll()
                    .child(content.clone()),
            )
            .footer(div())
    });
    Focusable::focus_handle(view.read(cx).text.read(cx), cx).focus(window, cx);
}

use okilum_core::capture::{self, ComposerDraft, CopyPreview, CopySource, Receipt, RecoveryInbox};

/// Emitted after a durable checkpoint when Cancel closes the mounted composer.
pub(crate) struct CaptureClosed;

pub(crate) struct CaptureComposer {
    draft: ComposerDraft,
    text: Entity<TextareaState>,
    folder: Entity<InputState>,
    reader: WeakEntity<Reader>,
    root: PathBuf,
    recovered: Vec<ComposerDraft>,
    dismissed_sources: Vec<capture::DismissedRecovery>,
    dismissed_inspection: Option<usize>,
    presentation_history: Option<capture::PresentationHistory>,
    unbound: Vec<CopySource>,
    legacy_selected: bool,
    copy_source: Option<CopySource>,
    copy_preview: Option<CopyPreview>,
    copy_folder: Entity<InputState>,
    copy_folder_chosen: bool,
    saved: Option<Receipt>,
    saved_recovery_copy: Option<ComposerDraft>,
    error: Option<String>,
    checkpoint_error: bool,
    busy: bool,
    loading: bool,
    generation: u64,
    persisted: Option<u64>,
    queue: async_channel::Sender<Work>,
    _inputs: Vec<Subscription>,
    worker: Option<Task<()>>,
    entry_snapshot: Option<Snapshot>,
    entry_namespace: Option<Arc<PreparedNamespace>>,
    entry_key: Option<EntryKey>,
    entry_task: Option<Task<()>>,
    entry_closed: Option<Subscription>,
    entry_retained: Option<Entity<Self>>,
    entry_visible: Rc<Cell<bool>>,
    entry_focus: FocusHandle,
    entry_close_ready: bool,
}
impl EventEmitter<CaptureClosed> for CaptureComposer {}

enum PreviewSource {
    Composer(Box<ComposerDraft>),
    Legacy(CopySource),
}

enum Work {
    Recover,
    InspectDismissed,
    InspectHistory,
    Reopen(capture::PresentationRevision),
    Dismiss(ComposerDraft),
    Preview(PreviewSource, PathBuf),
    Checkpoint(ComposerDraft, u64),
    Save(ComposerDraft),
    Close(ComposerDraft),
}

enum Completed {
    StorageHeld(String),
    Recovery(anyhow::Result<RecoveryInbox>),
    Inspected(anyhow::Result<Vec<capture::DismissedRecovery>>),
    History(anyhow::Result<capture::PresentationHistory>),
    Reopened(anyhow::Result<RecoveryInbox>),
    Dismissed(anyhow::Result<RecoveryInbox>),
    Preview(anyhow::Result<CopyPreview>),
    Checkpoint(u64, anyhow::Result<()>),
    Save(anyhow::Result<Receipt>),
    Close(anyhow::Result<()>),
}

impl CaptureComposer {
    /// `records` is a durable per-vault operational directory outside the index.
    /// Mount only for an explicitly opened vault, using its existing draft store.
    pub(crate) fn new(
        reader: WeakEntity<Reader>,
        root: PathBuf,
        drafts: PathBuf,
        records: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let text = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(4, 12)
                .placeholder("Write a thought to capture…")
        });
        let folder = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Vault-relative folder; blank uses the vault root")
        });
        let copy_folder = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Choose a vault-relative folder, or click Use vault root")
        });
        let copy_folder_input = cx.subscribe(&copy_folder, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) && !this.busy {
                this.copy_folder_chosen = true;
                this.copy_preview = None;
                cx.notify();
            }
        });
        let text_input = cx.subscribe(&text, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change)
                && !this.busy
                && !this.loading
                && !this.legacy_selected
                && !this.entry_close_ready
            {
                this.draft.text = this.text.read(cx).value().to_string();
                this.checkpoint(cx);
            }
        });
        let folder_input = cx.subscribe(&folder, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change)
                && !this.busy
                && !this.loading
                && !this.legacy_selected
                && !this.entry_close_ready
            {
                this.draft.folder = Some(PathBuf::from(this.folder.read(cx).value().as_str()));
                this.checkpoint(cx);
            }
        });
        let (queue, receiver) = async_channel::unbounded();
        let worker_root = root.clone();
        let worker = cx.spawn_in(window, async move |this, cx| {
            while let Ok(work) = receiver.recv().await {
                let root = worker_root.clone();
                let drafts = drafts.clone();
                let records = records.clone();
                let namespace = this.read_with(cx, |this, _| this.entry_namespace.clone()).ok().flatten();
                let completed = cx.background_executor().spawn(async move {
                    if let Some(namespace) = namespace { if let Err(error) = namespace.validate() { return Completed::StorageHeld(format!("Capture storage identity changed; draft retained: {error:#}")); } }
                    match work {
                        Work::InspectHistory => Completed::History(capture::inspect_presentation_history(&root, &records)),
                        Work::InspectDismissed => Completed::Inspected(capture::inspect_dismissed(&root, &records)),
                        Work::Reopen(revision) => Completed::Reopened(capture::reopen_dismissed(&root, &records, &revision).and_then(|_| capture::load_recovery(&root, &records))),
                        Work::Recover => Completed::Recovery(capture::load_recovery(&root, &records)),
                        Work::Dismiss(copy) => Completed::Dismissed(capture::dismiss_saved_copy(&root, &records, &copy).and_then(|_| capture::load_recovery(&root, &records))),
                        Work::Preview(source, folder) => Completed::Preview((|| {
                            let source = match source {
                                PreviewSource::Composer(draft) => CopySource::from_composer(&root, &records, &draft)?,
                                PreviewSource::Legacy(source) => source,
                            };
                            source.preview(&root, &folder, &records)
                        })()),
                        Work::Checkpoint(draft, generation) => Completed::Checkpoint(generation, draft.persist(&root, &records)),
                        Work::Close(draft) => Completed::Close(draft.persist(&root, &records)),
                        Work::Save(draft) => Completed::Save(draft.publish(&root, &drafts, &records)),
                    }
                }).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    match completed {
                        Completed::StorageHeld(error) => { this.busy = false; this.loading = false; this.error = Some(error); }
                        Completed::Recovery(result) => {
                            this.loading = false;
                            match result {
                                Ok(inbox) => {
                                    this.recovered = inbox.drafts;
                                    this.unbound = inbox.unbound;
                                },
                                Err(error) => this.error = Some(format!("Could not recover capture drafts: {error:#}")),
                            }
                        }
                        Completed::Dismissed(result) => {
                            this.presentation_history = None;
                            this.busy = false;
                            match result {
                                Ok(inbox) => {
                                    this.recovered = inbox.drafts;
                                    this.unbound = inbox.unbound;
                                    this.saved_recovery_copy = None;
                                    this.error = None;
                                }
                                Err(error) => this.error = Some(format!("Recovery source was not dismissed: {error:#}")),
                            }
                        }
                        Completed::History(result) => {
                            this.busy = false;
                            match result {
                                Ok(history) => { this.presentation_history = Some(history); this.error = None; }
                                Err(error) => this.error = Some(format!("Presentation history held: {error:#}")),
                            }
                        }
                        Completed::Inspected(result) => {
                            this.busy = false;
                            match result {
                                Ok(sources) => { this.dismissed_sources = sources; this.dismissed_inspection = None; this.error = None; }
                                Err(error) => this.error = Some(format!("Dismissed recovery inspection failed: {error:#}")),
                            }
                        }
                        Completed::Reopened(result) => {
                            this.presentation_history = None;
                            this.busy = false;
                            match result {
                                Ok(inbox) => { this.recovered = inbox.drafts; this.unbound = inbox.unbound; this.dismissed_sources.clear(); this.dismissed_inspection = None; this.error = None; }
                                Err(error) => this.error = Some(format!("Source was not reopened. Inspect again before retrying: {error:#}")),
                            }
                        }
                        Completed::Preview(result) => {
                            this.busy = false;
                            match result {
                                Ok(preview) => {
                                    this.copy_source = Some(preview.source().clone());
                                    this.copy_preview = Some(preview);
                                    this.error = None;
                                }
                                Err(error) => this.error = Some(format!("Copy preview could not be prepared: {error:#}")),
                            }
                        }
                        Completed::Checkpoint(generation, result) => match result {
                            Ok(()) => {
                                this.persisted = Some(generation);
                                if generation == this.generation && this.checkpoint_error {
                                    this.error = None;
                                    this.checkpoint_error = false;
                                }
                            }
                            Err(error) => {
                                this.checkpoint_error = true;
                                this.error = Some(format!("Draft is not safely stored yet. Text is retained here: {error:#}"));
                            },
                        },
                        Completed::Save(result) => {
                            this.busy = false;
                            this.checkpoint_error = false;
                            match result {
                                Ok(receipt) => {
                                    let relative = okilum_core::vault::note_path(&receipt.path);
                                    let root = this.root.clone();
                                    let _ = this.reader.update(cx, |reader, cx| {
                                        if reader.vault_root == root {
                                            Arc::make_mut(&mut reader.vault).register_created_note(&relative);
                                            reader.tree.entry_created(&relative, okilum_core::vault::EntryKind::Markdown);
                                            reader.queue_vault_mutation(okilum_core::Changes {
                                                changed: std::collections::BTreeSet::from([relative]),
                                                ..Default::default()
                                            }, cx);
                                            cx.notify();
                                        }
                                    });
                                    this.saved_recovery_copy = this.draft.copy_origin.as_ref().map(|_| this.draft.clone());
                                    this.saved = Some(receipt);
                                    this.copy_source = None;
                                    this.copy_preview = None;
                                    this.copy_folder_chosen = false;
                                    this.legacy_selected = false;
                                    this.draft = ComposerDraft { folder: this.draft.folder.clone(), ..Default::default() };
                                    this.text.update(cx, |input, cx| input.set_value("", window, cx));
                                    this.error = None;
                                    this.persisted = None;
                                }
                                Err(error) => this.error = Some(format!("Capture was not acknowledged. Retry keeps the same destination and text: {error:#}")),
                            }
                        }
                        Completed::Close(result) => {
                            this.busy = false;
                            this.checkpoint_error = false;
                            match result {
                                Ok(()) => {
                                    this.busy = this.entry_key.is_some();
                                    cx.emit(CaptureClosed);
                                },
                                Err(error) => this.error = Some(format!("Could not retain the draft. Keep this composer open: {error:#}")),
                            }
                        }
                    }
                    cx.notify();
                });
                // Drain queued checkpoints even if the mounted view was dropped.
            }
        });
        // Recovery runs before any newly queued checkpoint or save.
        let _ = queue.try_send(Work::Recover);
        Self {
            draft: ComposerDraft {
                folder: Some(PathBuf::new()),
                ..Default::default()
            },
            text,
            folder,
            reader,
            root,
            recovered: Vec::new(),
            dismissed_sources: Vec::new(),
            dismissed_inspection: None,
            presentation_history: None,
            unbound: Vec::new(),
            legacy_selected: false,
            copy_source: None,
            copy_preview: None,
            copy_folder,
            copy_folder_chosen: false,
            saved: None,
            saved_recovery_copy: None,
            error: None,
            checkpoint_error: false,
            busy: false,
            loading: true,
            generation: 0,
            persisted: None,
            queue,
            _inputs: vec![text_input, folder_input, copy_folder_input],
            worker: Some(worker),
            entry_snapshot: None,
            entry_namespace: None,
            entry_key: None,
            entry_task: None,
            entry_closed: None,
            entry_retained: None,
            entry_visible: Rc::new(Cell::new(false)),
            entry_focus: cx.focus_handle(),
            entry_close_ready: false,
        }
    }

    fn checkpoint(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.enqueue(Work::Checkpoint(self.draft.clone(), self.generation), cx);
    }

    fn enqueue(&mut self, work: Work, cx: &mut Context<Self>) {
        if self.queue.try_send(work).is_err() {
            self.busy = false;
            self.error = Some("Capture worker stopped. Your text is retained here; reopen the composer to recover stored drafts.".into());
        }
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        if !self
            .reader
            .upgrade()
            .is_some_and(|reader| reader.read(cx).vault_root == self.root)
        {
            self.checkpoint_error = false;
            self.error = Some("The owning Reader changed vaults or closed. Reopen this capture vault before saving; your draft is retained.".into());
            cx.notify();
            return;
        }
        if self.legacy_selected {
            self.error = Some("This legacy intent has no vault binding. Choose a copy folder and preview the destination before Save copy.".into());
            cx.notify();
            return;
        }
        if let Some(snapshot) = &self.entry_snapshot {
            if let Err(error) = current(&self.reader, snapshot, cx) {
                self.error = Some(format!("{error:#}"));
                cx.notify();
                return;
            }
        }
        if let Err(error) = self.draft.prepare_save() {
            self.checkpoint_error = false;
            self.error = Some(format!("{error:#}"));
            cx.notify();
            return;
        }
        self.busy = true;
        self.enqueue(Work::Save(self.draft.clone()), cx);
    }

    fn restore(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || !self.draft.text.is_empty() || self.draft.attempt.is_some() {
            return;
        }
        self.copy_source = None;
        self.copy_preview = None;
        self.copy_folder_chosen = false;
        self.legacy_selected = false;
        self.draft = self.recovered.remove(index);
        // Suppress input checkpoints while installing one recovered snapshot.
        self.busy = true;
        self.text.update(cx, |input, cx| {
            input.set_value(self.draft.text.clone(), window, cx)
        });
        self.folder.update(cx, |input, cx| {
            input.set_value(
                self.draft
                    .folder
                    .as_deref()
                    .unwrap_or(Path::new(""))
                    .to_string_lossy()
                    .to_string(),
                window,
                cx,
            )
        });
        self.busy = false;
        self.error = None;
        self.checkpoint(cx);
    }

    fn select_legacy(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || !self.draft.text.is_empty() || self.draft.attempt.is_some() {
            return;
        }
        let source = self.unbound[index].clone();
        self.busy = true;
        self.text.update(cx, |input, cx| {
            input.set_value(source.text().to_owned(), window, cx)
        });
        self.draft.text = source.text().to_owned();
        self.draft.folder = None;
        self.copy_source = Some(source);
        self.copy_preview = None;
        self.copy_folder_chosen = false;
        self.legacy_selected = true;
        self.busy = false;
        self.error = None;
        cx.notify();
    }

    fn preview_copy(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.loading || !self.copy_folder_chosen {
            return;
        }
        let source = if self.legacy_selected {
            let Some(source) = self.copy_source.clone() else {
                return;
            };
            PreviewSource::Legacy(source)
        } else {
            if self.draft.attempt.is_none() {
                return;
            }
            PreviewSource::Composer(Box::new(self.draft.clone()))
        };
        self.busy = true;
        self.copy_preview = None;
        self.enqueue(
            Work::Preview(
                source,
                PathBuf::from(self.copy_folder.read(cx).value().as_str()),
            ),
            cx,
        );
    }

    fn save_copy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        if !self
            .reader
            .upgrade()
            .is_some_and(|reader| reader.read(cx).vault_root == self.root)
        {
            self.error = Some(
                "The selected vault changed. Reopen the capture vault and preview again.".into(),
            );
            cx.notify();
            return;
        }
        if let Some(snapshot) = &self.entry_snapshot {
            if let Err(error) = current(&self.reader, snapshot, cx) {
                self.error = Some(format!("{error:#}"));
                cx.notify();
                return;
            }
        }
        let Some(preview) = &self.copy_preview else {
            return;
        };
        match preview.start_copy() {
            Ok(draft) => {
                // This explicit click alone creates a new operation. The original
                // draft and intent keep their bytes and their operation identity.
                self.busy = true;
                self.persisted = None;
                self.generation += 1;
                self.draft = draft;
                self.legacy_selected = false;
                self.folder.update(cx, |input, cx| {
                    input.set_value(
                        self.draft
                            .folder
                            .as_ref()
                            .unwrap()
                            .to_string_lossy()
                            .to_string(),
                        window,
                        cx,
                    )
                });
                self.copy_preview = None;
                self.copy_source = None;
                self.copy_folder_chosen = false;
                self.enqueue(Work::Save(self.draft.clone()), cx);
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                cx.notify();
            }
        }
    }

    fn request_close(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            self.error =
                Some("Wait for the current capture operation; your draft remains retained.".into());
            cx.notify();
            return;
        }
        if self.legacy_selected {
            cx.emit(CaptureClosed);
            return;
        }
        self.busy = true;
        self.enqueue(Work::Close(self.draft.clone()), cx);
    }
    fn finish_entry_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.entry_close_ready {
            return;
        }
        // Do not close another modal that was opened while the checkpoint ran.
        if self.entry_visible.get() && !self.entry_focus.contains_focused(window, cx) {
            return;
        }
        if self.entry_visible.get() {
            window.close_dialog(cx);
        }
        if let Some(key) = self.entry_key.take() {
            ENTRIES.with(|entries| {
                entries.borrow_mut().remove(&key);
            });
        }
        self.entry_retained = None;
        self.entry_closed = None;
        self.entry_task = None;
    }
    fn open_saved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(receipt) = &self.saved else { return };
        self.checkpoint_error = false;
        let relative = okilum_core::vault::note_path(&receipt.path);
        let root = self.root.clone();
        match self.reader.update(cx, |reader, cx| {
            if reader.vault_root != root {
                return Err(anyhow::anyhow!("The owning Reader has switched vaults. Reopen the capture vault to open this note."));
            }
            reader.open_note(&relative, None, window, cx);
            Ok(())
        }) {
            Ok(Ok(())) => self.error = None,
            Ok(Err(error)) => self.error = Some(error.to_string()),
            Err(_) => self.error = Some("The owning Reader was closed. Your saved capture remains in its vault.".into()),
        }
        cx.notify();
    }
}

impl Render for CaptureComposer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.entry_close_ready {
            cx.defer_in(window, |this, window, cx| {
                this.finish_entry_close(window, cx)
            });
        }
        let locked = self.busy
            || self.loading
            || self.entry_close_ready
            || self.draft.attempt.is_some()
            || self.legacy_selected;
        let mut view = v_flex()
            .track_focus(&self.entry_focus)
            .w_full()
            .gap_2()
            .p_4()
            .child(div().child("Capture a note"))
            .child(div().child(format!("Vault: {}", self.root.display())))
            .child(div().child("Capture folder (relative to this vault)"))
            .child(Input::new(&self.folder).disabled(locked))
            .child(Textarea::new(&self.text).disabled(locked));
        if self.loading {
            view = view.child("Recovering saved drafts…");
        } else if self.legacy_selected {
            view = view.child("Legacy intent retained. Its original vault is unknown. Choose a destination and use the previewed Save copy action to create a separate note.");
        } else if self.persisted == Some(self.generation) {
            view = view.child("Draft stored on this device");
        } else if !self.draft.text.is_empty() {
            view = view.child("Storing draft…");
        }
        if let Some(error) = &self.error {
            view = view.child(div().text_color(cx.theme().danger).child(error.clone()));
        }
        if let Some(receipt) = &self.saved {
            view =
                view.child(
                    h_flex()
                        .gap_2()
                        .child(format!("Saved: {}", receipt.path.display()))
                        .child(Button::new("capture-open").label("Open note").on_click(
                            cx.listener(|this, _, window, cx| this.open_saved(window, cx)),
                        )),
                );
        }
        view = view.child(
            Button::new("capture-inspect-dismissed")
                .label("Inspect dismissed recovery sources")
                .disabled(self.busy || self.loading)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.busy = true;
                    this.enqueue(Work::InspectDismissed, cx);
                })),
        );
        view = view.child(
            Button::new("capture-inspect-history")
                .label("Inspect presentation history")
                .disabled(self.busy || self.loading)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.busy = true;
                    this.enqueue(Work::InspectHistory, cx);
                })),
        );
        if let Some(history) = &self.presentation_history {
            let mut panel = v_flex().id("capture-presentation-history").gap_2().max_h(px(320.)).overflow_y_scroll().child("Read-only snapshot at inspection · refresh to see later decisions · verified head → predecessors; unlinked rows have no proven chronology");
            if !history.complete {
                panel = panel.child("History held/incomplete — retained records were not repaired. Refresh inspection to recover.");
            }
            for issue in &history.held {
                panel = panel.child(div().text_color(cx.theme().danger).child(issue.clone()));
            }
            for source in &history.sources {
                panel = panel.child(format!(
                    "{} · current {:?} · revision {} · marker {}",
                    source.marker,
                    source.current,
                    source.current_revision.as_deref().unwrap_or("held"),
                    source.marker_revision
                ));
                for event in &source.events {
                    panel = panel.child(format!(
                        "{:?} · {:?} · {} · predecessor {}",
                        event.relation,
                        event.status,
                        event.revision,
                        event.previous_revision.as_deref().unwrap_or("unproven")
                    ));
                }
                for issue in &source.held {
                    panel = panel.child(div().text_color(cx.theme().danger).child(issue.clone()));
                }
            }
            view = view.child(panel);
            view = view.child(
                Button::new("capture-close-history")
                    .label("Close history")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.presentation_history = None;
                        cx.notify();
                    })),
            );
        }
        for (index, source) in self.dismissed_sources.iter().enumerate() {
            view = view.child(
                Button::new(("capture-dismissed-source", index))
                    .label(format!("Inspect: {}", source.original_path.display()))
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.dismissed_inspection = Some(index);
                        cx.notify();
                    })),
            );
        }
        if let Some(source) = self
            .dismissed_inspection
            .and_then(|index| self.dismissed_sources.get(index))
        {
            let revision = source.revision.clone();
            view = view.child(v_flex().gap_2()
                .child(format!("Original: {} · Saved copy: {}", source.original_path.display(), source.saved_path.display()))
                .child(source.text.clone())
                .children(source.assets.iter().map(|asset| div().child(format!("{} · {} · {} bytes · SHA256 {}", asset.asset.id, asset.asset.display_name, asset.asset.byte_length, asset.asset.sha256))))
                .child("Reopen restores this source to recovery choices. It does not publish a note or remove files or staged attachments.")
                .child(Button::new("capture-reopen-dismissed").label("Reopen recovery source").disabled(self.busy)
                    .on_click(cx.listener(move |this, _, _, cx| { this.busy = true; this.enqueue(Work::Reopen(revision.clone()), cx); })))
                .child(Button::new("capture-close-inspection").label("Close inspection").disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| { this.dismissed_inspection = None; cx.notify(); }))));
        }
        if self.saved_recovery_copy.is_some() {
            view = view.child(
                Button::new("capture-dismiss-saved-source")
                    .label("Dismiss original recovery source — keep all files")
                    .disabled(self.busy || self.loading)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(copy) = this.saved_recovery_copy.clone() {
                            this.busy = true;
                            this.enqueue(Work::Dismiss(copy), cx);
                        }
                    })),
            );
        }
        for (index, draft) in self.recovered.iter().enumerate() {
            let preview: String = draft.text.chars().take(60).collect();
            view = view.child(
                Button::new(("capture-recover", index))
                    .label(format!("Recover draft: {preview}"))
                    .disabled(
                        self.busy || !self.draft.text.is_empty() || self.draft.attempt.is_some(),
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.restore(index, window, cx)),
                    ),
            );
        }
        for (index, source) in self.unbound.iter().enumerate() {
            view = view.child(
                Button::new(("capture-legacy", index))
                    .label(format!(
                        "Inspect unbound legacy intent: {}",
                        source.original_path().display()
                    ))
                    .disabled(
                        self.busy || !self.draft.text.is_empty() || self.draft.attempt.is_some(),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_legacy(index, window, cx)
                    })),
            );
        }
        if self.draft.attempt.is_some() || self.legacy_selected {
            view = view.child(div().child("Save a separate copy: choose its destination explicitly. The original note and intent are retained."))
                .child(Input::new(&self.copy_folder).disabled(self.busy))
                .child(h_flex().gap_2()
                    .child(Button::new("capture-copy-root").label("Use vault root").disabled(self.busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.copy_folder.update(cx, |input, cx| input.set_value("", window, cx));
                            this.copy_folder_chosen = true;
                            this.copy_preview = None;
                            cx.notify();
                        })))
                    .child(Button::new("capture-preview-copy").label("Preview copy in this vault")
                        .disabled(self.busy || !self.copy_folder_chosen)
                        .on_click(cx.listener(|this, _, _, cx| this.preview_copy(cx)))));
        }
        if let Some(preview) = &self.copy_preview {
            let origin = preview
                .source()
                .bound_root()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "Unknown — legacy intent has no vault binding".into());
            view = view.child(v_flex().gap_2()
                .child(format!("Original vault: {origin}"))
                .child(format!("Recorded original path: {}", preview.source().original_path().display()))
                .child(format!("Selected copy vault: {}", preview.root().display()))
                .child(format!("Selected copy folder: {}", if preview.folder().as_os_str().is_empty() { "(vault root)".into() } else { preview.folder().display().to_string() }))
                .child("Save copy will allocate a new Capture-<UUID>.md filename. The exact captured text shown above will be copied; existing notes will not be replaced.")
                .child(Button::new("capture-confirm-copy").label("Save copy").disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| this.save_copy(window, cx))))
                .child(Button::new("capture-dismiss-preview").label("Cancel preview").disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| { this.copy_preview = None; cx.notify(); }))));
        }
        view.child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("capture-cancel")
                        .debug_selector(|| "capture-cancel".into())
                        .label("Cancel — keep draft")
                        .disabled(self.busy || self.loading)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.request_close(cx);
                        })),
                )
                .child(
                    Button::new("capture-save")
                        .label(if self.busy {
                            "Saving…"
                        } else if self.draft.attempt.is_some() {
                            "Retry save"
                        } else {
                            "Save capture"
                        })
                        .disabled(
                            self.busy
                                || self.loading
                                || self.legacy_selected
                                || self.draft.text.trim().is_empty(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                ),
        )
    }
}

// Legacy parent mounts also retain the previous queue-drain guarantee when an
// element disappears: a failed UI update must not cancel already-authored IO.
impl Drop for CaptureComposer {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.detach();
        }
    }
}
