//! Discussion recovery stays with its original owner, independent of selection.
use super::discussion_send_outbox::{classify, Entry, Journal, Pending};
use super::*;

#[derive(Default)]
pub(super) struct DiscussionSendUi {
    pub entries: Vec<Entry>,
    pub active: BTreeSet<String>,
    pub separate: BTreeSet<String>,
    pub error: Option<String>,
    pub load_failed: bool,
    pub terminal_unconfirmed: BTreeSet<String>,
    pub outcomes: BTreeMap<String, String>,
    restore: Option<String>,
    expanded: bool,
    details: BTreeSet<String>,
    workspace_label: Option<String>,
    pub expected_conversation: Option<String>,
    #[cfg(test)]
    journal: Option<Journal>,
}
#[derive(Clone)]
pub(crate) struct OpenDiscussionOwner {
    pub workspace: Value,
    pub endpoint: SocketAddr,
    pub operation_id: String,
}
impl EventEmitter<OpenDiscussionOwner> for BrainView {}
fn owner_key(p: &Pending) -> String {
    serde_json::to_string(&(&p.workspace, &p.request.goal_id)).unwrap()
}
impl BrainView {
    pub(crate) fn set_discussion_workspace_label(&mut self, label: String) {
        self.discussion_send.workspace_label = Some(label);
    }
    fn discussion_journal(&self) -> Journal {
        #[cfg(test)]
        if let Some(journal) = &self.discussion_send.journal {
            return journal.clone();
        }
        Journal::open()
    }
    pub(super) fn ensure_discussion_send(&mut self) {
        match self.discussion_journal().entries() {
            Ok(mut entries) => {
                self.discussion_send.load_failed = false;
                for entry in &mut entries {
                    if self
                        .discussion_send
                        .terminal_unconfirmed
                        .contains(&entry.pending.request.operation_id)
                    {
                        entry.terminal = None;
                    }
                }
                // A failed publication/fsync can leave a memory-only guard. A
                // refresh cannot erase it and cannot cause a first dispatch.
                for old in &self.discussion_send.entries {
                    if !entries
                        .iter()
                        .any(|e| e.pending.request.operation_id == old.pending.request.operation_id)
                    {
                        entries.push(old.clone());
                    }
                }
                self.discussion_send.entries = entries;
            }
            Err(e) => {
                self.discussion_send.load_failed = true;
                self.discussion_send.error = Some(e);
            }
        }
    }
    fn discussion_owner(&self, p: &Pending) -> bool {
        self.expected_workspace.as_ref() == Some(&p.workspace)
            && self.goal_id() == p.request.goal_id
    }
    pub(super) fn discussion_guarded(&self) -> bool {
        self.discussion_send.load_failed
            || self.discussion_send.entries.iter().any(|e| {
                e.terminal.is_none()
                    && self.discussion_owner(&e.pending)
                    && (!self.correlated_capability()
                        || !self
                            .discussion_send
                            .separate
                            .contains(&owner_key(&e.pending)))
            })
    }
    pub(super) fn discussion_in_flight(&self) -> bool {
        self.discussion_send.entries.iter().any(|e| {
            self.discussion_owner(&e.pending)
                && self
                    .discussion_send
                    .active
                    .contains(&e.pending.request.operation_id)
        })
    }
    fn correlated_capability(&self) -> bool {
        self.capabilities["discussion_send_recovery"] == true
            && self.capabilities["workspace_guard"] == true
            && self.expected_workspace.as_ref() == Some(&self.capabilities["workspace"])
    }
    pub(super) fn send_correlated_discussion(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.correlated_capability() || self.capabilities["chat"] != true {
            self.discussion_send.error=Some("Reconnect this exact workspace before sending. Correlated sends never fall back to legacy delivery.".into());
            cx.notify();
            return;
        }
        let p = Pending::prepare(
            self.expected_workspace.clone().unwrap(),
            self.endpoint,
            self.goal_id(),
            text(&self.capabilities["actor"]),
            self.conversation_id.clone(),
            self.compose.read(cx).value().to_string(),
            self.selected_sources.iter().cloned().collect(),
        );
        let p = match p {
            Ok(p) => p,
            Err(e) => {
                self.discussion_send.error = Some(e);
                cx.notify();
                return;
            }
        };
        self.discussion_send.separate.remove(&owner_key(&p));
        self.discussion_send.entries.push(Entry {
            pending: p.clone(),
            terminal: None,
        });
        if let Err(e) = self.discussion_journal().retain(&p) {
            self.discussion_send.error = Some(format!(
                "{e} Nothing was dispatched; the draft remains retained."
            ));
            cx.notify();
            return;
        }
        self.dispatch_discussion(p, true, window, cx);
    }
    fn recover_discussion(&mut self, p: Pending, window: &mut Window, cx: &mut Context<Self>) {
        if !self.correlated_capability() || self.expected_workspace.as_ref() != Some(&p.workspace) {
            self.discussion_send.error=Some("Open the original workspace with Discussion recovery support. This operation will not be resent.".into());
            cx.notify();
            return;
        }
        self.dispatch_discussion(p, false, window, cx);
    }
    fn dispatch_discussion(
        &mut self,
        p: Pending,
        direct: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let operation = p.request.operation_id.clone();
        if !self.discussion_send.active.insert(operation.clone()) {
            return;
        }
        self.discussion_send.error = None;
        let endpoint = self.endpoint;
        let origin = p.clone();
        let journal = self.discussion_journal();
        let generation = self.request_generation;
        cx.spawn_in(window, async move |this, cx| {
            let response = cx
                .background_executor()
                .spawn(async move {
                    let request = if direct {
                        serde_json::to_value(&p.request).unwrap()
                    } else {
                        p.request.lookup()
                    };
                    let data = rpc_guarded(endpoint, request, Some(&p.workspace))?;
                    let status = classify(&p, &data, direct)?;
                    // Persist terminal proof even if the view closes or changes
                    // while the original request is on the wire.
                    journal.acknowledge(&p, &data, direct)?;
                    Ok::<_, String>((data, status))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_discussion_reply(origin, (direct, generation), response, window, cx);
            });
        })
        .detach();
        cx.notify();
    }
    fn finish_discussion_reply(
        &mut self,
        origin: Pending,
        context: (bool, u64),
        response: Result<(Value, &'static str), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (direct, generation) = context;
        let operation = origin.request.operation_id.clone();
        self.discussion_send.active.remove(&operation);
        match response {
            Ok((data, status)) => {
                self.discussion_send
                    .outcomes
                    .insert(operation.clone(), status.into());
                if matches!(status, "projected" | "rejected") {
                    self.discussion_send.terminal_unconfirmed.remove(&operation);
                    if let Some(entry) = self
                        .discussion_send
                        .entries
                        .iter_mut()
                        .find(|e| e.pending.request.operation_id == operation)
                    {
                        entry.terminal = Some(data.clone());
                    }
                }
                // A lookup/reconnect/late reply never changes selection.
                if direct
                    && status == "projected"
                    && generation == self.request_generation
                    && self.discussion_owner(&origin)
                {
                    self.new_conversation.drafts.remove(&origin.request.goal_id);
                    self.conversation_id = data["record"]["conversation_id"]
                        .as_str()
                        .map(str::to_owned);
                    if self.compose.read(cx).value().as_ref() == origin.draft {
                        self.compose
                            .update(cx, |input, cx| input.set_value("", window, cx));
                    }
                    self.conversation = Value::Null;
                    self.discussion_send.expected_conversation = self.conversation_id.clone();
                    self.notice="Conversation projection saved. Provider delivery and completion are separate; inspect the conversation.".into();
                    self.details_inner(false, window, cx);
                }
            }
            Err(e) => {
                self.discussion_send
                    .terminal_unconfirmed
                    .insert(operation.clone());
                self.discussion_send.error=Some(format!("{e} The original draft and operation remain retained. Recovery only checks saved state."));
            }
        }
        cx.notify();
    }
    /// Queue an explicit owner choice before a fresh view's startup task runs.
    /// The initial batch still loads capabilities and sources exactly once.
    pub(crate) fn queue_retained_discussion(&mut self, operation: String) {
        self.ensure_discussion_send();
        if self.discussion_send.entries.iter().any(|e| {
            e.pending.request.operation_id == operation
                && self.expected_workspace.as_ref() == Some(&e.pending.workspace)
        }) {
            self.discussion_send.restore = Some(operation);
        }
    }
    pub(super) fn initial_discussion_goal(&self) -> Option<String> {
        self.discussion_send.restore.as_ref().and_then(|operation| {
            self.discussion_send
                .entries
                .iter()
                .find(|e| &e.pending.request.operation_id == operation)
                .map(|e| e.pending.request.goal_id.clone())
        })
    }
    pub(crate) fn open_retained_discussion(
        &mut self,
        operation: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_discussion_send();
        let Some(p) = self
            .discussion_send
            .entries
            .iter()
            .find(|e| e.pending.request.operation_id == operation)
            .map(|e| e.pending.clone())
        else {
            return;
        };
        if self.expected_workspace.as_ref() != Some(&p.workspace) {
            if self.dirty(cx) || self.busy {
                return;
            }
            cx.emit(OpenDiscussionOwner {
                workspace: p.workspace,
                endpoint: p.endpoint,
                operation_id: operation,
            });
            return;
        }
        if self.dirty(cx) || self.busy {
            return;
        }
        self.discussion_send.restore = Some(operation);
        self.collection = Collection::Goals;
        self.surface = Surface::Conversation;
        if self.goal_id() == p.request.goal_id {
            self.restore_discussion_draft(window, cx);
        } else {
            self.select_goal(p.request.goal_id, window, cx);
        }
    }
    pub(super) fn restore_discussion_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.discussion_send.restore.clone() else {
            return;
        };
        let Some(p) = self
            .discussion_send
            .entries
            .iter()
            .find(|e| e.pending.request.operation_id == id)
            .map(|e| e.pending.clone())
        else {
            return;
        };
        if !self.discussion_owner(&p) {
            return;
        }
        self.discussion_send.restore = None;
        self.request_generation += 1;
        self.compose
            .update(cx, |input, cx| input.set_value(p.draft, window, cx));
        self.selected_sources = p.request.source_paths.into_iter().collect();
        self.conversation_id = p.request.conversation_id;
        self.conversation = Value::Null;
        self.discussion_send.expected_conversation = self.conversation_id.clone();
        if self.conversation_id.is_none() {
            self.new_conversation.drafts.insert(p.request.goal_id);
        } else {
            self.new_conversation.drafts.remove(&p.request.goal_id);
        }
        self.reset_discussion_context();
        if self.conversation_id.is_some() {
            self.details_inner(false, window, cx);
        }
    }

