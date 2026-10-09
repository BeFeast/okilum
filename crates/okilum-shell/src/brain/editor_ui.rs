//! Desktop-only recovery. One local durability job per view; no automatic RPC replay.
use super::editor_recovery::{Draft, EditorRecovery, RecoveryList};
use super::*;
mod auto_save;
#[cfg(test)]
use auto_save::save_attempt;
use auto_save::{save_attempt_with_mode, SaveAttempt, SaveMode, Saved};
mod close_ui;
#[cfg(test)]
mod decision_reuse_tests;
#[cfg(test)]
mod goal_criteria_tests;
#[cfg(test)]
mod managed_source_tests;
mod merge_ui;

#[derive(Default)]
pub(super) struct EditorUi {
    active: Option<Draft>,
    input_epoch: u64,
    allow_auto: bool,
    records: Vec<Draft>,
    problems: Vec<String>,
    flight: bool,
    error: Option<String>,
    save: Option<Value>,
    sending: Option<Value>,
    guarded_retry: Option<Value>,
    confirmed: bool,
    discarding: bool,
    merge: merge_ui::MergeUi,
    closing: Option<close_ui::CloseIntent>,
    #[cfg(test)]
    store: Option<EditorRecovery>,
}

/// An automatic child's CAS base belongs to its merged proposal, not to newer
/// visible text still authored against the original loaded base. Deliberate
/// manual-resolution operations keep their explicitly chosen conflict base.
fn conflict_base<'a>(draft: Option<&'a Draft>, conflict: &'a Value) -> &'a Value {
    if let Some(draft) = draft.filter(|draft| {
        draft.automatic_format
            && draft.conflict.as_ref() == Some(conflict)
            && conflict["conflict"]["conflict_id"]
                .as_str()
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
                .is_some_and(|id| id.get_version_num() == 8)
    }) {
        &draft.base
    } else {
        &conflict["base"]
    }
}

impl BrainView {
    #[cfg(test)]
    pub(super) fn new_managed_test(
        workspace: &Value,
        store: &editor_recovery::EditorRecovery,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::new_guarded(
            "127.0.0.1:1".parse().unwrap(),
            Some(workspace.clone()),
            window,
            cx,
        );
        view.editor.store = Some(store.clone());
        assert!(view.source.managed().is_some());
        view
    }
    /// Find may stay open during ordinary draft protection, but never recovery or Save.
    pub(super) fn editor_find_blocked(&self) -> bool {
        self.editor.error.is_some()
            || self.editor.guarded_retry.is_some()
            || self.editor.discarding
            || self.editor.save.is_some()
            || self.editor.sending.is_some()
    }
    pub(super) fn editor_can_begin_criteria(&self) -> bool {
        self.editor.error.is_none()
            && !self.editor.flight
            && !self.editor_blocks_navigation()
            && self.editor.guarded_retry.is_none()
    }
    fn editor_save_owner_matches(&self, workspace: &Value, draft_id: &str) -> bool {
        self.expected_workspace.as_ref() == Some(workspace)
            && self.editor.active.as_ref().map(|d| d.id.as_str()) == Some(draft_id)
    }
    pub(super) fn editor_guarded_save(&self) -> bool {
        self.editor.guarded_retry.is_some()
            || self
                .editor
                .active
                .as_ref()
                .and_then(|d| d.pending_save.as_ref())
                .is_some_and(editor_recovery::is_guarded)
            || self
                .editor
                .save
                .as_ref()
                .is_some_and(editor_recovery::is_guarded)
            || self
                .editor
                .sending
                .as_ref()
                .is_some_and(editor_recovery::is_guarded)
    }
    pub(super) fn editor_conflict_base<'a>(&'a self, conflict: &'a Value) -> &'a Value {
        conflict_base(self.editor.active.as_ref(), conflict)
    }
    pub(super) fn editor_input_changed(&mut self) {
        self.editor.input_epoch = self.editor.input_epoch.wrapping_add(1);
    }
    fn editor_store(&self) -> Result<EditorRecovery, String> {
        #[cfg(test)]
        if let Some(store) = &self.editor.store {
            return Ok(store.clone());
        }
        EditorRecovery::open(
            self.expected_workspace
                .as_ref()
                .ok_or("No bound workspace")?,
        )
    }

