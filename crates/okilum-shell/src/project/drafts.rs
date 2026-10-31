//! Per-conversation durable composer state. Disk work is serialized off the UI
//! thread, and quit awaits a barrier behind every queued edit.
use super::*;
use futures::channel::oneshot;
use okilum_t3::chat_drafts::{Attachment, Draft, Snapshot, Store};
use std::sync::{mpsc, Mutex};

type Key = (String, String, String);
type ChatDraft = Draft<Attachment>;
type Saved = Snapshot<Attachment>;
type Barrier = oneshot::Receiver<Result<(), String>>;

enum Work {
    Save,
    Flush(oneshot::Sender<Result<(), String>>),
}

/// The worker owns its store and revision. Dropping a view closes its queue;
/// already queued edits continue to drain against their original identity.
struct Writer {
    desired: Arc<Mutex<ChatDraft>>,
    sender: mpsc::Sender<Work>,
}

#[derive(Default)]
struct Writers {
    views: Vec<WeakEntity<ProjectView>>,
    workers: Vec<Arc<Writer>>,
}
impl Global for Writers {}

fn install_shutdown(cx: &mut App) {
    if cx.has_global::<Writers>() {
        return;
    }
    cx.set_global(Writers::default());
    cx.on_app_quit(|cx| {
        // Capture still-live composers before adding barriers. Keep workers
        // for released windows too, so closing the last window cannot race a
        // final asynchronous save against process exit.
        let views = cx.global::<Writers>().views.clone();
        for view in views {
            let _ = view.update(cx, |view, cx| view.draft_capture(cx));
        }
        let barriers: Vec<_> = cx
            .global::<Writers>()
            .workers
            .iter()
            .map(|w| w.barrier())
            .collect();
        async move {
            for barrier in barriers {
                if !matches!(barrier.await, Ok(Ok(()))) {
                    eprintln!("Manager draft could not be flushed before quit");
                }
            }
        }
    })
    .detach();
}
impl Writer {
    fn new(store: Store<Attachment>, initial: Saved) -> std::io::Result<Self> {
        let desired = Arc::new(Mutex::new(initial.draft.clone()));
        let pending = desired.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("manager-draft".into())
            .spawn(move || {
                let mut saved = initial;
                let mut failure = None;
                while let Ok(work) = receiver.recv() {
                    let draft = pending.lock().unwrap().clone();
                    if failure.is_none() && draft != saved.draft {
                        match store.save(saved.revision, draft) {
                            Ok(next) => saved = next,
                            Err(error) => failure = Some(error.to_string()),
                        }
                    }
                    if let Work::Flush(reply) = work {
                        let _ = reply.send(failure.clone().map_or(Ok(()), Err));
                    }
                }
            })?;
        Ok(Self { desired, sender })
    }

    fn save(&self, draft: ChatDraft) {
        *self.desired.lock().unwrap() = draft;
        let _ = self.sender.send(Work::Save);
    }

