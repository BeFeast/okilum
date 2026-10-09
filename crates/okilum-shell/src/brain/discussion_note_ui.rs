//! Transient new-note review. Only real source snapshots enter EditorRecovery.
use super::discussion_note::{self as note, Origin, PendingSave, Selection, Target};
use super::*;

pub(super) struct DiscussionNoteUi {
    target: Option<Target>,
    origin: Option<Origin>,
    endpoint: Option<SocketAddr>,
    title: Entity<InputState>,
    body: Entity<TextareaState>,
    filename: Entity<InputState>,
    operation: String,
    pending: Option<PendingSave>,
    receipt: Option<Value>,
    filename_conflict: bool,
    flight: bool,
    sequence: u64,
    error: Option<String>,
    pub(super) destination: Option<PendingNavigation>,
    pub(super) closing: bool,
}
impl DiscussionNoteUi {
    pub(super) fn new(window: &mut Window, cx: &mut Context<BrainView>) -> Self {
        Self {
            target: None,
            origin: None,
            endpoint: None,
            title: cx.new(|cx| InputState::new(window, cx).placeholder("Note title")),
            body: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .rows(12)
                    .placeholder("Note body")
            }),
            filename: cx.new(|cx| InputState::new(window, cx).placeholder("Optional filename.md")),
            operation: uuid(),
            pending: None,
            receipt: None,
            filename_conflict: false,
            flight: false,
            sequence: 0,
            error: None,
            destination: None,
            closing: false,
        }
    }
    pub(super) fn active(&self) -> bool {
        self.target.is_some()
    }
    pub(super) fn clear(&mut self) {
        self.sequence += 1;
        self.target = None;
        self.origin = None;
        self.endpoint = None;
        self.pending = None;
        self.receipt = None;
        self.filename_conflict = false;
        self.error = None;
        self.flight = false;
        self.destination = None;
        self.closing = false;
        self.operation = uuid();
    }
}

