//! Explicit, goal-bound new Discussion drafts. Opening one never dispatches.
use super::*;

#[derive(Default)]
pub(super) struct NewConversationUi {
    pub(super) drafts: BTreeSet<String>,
    // A null-ID send has no client-generated canonical identity. A lost reply
    // cannot be resolved by guessing from a matching message or newest row.
    pub(super) pending: BTreeSet<String>,
}
impl NewConversationUi {
    pub(super) fn drafting(&self, goal: &str) -> bool {
        self.drafts.contains(goal)
    }
}

impl BrainView {
    fn new_conversation_enabled(&self) -> bool {
        !self.busy
            && !self.discussion_guarded()
            && !self.discussion_in_flight()
            && !self.editor_closing()
            && !self.goal_id().is_empty()
            && self.capabilities["chat"] == true
            && self.pending_capture_id.is_none()
            && self.pending_message.is_none()
            && !self.new_conversation.pending.contains(&self.goal_id())
            && self.conversation["status"] != "running"
    }

    pub(super) fn discussion_send_enabled(&self) -> bool {
        !self.busy
            && !self.discussion_guarded()
            && !self.discussion_in_flight()
            && !self.editor_closing()
            && !self.discussion_note.active()
            && !self.goal_id().is_empty()
            && self.capabilities["chat"] == true
            && self.pending_capture_id.is_none()
            && !self.new_conversation.pending.contains(&self.goal_id())
            && self.conversation["status"] != "running"
            && (self.new_conversation.drafting(&self.goal_id())
                || self.conversation_id.is_some()
                || array(&self.snapshot["conversations"]).is_empty())
    }

    fn begin_new_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.new_conversation_enabled() {
            return;
        }
        if self.dirty(cx) || self.source_loading {
            self.error = Some("Keep or finish the current note draft before starting a new conversation. Your Discussion draft is retained.".into());
            cx.notify();
            return;
        }
        self.request_generation += 1;
        self.new_conversation.drafts.insert(self.goal_id());
        self.conversation_id = None;
        self.conversation = Value::Null;
        self.reset_discussion_context();
        self.error = None;
        self.notice = "New conversation draft. Nothing is sent until you choose Send.".into();
        self.compose.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    pub(super) fn acknowledge_checked_conversations(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.editor_closing() || self.discussion_note.active() {
            return;
        }
        if !self.new_conversation.pending.remove(&self.goal_id()) {
            return;
        }
        self.pending_message = None;
        self.error = None;
        self.notice = "You confirmed checking saved conversations. The previous delivery is still unknown. Nothing was resent; Send is a separate action.".into();
        cx.notify();
    }

