//! Per-conversation durable composer state. Disk work is serialized off the UI
//! thread, and quit awaits a barrier behind every queued edit.
use super::*;
use super::{composer::TypedScope, message_types::DraftMetadata};
use futures::channel::oneshot;
use okilum_t3::chat_drafts::{Attachment, Draft, Snapshot, Store};
use std::sync::{mpsc, Mutex, OnceLock};

type Key = (String, String, String);
type ChatDraft = Draft<Attachment>;
type Saved = Snapshot<Attachment>;
type DiskBarrier = oneshot::Receiver<Result<(), String>>;
pub(super) type Barrier = Task<Result<Result<(), String>, oneshot::Canceled>>;

/// Wait inside Send background work, using futures' thread-safe parker rather
/// than a GPUI task waker. The process-wide OS worker can reply after a window
/// (or its test scheduler) has gone away; it must never own a local UI future.
fn wait_for_disk(barrier: DiskBarrier, executor: &gpui::BackgroundExecutor) -> Barrier {
    executor.spawn(async move { futures::executor::block_on(barrier) })
}

/// One atomic composer value: metadata is local operational state, not an
/// attachment, source-text annotation, or a T3 transport field.
#[derive(Clone, Default, Debug, PartialEq)]
struct LocalDraft {
    draft: ChatDraft,
    metadata: Option<DraftMetadata>,
}
impl From<ChatDraft> for LocalDraft {
    fn from(draft: ChatDraft) -> Self {
        Self {
            draft,
            metadata: None,
        }
    }
}
impl LocalDraft {
    fn validate(&self) -> Result<(), String> {
        if let Some(metadata) = &self.metadata {
            super::message_types::TypedDraft::restore_metadata(
                self.draft.text.clone(),
                metadata.clone(),
            )
            .map_err(str::to_owned)?;
        }
        Ok(())
    }
}

/// Rotation compares this with its saved owner immediately before apply and
/// again after the durable barrier. Metadata-only edits increment the revision.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ComposerSnapshot {
    pub draft: ChatDraft,
    pub edit_revision: u64,
    pub has_authored_metadata: bool,
    pub typed_metadata: Option<DraftMetadata>,
}

/// A barrier receipt is valid only for this exact owner, restore epoch and full
/// snapshot. Awaiting its receiver alone never authorizes rotation claim ack.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct DraftCheckpoint {
    key: Key,
    epoch: u64,
    pub snapshot: ComposerSnapshot,
}

enum Work {
    Save(Arc<Mutex<Pending>>),
    Flush(Arc<Mutex<Pending>>, oneshot::Sender<Result<(), String>>),
    Drain(oneshot::Sender<Result<(), String>>),
}

struct Pending {
    store: Store<Attachment>,
    desired: Arc<Mutex<LocalDraft>>,
    saved: Saved,
    metadata: Option<DraftMetadata>,
    failure: Option<String>,
}
impl Pending {
    fn flush(&mut self) -> Result<(), String> {
        let draft = self.desired.lock().unwrap().clone();
        if self.failure.is_none()
            && (draft.draft != self.saved.draft || draft.metadata != self.metadata)
        {
            let result = (|| {
                draft.validate()?;
                let metadata = draft
                    .metadata
                    .as_ref()
                    .map(serde_json::to_value)
                    .transpose()
                    .map_err(|_| "Typed draft metadata cannot be encoded".to_owned())?;
                self.store
                    .save_with_metadata(self.saved.revision, draft.draft.clone(), metadata)
                    .map_err(|error| error.to_string())
            })();
            match result {
                Ok(next) => {
                    self.saved = next;
                    self.metadata = draft.metadata;
                }
                Err(error) => self.failure = Some(error),
            }
        }
        self.failure.clone().map_or(Ok(()), Err)
    }
}

/// One process-wide disk worker. Queued work owns its conversation until it is
/// written; closed windows need neither parked threads nor retained Writers.
fn queue() -> std::io::Result<mpsc::Sender<Work>> {
    static QUEUE: OnceLock<mpsc::Sender<Work>> = OnceLock::new();
    static START: Mutex<()> = Mutex::new(());
    let _start = START.lock().unwrap();
    if let Some(sender) = QUEUE.get() {
        return Ok(sender.clone());
    }
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("manager-drafts".into())
        .spawn(move || {
            let mut failed = false;
            while let Ok(work) = receiver.recv() {
                match work {
                    Work::Save(pending) => failed |= pending.lock().unwrap().flush().is_err(),
                    Work::Flush(pending, reply) => {
                        let result = pending.lock().unwrap().flush();
                        failed |= result.is_err();
                        let _ = reply.send(result);
                    }
                    Work::Drain(reply) => {
                        let _ = reply.send(if failed {
                            Err("A Manager draft could not be saved".into())
                        } else {
                            Ok(())
                        });
                    }
                }
            }
        })?;
    let _ = QUEUE.set(sender.clone());
    Ok(sender)
}

struct Writer {
    desired: Arc<Mutex<LocalDraft>>,
    pending: Arc<Mutex<Pending>>,
    sender: mpsc::Sender<Work>,
}

#[derive(Default)]
struct Writers {
    views: Vec<WeakEntity<ProjectView>>,
}
impl Global for Writers {}

fn install_shutdown(cx: &mut App) {
    if cx.has_global::<Writers>() {
        return;
    }
    cx.set_global(Writers::default());
    cx.on_app_quit(|cx| {
        let views = cx.global::<Writers>().views.clone();
        for view in views {
            let _ = view.update(cx, |view, cx| view.draft_capture(cx));
        }
        // This FIFO barrier follows work from both live and released windows.
        let (reply, barrier) = oneshot::channel();
        if let Ok(sender) = queue() {
            let _ = sender.send(Work::Drain(reply));
        }
        let barrier = wait_for_disk(barrier, cx.background_executor());
        async move {
            if !matches!(barrier.await, Ok(Ok(()))) {
                diagnostic_error!("Manager draft could not be flushed before quit");
            }
        }
    })
    .detach();
}
impl Writer {
    #[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
    fn new(store: Store<Attachment>, initial: Saved) -> std::io::Result<Self> {
        Self::new_retained(store, initial, None)
    }

    fn new_retained(
        store: Store<Attachment>,
        initial: Saved,
        metadata: Option<DraftMetadata>,
    ) -> std::io::Result<Self> {
        let desired = Arc::new(Mutex::new(LocalDraft {
            draft: initial.draft.clone(),
            metadata: metadata.clone(),
        }));
        let pending = Arc::new(Mutex::new(Pending {
            store,
            desired: desired.clone(),
            saved: initial,
            metadata,
            failure: None,
        }));
        Ok(Self {
            desired,
            pending,
            sender: queue()?,
        })
    }

    #[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
    fn save(&self, draft: ChatDraft) {
        self.save_retained(draft.into());
    }

    fn save_retained(&self, draft: LocalDraft) {
        *self.desired.lock().unwrap() = draft;
        let _ = self.sender.send(Work::Save(self.pending.clone()));
    }

    fn barrier(&self) -> DiskBarrier {
        let (sender, receiver) = oneshot::channel();
        let _ = self.sender.send(Work::Flush(self.pending.clone(), sender));
        receiver
    }
}