    fn barrier(&self) -> Barrier {
        let (sender, receiver) = oneshot::channel();
        let _ = self.sender.send(Work::Flush(sender));
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
}

/// Captures exactly the local draft handed to durable message preparation.
/// A later edit (including an attachment-only edit) must survive its completion.
#[derive(Clone)]
pub(super) struct Sent {
    key: Option<Key>,
    draft: ChatDraft,
}

impl ProjectView {
    /// Hide the previous conversation's composer while its replacement native
    /// credential is being restored. The old writer already owns its snapshot.
    pub(super) fn draft_prepare_identity(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.draft_key() != self.threads.draft.key {
            self.threads.draft.loading = true;
            self.compose.update(cx, |input, cx| {
                input.set_value("", window, cx);
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
        if self.threads.draft.subscriptions.is_empty() {
            install_shutdown(cx);
            let weak = cx.weak_entity();
            cx.global_mut::<Writers>().views.push(weak);
            let input = cx.subscribe_in(
                &self.compose,
                window,
                |this, _, _: &InputEvent, window, cx| this.draft_changed(window, cx),
            );
            self.threads.draft.subscriptions.push(input);
        }
        let Some(key) = self.draft_key() else { return };
        if self.threads.draft.key.as_ref() == Some(&key) {
            return;
        }
        self.draft_capture(cx);
        let draft = &mut self.threads.draft;
        draft.epoch = draft.epoch.wrapping_add(1);
        let epoch = draft.epoch;
        draft.key = Some(key.clone());
        draft.writer = None;
        draft.attachments.clear();
        draft.error = None;
        draft.loading = true;
        self.compose.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_disabled(true, cx);
        });
        let Some(state) = self.state.clone() else {
            self.threads.draft.loading = false;
            self.threads.draft.error = Some(
                "Draft storage is unavailable. Text is kept only while this window is open.".into(),
            );
            self.compose
                .update(cx, |input, cx| input.set_disabled(false, cx));
            return;
        };
        let environment = self.environment.clone().unwrap();
        let environment_id = self.identity.as_ref().map(|i| i.environment_id.clone());
        let identity_client = self.native.model_client().filter(|c| c.access_allowed());
        let store = Store::<Attachment>::new(&state, &environment, &key.1, &key.2);
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut saved = store.load().map_err(|error| error.to_string())?;
                    let original = saved.draft.clone();
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
                    let restored = saved.draft.clone();
                    // Start from the actual disk version, then retain restored status.
                    saved.draft = original;
                    let writer = Writer::new(store, saved)
                        .map_err(|_| "Could not start draft storage".to_owned())?;
                    writer.save(restored.clone());
                    Ok::<_, String>((writer, restored))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.threads.draft.epoch != epoch || this.draft_key().as_ref() != Some(&key) {
                    return;
                }
                this.threads.draft.loading = false;
                match result {
                    Ok((writer, restored)) => {
                        let writer = Arc::new(writer);
                        cx.global_mut::<Writers>().workers.push(writer.clone());
                        this.threads.draft.writer = Some(writer);
                        this.threads.draft.attachments = restored.attachments;
                        this.compose
                            .update(cx, |input, cx| input.set_value(restored.text, window, cx));
                    }
                    Err(error) => {
                        this.threads.draft.error =
                            Some(format!("{error}. The saved draft was preserved."))
                    }
                }
                this.compose
                    .update(cx, |input, cx| input.set_disabled(false, cx));
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
            writer.save(self.draft_sent(cx).draft);
        }
    }

    pub(super) fn draft_flush(&self, cx: &App) {
        self.draft_capture(cx);
        // The writer queue owns the captured bytes across switch/hide/drop.
    }

    pub(super) fn draft_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.draft_capture(cx);
        let Some(writer) = self.threads.draft.writer.as_ref() else {
            return;
        };
        let barrier = writer.barrier();
        let key = self.threads.draft.key.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = barrier.await;
            let _ = this.update(cx, |this, cx| {
                if this.threads.draft.key != key { return; }
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
        Sent {
            key: self.draft_key(),
            draft: ChatDraft {
                text: self.compose.read(cx).value().to_string(),
                attachments: self.threads.draft.attachments.clone(),
            },
        }
    }

    pub(super) fn draft_clear_after_send(
        &mut self,
        sent: &Sent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now = self.draft_sent(cx);
        if now.key == sent.key && now.draft == sent.draft {
            self.threads.draft.attachments.clear();
            self.compose
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.draft_changed(window, cx);
        }
    }

    pub(super) fn draft_attachments(&self) -> &[Attachment] {
        &self.threads.draft.attachments
    }

    // Slice 4 replaces this attachment guard when message preparation carries
    // real asset references. Never send text and silently discard its files.
    pub(super) fn draft_is_sendable(&self) -> bool {
        !self.threads.draft.loading && self.draft_attachments().is_empty()
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

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

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
        assert_eq!(writer.desired.lock().unwrap().text, "local window");
    }
}