    // New-note reviews own their close prompt; the Quit token alone is not a source draft.
    pub(super) fn editor_blocks_note_creation(&self) -> bool {
        self.editor.flight && self.source_snapshot.is_some()
            || self.editor.save.is_some()
            || self.editor.sending.is_some()
            || self
                .editor
                .active
                .as_ref()
                .is_some_and(|d| d.pending_save.is_some())
    }
    pub(super) fn editor_blocks_navigation(&self) -> bool {
        self.editor_closing()
            || self.editor.guarded_retry.is_some()
            || (self.editor.flight && self.source_snapshot.is_some())
            || self.editor.save.is_some()
            || self
                .editor
                .active
                .as_ref()
                .is_some_and(|d| d.pending_save.is_some())
    }
    pub(super) fn editor_flush(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor.save.is_some() {
            self.editor_protect(window, cx);
        }
    }
    pub(super) fn editor_merge_panel(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.merge_panel(cx)
    }
    pub(super) fn editor_detach(&mut self) {
        self.editor.merge = merge_ui::MergeUi::default();
        self.editor.active = None;
        self.editor.confirmed = false;
        self.editor.discarding = false;
    }

    pub(super) fn editor_read_failed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.discarding = false;
        self.editor_protect(window, cx);
    }
    pub(super) fn editor_enabled(&self) -> bool {
        self.expected_workspace.is_some()
    }
    pub(super) fn editor_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editor_enabled() || self.editor.flight {
            return;
        }
        let store = self.editor_store();
        self.editor.flight = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { store?.list() })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.editor.flight = false;
                this.editor_listing(result);
                this.editor_protect(window, cx);
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn editor_listing(&mut self, result: Result<RecoveryList, String>) {
        match result {
            Ok(list) => {
                if let Some(active) = &self.editor.active {
                    if let Some(found) = list.drafts.iter().find(|d| d.id == active.id) {
                        if found != active {
                            self.editor.error = Some("This recovery record changed. Keep the visible draft; restore the current record explicitly in a new editor window.".into());
                        }
                    } else {
                        self.editor.error = Some("Recovery record disappeared. Keep this visible draft; recovery must be reconciled before another Save.".into());
                    }
                }
                self.editor.records = list.drafts;
                self.editor.problems = list.problems;
            }
            Err(error) => self.editor.error = Some(error),
        }
    }
    pub(super) fn editor_protect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor_finish_close(window, cx) {
            return;
        }
        self.merge_check(cx);
        if !self.editor_enabled()
            || self.editor.flight
            || self.editor.error.is_some()
            || self.editor.discarding
            || !self.source_editable
        {
            return;
        }
        let Some(base) = self.source_snapshot.clone() else {
            return;
        };
        let value = self.source.value(cx).to_string();
        if self.editor.confirmed && self.editor.active.as_ref().is_some_and(|d| d.text == value) {
            if self.editor.save.is_some() {
                self.editor_send(window, cx);
            }
            return;
        }
        if self.editor.active.is_none()
            && value == self.source_original
            && self.editor.save.is_none()
        {
            return;
        }
        let store = self.editor_store();
        let expected = self.editor.active.clone();
        self.editor.flight = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let store = store?;
                    match expected {
                        Some(draft) => store.update(&draft, &value),
                        None => store.create(&base, &value),
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.editor.flight = false;
                match result {
                    Ok(draft) => {
                        this.editor.active = Some(draft);
                        this.editor.confirmed = true;
                        this.editor_protect(window, cx);
                    }
                    Err(error) => {
                        this.editor.error = Some(error);
                        if this
                            .editor
                            .save
                            .as_ref()
                            .is_some_and(editor_recovery::is_guarded)
                        {
                            this.editor.guarded_retry = this.editor.save.take();
                        } else {
                            this.editor.save = None;
                        }
                        this.source_loading = false;
                        this.sync_source_policy(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn editor_save(
        &mut self,
        request: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A refused Save must not release another durability job's input freeze.
        if self.editor_closing()
            || self.busy
            || (self.editor.flight && self.source_conflict.is_some())
        {
            return;
        }
        if self.editor.error.is_some() {
            self.source_loading = false;
            self.sync_source_policy(cx);
            return;
        }
        if self
            .editor
            .active
            .as_ref()
            .is_some_and(|d| d.pending_save.is_some())
        {
            self.error = Some(
                "The previous Save is uncertain. Use Retry this Save before saving more edits."
                    .into(),
            );
            self.source_loading = false;
            self.sync_source_policy(cx);
            return;
        }
        self.editor.allow_auto = self.source_conflict.is_none();
        self.editor.save = Some(request);
        self.source_loading = true;
        self.sync_source_policy(cx);
        self.editor_protect(window, cx);
    }
    pub(super) fn editor_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor.flight || self.busy {
            return;
        }
        let Some(draft) = self.editor.active.clone() else {
            return;
        };
        let Some(request) = self.editor.save.take() else {
            return;
        };
        let workspace = self.expected_workspace.clone().unwrap();
        let expected_workspace = workspace.clone();
        let expected_draft_id = draft.id.clone();
        let input_epoch = self.editor.input_epoch;
        let native_source = self.source.stamp(cx);
        let mode = if std::mem::take(&mut self.editor.allow_auto) {
            SaveMode::Fresh
        } else {
            SaveMode::Recovery
        };
        let store = self.editor_store();
        let endpoint = self.endpoint;
        self.editor.sending = Some(request.clone());
        self.editor.flight = true;
        self.busy = true;
        self.sync_source_policy(cx);
        self.source_loading = true;
        self.sync_source_policy(cx);
        // The worker owns the immutable draft identity, request and workspace.
        // Closing the process can leave the retained request for explicit recovery.
        cx.spawn_in(window, async move |this,cx| {
            let submitted = request.clone();
            let result = cx.background_executor().spawn(async move {
                let store = store?;
                Ok::<_,String>(save_attempt_with_mode(&store,&draft,&request,mode,|request|rpc_guarded(endpoint,request,Some(&workspace))))
            }).await;
            let _ = this.update_in(cx, |this,window,cx| {
                if !this.editor_save_owner_matches(&expected_workspace,&expected_draft_id) {
                    this.error=Some("A late Save belongs to another workspace or draft. The visible editor was preserved; inspect recovery in its original workspace.".into());
                    cx.notify(); return;
                }
                this.editor.flight = false;
                this.busy = false; this.sync_source_policy(cx);
                this.source_loading = false; this.sync_source_policy(cx);
                this.editor.sending = None;
                match result {
                    Ok(SaveAttempt {id,result,inventory:list}) => {
                        let inventory_error = match list { Ok(list) => {this.editor.records=list.drafts;this.editor.problems=list.problems;None}, Err(error) => Some(error) };
                        if this.expected_workspace.as_ref() != Some(&expected_workspace) || this.editor.active.as_ref().map(|d|d.id.as_str()) != Some(id.as_str()) { this.editor.error=Some("A late Save updated its retained recovery record; the visible editor was preserved.".into()); cx.notify(); return; }
                        match result {
                            Ok(Saved {draft,written:saved,submitted:actual,automatic,adopt_from,manual_reason}) => {
                                if saved && automatic {
                                    this.editor_apply_auto_receipt(draft.unwrap(), &actual, &submitted, adopt_from.as_deref(), input_epoch, native_source, inventory_error, window, cx);
                                    cx.notify();
                                    return;
                                }
                                this.editor.active = draft;
                                this.editor.confirmed = true;
                                this.editor.error = inventory_error;
                                this.error = None;
                                if saved {
                                    let mut base = actual["base"].clone();
                                    let bytes = STANDARD.decode(text(&actual["request"]["content_base64"])).unwrap_or_default();
                                    use sha2::{Digest,Sha256};
                                    base["content_base64"] = actual["request"]["content_base64"].clone();
                                    base["revision"] = json!(format!("sha256:{:x}",Sha256::digest(&bytes)));
                                    this.source_original = String::from_utf8(bytes).unwrap_or_default();
                                    this.editor_close_saved(&actual, &base, cx);
                                    this.source_snapshot = Some(base);
                                    this.source_conflict = None;
                                    this.pending_source_write = None;
                                    if actual["op"] == "discussion_decision_reuse_write" { this.reuse_saved_readback(actual,window,cx);cx.notify();return; }
                                    if actual["op"] == "goal_criteria_write" {
                                        this.criteria_saved_readback(actual,window,cx);
                                        cx.notify(); return;
                                    }
                                    this.notice = "Source saved; local recovery acknowledged".into();
                                    if this.navigation_after_source && !this.dirty(cx) { this.continue_navigation(window,cx); }
                                } else if let Some(conflict) = this.editor.active.as_ref().and_then(|d| d.conflict.clone()) {
                                    this.show_source_conflict(conflict,window,cx);
                                    if let Some(reason) = manual_reason { this.notice = reason; }
                                }
                            }
                            Err(error) => {
                                if let Some(retained) = this.editor.records.iter().find(|d| d.id == id).cloned() { this.editor.active = Some(retained); }
                                if editor_recovery::is_guarded(&submitted) && this.editor.active.as_ref().is_none_or(|d| d.pending_save.is_none()) {
                                    this.editor.guarded_retry = Some(submitted.clone());
                                }
                                this.editor.confirmed = false;
                                this.editor.error = Some(format!("Save is not confirmed: {error}. Recheck recovery before continuing."));
                            }
                        }
                    }
                    Err(error) => {
                        if editor_recovery::is_guarded(&submitted) { this.editor.guarded_retry = Some(submitted); }
                        this.editor.error = Some(error);
                    }
                }
                this.editor_protect(window,cx);
                cx.notify();
            });
        }).detach();
    }
    pub(super) fn editor_restore(
        &mut self,
        draft: Draft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editor_closing() || self.busy || self.editor.flight || self.dirty(cx) {
            return;
        }
        self.load_source(draft.base.clone(), window, cx);
        self.source.reset(draft.text.clone(), window, cx);
        self.editor.active = Some(draft.clone());
        self.editor.confirmed = false;
        self.editor.error = None;
        self.surface = Surface::Source;
        if let Some(conflict) = draft.conflict {
            self.show_source_conflict(conflict, window, cx);
            // An uncertain child owns an immutable CAS envelope. Refreshing its
            // conflict would be refused and strand Retry behind a local error.
            // Resolve the exact pending operation before refreshing conflict state.
            if draft.pending_save.is_none() {
                self.editor_refresh_conflict(window, cx);
            }
        }
        self.editor_protect(window, cx);
        self.schedule_preview(window, cx);
        cx.notify();
    }
    pub(super) fn editor_refresh_conflict(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.editor.active.clone() else {
            return;
        };
        let Some(conflict) = draft.conflict.clone() else {
            return;
        };
        if draft
            .pending_save
            .as_ref()
            .is_some_and(editor_recovery::is_guarded)
        {
            return;
        }
        let workspace = self.expected_workspace.clone().unwrap();
        let store = self.editor_store();
        let endpoint = self.endpoint;
        self.editor.flight = true;
        cx.spawn_in(window,async move |this,cx| {
            let result = cx.background_executor().spawn(async move {
                let current = rpc_guarded(endpoint,json!({"op":"source_conflict","brain_id":draft.base["brain_id"],"path":draft.base["path"],"conflict_id":conflict["conflict"]["conflict_id"]}),Some(&workspace))?;
                store?.refresh_conflict(&draft,&current)
            }).await;
            let _ = this.update_in(cx,|this,window,cx| {
                this.editor.flight = false;
                match result {
                    Ok(draft) => { let conflict = draft.conflict.clone().unwrap(); this.editor.active = Some(draft); this.editor.confirmed = true; this.show_source_conflict(conflict,window,cx); this.editor_protect(window,cx); }
                    Err(error) => this.editor.error = Some(format!("Conflict refresh failed: {error}")),
                }
                cx.notify();
            });
        }).detach();
    }
    pub(super) fn editor_discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor_closing() || self.busy || self.editor.flight {
            return;
        }
        let Some(draft) = self.editor.active.clone() else {
            if self.editor_enabled() && self.editor.error.is_some() {
                return;
            }
            self.editor.discarding = true;
            self.source_loading = true;
            self.sync_source_policy(cx);
            if let Some(source) = &self.source_snapshot {
                self.batch(
                    vec![json!({"op":"source_read","path":source["path"]})],
                    window,
                    cx,
                );
            }
            return;
        };
        let discarded_id = draft.id.clone();
        let store = self.editor_store();
        self.editor.flight = true;
        self.editor.discarding = true;
        self.source_loading = true;
        self.sync_source_policy(cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { store?.discard(&draft) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.editor.flight = false;
                match result {
                    Ok(()) => {
                        this.editor.active = None;
                        this.editor.guarded_retry = None;
                        this.editor.records.retain(|d| d.id != discarded_id);
                        this.editor.error = None;
                        if let Some(source) = &this.source_snapshot {
                            this.batch(
                                vec![json!({"op":"source_read","path":source["path"]})],
                                window,
                                cx,
                            );
                        }
                    }
                    Err(error) => {
                        this.editor.error = Some(error);
                        this.editor.discarding = false;
                        this.source_loading = false;
                        this.sync_source_policy(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn editor_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.editor_closing() {
            return self.editor_close_panel(cx);
        }
        let protected = self.editor.confirmed
            && self
                .editor
                .active
                .as_ref()
                .is_some_and(|d| d.text == self.source.value(cx).as_ref());
        let status = if self.editor.flight {
            "Protecting local draft…"
        } else if self.editor.error.is_some() {
            "Recovery needs attention"
        } else if protected {
            "Protected on this device"
        } else {
            "Local Markdown recovery"
        };
        let mut panel = v_flex().gap_2().child(div().text_sm().child(status));
        if let Some(error) = &self.editor.error {
            panel = panel.child(div().text_sm().child(error.clone()));
        }
        for problem in &self.editor.problems {
            panel = panel.child(div().text_sm().child(problem.clone()));
        }
        panel = panel.child(
            super::super::brand::control("editor-recheck", cx)
                .label("Recheck recovery")
                .disabled(self.editor.flight || self.busy)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.editor.error = None;
                    this.editor.confirmed = false;
                    this.editor_list(window, cx);
                })),
        );
        if let Some(draft) = self
            .editor
            .active
            .as_ref()
            .filter(|d| d.pending_save.is_some())
        {
            let explanation = if draft.has_terminal_criteria_conflict() {
                "The criteria Save returned a source conflict. Your draft is preserved. Discard changes to review the current criteria."
            } else if draft.has_terminal_guarded_conflict() {
                "The reuse Save returned a source conflict. Discard changes to review the current decision again."
            } else {
                "Previous Save is uncertain. Retry uses its original operation and bytes."
            };
            panel = panel.child(div().text_sm().child(explanation)).child(
                super::super::brand::control("editor-retry-save", cx)
                    .label("Retry this Save")
                    .disabled(self.editor.flight || self.busy || self.editor.error.is_some())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.editor.allow_auto = false;
                        this.editor.save = this
                            .editor
                            .active
                            .as_ref()
                            .and_then(|d| d.pending_save.clone());
                        this.editor_send(window, cx);
                    })),
            );
        }
        if self.editor.guarded_retry.is_some() {
            panel = panel.child(div().text_sm().child("The original guarded Save remains in this window. Retry protection and this exact Save before leaving."))
                .child(super::super::brand::control("criteria-retry-protection",cx).label("Retry original Save")
                    .disabled(self.busy || self.editor.flight || self.editor.error.is_some())
                    .on_click(cx.listener(|this,_,window,cx| {
                        this.editor.save = this.editor.guarded_retry.take();
                        this.editor.allow_auto = false;
                        this.editor_protect(window,cx);
                    })));
        }
        if !self.decision_reuse.active {
            if let Some(request) = self.decision_reuse.readback.clone() {
                panel = panel.child(
                    super::super::brand::control("reuse-recovery-readback", cx)
                        .label("Read current source after reuse Save")
                        .disabled(self.busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.reuse_saved_readback(request.clone(), window, cx)
                        })),
                );
            }
        }
        if !self.goal_criteria.active {
            if let Some(request) = self.goal_criteria.readback.clone() {
                panel = panel.child(
                    super::super::brand::control("criteria-recovery-readback", cx)
                        .label("Read current source after criteria Save")
                        .disabled(self.busy || self.editor.flight)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.criteria_saved_readback(request.clone(), window, cx)
                        })),
                );
            }
        }
        for draft in self.editor.records.clone() {
            if self
                .editor
                .active
                .as_ref()
                .is_some_and(|d| d.id == draft.id)
            {
                continue;
            }
            let label = format!("Recover draft — {}", text(&draft.base["path"]));
            panel = panel.child(
                super::super::brand::control(
                    SharedString::from(format!("editor-restore-{}", draft.id)),
                    cx,
                )
                .label(label)
                .disabled(self.busy || self.editor.flight || self.dirty(cx))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.editor_restore(draft.clone(), window, cx)
                })),
            );
        }
        panel.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use sha2::{Digest, Sha256};
    use std::{cell::Cell, path::PathBuf};

    pub(super) fn fixture() -> (PathBuf, Value, Value, EditorRecovery) {
        let dir = std::env::temp_dir().join(format!("okilum-editor-ui-{}", uuid()));
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/editor-ui","records_dir":"records","managed":true});
        let bytes = "\u{feff}Original\r\n[[Source|Alias]]\r\n";
        let base = json!({"schema":SCHEMA,"brain_id":workspace["brain_id"],"path":"notes/source.md","revision":format!("sha256:{:x}",Sha256::digest(bytes.as_bytes())),"content_base64":STANDARD.encode(bytes.as_bytes()),"media_type":"text/markdown"});
        let store = EditorRecovery::at(dir.clone(), &workspace).unwrap();
        (dir, workspace, base, store)
    }
    pub(super) fn receipt(request: &Value) -> Value {
        let bytes = STANDARD
            .decode(text(&request["request"]["content_base64"]))
            .unwrap();
        json!({"operation_id":request["request"]["operation_id"],"path":request["request"]["path"],"previous_revision":request["request"]["expected_revision"],"revision":format!("sha256:{:x}",Sha256::digest(bytes)),"outcome":"written"})
    }
    #[test]
    fn retention_failure_cannot_send_and_lists_current_generation() {
        let (dir, _, base, store) = fixture();
        let stale = store.create(&base, "Draft one").unwrap();
        let current = store.update(&stale, "Draft in another window").unwrap();
        let calls = Cell::new(0);
        let attempted = save_attempt(
            &store,
            &stale,
            &source_write(&base, "Draft one", &uuid()),
            |_| {
                calls.set(calls.get() + 1);
                unreachable!()
            },
        );
        assert!(attempted.result.is_err());
        assert_eq!(calls.get(), 0);
        assert_eq!(attempted.inventory.unwrap().drafts, vec![current]);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn lost_reply_retains_original_request_and_explicit_retry_preserves_later_text() {
        let (dir, _, base, store) = fixture();
        let draft = store.create(&base, "First proposal\r\n").unwrap();
        let request = source_write(&base, &draft.text, &uuid());
        let first = save_attempt(&store, &draft, &request, |wire| {
            assert_eq!(wire, request);
            assert_eq!(store.list().unwrap().drafts[0].pending_save, Some(wire));
            Err("Committed but TCP response lost".into())
        });
        assert!(first.result.is_err());
        let pending = first.inventory.unwrap().drafts.pop().unwrap();
        let newer = store.update(&pending, "Later proposal\r\n").unwrap();
        let retried = save_attempt(&store, &newer, &request, |wire| {
            assert_eq!(wire, request);
            Ok(receipt(&wire))
        });
        let retained = retried.result.unwrap().draft.unwrap();
        assert_eq!(retained.text, "Later proposal\r\n");
        assert_eq!(decode_source(&retained.base).unwrap(), "First proposal\r\n");
        assert!(retained.pending_save.is_none());
        assert_eq!(retried.inventory.unwrap().drafts, vec![retained]);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn wrong_receipt_keeps_uncertain_operation_instead_of_acknowledging() {
        let (dir, _, base, store) = fixture();
        let draft = store.create(&base, "Retained draft").unwrap();
        let request = source_write(&base, &draft.text, &uuid());
        let attempt = save_attempt(&store, &draft, &request, |wire| {
            let mut r = receipt(&wire);
            r["operation_id"] = json!(uuid());
            Ok(r)
        });
        assert!(attempt.result.is_err());
        assert_eq!(
            attempt.inventory.unwrap().drafts[0].pending_save,
            Some(request)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[gpui::test]
    fn background_protection_coalesces_typing_and_restart_restore_has_original_base(
        cx: &mut TestAppContext,
    ) {
        let (dir, workspace, base, store) = fixture();
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new_managed_test(&workspace, &store, window, cx);
            v.busy = true;
            v
        });
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            v.expected_workspace = Some(workspace.clone());
            v.editor.store = Some(store.clone());
            v.load_source(base.clone(), window, cx);
            v.source.reset("First typing\r\n", window, cx);
            v.editor_protect(window, cx);
            assert!(v.editor.flight);
            assert!(
                !v.editor.confirmed,
                "render path must not acknowledge a background write"
            );
            v.source.reset("Latest typing\r\n", window, cx);
            v.editor_protect(window, cx);
        });
        cx.run_until_parked();
        let retained = store.list().unwrap().drafts.pop().unwrap();
        assert_eq!(retained.text, "Latest typing\r\n");
        assert_eq!(retained.base, base);
        assert!(
            retained.pending_save.is_none(),
            "typing never requests a canonical Save"
        );
        view.update_in(cx, |v, window, cx| {
            assert!(v.editor.confirmed);
            assert!(!v.editor.flight);
            v.editor = EditorUi::default();
            v.editor.store = Some(store.clone());
            v.load_source(base.clone(), window, cx);
            v.busy = false;
            v.editor_restore(retained.clone(), window, cx);
            assert!(
                !v.editor.confirmed,
                "merely reading a record is not durability confirmation"
            );
            assert_eq!(v.source_snapshot, Some(base.clone()));
            assert_eq!(v.source.value(cx).as_ref(), retained.text);
            assert!(v.editor.save.is_none(), "restoration never replays a Save");
        });
        cx.run_until_parked();
        view.update_in(cx, |v, _, _| {
            assert!(v.editor.confirmed);
            assert!(!v.editor.flight);
        });
        assert_eq!(store.list().unwrap().drafts[0], retained);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[gpui::test]
    fn discard_freezes_editor_until_reload_and_failure_keeps_visible_text(cx: &mut TestAppContext) {
        let (dir, workspace, base, store) = fixture();
        let retained = store
            .create(&base, "Explicitly discarded text\r\n")
            .unwrap();
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new_managed_test(&workspace, &store, window, cx);
            v.busy = true;
            v
        });
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            v.expected_workspace = Some(workspace.clone());
            v.editor.store = Some(store.clone());
            v.load_source(base.clone(), window, cx);
            v.source.reset(retained.text.clone(), window, cx);
            v.editor.active = Some(retained.clone());
            v.editor.confirmed = true;
            v.busy = false;
            v.editor_discard(window, cx);
            assert!(
                v.source_loading,
                "the native input must become readonly before local deletion starts"
            );
            assert!(v.editor.flight);
            // Hold general transport: verify the local retirement/read boundary
            // independently of socket scheduling, then deliver the source read below.
            v.busy = true;
        });
        cx.run_until_parked();
        assert!(store.list().unwrap().drafts.is_empty());
        view.update_in(cx, |v, window, cx| {
            assert!(v.editor.active.is_none());
            assert!(
                v.source_loading,
                "readonly must span local retirement through source reload"
            );
            assert!(!v.editor.flight);
            v.finish_batch(
                vec![("source_read".into(), Ok(base.clone()))],
                false,
                v.request_generation,
                window,
                cx,
            );
            assert!(!v.source_loading);
            assert_eq!(v.source.value(cx).as_ref(), decode_source(&base).unwrap());
        });
        let retained = store.create(&base, "Keep on failed discard\r\n").unwrap();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join(text(&workspace["brain_id"])).join("recovery.lock"))
            .unwrap();
        lock.lock().unwrap();
        view.update_in(cx, |v, window, cx| {
            v.busy = false;
            v.editor.active = Some(retained.clone());
            v.editor.confirmed = true;
            v.source.reset(retained.text.clone(), window, cx);
            v.editor_discard(window, cx);
            assert!(v.source_loading);
        });
        cx.run_until_parked();
        view.update_in(cx, |v, _, cx| {
            assert!(
                !v.source_loading,
                "failed local retirement must unlock the preserved editor"
            );
            assert!(v.editor.error.is_some());
            assert_eq!(v.source.value(cx).as_ref(), retained.text);
        });
        drop(lock);
        assert_eq!(store.list().unwrap().drafts, vec![retained]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