#[derive(Default)]
pub(super) struct State {
    key: Option<Key>,
    epoch: u64,
    writer: Option<Arc<Writer>>,
    loading: bool,
    attachments: Vec<Attachment>,
    error: Option<String>,
    subscriptions: Vec<Subscription>,
    edit_revision: u64,
    last_edit: Option<LocalDraft>,
    handoff: HandoffIdentity,
    handoff_receiver: okilum_t3::draft_handoff::Receiver,
    handoff_surface_visible: bool,
    typed_generation: u64,
    suspended: Option<LocalDraft>,
}

/// Captures exactly the local draft handed to durable message preparation.
/// A later edit (including an attachment-only edit) must survive its completion.
#[derive(Clone)]
pub(super) struct Sent {
    key: Option<Key>,
    edit_revision: u64,
    draft: ChatDraft,
    metadata: Option<DraftMetadata>,
}

impl ProjectView {
    pub(super) fn draft_typed_owner_valid(&self, scope: &TypedScope) -> bool {
        !self.threads.draft.loading
            && self.threads.draft.typed_generation == scope.generation
            && self.threads.draft.key.as_ref()
                == Some(&(
                    scope.environment.clone(),
                    scope.project.clone(),
                    scope.thread.clone(),
                ))
    }
    pub(super) fn draft_suspend_typed(&mut self, cx: &mut Context<Self>) {
        self.draft_flush(cx);
        if !self.threads.draft.loading && self.composer.read(cx).typed_scope().is_some() {
            let sent = self.draft_sent(cx);
            self.threads.draft.suspended = Some(LocalDraft {
                draft: sent.draft,
                metadata: sent.metadata,
            });
        }
        self.threads.draft.typed_generation = self
            .threads
            .draft
            .typed_generation
            .checked_add(1)
            .expect("typed composer lease exhausted");
        self.draft_handoff_visibility(false);
    }
    /// Hide the previous conversation's composer while its replacement native
    /// credential is being restored. The old writer already owns its snapshot.
    pub(super) fn draft_prepare_identity(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.draft_key() != self.threads.draft.key {
            self.draft_handoff_visibility(false);
            // Capture has already been queued by edits for the previous owner.
            // Never reinterpret that owner's hidden token under the next key.
            self.composer
                .update(cx, |composer, cx| composer.clear_typed_context(cx));
            self.composer
                .update(cx, |composer, cx| composer.reset_history(cx));
            self.threads.draft.loading = true;
            let first_identity = self.threads.draft.key.is_none();
            self.compose.update(cx, |input, cx| {
                if !first_identity {
                    input.set_value("", window, cx);
                }
                input.set_disabled(true, cx);
            });
        }
    }

    fn draft_key(&self) -> Option<Key> {
        let binding = self.binding.as_ref()?;
        let thread = &self.chat.as_ref()?.thread_id;
        if binding.manager.thread_id.as_ref() != Some(thread) {
            return None;
        }
        Some((
            self.environment.as_ref()?.origin(),
            binding.manager.project_id.clone()?,
            thread.clone(),
        ))
    }

