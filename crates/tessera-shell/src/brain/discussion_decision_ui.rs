//! Exact user-decision review. Durable recovery belongs to backend SourceStore.
use super::*;
use sha2::{Digest, Sha256};

#[derive(Default)]
pub(super) struct DecisionUi {
    pub active: bool,
    identity: Value,
    workspace: Option<Value>,
    endpoint: Option<SocketAddr>,
    text: String,
    preview: Option<Entity<TextareaState>>,
    view: Value,
    request: Option<Value>,
    flight: bool,
    attempted: bool,
    sequence: u64,
    error: Option<String>,
    destination: Option<PendingNavigation>,
    closing: bool,
    details_open: bool,
}
fn selection(
    conversation: &Value,
    goal: &str,
    actor: &str,
    index: usize,
) -> Result<(Value, String), String> {
    let message = conversation["messages"]
        .get(index)
        .ok_or("Saved message is unavailable")?;
    if message["role"] != "user"
        || message["text"].as_str().is_none_or(|s| s.trim().is_empty())
        || conversation["goal_id"] != goal
        || actor.trim().is_empty()
    {
        return Err("Choose your nonempty saved user message in this goal.".into());
    }
    let matches: Vec<_> = array(&conversation["turn_contexts"])
        .into_iter()
        .filter(|t| t["user_message_index"].as_u64() == Some(index as u64))
        .collect();
    if matches.len() != 1 || matches[0]["availability"] != "available" {
        return Err("This historical message has no validated saved user turn; its text remains in Discussion.".into());
    }
    for id in [
        goal,
        text(&conversation["id"]).as_str(),
        text(&matches[0]["turn_id"]).as_str(),
    ] {
        if Uuid::parse_str(id).is_err() {
            return Err("Invalid original Discussion identity".into());
        }
    }
    Ok((
        json!({"goal_id":goal,"conversation_id":conversation["id"],"turn_id":matches[0]["turn_id"],"expected_actor_id":actor}),
        text(&message["text"]),
    ))
}
fn validate_reply(
    identity: &Value,
    body: &str,
    request: Option<&Value>,
    reply: &Value,
    workspace: &Value,
) -> Result<(), String> {
    if reply["identity"] != *identity {
        return Err("Reply belongs to another original user turn".into());
    }
    if !matches!(
        reply["state"].as_str(),
        Some(
            "not_recorded"
                | "existing_canonical"
                | "pending"
                | "conflict"
                | "already_saved"
                | "recovery_required"
        )
    ) {
        return Err("Unsupported decision recovery state".into());
    }
    for name in ["decision_id", "operation_id"] {
        let value = reply[name]
            .as_str()
            .ok_or("Decision reply omitted its stable identity")?;
        if Uuid::parse_str(value).is_err() {
            return Err("Invalid decision identity".into());
        }
    }
    let target = format!(
        "{}/discussion-decision-{}.md",
        text(&workspace["records_dir"]),
        text(&reply["decision_id"])
    );
    if reply["path"] != target {
        return Err("Decision target path does not match its identity".into());
    }
    if reply["state"] == "recovery_required" {
        return Ok(());
    }
    if reply["text"] != body {
        return Err("The saved user text changed; this exact-text review is retained.".into());
    }
    if !reply["request"].is_null() {
        let r = &reply["request"];
        if request.is_some_and(|original| original != r)
            || r["brain_id"] != workspace["brain_id"]
            || r["path"] != reply["path"]
            || r["operation_id"] != reply["operation_id"]
            || r["expected_revision"] != Value::Null
            || r["schema"] != SCHEMA
        {
            return Err("Reply does not match the frozen create-only save".into());
        }
        let bytes = STANDARD
            .decode(text(&r["content_base64"]))
            .map_err(|_| "Invalid proposed decision bytes")?;
        if bytes.len() > 8192 {
            return Err("Decision exceeds the supported complete-source limit".into());
        }
        let source = std::str::from_utf8(&bytes).map_err(|_| "Decision is not exact UTF-8")?;
        let front = source
            .strip_prefix("---\n")
            .ok_or("Decision frontmatter is missing")?;
        let mut offset = 0;
        let exact_body = front
            .split_inclusive('\n')
            .find_map(|line| {
                offset += line.len();
                (line == "---\n").then(|| &front[offset..])
            })
            .ok_or("Decision frontmatter is incomplete")?;
        if exact_body != body {
            return Err("Proposed source does not preserve the exact reviewed text".into());
        }
        if reply["state"] == "already_saved" {
            let receipt = &reply["receipt"];
            if receipt["operation_id"] != r["operation_id"]
                || receipt["path"] != r["path"]
                || receipt["previous_revision"] != Value::Null
                || receipt["outcome"] != "written"
                || receipt["revision"] != format!("sha256:{:x}", Sha256::digest(&bytes))
            {
                return Err("Decision acknowledgement does not match this exact save".into());
            }
        }
    } else if reply["state"] != "existing_canonical" {
        return Err("Decision reply omitted the exact source request".into());
    }
    Ok(())
}
impl BrainView {
    pub(super) fn start_discussion_decision(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy
            || self.discussion_decision.active
            || self.discussion_note.active()
            || self.goal_criteria.active
        {
            return;
        }
        if self.capabilities["discussion_decision_save"] != true
            || self
                .expected_workspace
                .as_ref()
                .is_none_or(|w| w["managed"] != true)
        {
            self.error = Some("This backend does not support keeping Discussion decisions.".into());
            cx.notify();
            return;
        }
        if self.source_dirty(cx) || self.source_loading {
            self.error =
                Some("Resolve the existing source draft before reviewing a user decision.".into());
            self.surface = Surface::Source;
            cx.notify();
            return;
        }
        match selection(
            &self.conversation,
            &self.goal_id(),
            &text(&self.capabilities["actor"]),
            index,
        ) {
            Ok((identity, body)) => {
                let preview = cx.new(|cx| {
                    let mut input = TextareaState::new(window, cx).rows(10);
                    input.set_value(body.clone(), window, cx);
                    input
                });
                *self.discussion_decision = DecisionUi {
                    preview: Some(preview),
                    active: true,
                    identity,
                    workspace: self.expected_workspace.clone(),
                    endpoint: Some(self.endpoint),
                    text: body,
                    ..Default::default()
                };
                self.decision_rpc(false, window, cx);
            }
            Err(e) => {
                self.error = Some(e);
                cx.notify();
            }
        }
    }
    pub(super) fn decision_rpc(&mut self, save: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || !self.discussion_decision.active || self.discussion_decision.flight {
            return;
        }
        if self.expected_workspace != self.discussion_decision.workspace
            || self.discussion_decision.endpoint != Some(self.endpoint)
            || self.goal_id() != text(&self.discussion_decision.identity["goal_id"])
        {
            self.discussion_decision.error =
                Some("Return to the original workspace and goal to recover this save.".into());
            cx.notify();
            return;
        }
        if save
            && (self.capabilities["discussion_decision_save"] != true
                || self.discussion_decision.request.is_none()
                || !matches!(
                    self.discussion_decision.view["state"].as_str(),
                    Some("not_recorded" | "pending")
                ))
        {
            return;
        }
        let state = &mut self.discussion_decision;
        state.sequence += 1;
        let sequence = state.sequence;
        state.flight = true;
        state.error = None;
        if save {
            state.attempted = true;
        }
        let identity = state.identity.clone();
        let workspace = state.workspace.clone();
        let expected = state.request.clone();
        let body = state.text.clone();
        let endpoint = self.endpoint;
        let mut wire = identity.clone();
        wire["op"] = json!(if save {
            "discussion_decision_save"
        } else {
            "discussion_decision_get"
        });
        if save {
            wire["request"] = state.request.clone().unwrap();
        }
        self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let guard = workspace.clone();
            let result = cx
                .background_executor()
                .spawn(async move { rpc_guarded(endpoint, wire, guard.as_ref()) })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                if !this.discussion_decision.active
                    || this.discussion_decision.sequence != sequence
                    || this.discussion_decision.identity != identity
                    || this.expected_workspace != workspace
                    || this.endpoint != endpoint
                {
                    return;
                }
                this.busy = false;
                this.discussion_decision.flight = false;
                match result.and_then(|mut reply| {
                    let unrecorded=reply["state"]=="not_recorded";
                    validate_reply(&identity,&body,if unrecorded {None}else{expected.as_ref()},&reply,workspace.as_ref().unwrap())?;
                    if unrecorded {
                        if let Some(original)=&expected {
                            if reply["origin"]!=this.discussion_decision.view["origin"] {
                                return Err("Original source changed while this exact review was open. Leave and review the saved turn again.".into());
                            }
                            // Lookup may prepare a newer timestamp, but retry retains
                            // the first explicitly reviewed proposal byte-for-byte.
                            reply["request"]=original.clone();
                        }
                    }
                    Ok(reply)
                }) {
                    Ok(reply) => {
                        if matches!(
                            reply["state"].as_str(),
                            Some("pending" | "conflict" | "already_saved")
                        ) {
                            this.discussion_decision.attempted = true;
                        }
                        if !reply["request"].is_null() {
                            this.discussion_decision.request = Some(reply["request"].clone());
                        }
                        this.discussion_decision.error = reply["error"].as_str().map(str::to_owned);
                        this.discussion_decision.view = reply;
                    }
                    Err(e) => {
                        this.discussion_decision.error = Some(e);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn decision_guard_navigation(
        &mut self,
        destination: PendingNavigation,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.discussion_decision.active {
            return false;
        }
        self.discussion_decision.destination = Some(destination);
        self.discussion_decision.error=Some("Finish this review or explicitly leave it before continuing. A submitted save remains recoverable from its original turn.".into());
        cx.notify();
        true
    }
    pub(super) fn decision_request_close(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.discussion_decision.active {
            return false;
        }
        self.discussion_decision.closing = true;
        self.discussion_decision.error=Some("Finish or explicitly leave this decision review before closing. Unsaved review is transient; submitted saves can be looked up from the original turn.".into());
        cx.notify();
        true
    }
    pub(super) fn decision_keep_reviewing(&mut self, cx: &mut Context<Self>) {
        self.discussion_decision.destination = None;
        self.discussion_decision.closing = false;
        if let Some(token) = self.app_quit_token {
            app_quit::queue_cancel(token, cx);
        }
        cx.notify();
    }
    fn leave_decision(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.discussion_decision.flight {
            return;
        }
        let state = std::mem::take(&mut self.discussion_decision);
        if state.closing {
            self.note_finish_close(window, cx);
        } else if let Some(destination) = state.destination {
            self.pending_navigation = Some(destination);
            self.continue_navigation(window, cx);
        }
        cx.notify();
    }
    pub(super) fn decision_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let state = &self.discussion_decision;
        let status=match (state.view["state"].as_str(),state.view["target_status"].as_str()) {
            (Some("already_saved"),Some("unchanged"))=>"Decision saved and read back. Available as unverified input for this goal.",
            (Some("already_saved"),_)=>"Save acknowledged. The current canonical decision changed, is missing or could not be read; recovery will not recreate it.",
            (Some("existing_canonical"),_)=>"This user decision is already retained. No new save was issued.",
            (Some("pending"),Some("target_matches_proposed"))=>"The exact canonical decision exists. Its save receipt is still pending; you can open the retained note.",
            (Some("pending"),_)=>"Save is pending. Recheck or retry the same exact save; its original identity stays fixed.",
            (Some("conflict"),_)=>"The decision target conflicts with this save. Its proposed text is preserved; no replacement was made.",
            (Some("recovery_required"),_)=>"Recovery needs attention. No new decision or replacement will be created.",
            _ if state.attempted=>"Save acknowledgement is unavailable. Recheck the original operation before continuing.",
            _=>"Review your exact saved words. Saving keeps them as reusable input for this goal; it does not verify factual claims.",
        };
        let can_save = !self.busy
            && state.request.is_some()
            && matches!(
                state.view["state"].as_str(),
                Some("not_recorded" | "pending")
            );
        let open = matches!(
            state.view["target_status"].as_str(),
            Some("unchanged" | "changed" | "target_matches_proposed")
        );
        let mut panel = v_flex()
            .id("discussion-decision-review")
            .debug_selector(|| "discussion-decision-review".into())
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .p_6()
            .gap_4()
            .child(div().text_2xl().child("Keep as goal decision"))
            .child(div().child(status))
            .when(state.view["reuse_state"] == "manual_only", |panel| panel.child("Current saved decision: manual selection only. The original Save remains historical."))
            .child(div().text_sm().child(format!(
                "Goal: {} · User: {}",
                text(&self.snapshot["goal"]["title"]),
                text(&state.identity["expected_actor_id"])
            )))
            .when_some(state.preview.as_ref(), |panel, preview| {
                panel.child(Textarea::new(preview).readonly(true))
            });
        panel = panel.child(
            super::super::brand::control("decision-details", cx)
                .label(if state.details_open {
                    "Hide details"
                } else {
                    "Details"
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.discussion_decision.details_open = !this.discussion_decision.details_open;
                    cx.notify();
                })),
        );
        if state.details_open {
            panel = panel.child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child(format!(
                        "Conversation: {} · Turn: {}",
                        text(&state.identity["conversation_id"]),
                        text(&state.identity["turn_id"])
                    )))
                    .child(div().text_sm().child(format!(
                        "Original source revision: {}",
                        text(&state.view["origin"]["source_revision"])
                    ))),
            );
        }
        if let Some(error) = &state.error {
            panel = panel.child(div().child(error.clone()));
        }
        if state.view["state"] == "conflict" {
            panel = panel.child(div().text_sm().child("Current target source"));
            if let Ok(current) = decode_source(&state.view["current_source"]) {
                panel = panel.child(div().whitespace_normal().child(current));
            } else {
                panel = panel
                    .child("The current target is missing or unavailable within the read limit.");
            }
        }
        panel = panel.child(
            super::super::brand::control("decision-copy", cx)
                .label("Copy exact text")
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(
                        this.discussion_decision.text.clone(),
                    ))
                })),
        );
        panel
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        super::super::brand::control("decision-save", cx)
                            .primary()
                            .label(if state.attempted {
                                "Retry exact save"
                            } else {
                                "Save decision"
                            })
                            .disabled(!can_save)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.decision_rpc(true, window, cx)
                            })),
                    )
                    .child(
                        super::super::brand::control("decision-recheck", cx)
                            .label("Recheck save")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.decision_rpc(false, window, cx)
                            })),
                    )
                    .child(
                        super::super::brand::control("decision-open", cx)
                            .label("Open retained decision")
                            .disabled(self.busy || !open)
                            .on_click(cx.listener(|this, _, window, cx| {
                                let path = text(&this.discussion_decision.view["path"]);
                                *this.discussion_decision = Default::default();
                                if let Some(token) = this.app_quit_token {
                                    app_quit::queue_cancel(token, cx);
                                }
                                this.open_source(path, window, cx);
                            })),
                    )
                    .child(
                        super::super::brand::control("decision-leave", cx)
                            .label(
                                if matches!(
                                    state.view["state"].as_str(),
                                    Some("already_saved" | "existing_canonical")
                                ) {
                                    "Done"
                                } else if state.attempted {
                                    "Leave review"
                                } else {
                                    "Cancel"
                                },
                            )
                            .disabled(self.busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.leave_decision(window, cx)),
                            ),
                    )
                    .child(
                        super::super::brand::control("decision-keep", cx)
                            .label("Keep reviewing")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.discussion_decision.destination = None;
                                this.discussion_decision.closing = false;
                                if let Some(token) = this.app_quit_token {
                                    app_quit::queue_cancel(token, cx);
                                }
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    const GOAL: &str = "00000000-0000-4000-8000-000000000001";
    const CHAT: &str = "00000000-0000-4000-8000-000000000002";
    const TURN: &str = "00000000-0000-4000-8000-000000000003";
    fn conversation() -> Value {
        json!({"id":CHAT,"goal_id":GOAL,"messages":[{"role":"user","text":" exact **text**\r\n"}],"turn_contexts":[{"user_message_index":0,"turn_id":TURN,"availability":"available"}]})
    }
    fn workspace() -> Value {
        json!({"brain_id":GOAL,"root":"/isolated","records_dir":"records","managed":true})
    }
    fn proposal() -> (Value, Value) {
        let (key, body) = selection(&conversation(), GOAL, "local operator", 0).unwrap();
        let request = json!({"schema":SCHEMA,"brain_id":GOAL,"path":format!("records/discussion-decision-{TURN}.md"),"operation_id":CHAT,"expected_revision":null,"content_base64":STANDARD.encode(format!("---\nschema: ai-brain/v1\n---\n{body}"))});
        let reply = json!({"identity":key,"decision_id":TURN,"operation_id":CHAT,"path":request["path"],"state":"not_recorded","text":body,"request":request,"receipt":null});
        (key, reply)
    }
    #[test]
    fn decision_selection_refuses_assistant_legacy_duplicate_and_cross_goal() {
        let c = conversation();
        assert_eq!(
            selection(&c, GOAL, "local operator", 0).unwrap().1,
            " exact **text**\r\n"
        );
        assert!(selection(&c, CHAT, "local operator", 0).is_err());
        let mut c = c;
        c["messages"][0]["role"] = json!("assistant");
        assert!(selection(&c, GOAL, "local operator", 0).is_err());
        c["messages"][0]["role"] = json!("user");
        c["turn_contexts"] = json!([]);
        assert!(selection(&c, GOAL, "local operator", 0).is_err());
        let mut c = conversation();
        let duplicate = c["turn_contexts"][0].clone();
        c["turn_contexts"].as_array_mut().unwrap().push(duplicate);
        assert!(selection(&c, GOAL, "local operator", 0).is_err());
    }
    #[test]
    fn decision_reply_requires_exact_reviewed_bytes_owner_and_receipt() {
        let (key, reply) = proposal();
        let body = reply["text"].as_str().unwrap();
        let request = &reply["request"];
        assert!(validate_reply(&key, body, None, &reply, &workspace()).is_ok());
        for field in ["operation_id", "path", "content_base64"] {
            let mut bad = reply.clone();
            bad["request"][field] = json!("changed");
            assert!(validate_reply(&key, body, Some(request), &bad, &workspace()).is_err());
        }
        let mut wrong_body = reply.clone();
        wrong_body["request"]["content_base64"] =
            json!(STANDARD.encode(b"---\nschema: ai-brain/v1\n---\nchanged"));
        assert!(validate_reply(&key, body, None, &wrong_body, &workspace()).is_err());
        let mut saved = reply.clone();
        saved["state"] = json!("already_saved");
        saved["receipt"] = json!({"operation_id":request["operation_id"],"path":request["path"],"previous_revision":null,"outcome":"written","revision":format!("sha256:{:x}",Sha256::digest(STANDARD.decode(text(&request["content_base64"])).unwrap()))});
        assert!(validate_reply(&key, body, Some(request), &saved, &workspace()).is_ok());
        saved["receipt"]["outcome"] = json!("unchanged");
        assert!(validate_reply(&key, body, Some(request), &saved, &workspace()).is_err());
    }
    #[gpui::test]
    fn decision_review_protects_navigation_and_cancel_preserves_existing_source(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.busy = true;
            view.expected_workspace = Some(workspace());
            view.snapshot = json!({"goal":{"id":GOAL,"title":"Fixture"}});
            let (identity, reply) = proposal();
            let body = text(&reply["text"]);
            let preview = cx.new(|cx| {
                let mut i = TextareaState::new(window, cx).rows(10);
                i.set_value(body.clone(), window, cx);
                i
            });
            view.discussion_decision = Box::new(DecisionUi {
                active: true,
                identity,
                text: body,
                preview: Some(preview),
                view: reply.clone(),
                request: Some(reply["request"].clone()),
                workspace: Some(workspace()),
                ..Default::default()
            });
            view
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("discussion-decision-review").is_some());
        view.update_in(cx, |v, window, cx| {
            v.busy = false;
            let original = v.source_original.clone();
            assert!(v.note_guard_navigation(PendingNavigation::Surface(Surface::Execution), cx));
            assert!(v.note_request_close(cx));
            assert_eq!(
                v.discussion_decision.request,
                Some(proposal().1["request"].clone())
            );
            v.discussion_decision.destination = None;
            v.discussion_decision.closing = false;
            v.leave_decision(window, cx);
            assert!(!v.discussion_decision.active);
            assert_eq!(v.source_original, original);
            assert!(!v.busy);
        });
    }
    #[gpui::test]
    fn decision_rpc_lost_ack_rechecks_original_request_and_never_downgrades(
        cx: &mut TestAppContext,
    ) {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        cx.update(gpui_component::init);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = listener.local_addr().unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let (_, prepared) = proposal();
        let exact_request = prepared["request"].clone();
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut requests = vec![];
            while !stop.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(v) => v,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let wire: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(wire["expected_workspace"], workspace());
                assert_eq!(wire["schema"], "ai-brain/workspace-v1");
                let count = requests.len();
                requests.push(wire.clone());
                if count == 1 {
                    assert_eq!(wire["op"], "discussion_decision_save");
                    assert_eq!(wire["request"], exact_request);
                    continue;
                }
                assert_eq!(wire["op"], "discussion_decision_get");
                let mut reply = prepared.clone();
                if count >= 2 {
                    reply["state"] = json!("already_saved");
                    reply["target_status"] = json!("unchanged");
                    reply["receipt"] = json!({"operation_id":exact_request["operation_id"],"path":exact_request["path"],"previous_revision":null,"outcome":"written","revision":format!("sha256:{:x}",Sha256::digest(STANDARD.decode(text(&exact_request["content_base64"])).unwrap()))});
                }
                writeln!(
                    stream,
                    "{}",
                    json!({"schema":wire["schema"],"id":wire["id"],"ok":true,"data":reply})
                )
                .unwrap();
            }
            requests
        });
        let (view, visual) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new(endpoint, window, cx);
            v.busy = true;
            v.expected_workspace = Some(workspace());
            v.snapshot = json!({"goal":{"id":GOAL,"title":"Fixture"}});
            v.conversation = conversation();
            v.conversation_id = Some(CHAT.into());
            v.capabilities = json!({"actor":"local operator","discussion_decision_save":true});
            v
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            v.busy = false;
            v.start_discussion_decision(0, window, cx);
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert_eq!(
                v.discussion_decision.view["state"], "not_recorded",
                "{:?}",
                v.discussion_decision.error
            );
            assert_eq!(
                v.discussion_decision
                    .preview
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .value()
                    .as_ref(),
                " exact **text**\r\n"
            );
            v.decision_rpc(true, window, cx);
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert!(v.discussion_decision.error.is_some());
            assert_eq!(
                v.discussion_decision.request,
                Some(proposal().1["request"].clone())
            );
            v.decision_rpc(false, window, cx);
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert_eq!(
                v.discussion_decision.view["state"], "already_saved",
                "{:?}",
                v.discussion_decision.error
            );
            assert!(!v.discussion_decision.flight);
            v.leave_decision(window, cx);
        });
        stopped.store(true, Ordering::Relaxed);
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests
                .iter()
                .filter(|r| r["op"] == "discussion_decision_save")
                .count(),
            1
        );
        assert!(requests.iter().all(|r| r.get("base").is_none()));
    }
    #[gpui::test]
    fn decision_readonly_exact_review_scrolls_to_last_line_without_editing(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let body = " Exact user text with preserved detail.\n".repeat(120);
        let original = body.clone();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            v.busy = true;
            let preview = cx.new(|cx| {
                let mut i = TextareaState::new(window, cx).rows(10);
                i.set_value(body.clone(), window, cx);
                i
            });
            *v.discussion_decision = DecisionUi {
                active: true,
                text: body,
                preview: Some(preview),
                ..Default::default()
            };
            v
        });
        cx.run_until_parked();
        let (bounds, before) = view.update(cx, |v, cx| {
            let p = v.discussion_decision.preview.as_ref().unwrap().read(cx);
            assert!(p.presentation().is_readonly());
            (p.input_bounds(), p.scroll_offset())
        });
        cx.simulate_event(ScrollWheelEvent {
            position: bounds.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(-100_000.))),
            ..Default::default()
        });
        cx.run_until_parked();
        view.update(cx, |v, cx| {
            let p = v.discussion_decision.preview.as_ref().unwrap().read(cx);
            assert!(
                p.scroll_offset().y < before.y,
                "positive control: long review must move within textarea"
            );
            assert!(
                p.visible_row_range().unwrap().end >= 120,
                "last exact line is reachable"
            );
            assert_eq!(p.value().as_ref(), original);
        });
    }
}