    pub(super) fn new_conversation_controls(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let goal = self.goal_id();
        let mut panel = v_flex().gap_2().flex_shrink_0().child(
            crate::brand::control("new-conversation", cx)
                .debug_selector(|| "brain-new-conversation".into())
                .label("New conversation")
                .disabled(!self.new_conversation_enabled() || self.new_conversation.drafting(&goal))
                .on_click(
                    cx.listener(|this, _, window, cx| this.begin_new_conversation(window, cx)),
                ),
        );
        if self.new_conversation.drafting(&goal) {
            panel = panel.child(
                div().id("new-conversation-draft")
                    .debug_selector(|| "brain-new-conversation-draft".into())
                    .text_sm()
                    .child("New conversation · empty history. Your draft and selected sources are retained. Send starts this conversation."),
            );
        }
        if self.new_conversation.pending.contains(&goal) {
            panel = panel
                .child(div().text_sm().child("The previous message may already be saved. Refresh and inspect saved conversations before deciding to send again. Delivery is still unknown; nothing was resent."))
                .child(crate::brand::control("checked-saved-conversations", cx)
                    .debug_selector(|| "brain-checked-saved-conversations".into())
                    .label("I checked saved conversations")
                    .disabled(self.busy || self.editor_closing() || self.discussion_note.active())
                    .on_click(cx.listener(|this, _, _, cx| this.acknowledge_checked_conversations(cx))));
        }
        panel.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::{TestAppContext, VisualTestContext};

    fn setup(cx: &mut TestAppContext) -> (Entity<BrainView>, &mut VisualTestContext) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut v = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            v.busy = true;
            v
        })
    }
    fn install(v: &mut BrainView, window: &mut Window, cx: &mut Context<BrainView>) {
        v.busy = false;
        v.capabilities = json!({"chat":true});
        v.snapshot = json!({"goal":{"id":"goal-a"},"conversations":[{"id":"old-a","status":"complete"}],"selected_conversation_id":"old-a","selected_source_paths":["pin.md"]});
        v.selected_goal_id = Some("goal-a".into());
        v.conversation_id = Some("old-a".into());
        v.conversation = json!({"id":"old-a","goal_id":"goal-a","status":"complete","messages":[{"role":"user","text":"Old question"}]});
        v.selected_sources.insert("pin.md".into());
        v.compose
            .update(cx, |i, cx| i.set_value("Retained question\n", window, cx));
    }
    #[gpui::test]
    fn new_conversation_is_local_preserves_draft_pins_and_ignores_old_reads(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = setup(cx);
        view.update_in(cx, |v, window, cx| {
            install(v, window, cx);
            let before = v.snapshot.clone();
            let old_generation = v.request_generation;
            v.begin_new_conversation(window, cx);
            assert!(!v.busy, "opening a draft must not start RPC");
            assert_eq!(v.snapshot, before);
            assert_eq!(v.compose.read(cx).value().as_ref(), "Retained question\n");
            assert!(v.selected_sources.contains("pin.md"));
            assert!(v.conversation.is_null());
            assert!(v.conversation_id.is_none());
            assert!(v.discussion_send_enabled());
            v.finish_batch(
                vec![(
                    "chat_get".into(),
                    Ok(json!({"id":"old-a","messages":[{"role":"user","text":"Stale"}]})),
                )],
                true,
                old_generation,
                window,
                cx,
            );
            assert!(v.conversation.is_null());
            v.finish_batch(
                vec![("snapshot".into(), Ok(before))],
                false,
                v.request_generation,
                window,
                cx,
            );
            assert!(v.conversation_id.is_none());
            assert!(v.conversation.is_null());
            assert!(v.new_conversation.drafting("goal-a"));
        });
    }
    #[gpui::test]
    fn new_conversation_mode_and_drafts_belong_to_their_goal(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        view.update_in(cx,|v,window,cx|{
            install(v,window,cx);let a=v.snapshot.clone();
            v.begin_new_conversation(window,cx);
            v.finish_batch(vec![("snapshot".into(),Ok(json!({"goal":{"id":"goal-b"},"conversations":[],"selected_source_paths":[]})))],false,v.request_generation,window,cx);
            assert_eq!(v.goal_id(),"goal-b");assert!(!v.new_conversation.drafting("goal-b"));
            assert_eq!(v.compose.read(cx).value().as_ref(),"");
            v.compose.update(cx,|i,cx|i.set_value("Question B",window,cx));
            v.finish_batch(vec![("snapshot".into(),Ok(a))],false,v.request_generation,window,cx);
            assert_eq!(v.goal_id(),"goal-a");assert!(v.conversation_id.is_none());
            assert_eq!(v.compose.read(cx).value().as_ref(),"Retained question\n");
            assert_eq!(v.goal_drafts["goal-b"].0,"Question B");
            assert!(v.selected_sources.contains("pin.md"));
        });
    }
    #[gpui::test]
    fn new_conversation_handlers_block_running_pending_capture_and_source_drafts(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = setup(cx);
        view.update_in(cx, |v, window, cx| {
            install(v, window, cx);
            for reason in ["busy", "running", "pending", "capture", "source"] {
                v.busy = reason == "busy";
                v.conversation["status"] = json!(if reason == "running" {
                    "running"
                } else {
                    "complete"
                });
                v.pending_message = (reason == "pending").then(|| ("Uncertain".into(), 0));
                v.pending_capture_id = (reason == "capture").then(|| "capture".into());
                v.source_editable = reason == "source";
                v.source_original = "saved".into();
                v.source.reset("draft", window, cx);
                v.begin_new_conversation(window, cx);
                assert!(!v.new_conversation.drafting("goal-a"), "{reason}");
                assert_eq!(v.conversation_id.as_deref(), Some("old-a"));
            }
            v.source_editable = false;
            v.begin_new_conversation(window, cx);
            assert!(v.new_conversation.drafting("goal-a"), "positive control");
        });
    }
    #[gpui::test]
    fn new_conversation_lost_ack_survives_refresh_navigation_and_goal_switch(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = setup(cx);
        view.update_in(cx,|v,window,cx|{
            install(v,window,cx);let a=v.snapshot.clone();v.begin_new_conversation(window,cx);
            v.new_conversation.pending.insert("goal-a".into());v.pending_message=Some(("Retained question".into(),0));
            v.finish_batch(vec![("chat_start".into(),Err("Incomplete backend reply".into()))],false,v.request_generation,window,cx);
            v.busy=false;assert!(!v.discussion_send_enabled());
            v.note_navigate_conversation("old-a".into(),window,cx);v.busy=false;
            v.finish_batch(vec![("chat_get".into(),Ok(json!({"id":"old-a","goal_id":"goal-a","messages":[{"role":"user","text":"Retained question"}]})))],false,v.request_generation,window,cx);
            assert!(v.new_conversation.pending.contains("goal-a"),"matching text is not acknowledgement");
            v.finish_batch(vec![("snapshot".into(),Ok(json!({"goal":{"id":"goal-b"},"conversations":[]})))],false,v.request_generation,window,cx);v.busy=false;
            assert!(!v.new_conversation.pending.contains("goal-b"));
            v.finish_batch(vec![("snapshot".into(),Ok(a))],false,v.request_generation,window,cx);v.busy=false;
            assert!(!v.discussion_send_enabled());
            v.begin_new_conversation(window,cx);assert!(!v.new_conversation.drafting("goal-a"));
            assert_eq!(v.compose.read(cx).value().as_ref(),"Retained question\n");
        });
    }
    #[gpui::test]
    fn new_conversation_limit_refusal_keeps_draft_and_allows_explicit_new(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        view.update_in(cx,|v,window,cx|{
            install(v,window,cx);v.pending_message=Some(("Retained question".into(),1));
            v.finish_batch(vec![("chat_start".into(),Ok(json!({"_chat_context_refused":"chat_context_limit: retained turn limit; start a new conversation"})))],false,v.request_generation,window,cx);
            assert!(v.pending_message.is_none());assert_eq!(v.conversation_id.as_deref(),Some("old-a"));
            assert_eq!(v.compose.read(cx).value().as_ref(),"Retained question\n");
            v.begin_new_conversation(window,cx);assert!(v.new_conversation.drafting("goal-a"));assert!(v.discussion_send_enabled());
        });
    }
    #[gpui::test]
    fn new_conversation_ack_must_establish_a_fresh_identity(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        view.update_in(cx, |v, window, cx| {
            install(v, window, cx);
            let old_id = uuid();
            v.snapshot["conversations"][0]["id"] = json!(old_id);
            v.conversation_id = Some(old_id.clone());
            v.begin_new_conversation(window, cx);
            v.new_conversation.pending.insert("goal-a".into());
            v.pending_message = Some(("Retained question".into(), 0));
            v.finish_batch(
                vec![("chat_start".into(), Ok(json!({"conversation_id":old_id})))],
                false,
                v.request_generation,
                window,
                cx,
            );
            assert!(v.new_conversation.pending.contains("goal-a"));
            assert!(v.conversation_id.is_none());
            let id = uuid();
            v.finish_batch(
                vec![("chat_start".into(), Ok(json!({"conversation_id":id})))],
                false,
                v.request_generation,
                window,
                cx,
            );
            assert_eq!(v.conversation_id.as_deref(), Some(id.as_str()));
            assert!(!v.new_conversation.pending.contains("goal-a"));
            assert!(!v.new_conversation.drafting("goal-a"));
            assert_eq!(v.compose.read(cx).value().as_ref(), "");
        });
    }
    #[test]
    fn new_conversation_refusal_requires_exact_server_envelope_and_error_kind() {
        use std::net::TcpListener;
        use std::thread;
        for case in ["limit", "other-error", "wrong-id", "transport-text"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["op"], "chat_start");
                assert!(request["conversation_id"].is_null());
                if case == "transport-text" {
                    writeln!(
                        reader.get_mut(),
                        "chat_context_limit: pretend transport failure"
                    )
                    .unwrap();
                } else {
                    let message = if case == "other-error" {
                        "storage write failed"
                    } else {
                        "chat_context_limit: retained turn limit; start a new conversation"
                    };
                    let reply = json!({"schema":request["schema"],"id":if case=="wrong-id"{json!("wrong")}else{request["id"].clone()},"ok":false,"error":{"code":"runtime_error","message":message}});
                    writeln!(reader.get_mut(), "{reply}").unwrap();
                }
            });
            let result = rpc(
                address,
                json!({"op":"chat_start","goal_id":"goal-a","conversation_id":null,"message":"Draft"}),
            );
            if case == "limit" {
                assert!(result.unwrap()["_chat_context_refused"].is_string());
            } else {
                assert!(result.is_err(), "{case} cannot release pending request");
            }
            server.join().unwrap();
        }
    }

    #[gpui::test]
    fn new_conversation_cannot_bypass_an_active_note_review(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        view.update_in(cx,|v,window,cx|{
            install(v,window,cx);
            let goal=uuid();v.snapshot["goal"]["id"]=json!(goal);v.selected_goal_id=Some(goal.clone());
            v.expected_workspace=Some(json!({"brain_id":uuid(),"root":"/isolated/new-conversation","records_dir":"records","managed":true}));
            v.capabilities["source_write"]=json!(true);
            v.start_new_note(window,cx);assert!(v.discussion_note.active());
            v.begin_new_conversation(window,cx);assert!(!v.new_conversation.drafting(&goal));
            assert!(!v.discussion_send_enabled());
            assert_eq!(v.compose.read(cx).value().as_ref(),"Retained question\n");
        });
    }
    #[gpui::test]
    fn new_conversation_explicit_acknowledgement_only_clears_this_goal_guard(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = setup(cx);
        view.update_in(cx, |v, window, cx| {
            install(v, window, cx);
            v.new_conversation
                .pending
                .extend(["goal-a".into(), "goal-b".into()]);
            v.goal_drafts
                .insert("goal-b".into(), ("Question B".into(), "Next B".into()));
            v.pending_message = Some(("Retained question".into(), 0));
            let transcript = v.conversation.clone();
            let generation = v.request_generation;
            v.busy = true;
            v.acknowledge_checked_conversations(cx);
            assert!(v.new_conversation.pending.contains("goal-a"));
            v.busy = false;
            v.acknowledge_checked_conversations(cx);
            assert!(!v.busy);
            assert_eq!(
                v.request_generation, generation,
                "acknowledgement must not dispatch"
            );
            assert!(v.pending_message.is_none());
            assert!(!v.new_conversation.pending.contains("goal-a"));
            assert!(v.new_conversation.pending.contains("goal-b"));
            assert_eq!(v.goal_drafts["goal-b"].0, "Question B");
            assert_eq!(v.conversation, transcript);
            assert_eq!(v.conversation_id.as_deref(), Some("old-a"));
            assert_eq!(v.compose.read(cx).value().as_ref(), "Retained question\n");
            assert!(v.selected_sources.contains("pin.md"));
            assert!(v.notice.contains("still unknown"));
            assert!(v.discussion_send_enabled());
            v.begin_new_conversation(window, cx);
            v.new_conversation.pending.insert("goal-a".into());
            v.acknowledge_checked_conversations(cx);
            assert!(v.new_conversation.drafting("goal-a"));
            assert!(v.conversation_id.is_none());
            assert!(v.conversation.is_null());
            assert!(!v.busy);
        });
    }
    #[gpui::test]
    fn new_conversation_guards_do_not_disable_legacy_sends_after_generic_errors(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = setup(cx);
        view.update_in(cx, |v, window, cx| {
            for existing in [true, false] {
                install(v, window, cx);
                if !existing {
                    v.snapshot["conversations"] = json!([]);
                    v.conversation_id = None;
                    v.conversation = Value::Null;
                }
                v.pending_message = Some(("Retained question".into(), 0));
                v.finish_batch(
                    vec![("chat_start".into(), Err("manual source unavailable".into()))],
                    false,
                    v.request_generation,
                    window,
                    cx,
                );
                v.busy = false;
                assert!(
                    v.discussion_send_enabled(),
                    "existing={existing}: preserve baseline retry availability"
                );
                assert!(
                    !v.new_conversation_enabled(),
                    "an unresolved legacy send cannot silently become New"
                );
            }
            install(v, window, cx);
            v.pending_message = None;
            v.begin_new_conversation(window, cx);
            v.new_conversation.pending.insert("goal-a".into());
            v.pending_message = Some(("Retained question".into(), 0));
            v.finish_batch(
                vec![("chat_start".into(), Err("connection lost".into()))],
                false,
                v.request_generation,
                window,
                cx,
            );
            v.busy = false;
            assert!(!v.discussion_send_enabled());
            v.acknowledge_checked_conversations(cx);
            assert!(v.discussion_send_enabled());
        });
    }
}