    /// Install once per view, before user input is enabled. Safe to call again
    /// after reconnect; a matching key retains its in-memory composer state.
    pub(super) fn draft_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.typed_clear_pending(window, cx);
        if self.threads.draft.subscriptions.is_empty() {
            install_shutdown(cx);
            let weak = cx.weak_entity();
            let live: Vec<_> = cx
                .global::<Writers>()
                .views
                .iter()
                .filter(|view| view.upgrade().is_some())
                .cloned()
                .collect();
            cx.global_mut::<Writers>().views = live;
            cx.global_mut::<Writers>().views.push(weak);
            let input = cx.subscribe_in(
                &self.compose,
                window,
                |this, _, _: &InputEvent, window, cx| this.draft_changed(window, cx),
            );
            self.threads.draft.subscriptions.push(input);
            let typed = cx.subscribe_in(
                &self.composer,
                window,
                |this, _, event: &super::composer::TypedDraftChanged, window, cx| {
                    let key = (
                        event.scope.environment.clone(),
                        event.scope.project.clone(),
                        event.scope.thread.clone(),
                    );
                    if this.threads.draft.key.as_ref() != Some(&key)
                        || this.draft_key().as_ref() != Some(&key)
                        || this.threads.draft.typed_generation != event.scope.generation
                    {
                        return;
                    }
                    let composer = this.composer.read(cx);
                    if composer.typed_source_text(cx) != event.text
                        || composer.typed_draft_metadata() != event.metadata
                    {
                        return;
                    }
                    this.draft_changed(window, cx);
                },
            );
            self.threads.draft.subscriptions.push(typed);
        }
        let Some(key) = self.draft_key() else { return };
        if self.threads.draft.key.as_ref() == Some(&key) {
            if !self.threads.draft.loading && self.active {
                if let Some(local) = self.threads.draft.suspended.take() {
                    let scope = TypedScope {
                        environment: key.0.clone(),
                        project: key.1.clone(),
                        thread: key.2.clone(),
                        generation: self.threads.draft.typed_generation,
                    };
                    self.threads.draft.attachments = local.draft.attachments;
                    let restored = self.composer.update(cx, |composer, cx| {
                        composer.restore_typed_context(
                            scope,
                            local.draft.text,
                            local.metadata,
                            window,
                            cx,
                        )
                    });
                    if restored.is_ok() {
                        self.draft_handoff_connection_ready(cx);
                    }
                }
            }
            return;
        }
        self.composer
            .update(cx, |composer, cx| composer.reset_history(cx));
        // Text may have been entered while the first connection was loading.
        // Preserve it, but never transfer an established key's text elsewhere.
        let initial = self.threads.draft.key.is_none().then(|| {
            let sent = self.draft_sent(cx);
            LocalDraft {
                draft: sent.draft,
                metadata: sent.metadata,
            }
        });
        self.draft_capture(cx);
        self.discuss.set_visible(None);
        let draft = &mut self.threads.draft;
        draft.handoff.invalidate();
        draft.epoch = draft.epoch.wrapping_add(1);
        let epoch = draft.epoch;
        draft.key = Some(key.clone());
        draft.writer = None;
        draft.attachments.clear();
        draft.last_edit = None;
        draft.suspended = None;
        draft.typed_generation = draft
            .typed_generation
            .checked_add(1)
            .expect("typed composer lease exhausted");
        let typed_generation = draft.typed_generation;
        draft.error = None;
        draft.loading = true;
        self.composer
            .update(cx, |composer, cx| composer.clear_typed_context(cx));
        self.compose.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_disabled(true, cx);
        });
        let Some(state) = self.state.clone() else {
            self.threads.draft.loading = false;
            self.threads.draft.error = Some(
                "Draft storage is unavailable. Text is kept only while this window is open.".into(),
            );
            let local = initial.clone().unwrap_or_default();
            self.threads.draft.attachments = local.draft.attachments;
            let scope = TypedScope {
                environment: key.0.clone(),
                project: key.1.clone(),
                thread: key.2.clone(),
                generation: typed_generation,
            };
            let _ = self.composer.update(cx, |composer, cx| {
                composer.restore_typed_context(scope, local.draft.text, local.metadata, window, cx)
            });
            self.compose
                .update(cx, |input, cx| input.set_disabled(false, cx));
            return;
        };
        let environment = self.environment.clone().unwrap();
        let environment_id = self.identity.as_ref().map(|i| i.environment_id.clone());
        let identity_client = self.native.model_client().filter(|c| c.access_allowed());
        let store = Store::<Attachment>::new(&state, &environment, &key.1, &key.2);
        cx.spawn_in(window, async move |this, cx| {
            let initial_copy = initial.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let (mut saved, metadata) = store
                        .load_with_metadata()
                        .map_err(|error| error.to_string())?;
                    let metadata: Option<DraftMetadata> = metadata
                        .map(serde_json::from_value)
                        .transpose()
                        .map_err(|_| {
                            "Retained typed draft metadata cannot be decoded".to_owned()
                        })?;
                    let original = saved.draft.clone();
                    LocalDraft {
                        draft: original.clone(),
                        metadata: metadata.clone(),
                    }
                    .validate()?;
                    let has_uploaded = saved.draft.attachments.iter().any(|attachment| {
                        matches!(
                            attachment.state,
                            okilum_t3::chat_drafts::AttachmentState::Uploaded { .. }
                        )
                    });
                    let environment_id = if has_uploaded {
                        environment_id.or_else(|| {
                            identity_client
                                .and_then(|client| client.server_identity().ok())
                                .map(|identity| identity.environment_id)
                        })
                    } else {
                        environment_id
                    };
                    // A local file handle or in-flight upload is not a durable asset.
                    saved
                        .draft
                        .restore_for(environment_id.as_deref().unwrap_or(""));
                    let restored = merge_initial_typed(
                        initial_copy,
                        LocalDraft {
                            draft: saved.draft.clone(),
                            metadata: metadata.clone(),
                        },
                    )?;
                    // Start from the actual disk version, then retain restored status.
                    saved.draft = original;
                    let writer = Writer::new_retained(store, saved, metadata)
                        .map_err(|_| "Could not start draft storage".to_owned())?;
                    writer.save_retained(restored.clone());
                    Ok::<_, String>((writer, restored))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.threads.draft.epoch != epoch || this.draft_key().as_ref() != Some(&key) {
                    return;
                }
                this.threads.draft.loading = false;
                let mut handoff_restored = false;
                match result {
                    Ok((writer, restored)) => {
                        let writer = Arc::new(writer);
                        this.threads.draft.writer = Some(writer);
                        if !this.active
                            || !reader_features::enabled(reader_features::Feature::Agents, cx)
                            || this.threads.draft.typed_generation != typed_generation
                        {
                            this.threads.draft.suspended = Some(restored);
                            cx.notify();
                            return;
                        }
                        this.threads.draft.attachments = restored.draft.attachments;
                        let scope = TypedScope {
                            environment: key.0.clone(),
                            project: key.1.clone(),
                            thread: key.2.clone(),
                            generation: typed_generation,
                        };
                        if let Err(error) = this.composer.update(cx, |composer, cx| {
                            composer.restore_typed_context(
                                scope,
                                restored.draft.text,
                                restored.metadata,
                                window,
                                cx,
                            )
                        }) {
                            this.threads.draft.error = Some(error.to_owned());
                        } else {
                            handoff_restored = true;
                        }
                    }
                    Err(error) => {
                        if let Some(initial) = initial {
                            if !this.active
                                || !reader_features::enabled(reader_features::Feature::Agents, cx)
                                || this.threads.draft.typed_generation != typed_generation
                            {
                                this.threads.draft.suspended = Some(initial);
                                this.threads.draft.error = Some(error);
                                cx.notify();
                                return;
                            }
                            this.threads.draft.attachments = initial.draft.attachments;
                            let scope = TypedScope {
                                environment: key.0.clone(),
                                project: key.1.clone(),
                                thread: key.2.clone(),
                                generation: typed_generation,
                            };
                            let _ = this.composer.update(cx, |composer, cx| {
                                composer.restore_typed_context(
                                    scope,
                                    initial.draft.text,
                                    initial.metadata,
                                    window,
                                    cx,
                                )
                            });
                        }
                        this.threads.draft.error =
                            Some(format!("{error}. The saved draft was preserved."))
                    }
                }
                this.compose
                    .update(cx, |input, cx| input.set_disabled(false, cx));
                if handoff_restored {
                    // The restore/draft epochs and visible lease were checked
                    // above. Re-read the current connection/access and physical
                    // surface now; the eventual port mints its current epoch.
                    this.draft_handoff_connection_ready(cx);
                }
                // Observe the restoration write as well: failed attachment or
                // metadata retention must be visible without another keystroke.
                if this.threads.draft.writer.is_some() {
                    this.draft_changed(window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn draft_capture(&self, cx: &App) {
        // Never save an old composer under a newly replaced binding/origin.
        if self.threads.draft.loading || self.draft_key() != self.threads.draft.key {
            return;
        }
        if let Some(writer) = &self.threads.draft.writer {
            let sent = self.draft_sent(cx);
            writer.save_retained(LocalDraft {
                draft: sent.draft,
                metadata: sent.metadata,
            });
        }
    }

    pub(super) fn composer_snapshot(&self, cx: &App) -> ComposerSnapshot {
        let sent = self.draft_sent(cx);
        ComposerSnapshot {
            draft: sent.draft,
            edit_revision: self.threads.draft.edit_revision,
            has_authored_metadata: sent.metadata.is_some(),
            typed_metadata: sent.metadata,
        }
    }

    /// Capture now, then wait on the existing serialized durable writer. Owner
    /// must re-check draft_checkpoint_matches after Ok(Ok(())) and before ack.
    /// This never prepares a message or dispatches an executor operation.
    pub(super) fn draft_durable_barrier(
        &self,
        cx: &App,
    ) -> Result<(DraftCheckpoint, Barrier), String> {
        let key = self
            .draft_key()
            .filter(|key| {
                !self.threads.draft.loading
                    && self.threads.draft.edit_revision != u64::MAX
                    && Some(key) == self.threads.draft.key.as_ref()
            })
            .ok_or("Composer owner changed or draft is still loading")?;
        let writer = self
            .threads
            .draft
            .writer
            .as_ref()
            .ok_or("Durable draft storage is unavailable")?;
        let snapshot = self.composer_snapshot(cx);
        writer.save_retained(LocalDraft {
            draft: snapshot.draft.clone(),
            metadata: snapshot.typed_metadata.clone(),
        });
        let checkpoint = DraftCheckpoint {
            key,
            epoch: self.threads.draft.epoch,
            snapshot,
        };
        Ok((
            checkpoint,
            wait_for_disk(writer.barrier(), cx.background_executor()),
        ))
    }

    pub(super) fn draft_checkpoint_matches(&self, checkpoint: &DraftCheckpoint, cx: &App) -> bool {
        !self.threads.draft.loading
            && self.threads.draft.edit_revision != u64::MAX
            && self.threads.draft.epoch == checkpoint.epoch
            && self.draft_key().as_ref() == Some(&checkpoint.key)
            && self.threads.draft.key.as_ref() == Some(&checkpoint.key)
            && self.composer_snapshot(cx) == checkpoint.snapshot
    }

    pub(super) fn draft_flush(&self, cx: &App) {
        self.draft_capture(cx);
        // The writer queue owns the captured bytes across switch/hide/drop.
    }

    pub(super) fn draft_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.threads.draft.loading || self.draft_key() != self.threads.draft.key {
            return;
        }
        let sent = self.draft_sent(cx);
        let local = LocalDraft {
            draft: sent.draft,
            metadata: sent.metadata,
        };
        if self.threads.draft.last_edit.as_ref() != Some(&local) {
            let Some(revision) = self.threads.draft.edit_revision.checked_add(1) else {
                self.threads.draft.error = Some(
                    "Composer edit revision exhausted; keep this window open and copy your draft"
                        .into(),
                );
                return;
            };
            self.threads.draft.edit_revision = revision;
            self.threads.draft.last_edit = Some(local);
        }
        self.draft_capture(cx);
        let Some(writer) = self.threads.draft.writer.as_ref() else {
            return;
        };
        let barrier = wait_for_disk(writer.barrier(), cx.background_executor());
        let key = self.threads.draft.key.clone();
        let epoch = self.threads.draft.epoch;
        cx.spawn_in(window, async move |this, cx| {
            let result = barrier.await;
            let _ = this.update(cx, |this, cx| {
                if this.threads.draft.key != key || this.threads.draft.epoch != epoch { return; }
                let error = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(format!("{error}. Keep this window open and copy your draft before closing.")),
                    Err(_) => Some("Draft storage stopped. Keep this window open and copy your draft before closing.".into()),
                };
                if this.threads.draft.error != error {
                    this.threads.draft.error = error;
                    cx.notify();
                }
            });
        }).detach();
    }

    pub(super) fn draft_sent(&self, cx: &App) -> Sent {
        // Hide/revocation clears the visible typed context. Its last complete
        // snapshot still belongs to this writer, including during quit flush.
        let suspended = self
            .composer
            .read(cx)
            .typed_scope()
            .is_none()
            .then_some(self.threads.draft.suspended.as_ref())
            .flatten();
        Sent {
            key: self.draft_key(),
            edit_revision: self.threads.draft.edit_revision,
            draft: suspended
                .map(|local| local.draft.clone())
                .unwrap_or_else(|| ChatDraft {
                    text: self.composer.read(cx).typed_source_text(cx),
                    attachments: self.threads.draft.attachments.clone(),
                }),
            metadata: suspended
                .map(|local| local.metadata.clone())
                .unwrap_or_else(|| {
                    self.composer
                        .read(cx)
                        .typed_has_authored_metadata()
                        .then(|| self.composer.read(cx).typed_draft_metadata())
                }),
        }
    }

    pub(super) fn draft_clear_after_send(
        &mut self,
        sent: &Sent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now = self.draft_sent(cx);
        if now.key == sent.key
            && now.edit_revision == sent.edit_revision
            && now.draft == sent.draft
            && now.metadata == sent.metadata
        {
            self.threads.draft.attachments.clear();
            if self.threads.draft.suspended.is_some() {
                self.threads.draft.suspended = Some(LocalDraft::default());
            }
            self.composer.update(cx, |composer, cx| {
                // Default metadata is intentionally absent after clearing. The
                // next draft must not inherit type selection or Undo suppression.
                let _ = composer.replace_typed_draft(String::new(), None, window, cx);
            });
            self.draft_changed(window, cx);
        }
    }

    pub(super) fn draft_attachments(&self) -> &[Attachment] {
        &self.threads.draft.attachments
    }

    // Restore must finish before native asset preparation or text-only MCP send.
    pub(super) fn draft_ready(&self) -> bool {
        !self.threads.draft.loading
    }

    pub(super) fn draft_unbound_context_allowed(&self) -> bool {
        !self.threads.draft.loading
            && self.threads.draft.key.is_none()
            && self.threads.draft.error.is_none()
    }

    // MCP cannot carry native asset references; keep its text-only guard.
    pub(super) fn draft_is_sendable(&self) -> bool {
        self.draft_ready() && self.draft_attachments().is_empty()
    }

    pub(super) fn draft_set_attachments(
        &mut self,
        attachments: Vec<Attachment>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.threads.draft.attachments = attachments;
        self.draft_changed(window, cx);
    }

    pub(super) fn draft_notice(&self) -> Option<&str> {
        self.threads.draft.error.as_deref()
    }

    pub(super) fn render_draft_attachments(&self, cx: &mut Context<Self>) -> AnyElement {
        use okilum_t3::chat_drafts::AttachmentState;
        let mut rows = v_flex().w_full().gap_1();
        for attachment in self.draft_attachments() {
            let id = attachment.id.clone();
            let status = match &attachment.state {
                AttachmentState::Pending => "Preparing…",
                AttachmentState::Error { .. } => "Could not attach — select again",
                AttachmentState::NeedsReselect => "Select this file again",
                AttachmentState::Uploaded { .. } => "Attached",
            };
            rows = rows.child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .whitespace_normal()
                            .child(attachment.name.clone()),
                    )
                    .child(
                        div()
                            .text_color(brand::palette(cx).text_muted)
                            .child(status),
                    )
                    .child(
                        Button::new(SharedString::from(format!("draft-remove-{id}")))
                            .ghost()
                            .small()
                            .label("Remove")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let attachments = this
                                    .draft_attachments()
                                    .iter()
                                    .filter(|a| a.id != id)
                                    .cloned()
                                    .collect();
                                this.draft_set_attachments(attachments, window, cx);
                            })),
                    ),
            );
        }
        rows.into_any_element()
    }
}