    fn separate_discussion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy
            || self.dirty(cx)
            || self.discussion_in_flight()
            || !self.correlated_capability()
        {
            return;
        }
        if let Some(workspace) = &self.expected_workspace {
            self.discussion_send
                .separate
                .insert(serde_json::to_string(&(workspace, self.goal_id())).unwrap());
        }
        self.discussion_send.error = None;
        self.new_conversation.drafts.insert(self.goal_id());
        self.conversation_id = None;
        self.conversation = Value::Null;
        self.compose
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.reset_discussion_context();
        self.notice="Write a separate message. The earlier unconfirmed message and its draft remain in Discussion recovery.".into();
        cx.notify();
    }
    pub(super) fn discussion_delivery_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut panel = v_flex().gap_2().flex_shrink_0();
        if !self.discussion_send.entries.is_empty() {
            let pending = self
                .discussion_send
                .entries
                .iter()
                .filter(|e| e.terminal.is_none())
                .count();
            panel = panel.child(
                crate::brand::control("discussion-deliveries", cx)
                    .label(format!(
                        "Discussion recovery · {pending} to check · {} kept",
                        self.discussion_send.entries.len()
                    ))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.discussion_send.expanded = !this.discussion_send.expanded;
                        cx.notify();
                    })),
            );
        }
        if let Some(e) = &self.discussion_send.error {
            panel = panel.child(div().text_sm().child(e.clone()));
        }
        if !self.discussion_send.expanded {
            return panel.into_any_element();
        }
        let mut rows = v_flex()
            .id("discussion-delivery-rows")
            .max_h(px(260.))
            .overflow_y_scroll()
            .gap_2();
        for entry in self.discussion_send.entries.clone() {
            let p = entry.pending;
            let id = p.request.operation_id.clone();
            let same_workspace = self.expected_workspace.as_ref() == Some(&p.workspace);
            let status = entry
                .terminal
                .as_ref()
                .and_then(|d| classify(&p, d, d.get("_discussion_send_error").is_some()).ok())
                .or_else(|| self.discussion_send.outcomes.get(&id).map(String::as_str))
                .unwrap_or("unknown");
            let label = match status {
                "projected" => "Message saved to conversation; reply status is not confirmed here",
                "rejected" => "Request rejected before recording; draft kept",
                "pending_projection" => "Conversation save pending; delivery is not confirmed",
                "projection_conflict" => "Conversation could not be updated; original message kept",
                _ => "Send not confirmed; draft kept",
            };
            // Names are presentation only. Never resolve another workspace's
            // goal UUID against the currently loaded snapshot.
            let goal = same_workspace
                .then(|| {
                    self.snapshot["goals"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .chain(std::iter::once(&self.snapshot["goal"]))
                        .find(|goal| goal["id"] == p.request.goal_id)
                        .and_then(|goal| goal["title"].as_str())
                        .filter(|title| !title.trim().is_empty())
                })
                .flatten()
                .unwrap_or("Original goal (name unavailable)");
            let workspace = if same_workspace {
                self.discussion_send
                    .workspace_label
                    .as_deref()
                    .filter(|label| !label.trim().is_empty())
                    .unwrap_or("Current workspace")
            } else {
                "Original workspace (not open)"
            };
            let mut draft: String = p.draft.chars().take(120).collect();
            if p.draft.chars().nth(120).is_some() {
                draft.push('…');
            }
            let details_open = self.discussion_send.details.contains(&id);
            let detail_id = id.clone();
            let open = id.clone();
            let recover = p.clone();
            let mut row = v_flex()
                .gap_1()
                .child(div().text_sm().child(format!("{goal} · {workspace}")))
                .child(div().text_sm().child(label))
                .child(
                    div()
                        .text_sm()
                        .text_ellipsis()
                        .child(format!("Draft: {draft}")),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            crate::brand::control(
                                SharedString::from(format!("discussion-owner-{id}")),
                                cx,
                            )
                            .label(if same_workspace {
                                "Open goal and draft"
                            } else {
                                "Open original workspace"
                            })
                            .disabled(self.busy || self.dirty(cx))
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.open_retained_discussion(open.clone(), window, cx)
                                },
                            )),
                        )
                        .child(
                            crate::brand::control(
                                SharedString::from(format!("discussion-recover-{id}")),
                                cx,
                            )
                            .label("Check message status")
                            .disabled(
                                !same_workspace
                                    || !self.correlated_capability()
                                    || self.discussion_send.active.contains(&id)
                                    || entry.terminal.is_some(),
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.recover_discussion(recover.clone(), window, cx)
                                },
                            )),
                        )
                        .child(
                            crate::brand::control(
                                SharedString::from(format!("discussion-details-{id}")),
                                cx,
                            )
                            .label(if details_open {
                                "Hide details"
                            } else {
                                "Details"
                            })
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if !this.discussion_send.details.remove(&detail_id) {
                                        this.discussion_send.details.insert(detail_id.clone());
                                    }
                                    cx.notify();
                                },
                            )),
                        ),
                );
            if details_open {
                let evidence = json!({
                    "workspace": p.workspace,
                    "endpoint": p.endpoint.to_string(),
                    "goal_id": p.request.goal_id,
                    "operation_id": id,
                    "actor_id": p.request.expected_actor_id,
                    "original_conversation_id": p.request.conversation_id,
                    "request_sha256": p.request.request_sha256,
                    "source_paths": p.request.source_paths,
                    "recovery_status": status,
                    "saved_receipt": entry.terminal,
                });
                row = row.child(
                    v_flex().gap_1()
                        .child(div().text_sm().child(
                            "A saved conversation does not by itself confirm a successful AI reply.",
                        ))
                        .child(div().text_xs().whitespace_normal().child(
                            serde_json::to_string_pretty(&evidence).unwrap(),
                        )),
                );
            }
            if self.discussion_owner(&p) && entry.terminal.is_none() {
                row = row.child(
                    crate::brand::control(
                        SharedString::from(format!("discussion-separate-{id}")),
                        cx,
                    )
                    .label("Write a separate message")
                    .disabled(
                        self.busy
                            || self.dirty(cx)
                            || self.discussion_in_flight()
                            || !self.correlated_capability(),
                    )
                    .on_click(
                        cx.listener(|this, _, window, cx| this.separate_discussion(window, cx)),
                    ),
                );
            }
            rows = rows.child(row);
        }
        panel.child(rows).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::super::discussion_send_outbox::tests::{pending, projected};
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
    fn install(v: &mut BrainView, p: &Pending, window: &mut Window, cx: &mut Context<BrainView>) {
        v.busy = false;
        v.expected_workspace = Some(p.workspace.clone());
        v.snapshot = json!({"goal":{"id":p.request.goal_id},"conversations":[]});
        v.selected_goal_id = Some(p.request.goal_id.clone());
        v.capabilities = json!({"chat":true,"discussion_send_recovery":true,"workspace_guard":true,"workspace":p.workspace,"actor":"operator"});
        v.discussion_send = DiscussionSendUi::default();
        v.discussion_send.entries.push(Entry {
            pending: p.clone(),
            terminal: None,
        });
        v.conversation = Value::Null;
        v.conversation_id = None;
        v.compose
            .update(cx, |i, cx| i.set_value(p.draft.clone(), window, cx));
    }
    #[gpui::test]
    fn discussion_durable_guard_survives_legacy_ack_and_capability_downgrade(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = setup(cx);
        let p = pending();
        view.update_in(cx, |v, window, cx| {
            install(v, &p, window, cx);
            assert!(!v.discussion_send_enabled());
            v.new_conversation.pending.insert(p.request.goal_id.clone());
            v.acknowledge_checked_conversations(cx);
            assert!(!v.discussion_send_enabled());
            assert!(v.discussion_send.entries[0].terminal.is_none());
            v.separate_discussion(window, cx);
            assert!(v.discussion_send_enabled());
            assert_eq!(v.compose.read(cx).value().as_ref(), "");
            assert_eq!(v.discussion_send.entries[0].pending, p);
            v.capabilities["discussion_send_recovery"] = json!(false);
            assert!(
                !v.discussion_send_enabled(),
                "No legacy fallback for unresolved correlated operation"
            );
            let before = v.request_generation;
            v.recover_discussion(p.clone(), window, cx);
            assert_eq!(before, v.request_generation);
            assert!(v.discussion_send.active.is_empty());
        });
    }
    #[gpui::test]
    fn discussion_unknown_owner_and_late_receipts_do_not_select_or_clear_other_draft(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = setup(cx);
        let p = pending();
        view.update_in(cx, |v, window, cx| {
            install(v, &p, window, cx);
            let other = uuid();
            v.snapshot["goal"]["id"] = json!(other);
            v.selected_goal_id = Some(other.clone());
            v.compose
                .update(cx, |i, cx| i.set_value("Other goal draft", window, cx));
            let selected = uuid();
            v.conversation_id = Some(selected.clone());
            v.discussion_send
                .active
                .insert(p.request.operation_id.clone());
            v.finish_discussion_reply(
                p.clone(),
                (true, v.request_generation),
                Ok((projected(&p), "projected")),
                window,
                cx,
            );
            assert_eq!(v.goal_id(), other);
            assert_eq!(v.conversation_id.as_deref(), Some(selected.as_str()));
            assert_eq!(v.compose.read(cx).value().as_ref(), "Other goal draft");
            assert!(v.discussion_send.entries[0].terminal.is_some());
            assert!(!v.discussion_guarded());
            install(v, &p, window, cx);
            let original = v.conversation_id.clone();
            v.finish_discussion_reply(
                p.clone(),
                (false, v.request_generation),
                Ok((projected(&p), "projected")),
                window,
                cx,
            );
            assert_eq!(v.conversation_id, original, "Lookup never navigates");
            assert_eq!(v.compose.read(cx).value().as_ref(), p.draft);
        });
    }
    #[gpui::test]
    fn discussion_restore_existing_target_clears_other_transcript_and_blocks_stale_read(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = setup(cx);
        let mut p = pending();
        p.request.conversation_id = Some(uuid());
        p.request.request_sha256 = p.request.digest(&text(&p.workspace["brain_id"]));
        view.update_in(cx, |v, window, cx| {
            install(v, &p, window, cx);
            let other = uuid();
            v.conversation_id = Some(other.clone());
            v.conversation =
                json!({"id":other,"status":"complete","messages":[{"text":"Other transcript"}]});
            v.discussion_send.restore = Some(p.request.operation_id.clone());
            v.busy = true;
            v.restore_discussion_draft(window, cx);
            assert!(v.conversation.is_null());
            assert_eq!(v.conversation_id, p.request.conversation_id);
            assert_eq!(v.compose.read(cx).value().as_ref(), p.draft);
            let generation = v.request_generation;
            v.finish_batch(
                vec![(
                    "chat_get".into(),
                    Ok(json!({"id":other,"messages":[{"text":"Stale transcript"}]})),
                )],
                false,
                generation,
                window,
                cx,
            );
            assert!(v.conversation.is_null());
            assert_eq!(v.conversation_id, p.request.conversation_id);
        });
    }
    #[gpui::test]
    fn discussion_restart_entries_never_restore_or_rebind_without_explicit_navigation(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = setup(cx);
        let p = pending();
        view.update_in(cx, |v, window, cx| {
            install(v, &p, window, cx);
            let original = p.workspace.clone();
            v.expected_workspace.as_mut().unwrap()["root"] = json!("/different/root");
            let before = v.snapshot.clone();
            v.compose.update(cx, |i, cx| {
                i.set_value("Current workspace draft", window, cx)
            });
            v.ensure_discussion_send();
            assert_eq!(v.snapshot, before);
            assert_eq!(
                v.compose.read(cx).value().as_ref(),
                "Current workspace draft"
            );
            assert_eq!(v.discussion_send.entries[0].pending.workspace, original);
            assert!(!v.discussion_guarded());
            let mut second = p.clone();
            second.workspace["root"] = json!("/different/root");
            assert_ne!(owner_key(&p), owner_key(&second));
            v.discussion_send.separate.insert(owner_key(&p));
            v.discussion_send.entries.push(Entry {
                pending: second,
                terminal: None,
            });
            assert!(v.discussion_guarded());
        });
    }
    #[gpui::test]
    fn discussion_actual_send_is_persisted_before_wire_and_restart_recovery_is_get_only(
        cx: &mut TestAppContext,
    ) {
        use std::net::TcpListener;
        let (view, cx) = setup(cx);
        cx.run_until_parked();
        let p = pending();
        let root = std::env::temp_dir().join(format!("discussion-native-{}", uuid()));
        let journal = Journal::at(root.clone());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        let observed = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let observed_thread = observed.clone();
        let inspect = journal.clone();
        let server = std::thread::spawn(move || {
            for turn in 0..3 {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                observed_thread.lock().unwrap().push(request.clone());
                let retained = inspect.entries().unwrap();
                assert!(retained
                    .iter()
                    .any(|e| e.pending.request.operation_id == request["operation_id"]));
                if turn == 0 {
                    assert_eq!(request["op"], "chat_send");
                    continue;
                } // Lost ACK.
                if turn == 1 {
                    assert_eq!(request["op"], "chat_send_get");
                    assert!(request.get("message").is_none());
                } else {
                    assert_eq!(request["op"], "chat_send");
                }
                let data = json!({"schema":"tessera-discussion-send-result/v1","operation_id":request["operation_id"],"brain_id":request["expected_workspace"]["brain_id"],"goal_id":request["goal_id"],"actor_id":request["expected_actor_id"],"request_sha256":request["request_sha256"],"status":"unknown","record":null,"source_receipt":null});
                writeln!(
                    reader.get_mut(),
                    "{}",
                    json!({"schema":request["schema"],"id":request["id"],"ok":true,"data":data})
                )
                .unwrap();
            }
        });
        view.update_in(cx, |v, window, cx| {
            install(v, &p, window, cx);
            v.discussion_send.entries.clear();
            v.discussion_send.journal = Some(journal.clone());
            v.endpoint = endpoint;
            v.send_message(window, cx);
        });
        cx.run_until_parked();
        let retained = journal.entries().unwrap()[0].pending.clone();
        assert!(journal.entries().unwrap()[0].terminal.is_none());
        view.update_in(cx, |v, window, cx| {
            assert!(!v.discussion_send_enabled());
            let draft = v.compose.read(cx).value().to_string();
            v.discussion_send = DiscussionSendUi::default();
            v.discussion_send.journal = Some(journal.clone());
            v.ensure_discussion_send();
            assert_eq!(v.compose.read(cx).value().as_ref(), draft);
            assert!(!v.discussion_send_enabled());
            v.capabilities["actor"] = json!("changed-local-actor");
            v.recover_discussion(retained.clone(), window, cx);
        });
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            assert!(v.discussion_send.entries[0].terminal.is_none());
            v.separate_discussion(window, cx);
            v.compose.update(cx, |i, cx| {
                i.set_value("A deliberately separate message", window, cx)
            });
            v.send_message(window, cx);
        });
        cx.run_until_parked();
        server.join().unwrap();
        let wire = observed.lock().unwrap();
        assert_eq!(wire.len(), 3);
        assert_eq!(wire[0]["operation_id"], wire[1]["operation_id"]);
        assert_eq!(wire[1]["expected_actor_id"], "operator");
        assert_ne!(wire[0]["operation_id"], wire[2]["operation_id"]);
        assert_eq!(wire[2]["expected_actor_id"], "changed-local-actor");
        assert_eq!(journal.entries().unwrap().len(), 2);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[gpui::test]
    fn discussion_failed_local_publication_sends_nothing_and_keeps_guard(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.run_until_parked();
        let p = pending();
        let root = std::env::temp_dir().join(format!("discussion-failed-publish-{}", uuid()));
        std::fs::write(&root, b"not a directory").unwrap();
        view.update_in(cx, |v, window, cx| {
            install(v, &p, window, cx);
            v.discussion_send.entries.clear();
            v.discussion_send.journal = Some(Journal::at(root.clone()));
            v.send_message(window, cx);
            assert!(v.discussion_send.active.is_empty());
            assert_eq!(v.discussion_send.entries.len(), 1);
            assert!(!v.discussion_send_enabled());
            assert_eq!(v.compose.read(cx).value().as_ref(), p.draft);
        });
        cx.run_until_parked();
        std::fs::remove_file(root).unwrap();
    }
    #[gpui::test]
    fn discussion_publication_then_failure_reopens_lookup_only_without_first_dispatch(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = setup(cx);
        cx.run_until_parked();
        let p = pending();
        let root = std::env::temp_dir().join(format!("discussion-after-publish-{}", uuid()));
        let journal = Journal::at(root.clone());
        view.update_in(cx, |v, window, cx| {
            install(v, &p, window, cx);
            v.discussion_send.entries.clear();
            v.discussion_send.journal = Some(journal.clone().failing_after_publish());
            v.send_message(window, cx);
            assert!(v.discussion_send.active.is_empty());
            assert!(!v.discussion_send_enabled());
        });
        cx.run_until_parked();
        let retained = journal.entries().unwrap();
        assert_eq!(retained.len(), 1);
        assert!(retained[0].terminal.is_none());
        assert_eq!(retained[0].pending.request.lookup()["op"], "chat_send_get");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[gpui::test]
    fn discussion_terminal_publication_failure_blocks_until_exact_lookup_receipt_is_saved(
        cx: &mut TestAppContext,
    ) {
        use std::net::TcpListener;
        let (view, cx) = setup(cx);
        cx.run_until_parked();
        let p = pending();
        let root = std::env::temp_dir().join(format!("discussion-terminal-failure-{}", uuid()));
        let journal = Journal::at(root.clone());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        let inspect = journal.clone();
        let server = std::thread::spawn(move || {
            let mut first = None;
            for op in ["chat_send", "chat_send_get"] {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["op"], op);
                let path = std::fs::read_dir(inspect.root_for_test())
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .find(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
                    .unwrap();
                let pending: Pending =
                    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
                let data = first.get_or_insert_with(|| projected(&pending));
                writeln!(
                    reader.get_mut(),
                    "{}",
                    json!({"schema":request["schema"],"id":request["id"],"ok":true,"data":data})
                )
                .unwrap();
            }
        });
        view.update_in(cx, |v, window, cx| {
            install(v, &p, window, cx);
            v.discussion_send.entries.clear();
            v.discussion_send.journal = Some(journal.clone());
            v.endpoint = endpoint;
            v.send_message(window, cx);
        });
        let retained = journal.entries().unwrap()[0].pending.clone();
        let done = root.join(format!("{}.done", retained.request.operation_id));
        std::fs::create_dir(&done).unwrap();
        cx.run_until_parked();
        view.update_in(cx, |v, _, cx| {
            assert!(!v.discussion_send_enabled());
            assert!(v.discussion_send.entries[0].terminal.is_none());
            assert_eq!(v.compose.read(cx).value().as_ref(), p.draft);
        });
        std::fs::remove_dir(done).unwrap();
        view.update_in(cx, |v, window, cx| {
            v.recover_discussion(retained.clone(), window, cx)
        });
        cx.run_until_parked();
        server.join().unwrap();
        assert!(journal.entries().unwrap()[0].terminal.is_some());
        view.update_in(cx, |v, _, cx| {
            assert!(v.conversation_id.is_none());
            assert_eq!(v.compose.read(cx).value().as_ref(), p.draft);
        });
        std::fs::remove_dir_all(root).unwrap();
    }
    #[gpui::test]
    fn discussion_fresh_owner_view_keeps_full_startup_batch_before_restoring(
        cx: &mut TestAppContext,
    ) {
        use std::net::TcpListener;
        cx.update(gpui_component::init);
        let p = pending();
        let root = std::env::temp_dir().join(format!("discussion-startup-{}", uuid()));
        let journal = Journal::at(root.clone());
        journal.retain(&p).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        let expected = p.clone();
        let server = std::thread::spawn(move || {
            for op in ["capabilities", "snapshot", "source_list"] {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["op"], op);
                let data = match op {
                    "capabilities" => {
                        json!({"chat":true,"discussion_send_recovery":true,"workspace_guard":true,"workspace":expected.workspace,"actor":"operator"})
                    }
                    "snapshot" => {
                        assert_eq!(request["goal_id"], expected.request.goal_id);
                        json!({"goal":{"id":expected.request.goal_id},"goals":[{"id":expected.request.goal_id}],"conversations":[],"selected_source_paths":[],"selected_conversation_id":null})
                    }
                    _ => json!({"sources":[{"path":"startup-proof.md"}]}),
                };
                writeln!(
                    reader.get_mut(),
                    "{}",
                    json!({"schema":request["schema"],"id":request["id"],"ok":true,"data":data})
                )
                .unwrap();
            }
        });
        let (view, visual) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new_guarded(endpoint, Some(p.workspace.clone()), window, cx);
            v.discussion_send.journal = Some(journal.clone());
            v.queue_retained_discussion(p.request.operation_id.clone());
            assert!(
                !v.busy,
                "Queueing an explicit owner must not suppress startup"
            );
            v
        });
        visual.run_until_parked();
        server.join().unwrap();
        view.update_in(visual, |v, _, cx| {
            assert!(v.correlated_capability());
            assert_eq!(v.goal_id(), p.request.goal_id);
            assert_eq!(v.compose.read(cx).value().as_ref(), p.draft);
            assert_eq!(v.source_list[0]["path"], "startup-proof.md");
            assert!(v.discussion_send.restore.is_none());
            assert!(!v.busy);
        });
        std::fs::remove_dir_all(root).unwrap();
    }
    #[gpui::test]
    fn discussion_nonterminal_lookup_cannot_release_unconfirmed_terminal_publication(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = setup(cx);
        cx.run_until_parked();
        let p = pending();
        let root = std::env::temp_dir().join(format!("discussion-terminal-visible-{}", uuid()));
        let journal = Journal::at(root.clone());
        journal.retain(&p).unwrap();
        let projected = projected(&p);
        // The terminal file is visible but the publication call reported failure.
        let failure = journal
            .clone()
            .failing_after_terminal_publish()
            .acknowledge(&p, &projected, true)
            .unwrap_err();
        assert!(root
            .join(format!("{}.done", p.request.operation_id))
            .exists());
        view.update_in(cx, |v, window, cx| {
            install(v, &p, window, cx);
            v.discussion_send.journal = Some(journal.clone());
            v.finish_discussion_reply(
                p.clone(),
                (true, v.request_generation),
                Err(failure),
                window,
                cx,
            );
            for status in ["unknown", "pending_projection", "projection_conflict"] {
                let mut data = projected.clone();
                data["status"] = json!(status);
                data["source_receipt"] = Value::Null;
                if status == "unknown" {
                    data["record"] = Value::Null;
                }
                journal.acknowledge(&p, &data, false).unwrap();
                v.finish_discussion_reply(
                    p.clone(),
                    (false, v.request_generation),
                    Ok((data, status)),
                    window,
                    cx,
                );
                v.ensure_discussion_send();
                assert!(
                    v.discussion_send
                        .terminal_unconfirmed
                        .contains(&p.request.operation_id),
                    "{status} cannot prove terminal durability"
                );
                assert!(v.discussion_send.entries[0].terminal.is_none());
                assert!(!v.discussion_send_enabled());
                assert_eq!(v.compose.read(cx).value().as_ref(), p.draft);
            }
            journal.acknowledge(&p, &projected, false).unwrap();
            v.finish_discussion_reply(
                p.clone(),
                (false, v.request_generation),
                Ok((projected, "projected")),
                window,
                cx,
            );
            v.ensure_discussion_send();
            assert!(v.discussion_send.entries[0].terminal.is_some());
            assert!(!v
                .discussion_send
                .terminal_unconfirmed
                .contains(&p.request.operation_id));
        });
        std::fs::remove_dir_all(root).unwrap();
    }
    #[gpui::test]
    fn discussion_new_view_confirms_visible_terminal_durability_before_releasing_guard(
        cx: &mut TestAppContext,
    ) {
        let p = pending();
        let root = std::env::temp_dir().join(format!("discussion-new-view-durable-{}", uuid()));
        let journal = Journal::at(root.clone());
        journal.retain(&p).unwrap();
        let data = projected(&p);
        assert!(journal
            .failing_after_terminal_publish()
            .acknowledge(&p, &data, true)
            .is_err());
        let original_bytes =
            std::fs::read(root.join(format!("{}.json", p.request.operation_id))).unwrap();
        let terminal_bytes =
            std::fs::read(root.join(format!("{}.done", p.request.operation_id))).unwrap();
        let (view, cx) = setup(cx);
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            install(v, &p, window, cx);
            v.discussion_send.entries.clear();
            assert!(
                v.discussion_send.terminal_unconfirmed.is_empty(),
                "A fresh view has no earlier memory guard"
            );
            v.discussion_send.journal =
                Some(Journal::at(root.clone()).failing_terminal_confirmation());
            v.ensure_discussion_send();
            assert!(v.discussion_send.load_failed);
            assert!(!v.discussion_send_enabled());
            assert!(v.discussion_send.active.is_empty());
            assert_eq!(v.compose.read(cx).value().as_ref(), p.draft);
            v.discussion_send.journal = Some(Journal::at(root.clone()));
            v.ensure_discussion_send();
            assert!(!v.discussion_send.load_failed);
            assert!(v.discussion_send.entries[0].terminal.is_some());
            assert!(v.discussion_send.active.is_empty());
        });
        assert_eq!(
            std::fs::read(root.join(format!("{}.json", p.request.operation_id))).unwrap(),
            original_bytes
        );
        assert_eq!(
            std::fs::read(root.join(format!("{}.done", p.request.operation_id))).unwrap(),
            terminal_bytes
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