impl BrainView {
    fn note_source_blocked(&self, cx: &App) -> bool {
        self.editor_blocks_note_creation()
            || (self.source_editable && self.source.value(cx).as_ref() != self.source_original)
    }
    pub(super) fn note_writable(&self) -> bool {
        self.expected_workspace
            .as_ref()
            .is_some_and(|w| w["managed"] == true)
            && self.capabilities["source_write"] == true
    }
    pub(super) fn note_target_matches(&self, selection: &Selection, endpoint: SocketAddr) -> bool {
        self.endpoint == endpoint
            && self.expected_workspace.as_ref() == Some(&selection.workspace)
            && self.goal_id() == selection.goal_id
            && self.conversation_id.as_ref() == Some(&selection.conversation_id)
            && Selection::capture(
                &selection.workspace,
                &self.goal_id(),
                &self.conversation,
                selection.message_index,
            )
            .is_ok_and(|current| current == *selection)
    }
    fn note_review_target_matches(&self, target: &Target, endpoint: SocketAddr) -> bool {
        self.endpoint == endpoint
            && self.expected_workspace.as_ref() == Some(target.workspace())
            && self.goal_id() == target.goal_id()
            && match target {
                Target::UserAuthored { .. } => true,
                Target::Discussion(selection) => self.note_target_matches(selection, endpoint),
            }
    }
    pub(super) fn start_new_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.discussion_note.active() {
            return;
        }
        if self.goal_id().is_empty() {
            self.error = Some("Select a goal before creating a note.".into());
            cx.notify();
            return;
        }
        if !self.note_writable() {
            self.error = Some("A writable managed workspace is required to create a note.".into());
            cx.notify();
            return;
        }
        let target =
            Target::user_authored(self.expected_workspace.as_ref().unwrap(), &self.goal_id());
        match target {
            Ok(target) => self.prepare_new_note(target, self.endpoint, window, cx),
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }
    pub(super) fn prepare_new_note(
        &mut self,
        target: Target,
        endpoint: SocketAddr,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.discussion_note.active() {
            return;
        }
        if !target.is_user_authored()
            || !self.note_writable()
            || !self.note_review_target_matches(&target, endpoint)
        {
            self.error = Some("The goal or workspace changed before creating the note. Start New note again from the intended goal.".into());
            cx.notify();
            return;
        }
        if self.note_source_blocked(cx) {
            self.pending_navigation = Some(PendingNavigation::NewNote(target, endpoint));
            self.surface = Surface::Source;
            self.error =
                Some("Save or discard the source draft before creating a new note.".into());
            cx.notify();
            return;
        }
        if self.source_loading {
            self.error =
                Some("Wait for the source to finish loading before creating a note.".into());
            cx.notify();
            return;
        }
        let state = &mut self.discussion_note;
        state.clear();
        state.target = Some(target);
        state.endpoint = Some(self.endpoint);
        state
            .title
            .update(cx, |input, cx| input.set_value("", window, cx));
        state
            .body
            .update(cx, |input, cx| input.set_value("", window, cx));
        state
            .filename
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.show_capture = false;
        self.error = None;
        state.title.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }
    pub(super) fn start_discussion_note(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.discussion_note.active() || !self.note_writable() {
            return;
        }
        let selection = self
            .expected_workspace
            .as_ref()
            .ok_or_else(|| "A writable managed workspace is required.".to_owned())
            .and_then(|workspace| {
                Selection::capture(workspace, &self.goal_id(), &self.conversation, index)
            });
        match selection {
            Ok(selection) => self.prepare_discussion_note(selection, window, cx),
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }
    pub(super) fn prepare_discussion_note(
        &mut self,
        selection: Selection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.discussion_note.flight || !self.note_writable() {
            return;
        }
        if self.note_source_blocked(cx) {
            self.pending_navigation = Some(PendingNavigation::DiscussionNote(selection));
            self.surface = Surface::Source;
            self.error = Some(
                "Save or discard the source draft before reviewing the answer as a note.".into(),
            );
            cx.notify();
            return;
        }
        if self.source_loading || !self.note_target_matches(&selection, self.endpoint) {
            self.error = Some("The source or Discussion changed. Reload the saved answer before preparing a note.".into());
            cx.notify();
            return;
        }
        let endpoint = self.endpoint;
        let state = &mut self.discussion_note;
        state.target = Some(Target::Discussion(selection.clone()));
        state.endpoint = Some(endpoint);
        state.sequence += 1;
        let sequence = state.sequence;
        state.flight = true;
        state.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let captured = selection.clone();
            let result = cx.background_executor().spawn(async move {
                let snapshot = rpc_guarded(endpoint, json!({"op":"source_read","path":captured.path}), Some(&captured.workspace))?;
                note::prepare(&captured, &snapshot)
            }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.discussion_note.sequence != sequence || this.discussion_note.target.as_ref() != Some(&Target::Discussion(selection.clone())) { return; }
                this.discussion_note.flight = false;
                if !this.note_target_matches(&selection, endpoint) || !this.note_writable() || this.note_source_blocked(cx) {
                    this.discussion_note.error = Some("Discussion or workspace changed during the read. The review is retained; reload the original answer explicitly.".into());
                } else {
                    match result {
                        Ok(origin) => {
                            let title = note::suggested_title(&origin);
                            this.discussion_note.title.update(cx, |input,cx| input.set_value(title,window,cx));
                            this.discussion_note.body.update(cx, |input,cx| input.set_value(origin.selection.text.clone(),window,cx));
                            this.discussion_note.filename.update(cx, |input,cx| input.set_value("",window,cx));
                            this.discussion_note.origin = Some(origin);
                        }
                        Err(error) => this.discussion_note.error = Some(error),
                    }
                }
                cx.notify();
            });
        }).detach();
    }
    pub(super) fn save_discussion_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.discussion_note.flight || self.busy {
            return;
        }
        let Some(target) = self.discussion_note.target.clone() else {
            return;
        };
        let Some(endpoint) = self.discussion_note.endpoint else {
            return;
        };
        if !self.note_review_target_matches(&target, endpoint)
            || !self.note_writable()
            || self.note_source_blocked(cx)
        {
            self.discussion_note.error = Some("Return to the original writable goal before saving. The complete review and any uncertain request are retained.".into());
            cx.notify();
            return;
        }
        if self.discussion_note.pending.is_none() {
            let title = self.discussion_note.title.read(cx).value().to_string();
            let body = self.discussion_note.body.read(cx).value().to_string();
            let filename = self.discussion_note.filename.read(cx).value().to_string();
            match note::freeze_target(
                &target,
                self.discussion_note.origin.as_ref(),
                &title,
                &body,
                (!filename.is_empty()).then_some(filename.as_str()),
                &self.discussion_note.operation,
            ) {
                Ok(pending) => self.discussion_note.pending = Some(pending),
                Err(error) => {
                    self.discussion_note.error = Some(error);
                    cx.notify();
                    return;
                }
            }
        }
        let pending = self.discussion_note.pending.clone().unwrap();
        let receipt = self.discussion_note.receipt.clone();
        let sequence = self.discussion_note.sequence;
        self.discussion_note.flight = true;
        self.discussion_note.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this,cx| {
            let frozen = pending.clone();
            let result = cx.background_executor().spawn(async move {
                let receipt = if let Some(receipt) = receipt { receipt } else {
                    match rpc_guarded(endpoint, frozen.request.clone(), Some(&frozen.workspace)) {
                        Ok(receipt) => receipt,
                        Err(error) => return (None, Err(error)),
                    }
                };
                if let Err(error) = note::validate_receipt(&frozen,&receipt) { return (Some(receipt), Err(error)); }
                let readback = rpc_guarded(endpoint,json!({"op":"source_read","path":frozen.path}),Some(&frozen.workspace))
                    .and_then(|snapshot| note::validate_readback(&frozen,&snapshot).map(|state| (snapshot,state)));
                (Some(receipt),readback)
            }).await;
            let _ = this.update_in(cx, |this,window,cx| {
                if this.discussion_note.sequence != sequence || this.discussion_note.pending.as_ref() != Some(&pending) { return; }
                this.discussion_note.flight = false;
                let (receipt, readback) = result;
                // Only an exact validated receipt changes retries into read-only reconciliation.
                if let Some(receipt) = &receipt {
                    if note::validate_receipt(&pending,receipt).is_ok() { this.discussion_note.receipt = Some(receipt.clone()); }
                }
                if !this.note_review_target_matches(&target,endpoint) || this.note_source_blocked(cx) {
                    this.discussion_note.error = Some("The target changed before save readback. The frozen request is retained; return to its original goal to reconcile.".into());
                    cx.notify(); return;
                }
                if receipt.as_ref().and_then(|r| r.get("source_conflict"))
                    .is_some_and(|view| note::validate_filename_conflict(&pending,view).is_ok()) {
                    this.discussion_note.filename_conflict = true;
                    this.discussion_note.error = Some("That filename already exists. Enter another filename and explicitly save it; the existing note will not be overwritten.".into());
                    cx.notify(); return;
                }
                match readback {
                    Ok((snapshot,state)) => {
                        let destination = this.discussion_note.destination.take();
                        let closing = this.discussion_note.closing;
                        this.discussion_note.clear();
                        this.load_source(snapshot,window,cx);
                        this.surface = Surface::Source;
                        this.collection = Collection::Sources;
                        this.notice = if matches!(state,note::Readback::Changed) {
                            "Note saved. Its source changed since saving; the current validated content is open.".into()
                        } else { "Note saved. Its current source is open.".into() };
                        this.error = if matches!(state,note::Readback::Changed) { Some(this.notice.clone()) } else { None };
                        this.batch(vec![json!({"op":"source_list"})],window,cx);
                        if closing { this.note_finish_close(window,cx); }
                        else if let Some(destination) = destination {
                            this.pending_navigation = Some(destination);
                            this.navigation_after_source = true;
                            // Source-list completion resumes the destination without replacing a draft.
                        }
                    }
                    Err(error) => { this.discussion_note.error = Some(format!("{error} The review is retained. Retry only this exact save or its readback.")); }
                }
                cx.notify();
            });
        }).detach();
    }
    pub(super) fn note_change_filename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.discussion_note.filename_conflict || self.discussion_note.flight {
            return;
        }
        let state = &self.discussion_note;
        let filename = state.filename.read(cx).value().to_string();
        if filename.is_empty() || state.pending.as_ref().is_some_and(|p| p.path == filename) {
            self.discussion_note.error =
                Some("Choose a different filename before saving a new operation.".into());
            cx.notify();
            return;
        }
        let Some(target) = state.target.as_ref() else {
            return;
        };
        let operation = uuid();
        match note::freeze_target(
            target,
            state.origin.as_ref(),
            &state.title.read(cx).value(),
            &state.body.read(cx).value(),
            Some(&filename),
            &operation,
        ) {
            Ok(pending) => {
                self.discussion_note.pending = Some(pending);
                self.discussion_note.operation = operation;
                self.discussion_note.receipt = None;
                self.discussion_note.filename_conflict = false;
                self.save_discussion_note(window, cx);
            }
            Err(error) => {
                self.discussion_note.error = Some(error);
                cx.notify();
            }
        }
    }
    pub(super) fn note_finish_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(token) = self.app_quit_token {
            self.editor_request_app_quit(token, window, cx);
            app_quit::queue_finish(token, cx);
        } else if self.editor_request_close(window, cx) {
            window.remove_window();
        }
    }
    pub(super) fn note_guard_navigation(
        &mut self,
        destination: PendingNavigation,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.reuse_guard_navigation(destination.clone(), cx) {
            return true;
        }
        if self.decision_guard_navigation(destination.clone(), cx) {
            return true;
        }
        if self.criteria_guard_navigation(destination.clone(), cx) {
            return true;
        }
        if !self.discussion_note.active() {
            return false;
        }
        self.discussion_note.destination = Some(destination);
        self.discussion_note.error = Some(
            "Keep editing, save the note, or explicitly leave this review before continuing."
                .into(),
        );
        cx.notify();
        true
    }
    pub(super) fn note_request_close(&mut self, cx: &mut Context<Self>) -> bool {
        if self.reuse_request_close(cx) {
            return true;
        }
        if self.decision_request_close(cx) {
            return true;
        }
        if self.criteria_request_close(cx) {
            return true;
        }
        if !self.discussion_note.active() {
            return false;
        }
        self.discussion_note.closing = true;
        self.discussion_note.error = Some("This note review has not been saved and read back. Keep editing, Save note, or explicitly leave before closing.".into());
        cx.notify();
        true
    }
    pub(super) fn note_keep_editing(&mut self, cx: &mut Context<Self>) {
        self.discussion_note.destination = None;
        self.discussion_note.closing = false;
        if let Some(token) = self.app_quit_token {
            app_quit::queue_cancel(token, cx);
        }
        cx.notify();
    }
    pub(super) fn leave_discussion_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.discussion_note.flight {
            return;
        }
        let destination = self.discussion_note.destination.take();
        let closing = self.discussion_note.closing;
        self.discussion_note.clear();
        if closing {
            self.note_finish_close(window, cx);
        } else if let Some(destination) = destination {
            self.pending_navigation = Some(destination);
            self.continue_navigation(window, cx);
        }
        cx.notify();
    }
    pub(super) fn note_navigate_surface(
        &mut self,
        surface: Surface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.note_guard_navigation(PendingNavigation::Surface(surface), cx) {
            return;
        }
        let changed = self.surface != surface;
        let source_collection = self.collection == Collection::Sources;
        if changed {
            self.clear_open_link_preview();
            self.reset_todoist_picker();
        }
        self.surface = surface;
        if surface == Surface::Context {
            self.open_context(window, cx);
        }
        if surface == Surface::Source {
            self.select_collection(Collection::Sources, window, cx);
            if changed
                && source_collection
                && !self.show_capture
                && self.source.managed().is_some()
                && self.source_snapshot.is_some()
            {
                self.schedule_preview(window, cx);
            }
        }
        cx.notify();
    }
    pub(super) fn note_navigate_conversation(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.note_guard_navigation(PendingNavigation::Conversation(id.clone()), cx) {
            return;
        }
        if self.busy || self.editor_closing() {
            return;
        }
        self.new_conversation.drafts.remove(&self.goal_id());
        self.reset_discussion_context();
        self.conversation = Value::Null;
        self.conversation_id = Some(id);
        if !self.new_conversation.pending.contains(&self.goal_id()) {
            self.pending_message = None;
        }
        self.persist_selection(window, cx);
    }
    pub(super) fn discussion_note_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let state = &self.discussion_note;
        let frozen = state.pending.is_some();
        let leaving = state.destination.is_some() || state.closing;
        let user_authored = state.target.as_ref().is_some_and(Target::is_user_authored);
        let ready = user_authored || state.origin.is_some();
        let busy = state.flight || self.busy;
        let mut panel = v_flex().id("discussion-note-review").flex_1().min_w_0().h_full().overflow_y_scroll().p_6().gap_3()
            .child(div().text_xl().font_weight(FontWeight::BOLD).child(if user_authored { "New note" } else { "Save Discussion answer as note" }))
            .child(div().text_sm().child(if user_authored { "Create an ordinary note for the current goal. The body is optional." } else { "Assistant-derived text is unverified. Review it before saving an ordinary note owned by this goal." }));
        if ready {
            panel = panel
                .child(div().child("Title"))
                .child(Input::new(&state.title).disabled(frozen || busy))
                .child(div().child(if user_authored {
                    "Body (optional)"
                } else {
                    "Body"
                }))
                .child(
                    Textarea::new(&state.body)
                        .disabled(frozen || busy)
                        .h(px(300.))
                        .flex_shrink_0(),
                )
                .child(
                    div()
                        .text_sm()
                        .child("Filename (optional; a unique name is generated when blank)"),
                )
                .child(
                    Input::new(&state.filename)
                        .disabled((frozen && !state.filename_conflict) || busy),
                );
        } else {
            panel = panel.child(
                div().child("Checking the saved answer against its canonical Discussion source…"),
            );
        }
        if let Some(error) = &state.error {
            panel = panel.child(div().text_sm().child(error.clone()));
        }
        if frozen && !state.filename_conflict {
            panel = panel.child(div().text_sm().child(if state.receipt.is_some() {
            "Save acknowledged. Readback is unresolved; Retry readback will only read the saved path."
        } else { "This save may have reached the backend. Title, body and request are frozen; Reconcile save reuses the identical operation." }));
        }
        let save_label = if state.filename_conflict {
            "Save with another filename"
        } else if state.receipt.is_some() {
            "Retry readback"
        } else if frozen {
            "Reconcile save"
        } else if leaving {
            "Save note and continue"
        } else {
            "Save note"
        };
        let leave_label = if state.filename_conflict {
            "Cancel"
        } else if state.receipt.is_some() {
            "Leave with unresolved readback"
        } else if frozen {
            "Leave with uncertain save"
        } else if leaving {
            "Discard review and continue"
        } else {
            "Cancel"
        };
        panel = panel.child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(
                    super::super::brand::control("discussion-note-save", cx)
                        .primary()
                        .label(save_label)
                        .disabled(!ready || busy || !self.note_writable())
                        .on_click(cx.listener(|this, _, window, cx| {
                            if this.discussion_note.filename_conflict {
                                this.note_change_filename(window, cx);
                            } else {
                                this.save_discussion_note(window, cx);
                            }
                        })),
                )
                .child(
                    super::super::brand::control("discussion-note-leave", cx)
                        .label(leave_label)
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.leave_discussion_note(window, cx)
                        })),
                )
                .when(leaving, |row| {
                    row.child(
                        super::super::brand::control("discussion-note-stay", cx)
                            .label("Keep editing")
                            .on_click(cx.listener(|this, _, _, cx| this.note_keep_editing(cx))),
                    )
                })
                .when(!ready, |row| {
                    row.child(
                        super::super::brand::control("discussion-note-reload", cx)
                            .label("Reload saved answer")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Some(Target::Discussion(selection)) =
                                    this.discussion_note.target.clone()
                                {
                                    this.prepare_discussion_note(selection, window, cx);
                                }
                            })),
                    )
                }),
        );
        panel.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use sha2::{Digest, Sha256};
    const BRAIN: &str = "01000000-0000-4000-8000-000000000001";
    const GOAL: &str = "02000000-0000-4000-8000-000000000001";
    const CHAT: &str = "03000000-0000-4000-8000-000000000001";

    pub(super) fn install(
        view: &mut BrainView,
        window: &mut Window,
        cx: &mut Context<BrainView>,
    ) -> Selection {
        let workspace = json!({"brain_id":BRAIN,"root":"/isolated/discussion-note-ui","records_dir":"records","managed":true});
        view.expected_workspace = Some(workspace.clone());
        view.capabilities = json!({"source_write":true});
        view.snapshot = json!({"goal":{"id":GOAL}});
        view.selected_goal_id = Some(GOAL.into());
        view.conversation_id = Some(CHAT.into());
        view.conversation = json!({"id":CHAT,"goal_id":GOAL,"path":format!("records/conversation-{CHAT}.md"),"messages":[{"role":"assistant","text":"Original answer\r\n"}]});
        let selection = Selection::capture(&workspace, GOAL, &view.conversation, 0).unwrap();
        let origin = Origin {
            selection: selection.clone(),
            source_revision: format!("sha256:{}", "a".repeat(64)),
        };
        view.discussion_note.target = Some(Target::Discussion(selection.clone()));
        view.discussion_note.endpoint = Some(view.endpoint);
        view.discussion_note.origin = Some(origin);
        view.discussion_note.title.update(cx, |input, cx| {
            input.set_value("Reviewed title", window, cx)
        });
        view.discussion_note.body.update(cx, |input, cx| {
            input.set_value("Reviewed bytes\r\n", window, cx)
        });
        view.busy = false;
        selection
    }
    pub(super) fn pending(view: &BrainView, cx: &App) -> PendingSave {
        note::freeze(
            view.discussion_note.origin.as_ref().unwrap(),
            &view.discussion_note.title.read(cx).value(),
            &view.discussion_note.body.read(cx).value(),
            None,
            &view.discussion_note.operation,
        )
        .unwrap()
    }

    #[gpui::test]
    fn new_note_requires_writable_goal_and_preserves_source_and_other_drafts(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.busy = true;
            view
        });
        view.update_in(cx, |v, window, cx| {
            install(v, window, cx);
            v.discussion_note.clear();
            v.snapshot = json!({});
            v.start_new_note(window, cx);
            assert!(!v.discussion_note.active());
            assert!(v.error.as_ref().unwrap().contains("Select a goal"));
            v.snapshot = json!({"goal":{"id":GOAL}});
            v.capabilities["source_write"] = json!(false);
            v.start_new_note(window, cx);
            assert!(!v.discussion_note.active());
            assert!(v.error.as_ref().unwrap().contains("writable"));
            v.capabilities["source_write"] = json!(true);
            v.conversation_id = None;
            v.conversation = Value::Null;
            v.source_original = "Original source".into();
            v.source_editable = true;
            v.source.reset("Unsaved source bytes\r\n", window, cx);
            v.compose.update(cx, |input, cx| {
                input.set_value("Unsent discussion", window, cx)
            });
            v.next_step
                .update(cx, |input, cx| input.set_value("Unsent stage", window, cx));
            v.selected_sources.insert("retained-context.md".into());
            v.start_new_note(window, cx);
            assert!(!v.discussion_note.active());
            let Some(PendingNavigation::NewNote(target, endpoint)) = v.pending_navigation.take()
            else {
                panic!("Expected existing source departure guard")
            };
            assert_eq!(v.source.value(cx).as_ref(), "Unsaved source bytes\r\n");
            v.source_editable = false;
            v.prepare_new_note(target, endpoint, window, cx);
            assert!(v.discussion_note.active());
            assert!(v.discussion_note.origin.is_none());
            assert!(v.discussion_note.body.read(cx).value().is_empty());
            v.leave_discussion_note(window, cx);
            assert!(!v.discussion_note.active());
            assert!(v.discussion_note.pending.is_none());
            assert_eq!(v.compose.read(cx).value().as_ref(), "Unsent discussion");
            assert_eq!(v.next_step.read(cx).value().as_ref(), "Unsent stage");
            assert!(v.selected_sources.contains("retained-context.md"));
            assert_eq!(v.source.value(cx).as_ref(), "Unsaved source bytes\r\n");
        });
    }

    #[gpui::test]
    fn new_note_target_ignores_conversation_but_retains_goal_workspace_and_endpoint(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.busy = true;
            view
        });
        view.update_in(cx, |v, window, cx| {
            install(v, window, cx);
            v.discussion_note.clear();
            v.conversation_id = None;
            v.conversation = Value::Null;
            v.start_new_note(window, cx);
            let target = v.discussion_note.target.clone().unwrap();
            let endpoint = v.endpoint;
            v.conversation_id = Some(uuid());
            assert!(v.note_review_target_matches(&target, endpoint));
            v.discussion_note
                .title
                .update(cx, |input, cx| input.set_value("User title", window, cx));
            let huge = "λ".repeat(editor_recovery::MAX_TEXT_BYTES / 2);
            v.discussion_note
                .body
                .update(cx, |input, cx| input.set_value(huge.clone(), window, cx));
            v.save_discussion_note(window, cx);
            assert!(v.discussion_note.pending.is_none());
            assert_eq!(v.discussion_note.body.read(cx).value().as_ref(), huge);
            v.snapshot["goal"]["id"] = json!(uuid());
            assert!(!v.note_review_target_matches(&target, endpoint));
            v.save_discussion_note(window, cx);
            assert!(v.discussion_note.pending.is_none());
            assert_eq!(
                v.discussion_note.title.read(cx).value().as_ref(),
                "User title"
            );
            v.snapshot["goal"]["id"] = json!(GOAL);
            v.expected_workspace.as_mut().unwrap()["root"] = json!("/another/workspace");
            assert!(!v.note_review_target_matches(&target, endpoint));
            v.expected_workspace = Some(target.workspace().clone());
            v.endpoint = "127.0.0.1:2".parse().unwrap();
            assert!(!v.note_review_target_matches(&target, endpoint));
            v.discussion_note.clear();
            v.prepare_new_note(target, endpoint, window, cx);
            assert!(!v.discussion_note.active());
        });
    }

    #[gpui::test]
    fn discussion_note_guards_navigation_close_quit_and_retains_review(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            v.busy = true;
            v
        });
        view.update_in(cx, |v, window, cx| {
            let selection = install(v, window, cx);
            assert!(v.dirty(cx));
            v.select_goal(uuid(), window, cx);
            assert!(matches!(
                v.discussion_note.destination,
                Some(PendingNavigation::Goal(_))
            ));
            v.note_keep_editing(cx);
            assert!(v.discussion_note.destination.is_none());
            v.select_collection(Collection::Inbox, window, cx);
            assert!(v.collection == Collection::Goals);
            assert!(v.discussion_note.active());
            assert!(!v.editor_request_close(window, cx));
            assert!(v.discussion_note.closing);
            v.note_keep_editing(cx);
            v.editor_request_app_quit(123, window, cx);
            assert!(!v.editor_app_quit_ready(123, cx));
            assert_eq!(
                v.discussion_note.target.as_ref(),
                Some(&Target::Discussion(selection.clone()))
            );
            assert_eq!(
                v.discussion_note.body.read(cx).value().as_ref(),
                "Reviewed bytes\r\n"
            );
            assert_eq!(
                v.discussion_note.title.read(cx).value().as_ref(),
                "Reviewed title"
            );
            assert!(v.discussion_note.pending.is_none());
        });
    }
    #[gpui::test]
    fn discussion_note_origin_waits_for_existing_source_and_cancel_keeps_other_drafts(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            v.busy = true;
            v
        });
        view.update_in(cx, |v, window, cx| {
            install(v, window, cx);
            v.discussion_note.clear();
            v.source_original = "Original source".into();
            v.source_editable = true;
            v.source.reset("Unsaved source bytes\r\n", window, cx);
            v.compose.update(cx, |input, cx| {
                input.set_value("Unsent discussion", window, cx)
            });
            v.next_step
                .update(cx, |input, cx| input.set_value("Unsent stage", window, cx));
            v.start_discussion_note(0, window, cx);
            assert!(!v.discussion_note.active());
            assert!(matches!(
                v.pending_navigation,
                Some(PendingNavigation::DiscussionNote(_))
            ));
            assert_eq!(v.source.value(cx).as_ref(), "Unsaved source bytes\r\n");
            v.pending_navigation = None;
            v.source_editable = false;
            install(v, window, cx);
            v.leave_discussion_note(window, cx);
            assert!(!v.discussion_note.active());
            assert_eq!(v.compose.read(cx).value().as_ref(), "Unsent discussion");
            assert_eq!(v.next_step.read(cx).value().as_ref(), "Unsent stage");
            assert_eq!(v.source.value(cx).as_ref(), "Unsaved source bytes\r\n");
        });
    }
    #[gpui::test]
    fn discussion_note_invalid_input_and_changed_target_preserve_all_review_bytes(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            v.busy = true;
            v
        });
        view.update_in(cx, |v, window, cx| {
            let selection = install(v, window, cx);
            v.discussion_note
                .filename
                .update(cx, |input, cx| input.set_value("_reserved.md", window, cx));
            v.save_discussion_note(window, cx);
            assert!(v.discussion_note.pending.is_none());
            assert!(v.discussion_note.error.is_some());
            let huge = "λ".repeat(editor_recovery::MAX_TEXT_BYTES / 2);
            v.discussion_note
                .filename
                .update(cx, |input, cx| input.set_value("", window, cx));
            v.discussion_note
                .body
                .update(cx, |input, cx| input.set_value(huge.clone(), window, cx));
            v.save_discussion_note(window, cx);
            assert!(v.discussion_note.pending.is_none());
            assert_eq!(v.discussion_note.body.read(cx).value().as_ref(), huge);
            assert!(v.note_target_matches(&selection, v.endpoint));
            v.expected_workspace.as_mut().unwrap()["root"] = json!("/different/root");
            assert!(!v.note_target_matches(&selection, v.endpoint));
            v.save_discussion_note(window, cx);
            assert!(v.discussion_note.pending.is_none());
            assert_eq!(v.discussion_note.body.read(cx).value().as_ref(), huge);
        });
    }
    #[gpui::test]
    fn discussion_note_uncertain_departure_is_explicit_and_inflight_cannot_leave(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            v.busy = true;
            v
        });
        view.update_in(cx, |v, window, cx| {
            install(v, window, cx);
            let frozen = pending(v, cx);
            v.discussion_note.pending = Some(frozen.clone());
            v.discussion_note.flight = true;
            v.leave_discussion_note(window, cx);
            assert_eq!(v.discussion_note.pending, Some(frozen.clone()));
            v.discussion_note.flight = false;
            v.note_keep_editing(cx);
            assert_eq!(v.discussion_note.pending, Some(frozen));
            v.leave_discussion_note(window, cx);
            assert!(!v.discussion_note.active());
        });
    }

    #[gpui::test]
    fn discussion_note_retries_identical_wire_and_acknowledged_readback_never_rewrites(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            v.busy = true;
            v
        });
        cx.run_until_parked();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut requests = vec![];
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                // Malformed receipt/readback is deliberately inconclusive.
                writeln!(
                    stream,
                    "{}",
                    json!({"schema":request["schema"],"id":request["id"],"ok":true,"data":{}})
                )
                .unwrap();
                requests.push(request);
            }
            requests
        });
        view.update_in(cx, |v, window, cx| {
            v.endpoint = endpoint;
            install(v, window, cx);
            v.save_discussion_note(window, cx);
            v.save_discussion_note(window, cx);
        });
        cx.run_until_parked();
        let frozen = view.update_in(cx, |v, window, cx| {
            assert!(!v.discussion_note.flight);
            let frozen = v.discussion_note.pending.clone().unwrap();
            assert!(v.discussion_note.receipt.is_none());
            v.save_discussion_note(window, cx);
            frozen
        });
        cx.run_until_parked();
        view.update_in(cx,|v,window,cx| {
            assert_eq!(v.discussion_note.pending,Some(frozen.clone()));
            let bytes=STANDARD.decode(text(&frozen.request["request"]["content_base64"])).unwrap();
            v.discussion_note.receipt=Some(json!({"operation_id":frozen.request["request"]["operation_id"],"path":frozen.path,"previous_revision":null,"revision":format!("sha256:{:x}",Sha256::digest(bytes)),"outcome":"written"}));
            v.save_discussion_note(window,cx);
        });
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            assert!(v.discussion_note.receipt.is_some());
            v.save_discussion_note(window, cx);
        });
        cx.run_until_parked();
        let requests = server.join().unwrap();
        assert_eq!(requests[0]["request"], requests[1]["request"]);
        assert_eq!(requests[0]["request"], frozen.request["request"]);
        assert!(requests[0].get("base").is_none());
        assert_eq!(requests[0]["expected_workspace"], frozen.workspace);
        assert_eq!(
            requests.iter().map(|r| text(&r["op"])).collect::<Vec<_>>(),
            ["source_write", "source_write", "source_read", "source_read"]
        );
        view.update_in(cx, |v, _, cx| {
            assert_eq!(v.discussion_note.pending, Some(frozen));
            assert_eq!(
                v.discussion_note.body.read(cx).value().as_ref(),
                "Reviewed bytes\r\n"
            );
        });
    }
    #[gpui::test]
    fn discussion_note_canonical_prepare_and_successful_save_finish_navigation_close_and_quit(
        cx: &mut TestAppContext,
    ) {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        cx.update(gpui_component::init);
        for (user_authored, destination) in [
            (false, "navigation"),
            (false, "close"),
            (false, "quit"),
            (true, "navigation"),
            (true, "close"),
            (true, "quit"),
            (true, "changed-goal"),
        ] {
            let body = if user_authored && destination == "navigation" {
                ""
            } else {
                "Edited exact bytes\r\n"
            };
            let workspace = json!({"brain_id":BRAIN,"root":"/isolated/discussion-note-ui","records_dir":"records","managed":true});
            let directory = std::env::temp_dir().join(format!("okilum-note-lifecycle-{}", uuid()));
            let recovery =
                editor_recovery::EditorRecovery::at(directory.clone(), &workspace).unwrap();
            let (view, visual) = cx.add_window_view(|window, cx| {
                let mut view = BrainView::new_managed_test(&workspace, &recovery, window, cx);
                view.busy = true;
                view
            });
            visual.run_until_parked();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let endpoint = listener.local_addr().unwrap();
            let stopped = Arc::new(AtomicBool::new(false));
            let stop = stopped.clone();
            let server = std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(15);
                let mut requests = vec![];
                let mut saved: Option<Value> = None;
                while !stop.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
                    let (mut stream, _) = match listener.accept() {
                        Ok(pair) => pair,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                            continue;
                        }
                        Err(error) => panic!("{error}"),
                    };
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut line = String::new();
                    BufReader::new(stream.try_clone().unwrap())
                        .read_line(&mut line)
                        .unwrap();
                    let request: Value = serde_json::from_str(&line).unwrap();
                    let data = match request["op"].as_str().unwrap() {
                        "source_read" if text(&request["path"]).starts_with("records/") => {
                            let record = json!({"schema":SCHEMA,"record_type":"conversation","brain_id":BRAIN,"id":CHAT,"goal_id":GOAL,"path":request["path"],"messages":[{"role":"assistant","text":"Original answer\r\n"}]});
                            let bytes = format!(
                                "---\n{}---\n# Saved conversation\n",
                                serde_yaml::to_string(&record).unwrap()
                            );
                            json!({"schema":SCHEMA,"brain_id":BRAIN,"path":request["path"],"revision":format!("sha256:{:x}",Sha256::digest(bytes.as_bytes())),"content_base64":STANDARD.encode(bytes.as_bytes())})
                        }
                        "source_write" => {
                            assert!(request.get("base").is_none());
                            assert!(request["request"]["expected_revision"].is_null());
                            let bytes = STANDARD
                                .decode(text(&request["request"]["content_base64"]))
                                .unwrap();
                            let revision = format!("sha256:{:x}", Sha256::digest(&bytes));
                            saved = Some(
                                json!({"schema":SCHEMA,"brain_id":BRAIN,"path":request["request"]["path"],"revision":revision,"content_base64":STANDARD.encode(&bytes)}),
                            );
                            json!({"operation_id":request["request"]["operation_id"],"path":request["request"]["path"],"previous_revision":null,"revision":revision,"outcome":"written"})
                        }
                        "source_read" => saved.clone().unwrap(),
                        "source_list" => json!({"entries":[]}),
                        "source_preview" => json!({"markdown":"Saved note","assets":[]}),
                        other => panic!("Unexpected operation {other}"),
                    };
                    writeln!(
                        stream,
                        "{}",
                        json!({"schema":request["schema"],"id":request["id"],"ok":true,"data":data})
                    )
                    .unwrap();
                    requests.push(request);
                }
                (requests, saved)
            });
            view.update_in(visual, |v, window, cx| {
                v.endpoint = endpoint;
                install(v, window, cx);
                v.discussion_note.clear();
                if user_authored {
                    v.conversation_id = None;
                    v.conversation = Value::Null;
                    v.start_new_note(window, cx);
                    assert!(v.discussion_note.origin.is_none());
                } else {
                    v.start_discussion_note(0, window, cx);
                }
            });
            visual.run_until_parked();
            view.update_in(visual, |v, window, cx| {
                assert!(
                    v.discussion_note.active()
                        && (user_authored || v.discussion_note.origin.is_some()),
                    "{:?}",
                    v.discussion_note.error
                );
                v.discussion_note.title.update(cx, |input, cx| {
                    input.set_value("Reviewed title", window, cx)
                });
                v.discussion_note
                    .body
                    .update(cx, |input, cx| input.set_value(body, window, cx));
                if user_authored && destination != "navigation" {
                    // Selecting a conversation is independent of the user-authored target.
                    v.conversation_id = Some(uuid());
                }
                match destination {
                    "navigation" => {
                        v.note_guard_navigation(PendingNavigation::Surface(Surface::Execution), cx);
                    }
                    "close" => assert!(!v.editor_request_close(window, cx)),
                    "quit" => {
                        v.editor_request_app_quit(99, window, cx);
                        assert!(!v.editor_app_quit_ready(99, cx));
                    }
                    "changed-goal" => {}
                    _ => unreachable!(),
                }
                v.save_discussion_note(window, cx);
                assert!(v.discussion_note.flight, "{:?}", v.discussion_note.error);
                if destination == "changed-goal" {
                    v.snapshot["goal"]["id"] = json!(uuid());
                }
            });
            visual.run_until_parked();
            if destination == "changed-goal" {
                view.update_in(visual, |v, window, cx| {
                    assert!(v.discussion_note.active());
                    assert!(v.discussion_note.receipt.is_some());
                    assert!(v.discussion_note.pending.is_some());
                    assert!(v.source_snapshot.is_none());
                    assert_eq!(v.discussion_note.body.read(cx).value().as_ref(), body);
                    v.snapshot["goal"]["id"] = json!(GOAL);
                    v.save_discussion_note(window, cx);
                });
                visual.run_until_parked();
            }
            stopped.store(true, Ordering::Relaxed);
            let (requests, saved) = server.join().unwrap();
            assert_eq!(
                requests
                    .iter()
                    .filter(|r| r["op"] == "source_write")
                    .count(),
                1
            );
            let write_index = if user_authored { 0 } else { 1 };
            if !user_authored {
                assert_eq!(requests[0]["op"], "source_read");
            }
            assert_eq!(requests[write_index]["op"], "source_write");
            assert_eq!(requests[write_index + 1]["op"], "source_read");
            assert!(requests.iter().all(|r| matches!(
                r["op"].as_str(),
                Some("source_write" | "source_read" | "source_list" | "source_preview")
            )));
            let saved = saved.unwrap();
            if user_authored {
                let bytes = STANDARD.decode(text(&saved["content_base64"])).unwrap();
                let markdown = String::from_utf8(bytes).unwrap();
                let (yaml, content) = markdown
                    .strip_prefix("---\n")
                    .unwrap()
                    .split_once("---\n")
                    .unwrap();
                assert_eq!(
                    serde_yaml::from_str::<Value>(yaml).unwrap(),
                    json!({"type":"Note","goal_id":GOAL})
                );
                assert_eq!(content, format!("# Reviewed title\n\n{body}"));
            }
            if destination == "close" {
                assert!(
                    visual.windows().is_empty(),
                    "normal close must finish after successful readback"
                );
                view.read_with(visual, |v, cx| {
                    assert!(!v.discussion_note.active());
                    assert_eq!(v.source_snapshot.as_ref(), Some(&saved));
                    assert!(!v.dirty(cx));
                });
            } else {
                view.update_in(visual, |v, window, cx| {
                    assert!(!v.discussion_note.active());
                    assert_eq!(v.source_snapshot.as_ref(), Some(&saved));
                    assert!(v.source.value(cx).ends_with(body));
                    if destination == "navigation" {
                        assert!(v.surface == Surface::Execution);
                        assert!(v.pending_navigation.is_none());
                    } else if destination == "quit" {
                        assert!(v.editor_app_quit_ready(99, cx));
                        v.editor_cancel_app_quit(99, cx);
                    }
                    window.remove_window();
                });
            }
            assert!(recovery.list().unwrap().drafts.is_empty());
            if directory.exists() {
                std::fs::remove_dir_all(directory).unwrap();
            }
        }
    }
}