fn merge_initial_typed(
    initial: Option<LocalDraft>,
    saved: LocalDraft,
) -> Result<LocalDraft, String> {
    match initial.filter(|initial| *initial != LocalDraft::default()) {
        None => Ok(saved),
        Some(initial) if saved == LocalDraft::default() || saved == initial => Ok(initial),
        Some(_) => Err("Current draft differs from the saved draft; neither was overwritten. Keep this window open and copy your draft before closing".into()),
    }
}

#[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
fn merge_initial(initial: Option<ChatDraft>, saved: ChatDraft) -> Result<ChatDraft, String> {
    match initial.filter(|initial| *initial != ChatDraft::default()) {
        None => Ok(saved),
        Some(initial) if saved == ChatDraft::default() || saved == initial => Ok(initial),
        Some(_) => Err("Current text differs from the saved draft; neither was overwritten. Keep this window open and copy your text before closing".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn disk_reply_probe_detects_an_unbridged_foreground_waker() {
        let dispatcher = Arc::new(gpui::TestDispatcher::new(0));
        let foreground = gpui::ForegroundExecutor::new(dispatcher.clone());
        let (reply, barrier) = oneshot::channel::<Result<(), String>>();
        foreground.spawn(barrier).detach();
        assert!(dispatcher.tick(false));
        dispatcher.drain_tasks();
        assert!(std::thread::spawn(move || reply.send(Ok(())))
            .join()
            .unwrap()
            .is_ok());
        // Retire the unsafe runnable on its owner before asking the scheduler
        // for its verdict. Ending it before the raw reply would abort the whole
        // process in the local future's destructor, as in the original defect.
        dispatcher.drain_tasks();
        let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            dispatcher.scheduler().end_test();
        }))
        .expect_err("positive control: a raw disk reply must fail the scheduler audit");
        let message = failure
            .downcast_ref::<String>()
            .expect("scheduler panic describes the foreign wake");
        assert!(message.contains("Detected activity on thread"), "{message}");
    }

    #[test]
    fn disk_reply_after_local_task_teardown_cannot_wake_the_finished_ui_executor() {
        let dispatcher = Arc::new(gpui::TestDispatcher::new(0));
        let background = gpui::BackgroundExecutor::new(dispatcher.clone());
        let foreground = gpui::ForegroundExecutor::new(dispatcher.clone());
        let (reply, barrier) = oneshot::channel();
        let started = std::rc::Rc::new(std::cell::Cell::new(false));
        let observed = started.clone();
        let task = foreground.spawn(async move {
            observed.set(true);
            wait_for_disk(barrier, &background).await
        });
        // Poll the local waiter, but hold disk completion until after teardown.
        assert!(dispatcher.tick(false));
        assert!(
            started.get(),
            "positive control: the local waiter was polled"
        );
        // Edit notifications are detached, so teardown must retire their
        // receivers even without a retained local Task handle to cancel.
        task.detach();
        dispatcher.drain_tasks();
        dispatcher.scheduler().end_test();
        let result = std::thread::spawn(move || reply.send(Ok(())))
            .join()
            .expect("late disk completion must not touch a finished UI scheduler");
        assert!(
            result.is_err(),
            "the cancelled waiter released its receiver"
        );
    }

    #[test]
    fn first_connection_preserves_early_input_without_overwriting_a_different_saved_draft() {
        let local = ChatDraft {
            text: "Typed before connection".into(),
            attachments: vec![],
        };
        let saved = ChatDraft {
            text: "Retained on disk".into(),
            attachments: vec![],
        };
        assert_eq!(
            merge_initial(Some(local.clone()), ChatDraft::default()).unwrap(),
            local
        );
        assert_eq!(merge_initial(None, saved.clone()).unwrap(), saved);
        assert!(merge_initial(Some(local), saved).is_err());
    }

    #[test]
    fn released_conversation_writers_drain_and_release_their_retained_state() {
        let state = tempfile::tempdir().unwrap();
        let env = Environment::new("qa", "https://drafts.invalid").unwrap();
        let mut released = Vec::new();
        let mut stores = Vec::new();
        for n in 0..32 {
            let store =
                Store::<Attachment>::new(state.path(), &env, "project", &format!("thread-{n}"));
            let writer = Writer::new(store.clone(), store.load().unwrap()).unwrap();
            released.push(Arc::downgrade(&writer.pending));
            writer.save(ChatDraft {
                text: format!("Retained after close {n}"),
                attachments: vec![],
            });
            stores.push(store);
            drop(writer);
        }
        let (reply, barrier) = oneshot::channel();
        queue().unwrap().send(Work::Drain(reply)).unwrap();
        // Other tests deliberately inject persistence failures into the shared
        // queue. This test checks its own retained writes and releases directly.
        let _ = futures::executor::block_on(barrier).unwrap();
        assert!(
            released.iter().all(|pending| pending.upgrade().is_none()),
            "closed conversations must not remain owned by the process registry"
        );
        for (n, store) in stores.iter().enumerate() {
            assert_eq!(
                store.load().unwrap().draft.text,
                format!("Retained after close {n}")
            );
        }
    }

    fn typed_local(text: &str, kind: okilum_t3::message_types::MessageType) -> LocalDraft {
        let mut typed = super::super::message_types::TypedDraft::from_text(text.into());
        typed.select(kind);
        LocalDraft {
            draft: ChatDraft {
                text: typed.text.clone(),
                attachments: vec![],
            },
            metadata: Some(typed.retained_metadata()),
        }
    }

    #[test]
    fn typed_metadata_and_undo_survive_writer_release_and_restart() {
        use okilum_t3::message_types::MessageType;
        let state = tempfile::tempdir().unwrap();
        let env = Environment::new("qa", "https://drafts.invalid").unwrap();
        let store = Store::<Attachment>::new(state.path(), &env, "project", "typed-thread");
        let writer = Writer::new(store.clone(), store.load().unwrap()).unwrap();
        let first = typed_local("same text", MessageType::Bug);
        writer.save_retained(first.clone());
        futures::executor::block_on(writer.barrier())
            .unwrap()
            .unwrap();
        let (snapshot, metadata) = store.load_with_metadata().unwrap();
        assert_eq!(snapshot.draft, first.draft);
        assert_eq!(
            metadata,
            first
                .metadata
                .as_ref()
                .map(|value| serde_json::to_value(value).unwrap())
        );
        let revision = snapshot.revision;
        let second = typed_local("same text", MessageType::Decision);
        writer.save_retained(second.clone());
        futures::executor::block_on(writer.barrier())
            .unwrap()
            .unwrap();
        drop(writer);
        let (snapshot, metadata) = store.load_with_metadata().unwrap();
        assert_eq!(
            snapshot.revision,
            revision + 1,
            "metadata-only edit must commit a new revision"
        );
        let metadata = metadata.map(serde_json::from_value).transpose().unwrap();
        let writer = Writer::new_retained(store.clone(), snapshot, metadata).unwrap();
        assert_eq!(*writer.desired.lock().unwrap(), second);
        let mut typed = super::super::message_types::TypedDraft::from_text("BUG body".into());
        assert!(typed.detect_token());
        assert!(typed.undo_token());
        let suppressed = LocalDraft {
            draft: ChatDraft {
                text: typed.text.clone(),
                attachments: vec![],
            },
            metadata: Some(typed.retained_metadata()),
        };
        writer.save_retained(suppressed.clone());
        futures::executor::block_on(writer.barrier())
            .unwrap()
            .unwrap();
        let (snapshot, metadata) = store.load_with_metadata().unwrap();
        let restored = super::super::message_types::TypedDraft::restore_metadata(
            snapshot.draft.text,
            serde_json::from_value(metadata.unwrap()).unwrap(),
        )
        .unwrap();
        assert!(restored.conversion_suppressed);
        assert_eq!(restored.source_text(), "BUG body");
        assert!(
            Store::<Attachment>::new(state.path(), &env, "project", "other-thread")
                .load()
                .unwrap()
                .draft
                .text
                .is_empty()
        );
    }

    #[test]
    fn failed_typed_save_keeps_winning_disk_and_full_local_draft() {
        use okilum_t3::message_types::MessageType;
        let state = tempfile::tempdir().unwrap();
        let env = Environment::new("qa", "https://drafts.invalid").unwrap();
        let store = Store::<Attachment>::new(state.path(), &env, "project", "typed-conflict");
        let writer = Writer::new(store.clone(), store.load().unwrap()).unwrap();
        let winner = typed_local("other window", MessageType::Decision);
        store
            .save_with_metadata(
                0,
                winner.draft.clone(),
                winner
                    .metadata
                    .as_ref()
                    .map(|value| serde_json::to_value(value).unwrap()),
            )
            .unwrap();
        let local = typed_local("unsaved local text", MessageType::Bug);
        writer.save_retained(local.clone());
        assert!(futures::executor::block_on(writer.barrier())
            .unwrap()
            .is_err());
        assert_eq!(*writer.desired.lock().unwrap(), local);
        let (snapshot, metadata) = store.load_with_metadata().unwrap();
        assert_eq!(snapshot.draft, winner.draft);
        assert_eq!(
            metadata,
            winner
                .metadata
                .as_ref()
                .map(|value| serde_json::to_value(value).unwrap())
        );
        // Repeated flush cannot silently rebase a failed writer onto the winner.
        assert!(futures::executor::block_on(writer.barrier())
            .unwrap()
            .is_err());
        assert_eq!(store.load_with_metadata().unwrap().0, snapshot);
        assert!(merge_initial_typed(Some(local), winner).is_err());
    }

    fn binding(project: &str, thread: &str) -> AreaBinding {
        AreaBinding::parse(&format!("t3:\n  schema_version: 1\n  environment: fixture\n  manager: {{project_id: '{project}', thread_id: '{thread}', workspace_mode: local}}\n  executor: {{project_id: executors}}\n  default_models:\n    manager: {{instanceId: codex, model: gpt-6.1-sol}}\n    executor: {{instanceId: codex, model: gpt-6.1-sol}}\n")).unwrap().unwrap()
    }

    fn attachment() -> Attachment {
        Attachment {
            id: "local-file".into(),
            kind: okilum_t3::chat_drafts::AttachmentKind::File,
            name: "notes.txt".into(),
            mime_type: "text/plain".into(),
            size_bytes: 42,
            source: None,
            state: okilum_t3::chat_drafts::AttachmentState::Pending,
        }
    }

    #[gpui::test]
    fn restart_restores_multiline_text_and_attachment_row_without_crossing_identity(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::test_init);
        let state = tempfile::tempdir().unwrap();
        let env = Environment::new("fixture", "https://drafts.invalid").unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = ProjectView::empty(Some(state.path().to_owned()), window, cx);
                view.binding = Some(binding("project", "manager"));
                view.chat = Some(Conversation::new("manager"));
                view.environment = Some(env.clone());
                view.draft_restore(window, cx);
                view
            });
            entity = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = entity.unwrap();
        visual.run_until_parked();
        view.update_in(visual, |view, window, cx| {
            assert!(!view.threads.draft.loading);
            view.compose.update(cx, |input, cx| {
                input.set_value("First line\nSecond line", window, cx)
            });
            view.draft_set_attachments(vec![attachment()], window, cx);
            view.draft_flush(cx);
            futures::executor::block_on(view.threads.draft.writer.as_ref().unwrap().barrier())
                .unwrap()
                .unwrap();
            // Replace the session as a restart would. A different project with
            // the same origin/thread must not receive this draft.
            view.binding = Some(binding("other-project", "manager"));
            view.draft_restore(window, cx);
        });
        visual.run_until_parked();
        view.update_in(visual, |view, window, cx| {
            assert!(view.compose.read(cx).value().is_empty());
            assert!(view.draft_attachments().is_empty());
            view.binding = Some(binding("project", "manager"));
            view.environment = Some(Environment::new("other", "https://other.invalid").unwrap());
            view.draft_restore(window, cx);
        });
        visual.run_until_parked();
        view.update_in(visual, |view, window, cx| {
            assert!(view.compose.read(cx).value().is_empty());
            view.environment = Some(env);
            view.draft_restore(window, cx);
        });
        visual.run_until_parked();
        view.update_in(visual, |view, window, cx| {
            assert_eq!(
                view.compose.read(cx).value().as_str(),
                "First line\nSecond line"
            );
            assert_eq!(view.draft_attachments()[0].name, "notes.txt");
            assert!(matches!(
                view.draft_attachments()[0].state,
                okilum_t3::chat_drafts::AttachmentState::NeedsReselect
            ));
            let before = view.binding.clone();
            let sent = view.draft_sent(cx);
            let mut added = attachment();
            added.id = "another-file".into();
            view.draft_set_attachments(vec![added], window, cx);
            view.draft_clear_after_send(&sent, window, cx);
            assert_eq!(
                view.compose.read(cx).value().as_str(),
                "First line\nSecond line",
                "attachment-only edit must survive send completion"
            );
            let sent = view.draft_sent(cx);
            view.draft_clear_after_send(&sent, window, cx);
            assert!(view.compose.read(cx).value().is_empty());
            assert!(view.draft_attachments().is_empty());
            assert_eq!(view.binding, before);
            futures::executor::block_on(view.threads.draft.writer.as_ref().unwrap().barrier())
                .unwrap()
                .unwrap();
        });
        assert_eq!(
            Store::<Attachment>::new(
                state.path(),
                &Environment::new("alias", "https://drafts.invalid/").unwrap(),
                "project",
                "manager"
            )
            .load()
            .unwrap()
            .draft,
            ChatDraft::default()
        );
    }

    #[test]
    fn queued_edits_and_empty_send_survive_worker_release_and_restart() {
        let state = tempfile::tempdir().unwrap();
        let env = Environment::new("qa", "https://drafts.invalid").unwrap();
        let store = Store::new(state.path(), &env, "project", "thread");
        let writer = Writer::new(store.clone(), store.load().unwrap()).unwrap();
        for n in 0..40 {
            writer.save(ChatDraft {
                text: format!("Draft {n}\nfinal line"),
                attachments: vec![],
            });
        }
        futures::executor::block_on(writer.barrier())
            .unwrap()
            .unwrap();
        assert_eq!(store.load().unwrap().draft.text, "Draft 39\nfinal line");
        writer.save(ChatDraft::default());
        futures::executor::block_on(writer.barrier())
            .unwrap()
            .unwrap();
        drop(writer);
        assert_eq!(store.load().unwrap().draft, ChatDraft::default());
    }

    #[test]
    fn stale_window_cannot_overwrite_other_window_and_reports_the_failure() {
        let state = tempfile::tempdir().unwrap();
        let env = Environment::new("qa", "https://drafts.invalid").unwrap();
        let store = Store::new(state.path(), &env, "project", "thread");
        let writer = Writer::new(store.clone(), store.load().unwrap()).unwrap();
        store
            .save(
                0,
                ChatDraft {
                    text: "other window".into(),
                    attachments: vec![],
                },
            )
            .unwrap();
        writer.save(ChatDraft {
            text: "local window".into(),
            attachments: vec![],
        });
        assert!(futures::executor::block_on(writer.barrier())
            .unwrap()
            .is_err());
        assert_eq!(store.load().unwrap().draft.text, "other window");
        assert_eq!(writer.desired.lock().unwrap().draft.text, "local window");
    }
    #[gpui::test]
    fn history_recall_is_durable_and_same_text_thread_switch_ends_browsing(
        cx: &mut gpui::TestAppContext,
    ) {
        // The real serialized disk worker completes draft barriers on its OS
        // thread; use GPUI's documented mixed real-I/O test mode.
        cx.executor().allow_parking();
        cx.update(crate::test_init);
        cx.update(reader_features::enable_test_modules);
        let state = tempfile::tempdir().unwrap();
        let mut project = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                // A mounted composer has an active typed owner (#2321/#2324).
                // No gateway is needed for this durable local-history test.
                let mut view = ProjectView::empty(Some(state.path().to_owned()), window, cx);
                view.binding = Some(binding("fixture-managers", "mcp:fixture-manager"));
                view.environment =
                    Some(Environment::new("fixture", "https://drafts.invalid").unwrap());
                view.chat = Some(Conversation::new("mcp:fixture-manager"));
                view.active = true;
                view.draft_restore(window, cx);
                view
            });
            let composer = view.read(cx).composer.clone();
            project = Some(view);
            Root::new(composer, window, cx)
        });
        let project = project.unwrap();
        visual.simulate_resize(size(px(600.), px(600.)));
        visual.run_until_parked();
        let row = |position: u64, text: &str| {
            serde_json::from_value::<okilum_t3::TimelineItem>(serde_json::json!({
            "position":position,"itemId":format!("history-{position}"),"type":"user_message","status":"completed","text":text,"updatedAt":"now"
        })).unwrap()
        };
        project.update_in(visual, |view, window, cx| {
            assert!(view.draft_ready());
            view.feed.update(cx, |feed, cx| {
                feed.reset(cx);
                feed.apply(
                    [row(1, "old thread older"), row(2, "shared")].iter(),
                    Some(&[]),
                    cx,
                );
            });
            view.compose.update(cx, |input, cx| input.focus(window, cx));
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        let (environment, old_thread) = project.update(visual, |view, cx| {
            assert_eq!(
                view.compose.read(cx).value().as_str(),
                "shared",
                "positive control: actual capture handler recalled history"
            );
            futures::executor::block_on(view.threads.draft.writer.as_ref().unwrap().barrier())
                .unwrap()
                .unwrap();
            (
                view.environment.clone().unwrap(),
                view.chat.as_ref().unwrap().thread_id.clone(),
            )
        });
        assert_eq!(
            Store::<Attachment>::new(state.path(), &environment, "fixture-managers", &old_thread)
                .load()
                .unwrap()
                .draft
                .text,
            "shared",
            "recall must use the ordinary durable draft writer"
        );
        let next_store = Store::<Attachment>::new(
            state.path(),
            &environment,
            "fixture-managers",
            "other-manager",
        );
        next_store
            .save(
                0,
                ChatDraft {
                    text: "shared".into(),
                    attachments: vec![],
                },
            )
            .unwrap();
        project.update_in(visual, |view, window, cx| {
            view.binding = Some(binding("fixture-managers", "other-manager"));
            view.chat = Some(Conversation::new("other-manager"));
            view.feed.update(cx, |feed, cx| {
                feed.reset(cx);
                feed.apply(
                    [row(3, "new thread older"), row(4, "shared")].iter(),
                    Some(&[]),
                    cx,
                );
            });
            view.draft_prepare_identity(window, cx);
            view.draft_restore(window, cx);
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        project.update_in(visual, |view, window, cx| {
            assert_eq!(
                view.compose.read(cx).value().as_str(),
                "shared",
                "matching restored text must not inherit the old thread's recall position"
            );
            view.compose
                .update(cx, |input, cx| input.set_value("", window, cx));
            view.draft_changed(window, cx);
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        project.read_with(visual, |view, cx| {
            assert_eq!(
                view.compose.read(cx).value().as_str(),
                "new thread older",
                "the new scope owns its own history"
            )
        });
    }
}

// BEGIN PURE HANDOFF IDENTITY
/// Separate from the disk restore epoch: hiding/replacing a composer must also
/// revoke pending handoffs, even when the durable conversation key is unchanged.
#[derive(Default)]
struct HandoffIdentity {
    visible: bool,
    scope: Option<(Key, u64, u64)>,
    generation: Option<u64>,
}
impl HandoffIdentity {
    fn invalidate(&mut self) {
        self.scope = None;
        self.generation = None;
    }

    fn resolve(&mut self, scope: Option<(Key, u64, u64)>) -> Option<u64> {
        let scope = scope.filter(|_| self.visible);
        if scope.is_none() {
            self.invalidate();
            return None;
        }
        if self.scope != scope {
            // Process-wide identity also prevents replay into a new ProjectView
            // whose per-view connection/draft epochs start at zero again.
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            self.generation = NEXT
                .try_update(
                    std::sync::atomic::Ordering::Relaxed,
                    std::sync::atomic::Ordering::Relaxed,
                    |id| id.checked_add(1),
                )
                .ok();
            self.scope = scope;
        }
        self.generation
    }
}
#[cfg(test)]
mod handoff_identity_tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn discuss_1784_view_and_connection_epochs_are_independent_components() {
        let key = ("origin".into(), "project".into(), "thread".into());
        let mut state = HandoffIdentity {
            visible: true,
            ..Default::default()
        };
        let first = state.resolve(Some((key.clone(), 1, 2))).unwrap();
        assert_eq!(state.resolve(Some((key.clone(), 1, 2))), Some(first));
        let replaced = state.resolve(Some((key.clone(), 2, 1))).unwrap();
        assert_ne!(
            replaced, first,
            "equal sums cannot alias view and connection epochs"
        );
        let returned = state.resolve(Some((key, 1, 2))).unwrap();
        assert_ne!(returned, first, "A → B → A must receive a fresh identity");
    }
    #[test]
    fn visible_identity_revokes_on_hide_access_and_replacement() {
        let scope = (("origin".into(), "project".into(), "thread".into()), 1, 1);
        let mut state = HandoffIdentity::default();
        assert_eq!(state.resolve(Some(scope.clone())), None);
        state.visible = true;
        let first = state.resolve(Some(scope.clone())).unwrap();
        assert_eq!(state.resolve(Some(scope.clone())), Some(first));
        state.visible = false;
        assert_eq!(state.resolve(Some(scope.clone())), None);
        state.visible = true;
        let reopened = state.resolve(Some(scope.clone())).unwrap();
        assert_ne!(first, reopened);
        assert_eq!(state.resolve(None), None); // live access was revoked
        let restored = state.resolve(Some(scope.clone())).unwrap();
        assert_ne!(restored, reopened);
        state.invalidate(); // owner replaced same-thread surface/access
        assert_ne!(state.resolve(Some(scope.clone())).unwrap(), restored);
        let mut other_view = HandoffIdentity {
            visible: true,
            ..Default::default()
        };
        assert_ne!(other_view.resolve(Some(scope)).unwrap(), restored);
    }
}
// END PURE HANDOFF IDENTITY

/// A short-lived UI-thread adapter; target validation and insertion cannot be
/// separated by an async await. The owner must never use it for a webview panel.
pub(super) struct NativeComposerPort<'a, 'cx> {
    view: &'a mut ProjectView,
    window: &'a mut Window,
    cx: &'a mut Context<'cx, ProjectView>,
}

impl ProjectView {
    /// Reader owns physical visibility. Identical render notifications leave
    /// the current lease untouched; explicit lifecycle changes revoke it.
    pub(crate) fn draft_handoff_surface_visible(&mut self, visible: bool, cx: &App) {
        if self.threads.draft.handoff_surface_visible == visible {
            return;
        }
        self.threads.draft.handoff_surface_visible = visible;
        self.draft_handoff_connection_changing();
        if visible {
            self.draft_handoff_connection_ready(cx);
        }
    }

    pub(super) fn draft_handoff_surface_is_visible(&self) -> bool {
        self.threads.draft.handoff_surface_visible
    }

    pub(super) fn draft_handoff_owner_ready(&self, cx: &App) -> bool {
        self.threads.draft.error.is_none()
            && self.draft_key() == self.threads.draft.key
            && self
                .composer
                .read(cx)
                .typed_scope()
                .is_some_and(|scope| self.draft_typed_owner_valid(scope))
    }

    /// Required owner hook before showing/hiding/replacing the native surface,
    /// and before revoking/replacing access (even for the same thread). Repeated
    /// calls deliberately invalidate old requests. Webview visibility uses false.
    pub(super) fn draft_handoff_visibility(&mut self, visible: bool) {
        self.threads.draft.handoff.invalidate();
        self.threads.draft.handoff.visible = visible;
        self.discuss.set_visible(None);
    }

    pub(super) fn draft_handoff_target(
        &mut self,
        cx: &App,
    ) -> Option<okilum_t3::draft_handoff::Target> {
        let allowed = self.active
            && self.section == ProjectSection::Manager
            && !self.executors_open
            && self.threads.draft.handoff_surface_visible
            && self.draft_handoff_owner_ready(cx)
            && !self.native.deleted()
            && reader_features::enabled(reader_features::Feature::Agents, cx)
            && self.native.model_client().is_some_and(|client| {
                client.access_allowed()
                    && !self
                        .threads
                        .handoff_revoked
                        .as_ref()
                        .is_some_and(|revoked| Arc::ptr_eq(revoked, &client))
            });
        let key = self
            .draft_key()
            .filter(|key| allowed && Some(key) == self.threads.draft.key.as_ref());
        // Keep epochs as independent identity components. Arithmetic mixing
        // would collide for different connection/view generations.
        let scope = key
            .clone()
            .map(|key| (key, self.generation, self.native.connection_epoch));
        let generation = self.threads.draft.handoff.resolve(scope)?;
        let (_, project, thread) = key?;
        Some(okilum_t3::draft_handoff::Target {
            project,
            thread,
            generation,
        })
    }

    pub(super) fn draft_handoff_port<'a, 'cx>(
        &'a mut self,
        window: &'a mut Window,
        cx: &'a mut Context<'cx, Self>,
    ) -> NativeComposerPort<'a, 'cx> {
        NativeComposerPort {
            view: self,
            window,
            cx,
        }
    }
}

impl okilum_t3::draft_handoff::ComposerPort for NativeComposerPort<'_, '_> {
    fn append(
        &mut self,
        request: &okilum_t3::draft_handoff::Request,
    ) -> Result<okilum_t3::draft_handoff::Acknowledgement, okilum_t3::draft_handoff::Refusal> {
        let target = self.view.draft_handoff_target(self.cx);
        let input = self.view.compose.read(self.cx);
        if input.presentation().is_disabled()
            || !input.is_editable()
            || input.ime_settling(self.cx)
            || input.completion_active(self.cx)
        {
            return Err(okilum_t3::draft_handoff::Refusal::Unavailable);
        }
        let has_source = !self
            .view
            .composer
            .read(self.cx)
            .typed_source_text(self.cx)
            .is_empty();
        // Move only the replay guard out, allowing the normal InputEvent capture
        // to run without borrowing draft state across the input transaction.
        let mut receiver = std::mem::take(&mut self.view.threads.draft.handoff_receiver);
        let result = receiver.apply(target.as_ref(), request, |text| {
            self.view.compose.update(self.cx, |input, cx| {
                let end = input.value().len();
                input.set_selected_range(end..end, cx);
                let insertion = if !has_source {
                    text.to_owned()
                } else {
                    format!("\n\n{text}")
                };
                input.insert(insertion, self.window, cx);
                self.window.focus(&input.focus_handle(cx), cx);
            });
            Ok(())
        });
        self.view.threads.draft.handoff_receiver = receiver;
        if result.is_ok() {
            // Preserve the ordinary serialized revision-aware writer and its
            // visible failure path. This acknowledgement confirms insertion,
            // never disk durability or message preparation/send.
            self.view.draft_changed(self.window, self.cx);
            self.cx.notify();
        }
        result
    }
}

#[cfg(test)]
mod typed_1478_tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn typed_1478_hide_quit_flush_retains_full_token_and_revokes_old_visible_scope(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        cx.update(crate::test_init);
        cx.update(reader_features::enable_test_modules);
        let state = tempfile::tempdir().unwrap();
        let environment = Environment::new("fixture", "https://typed.invalid").unwrap();
        let mut owner = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = ProjectView::empty(Some(state.path().to_owned()), window, cx);
                view.binding = AreaBinding::parse("t3:\n  schema_version: 1\n  environment: fixture\n  manager: {project_id: project, thread_id: thread, workspace_mode: local}\n  executor: {project_id: executors}\n  default_models:\n    manager: {instanceId: codex, model: gpt-6.1-sol}\n    executor: {instanceId: codex, model: gpt-6.1-sol}\n").unwrap();
                view.chat = Some(Conversation::new("thread"));
                view.environment = Some(environment.clone());
                view.draft_restore(window, cx);
                view
            });
            let composer = view.read(cx).composer.clone();
            owner = Some(view);
            Root::new(composer, window, cx)
        });
        let view = owner.unwrap();
        visual.run_until_parked();
        let old_scope = view.update_in(visual, |view, window, cx| {
            let mut draft =
                super::super::message_types::TypedDraft::from_text("BUG: Full\r\n🧠  ".into());
            assert!(draft.detect_token());
            view.composer.update(cx, |composer, cx| {
                composer
                    .replace_typed_draft(
                        draft.text.clone(),
                        Some(draft.retained_metadata()),
                        window,
                        cx,
                    )
                    .unwrap()
            });
            view.draft_changed(window, cx);
            let old = view.composer.read(cx).typed_scope().unwrap().clone();
            assert_eq!(view.compose.read(cx).value().as_str(), " Full\r\n🧠  ");
            view.set_active(false, window, cx);
            assert!(view.composer.read(cx).typed_scope().is_none());
            assert!(!view.draft_typed_owner_valid(&old));
            // The ordinary quit path calls capture again after hide.
            view.draft_flush(cx);
            futures::executor::block_on(view.threads.draft.writer.as_ref().unwrap().barrier())
                .unwrap()
                .unwrap();
            old
        });
        let store = Store::<Attachment>::new(state.path(), &environment, "project", "thread");
        let (saved, metadata) = store.load_with_metadata().unwrap();
        assert_eq!(saved.draft.text, "BUG: Full\r\n🧠  ");
        let restored = super::super::message_types::TypedDraft::restore_metadata(
            saved.draft.text,
            serde_json::from_value(metadata.unwrap()).unwrap(),
        )
        .unwrap();
        assert!(restored.has_token_pill());
        let edit_revision = view.update_in(visual, |view, window, cx| {
            // Reopen through the draft boundary only; no native restore/client.
            view.active = true;
            view.draft_restore(window, cx);
            view.typed_sync(cx);
            assert_ne!(view.composer.read(cx).typed_scope(), Some(&old_scope));
            assert_eq!(view.composer.read(cx).typed_source_text(cx), restored.text);
            // A spurious capture would increment the revision. Make the guard
            // observable even though the current visible draft is unchanged.
            view.threads.draft.last_edit = None;
            let edit_revision = view.threads.draft.edit_revision;
            view.composer.update(cx, |_, cx| {
                cx.emit(super::super::composer::TypedDraftChanged {
                    scope: old_scope.clone(),
                    text: "stale".into(),
                    metadata: restored.retained_metadata(),
                })
            });
            edit_revision
        });
        visual.run_until_parked();
        view.read_with(visual, |view, cx| {
            assert_eq!(view.composer.read(cx).typed_source_text(cx), restored.text);
            assert_eq!(
                view.threads.draft.edit_revision, edit_revision,
                "old-generation event cannot capture a reopened composer"
            );
        });
        view.update_in(visual, |view, window, cx| {
            let sent = view.draft_sent(cx);
            view.set_active(false, window, cx);
            view.draft_clear_after_send(&sent, window, cx);
            futures::executor::block_on(view.threads.draft.writer.as_ref().unwrap().barrier())
                .unwrap()
                .unwrap();
            assert_eq!(
                store.load_with_metadata().unwrap().0.draft.text,
                "",
                "retained send completion while hidden clears the suspended draft too"
            );
            view.active = true;
            view.draft_restore(window, cx);
            assert!(view.composer.read(cx).typed_source_text(cx).is_empty());
            assert!(!view.composer.read(cx).typed_has_authored_metadata());
        });
    }
}
