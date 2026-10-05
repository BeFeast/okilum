//! Read-only reuse review; pending attempts use the existing guarded editor outbox.
use super::*;
use tessera_core::decision_reuse::{self as reuse, Disposition};
#[derive(Default)]
pub(super) struct ReuseUi {
    pub active: bool,
    goal: String,
    decision: String,
    workspace: Option<Value>,
    view: Value,
    preview: Option<Entity<TextareaState>>,
    frozen: bool,
    details: bool,
    error: Option<String>,
    pub(super) readback: Option<Value>,
    readback_source: Option<Value>,
    destination: Option<PendingNavigation>,
    closing: bool,
}
impl ReuseUi {
    fn cancel_departure(&mut self) {
        self.destination = None;
        self.closing = false;
    }
}
fn reuse_state_label(view: &Value) -> &'static str {
    match view["state"].as_str() {
        Some("automatic") => "Added automatically to future goal context",
        Some("manual_only") => "Manual selection only",
        _ => "Current decision reuse is unavailable; recheck the current source",
    }
}
/// Bind presentation and source loading to the exact requested workspace/decision.
/// Unavailable is an authoritative state, never evidence of automatic admission.
fn validate_reuse_view(
    view: &Value,
    goal: &str,
    decision: &str,
    brain: &str,
    path: &str,
) -> Result<Option<String>, String> {
    if view["goal_id"] != goal || view["decision_id"] != decision || view["path"] != path {
        return Err("Reuse reply belongs to another decision".into());
    }
    let unavailable = view["state"] == "unavailable";
    if !unavailable && view["state"] != "automatic" && view["state"] != "manual_only" {
        return Err("Unknown current decision reuse state".into());
    }
    if unavailable && view["eligible"] != false {
        return Err("Unavailable decision cannot be eligible".into());
    }
    if view["source"].is_null() && unavailable {
        return Ok(None);
    }
    let source: tessera_core::source::SourceSnapshot =
        serde_json::from_value(view["source"].clone()).map_err(|_| "Invalid reuse source")?;
    if source.schema != SCHEMA
        || source.media_type != "text/markdown"
        || source.brain_id != brain
        || source.path != path
        || source.content_base64.len() > reuse::MAX_BYTES.div_ceil(3) * 4
    {
        return Err("Reuse source ownership or size mismatch".into());
    }
    let raw = decode_source(&view["source"])?;
    if raw.len() > reuse::MAX_BYTES || reuse::revision(raw.as_bytes()) != source.revision {
        return Err("Reuse source revision or size mismatch".into());
    }
    if unavailable {
        return Ok(None);
    }
    let (original, policy) = reuse::original(&source, goal, decision)?;
    if (policy.is_some()) != (view["state"] == "manual_only") {
        return Err("Reuse state differs from current source".into());
    }
    let raw = decode_source(&serde_json::to_value(original).map_err(|e| e.to_string())?)?;
    let front = raw
        .strip_prefix("---\r\n")
        .or_else(|| raw.strip_prefix("---\n"))
        .ok_or("Missing decision frontmatter")?;
    let mut offset = 0;
    for line in front.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let body = &front[offset + line.len()..];
            if view["text"].as_str() != Some(body) {
                return Err("Reuse review text differs from its source".into());
            }
            return Ok(Some(body.to_owned()));
        }
        offset += line.len();
    }
    Err("Unterminated decision frontmatter".into())
}
impl BrainView {
    pub(super) fn open_decision_reuse(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy
            || self.editor_closing()
            || self.capabilities["discussion_decision_reuse"] != true
        {
            return;
        }
        if self.note_guard_navigation(PendingNavigation::DecisionReuse(id.clone()), cx) {
            return;
        }
        if self.source_dirty(cx) {
            self.pending_navigation = Some(PendingNavigation::DecisionReuse(id));
            cx.notify();
            return;
        }
        if !self.editor_can_begin_criteria() {
            self.error = Some("Recheck Source recovery before opening reuse settings.".into());
            self.surface = Surface::Source;
            cx.notify();
            return;
        }
        let goal = self.goal_id();
        let workspace = self.expected_workspace.clone();
        let endpoint = self.endpoint;
        self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let owner = workspace.clone();
            let g = goal.clone();
            let d = id.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    rpc_guarded(
                        endpoint,
                        json!({"op":"discussion_decision_reuse_get","goal_id":g,"decision_id":d}),
                        owner.as_ref(),
                    )
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                if this.expected_workspace != workspace || this.goal_id() != goal || this.dirty(cx)
                {
                    this.error = Some(
                        "The original reuse review changed while loading. Open it again.".into(),
                    );
                    cx.notify();
                    return;
                }
                let brain = text(&workspace.as_ref().unwrap_or(&Value::Null)["brain_id"]);
                let path = format!(
                    "{}/discussion-decision-{id}.md",
                    text(&workspace.as_ref().unwrap_or(&Value::Null)["records_dir"])
                );
                match result.and_then(|view| {
                    let body = validate_reuse_view(&view, &goal, &id, &brain, &path)?;
                    Ok((view, body))
                }) {
                    Ok((view, body)) => {
                        let preview = body.map(|body| {
                            cx.new(|cx| {
                                let mut p = TextareaState::new(window, cx).rows(8);
                                p.set_value(body, window, cx);
                                p
                            })
                        });
                        this.decision_reuse = ReuseUi {
                            active: true,
                            goal,
                            decision: id,
                            workspace,
                            view,
                            preview,
                            ..Default::default()
                        };
                        this.error = None;
                    }
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn save_decision_reuse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let s = &self.decision_reuse;
        if !s.active || s.frozen || self.busy || self.editor_closing() || s.view["eligible"] != true
        {
            return;
        }
        if self.expected_workspace != s.workspace || self.goal_id() != s.goal {
            self.decision_reuse.error =
                Some("Reuse settings belong to another goal/workspace.".into());
            cx.notify();
            return;
        }
        let outcome = (|| -> Result<(String, Value), String> {
            let base = serde_json::from_value(s.view["source"].clone())
                .map_err(|_| "Invalid reuse base")?;
            let operation = uuid();
            let at = time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(|e| e.to_string())?;
            let policy = Disposition::new(
                operation.clone(),
                text(&self.capabilities["actor"]),
                at,
                text(&s.view["source"]["revision"]),
            );
            let proposed = reuse::transform(&base, &s.goal, &s.decision, &policy)?;
            let mut request = source_write(&s.view["source"], &proposed, &operation);
            request["op"] = json!("discussion_decision_reuse_write");
            request["goal_id"] = json!(s.goal);
            request["decision_id"] = json!(s.decision);
            Ok((proposed, request))
        })();
        match outcome {
            Ok((proposed, request)) => {
                let base = s.view["source"].clone();
                self.decision_reuse.frozen = true;
                self.decision_reuse.error = None;
                self.load_source(base, window, cx);
                self.source.reset(proposed, window, cx);
                self.editor_save(request, window, cx);
            }
            Err(e) => self.decision_reuse.error = Some(e),
        }
        cx.notify();
    }
    pub(super) fn reuse_guard_navigation(
        &mut self,
        destination: PendingNavigation,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.decision_reuse.active {
            return false;
        }
        self.decision_reuse.destination = Some(destination);
        self.decision_reuse.error =
            Some("Finish or explicitly leave this reuse review before continuing.".into());
        cx.notify();
        true
    }
    pub(super) fn reuse_request_close(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.decision_reuse.active {
            return false;
        }
        self.decision_reuse.closing = true;
        self.decision_reuse.error = Some(
            "This reuse review is open. Keep reviewing or explicitly leave before closing.".into(),
        );
        cx.notify();
        true
    }
    pub(super) fn reuse_keep_reviewing(&mut self, cx: &mut Context<Self>) {
        self.decision_reuse.cancel_departure();
        self.decision_reuse.error = None;
        if let Some(token) = self.app_quit_token {
            app_quit::queue_cancel(token, cx);
        }
        cx.notify();
    }
    pub(super) fn reuse_leave_for_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reuse_keep_reviewing(cx);
        self.leave_reuse(window, cx);
    }
    fn leave_reuse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let state = std::mem::take(&mut self.decision_reuse);
        self.surface = if state.frozen {
            Surface::Source
        } else {
            Surface::Context
        };
        if state.closing {
            self.note_finish_close(window, cx);
        } else if let Some(destination) = state.destination {
            self.pending_navigation = Some(destination);
            self.continue_navigation(window, cx);
        }
        cx.notify();
    }
    pub(super) fn reuse_saved_readback(
        &mut self,
        request: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.decision_reuse.readback = Some(request.clone());
        if self
            .expected_workspace
            .as_ref()
            .is_none_or(|workspace| workspace["brain_id"] != request["request"]["brain_id"])
            || self.source_dirty(cx)
            || self.source_snapshot.as_ref().is_none_or(|s| {
                s["content_base64"] != request["request"]["content_base64"]
                    && self.decision_reuse.readback_source.as_ref() != Some(s)
            })
        {
            self.error=Some("Earlier reuse Save is acknowledged. Preserve the newer local draft before current-source readback.".into());
            cx.notify();
            return;
        }
        let workspace = self.expected_workspace.clone();
        let owner = workspace.clone();
        let endpoint = self.endpoint;
        self.busy = true;
        self.source_loading = true;
        self.sync_source_policy(cx);
        cx.spawn_in(window,async move |this,cx|{
            let expected=request.clone();let g=request["goal_id"].clone();let id=request["decision_id"].clone();
            let result=cx.background_executor().spawn(async move {rpc_guarded(endpoint,json!({"op":"discussion_decision_reuse_get","goal_id":g,"decision_id":id}),owner.as_ref())}).await;
            let _=this.update_in(cx,|this,window,cx|{
                this.busy=false;this.source_loading=false;this.sync_source_policy(cx);
                if this.expected_workspace!=workspace || this.source_snapshot.as_ref().is_none_or(|s|s["path"]!=expected["request"]["path"]) {this.error=Some("Reuse Save is acknowledged; the visible source changed before readback.".into());cx.notify();return;}
                match result.and_then(|view|{
                    let body = validate_reuse_view(&view, &text(&expected["goal_id"]), &text(&expected["decision_id"]), &text(&expected["request"]["brain_id"]), &text(&expected["request"]["path"]))?;
                    Ok((view, body))
                }){
                    Ok((view, body))=>{
                        let changed=view["source"]["content_base64"]!=expected["request"]["content_base64"];
                        if !view["source"].is_null() {this.load_source(view["source"].clone(),window,cx);}
                        let unavailable = view["state"] == "unavailable";
                        this.decision_reuse.readback=unavailable.then_some(expected.clone());
                        this.decision_reuse.readback_source=unavailable.then(|| this.source_snapshot.clone()).flatten();
                        this.decision_reuse.frozen=false;
                        if this.decision_reuse.active {
                            this.decision_reuse.preview = body.map(|body| cx.new(|cx| {
                                let mut p = TextareaState::new(window,cx).rows(8);
                                p.set_value(body,window,cx);p
                            }));
                            this.decision_reuse.view=view.clone();
                            this.decision_reuse.error=None;
                        }
                        this.notice=if unavailable {"Reuse Save acknowledged. Current decision reuse is unavailable; recheck current source."} else if changed{"Reuse Save acknowledged. The current source changed afterwards; inspect its current state."}else{"Manual selection only. Saved and read back."}.into();
                        if !this.decision_reuse.active {this.surface=Surface::Source;}
                        // The Context form is retained behind this review. Refresh
                        // its brief and source staleness together; context_get's
                        // existing dirty guard preserves unsent guidance and pins.
                        this.batch(vec![json!({"op":"goal_context_brief","goal_id":expected["goal_id"]}),json!({"op":"context_get","goal_id":expected["goal_id"]})],window,cx);
                    }
                    Err(e)=>{this.decision_reuse.error=Some(format!("Reuse Save acknowledged, current source unavailable: {e}. Recheck current source."));this.error=this.decision_reuse.error.clone();}
                }
                cx.notify();
            });
        }).detach();
    }
    pub(super) fn decision_reuse_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let s = &self.decision_reuse;
        let manual = s.view["state"] == "manual_only";
        let mut panel=v_flex().id("decision-reuse-review").debug_selector(||"decision-reuse-review".into()).flex_1().min_w_0().min_h_0().h_full().overflow_y_scroll().p_6().gap_4()
            .child(div().text_2xl().child("Decision reuse settings"))
            .child(reuse_state_label(&s.view))
            .child("This decision stays saved. Future automatic goal context will leave it out. You can still select it explicitly; earlier conversations and prepared work keep their recorded context.")
            .child(div().text_sm().child(format!("Goal: {} · Original user: {}",text(&self.snapshot["goal"]["title"]),text(&s.view["actor_id"]))));
        if let Some(preview) = &s.preview {
            panel = panel.child(Textarea::new(preview).readonly(true));
        }
        panel = panel.child(
            super::super::brand::control("reuse-details", cx)
                .ghost()
                .label(if s.details { "Hide details" } else { "Details" })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.decision_reuse.details = !this.decision_reuse.details;
                    cx.notify();
                })),
        );
        if s.details {
            panel = panel.child(div().text_sm().child(format!(
                "Conversation: {} · Turn: {} · Original revision: {}",
                text(&s.view["origin"]["conversation_id"]),
                text(&s.view["origin"]["turn_id"]),
                text(&s.view["origin"]["source_revision"])
            )));
        }
        if let Some(e) = &s.error {
            panel = panel.child(div().child(e.clone()));
        }
        if s.view["reason"].is_string() {
            panel = panel.child(div().child(text(&s.view["reason"])));
        }
        if s.destination.is_some() || s.closing {
            panel = panel.child(
                super::super::brand::control("reuse-keep-reviewing", cx)
                    .label("Keep reviewing")
                    .on_click(cx.listener(|this, _, _, cx| this.reuse_keep_reviewing(cx))),
            );
        }
        let unavailable_without_readback = s.view["state"] == "unavailable" && s.readback.is_none();
        let frozen = s.frozen;
        let eligible = s.view["eligible"] == true;
        if frozen {
            panel = panel.child(self.editor_panel(cx));
            if let Some(conflict) = &self.source_conflict {
                panel=panel.child(div().text_sm().child("Source conflict: the reviewed choice is preserved. Leave this review for Source, discard the terminal draft, then review the current decision again."));
                for side in ["base", "current", "proposed"] {
                    panel = panel.child(div().text_sm().whitespace_normal().child(
                        decode_source(&conflict[side]).unwrap_or_else(|_| {
                            "Current source unavailable within read limit".into()
                        }),
                    ));
                }
            }
        }
        if let Some(request) = self.decision_reuse.readback.clone() {
            panel = panel.child(
                super::super::brand::control("reuse-readback", cx)
                    .label("Read current source")
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.reuse_saved_readback(request.clone(), window, cx)
                    })),
            );
        }
        if unavailable_without_readback && !frozen {
            panel = panel.child(
                super::super::brand::control("reuse-recheck", cx)
                    .label("Recheck current source")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| {
                        let decision = this.decision_reuse.decision.clone();
                        this.reuse_keep_reviewing(cx);
                        this.decision_reuse = ReuseUi::default();
                        this.open_decision_reuse(decision, window, cx)
                    })),
            );
        }
        if manual && !frozen {
            panel = panel
                .child(
                    super::super::brand::control("reuse-include", cx)
                        .label("Include in context")
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            let view = this.decision_reuse.view.clone();
                            this.reuse_leave_for_action(window, cx);
                            this.include_manual_decision(view, cx);
                        })),
                )
                .child(
                    super::super::brand::control("reuse-original", cx)
                        .label("Open original")
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            let path = text(&this.decision_reuse.view["path"]);
                            this.reuse_leave_for_action(window, cx);
                            this.open_source(path, window, cx);
                        })),
                );
        }
        panel = panel.child(
            h_flex()
                .gap_2()
                .child(
                    super::super::brand::control("reuse-stop", cx)
                        .label("Stop adding automatically")
                        .disabled(self.busy || frozen || !eligible)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.save_decision_reuse(window, cx)),
                        ),
                )
                .child(
                    super::super::brand::control("reuse-cancel", cx)
                        .label(if frozen {
                            "Leave review for Source"
                        } else if manual {
                            "Close"
                        } else {
                            "Cancel"
                        })
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, window, cx| this.leave_reuse(window, cx))),
                ),
        );
        panel.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    fn fixture() -> (Value, String, String, String, String) {
        let goal = uuid();
        let decision = uuid();
        let brain = uuid();
        let path = format!("records/discussion-decision-{decision}.md");
        let body = "  Exact decision λ\n\n";
        let raw=format!("---\nschema: ai-brain/v1\nrecord_type: discussion-decision\nbrain_id: {brain}\nid: {decision}\ngoal_id: {goal}\nactor_id: local:oleg\n---\n{body}");
        let source = json!({"schema":SCHEMA,"brain_id":brain,"path":path,"revision":reuse::revision(raw.as_bytes()),"content_base64":STANDARD.encode(raw),"media_type":"text/markdown"});
        (
            json!({"goal_id":goal,"decision_id":decision,"path":path,"state":"automatic","eligible":true,"text":body,"source":source}),
            goal,
            decision,
            brain,
            path,
        )
    }
    #[test]
    fn reuse_view_binds_body_state_owner_path_and_revision() {
        let (view, g, id, brain, path) = fixture();
        assert_eq!(
            validate_reuse_view(&view, &g, &id, &brain, &path)
                .unwrap()
                .as_deref(),
            view["text"].as_str()
        );
        for (field, value) in [
            ("brain_id", json!(uuid())),
            ("path", json!("records/other.md")),
            ("revision", json!("sha256:wrong")),
        ] {
            let mut bad = view.clone();
            bad["source"][field] = value;
            assert!(validate_reuse_view(&bad, &g, &id, &brain, &path).is_err());
        }
        for (field, value) in [
            ("text", json!("Stale preview")),
            ("state", json!("manual_only")),
            ("state", json!("unknown")),
        ] {
            let mut bad = view.clone();
            bad[field] = value;
            assert!(validate_reuse_view(&bad, &g, &id, &brain, &path).is_err());
        }
    }
    #[test]
    fn reuse_view_changed_and_unavailable_never_keep_old_preview_or_automatic_label() {
        let (mut view, g, id, brain, path) = fixture();
        let raw = decode_source(&view["source"])
            .unwrap()
            .replace("Exact decision", "Changed decision");
        view["source"]["content_base64"] = json!(STANDARD.encode(&raw));
        view["source"]["revision"] = json!(reuse::revision(raw.as_bytes()));
        view["text"] = json!("  Changed decision λ\n\n");
        assert_eq!(
            validate_reuse_view(&view, &g, &id, &brain, &path)
                .unwrap()
                .unwrap(),
            "  Changed decision λ\n\n"
        );
        view["state"] = json!("unavailable");
        view["eligible"] = json!(false);
        assert_eq!(
            validate_reuse_view(&view, &g, &id, &brain, &path).unwrap(),
            None
        );
        assert!(reuse_state_label(&view).contains("unavailable"));
        view["source"] = Value::Null;
        assert_eq!(
            validate_reuse_view(&view, &g, &id, &brain, &path).unwrap(),
            None
        );
    }
}
