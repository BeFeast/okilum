//! Action drafts invalidate this entity, never the virtualized history.
use super::*;
use okilum_t3::actions::{DispatchMode, Intent, Journal, Record, State};
use serde_json::json;
#[cfg(test)]
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

/// A request row observes only action state; editing an answer never rebuilds
/// the virtualized feed. The source row and live authority must both match.
pub(in crate::project) struct RequestCard {
    actions: Entity<Actions>,
    item: Arc<serde_json::Value>,
    local: bool,
    _actions: Subscription,
}
impl RequestCard {
    pub(in crate::project) fn new(
        actions: Entity<Actions>,
        item: Arc<serde_json::Value>,
        local: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let observation = cx.observe(&actions, |_, _, cx| cx.notify());
        Self {
            actions,
            item,
            local,
            _actions: observation,
        }
    }
    pub(in crate::project) fn set_item(
        &mut self,
        item: Arc<serde_json::Value>,
        local: bool,
        cx: &mut Context<Self>,
    ) {
        if self.item != item || self.local != local {
            self.item = item;
            self.local = local;
            cx.notify();
        }
    }
}
impl Render for RequestCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.item["requestId"].as_str().unwrap_or("");
        let pending = self.local
            && self
                .actions
                .read(cx)
                .state
                .requests
                .iter()
                .any(|r| r.id == id && r.item["id"] == self.item["id"]);
        let mut row = v_flex().w_full().min_w_0().gap_1();
        if pending {
            row = row.child(
                self.actions
                    .update(cx, |actions, cx| actions.render_request(id, &self.item, cx)),
            );
        } else {
            if self.item["type"] == "approval_request" {
                for field in ["prompt", "reason", "decision"] {
                    if let Some(text) = self.item[field].as_str() {
                        row = row.child(div().whitespace_normal().child(text.to_owned()));
                    }
                }
            } else if let Some(questions) = self.item["questions"].as_array() {
                for question in questions {
                    row = row.child(
                        div()
                            .whitespace_normal()
                            .child(question["question"].as_str().unwrap_or("").to_owned()),
                    );
                    if let Some(options) = question["options"].as_array() {
                        row = row.child(
                            div().whitespace_normal().child(
                                options
                                    .iter()
                                    .filter_map(|o| o["label"].as_str())
                                    .collect::<Vec<_>>()
                                    .join(" · "),
                            ),
                        );
                    }
                }
                if let Some(answers) = self.item["questionAnswer"]["answers"].as_object() {
                    for answer in answers.values() {
                        let text = match answer {
                            serde_json::Value::String(text) => text.clone(),
                            serde_json::Value::Array(values) => values
                                .iter()
                                .filter_map(|v| v.as_str())
                                .collect::<Vec<_>>()
                                .join(", "),
                            _ => answer.to_string(),
                        };
                        row = row.child(div().whitespace_normal().child(format!("Answer: {text}")));
                    }
                }
            }
            row = row.child(div().text_color(brand::palette(cx).text_muted).child(
                if self.local && self.item["status"] == "pending" {
                    "Waiting for supported response controls.".to_owned()
                } else {
                    self.item["status"].as_str().unwrap_or("Request").to_owned()
                },
            ));
        }
        row
    }
}

pub(crate) struct Actions {
    parent: WeakEntity<ProjectView>,
    pub(crate) state: State,
    records: Vec<Record>,
    inputs: HashMap<(String, String), Entity<InputState>>,
    selections: HashMap<(String, String), Vec<String>>,
    subscriptions: HashMap<(String, String), Subscription>,
    busy: bool,
    enabled: bool,
    revision: u64,
    error: Option<String>,
}
impl Actions {
    pub(super) fn new(parent: WeakEntity<ProjectView>) -> Self {
        Self {
            parent,
            state: State::default(),
            records: Vec::new(),
            inputs: HashMap::new(),
            selections: HashMap::new(),
            subscriptions: HashMap::new(),
            busy: false,
            enabled: false,
            revision: 0,
            error: None,
        }
    }
    pub(super) fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.enabled != enabled {
            self.enabled = enabled;
            cx.notify();
        }
    }
    pub(super) fn set_state(&mut self, state: State, window: &mut Window, cx: &mut Context<Self>) {
        if self.state == state {
            return;
        }
        let mut keep = std::collections::HashSet::new();
        for request in &state.requests {
            let Some(questions) = request.item["questions"].as_array() else {
                continue;
            };
            for question in questions {
                let Some(id) = question["id"].as_str() else {
                    continue;
                };
                let key = (request.id.clone(), id.to_owned());
                keep.insert(key.clone());
                if self.inputs.contains_key(&key) {
                    continue;
                }
                let mut initial = question["initialAnswer"].as_str().unwrap_or("").to_owned();
                if question["options"].as_array().is_some_and(|options| {
                    options.iter().any(|o| {
                        o["value"].as_str().or(o["label"].as_str()) == Some(initial.as_str())
                    })
                }) {
                    self.selections.insert(key.clone(), vec![initial.clone()]);
                    initial.clear();
                }
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder("Your answer")
                        .default_value(initial)
                });
                self.subscriptions.insert(
                    key.clone(),
                    cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify()),
                );
                self.inputs.insert(key, input);
            }
        }
        // An unsynchronized reconnect cannot resolve a user's pending draft.
        if state.ready {
            self.inputs.retain(|key, _| keep.contains(key));
            self.selections.retain(|key, _| keep.contains(key));
            self.subscriptions.retain(|key, _| keep.contains(key));
        }
        self.state = state;
        cx.notify();
    }
    fn request_retained(&self, id: &str) -> bool {
        self.records.iter().any(|r| matches!(&r.command.intent, Intent::Respond { request_id, .. } | Intent::Dismiss { request_id } if request_id == id))
    }
    fn retry_allowed(&self, record: &Record) -> bool {
        record.sequence.is_some()
            || (self.state.ready && matches!(record.command.intent, Intent::Message { .. }))
            || self.state.allows(&record.command.intent)
    }
    fn intent(&self, request: &str, cx: &App) -> Intent {
        let mut answers = BTreeMap::new();
        if let Some(questions) = self
            .state
            .requests
            .iter()
            .find(|r| r.id == request)
            .and_then(|r| r.item["questions"].as_array())
        {
            for question in questions {
                let Some(id) = question["id"].as_str() else {
                    continue;
                };
                let key = (request.to_owned(), id.to_owned());
                let custom = self
                    .inputs
                    .get(&key)
                    .map(|i| i.read(cx).value().to_string())
                    .unwrap_or_default();
                if question["allowCustomAnswer"] != false && !custom.trim().is_empty() {
                    answers.insert(id.to_owned(), json!(custom));
                } else if let Some(selected) = self.selections.get(&key).filter(|s| !s.is_empty()) {
                    answers.insert(
                        id.to_owned(),
                        if question["multiSelect"] == true {
                            if self.state.requests.iter().any(|r| {
                                r.id == request && r.response_mode.as_deref() == Some("message")
                            }) {
                                json!(serde_json::to_string(selected).expect("choices serialize"))
                            } else {
                                json!(selected)
                            }
                        } else {
                            json!(selected[0])
                        },
                    );
                }
            }
        }
        Intent::Respond {
            request_id: request.to_owned(),
            decision: None,
            answers: Some(answers),
        }
    }
    fn dispatch(&self, intent: Intent, window: &mut Window, cx: &mut Context<Self>) {
        let parent = self.parent.clone();
        // Release the request entity before the parent reads its live authority
        // and marks the operation busy. A button callback otherwise re-borrows
        // the Actions entity through native_command.
        window.defer(cx, move |window, cx| {
            let _ = parent.update(cx, |view, cx| {
                view.native_command(Some(intent), None, None, window, cx)
            });
        });
    }
}
impl Actions {
    pub(super) fn render_request(
        &mut self,
        id: &str,
        item: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(request) = self
            .state
            .requests
            .iter()
            .find(|r| r.id == id && r.item["id"] == item["id"])
            .cloned()
        else {
            return div().into_any_element();
        };
        let request_id = request.id.clone();
        let retained = self.request_retained(&request_id);
        let disabled =
            self.busy || !self.enabled || !self.state.ready || !request.answerable || retained;
        let mut card = v_flex()
            .w_full()
            .min_w_0()
            .gap_1()
            .id(SharedString::from(format!("project-request-{request_id}")))
            .debug_selector({
                let id = request_id.clone();
                move || format!("project-request-{id}")
            });
        if request.item["type"] == "approval_request" {
            let label = request.item["prompt"]
                .as_str()
                .or(request.item["reason"].as_str())
                .unwrap_or("Approval requested")
                .to_owned();
            card = card.child(div().whitespace_normal().child(label));
            if let Some(reason) = request.item["reason"]
                .as_str()
                .filter(|r| Some(*r) != request.item["prompt"].as_str())
            {
                card = card.child(div().whitespace_normal().child(reason.to_owned()));
            }
            if let Some(app) = request.item["appName"].as_str() {
                card = card.child(div().whitespace_normal().child(app.to_owned()));
            }
            if let Some(reason) = request.item["fullAccessOverride"].as_str() {
                card = card.child(match reason {
                    "ask_rule" => "Your approval rule requires this decision.",
                    "provider_check" => "The provider requires this decision.",
                    _ => "This action requires an additional approval.",
                });
            }
            let options = request.item["options"]
                .as_array()
                .cloned()
                .unwrap_or_else(|| {
                    vec![
                        json!({"decision":"accept","label":"Allow once"}),
                        json!({"decision":"decline","label":"Decline"}),
                    ]
                });
            let mut buttons = h_flex().items_start().flex_wrap().gap_1();
            for (n, option) in options.iter().enumerate() {
                let Some(decision) = option["decision"].as_str() else {
                    continue;
                };
                let intent = Intent::Respond {
                    request_id: request_id.clone(),
                    decision: Some(decision.into()),
                    answers: None,
                };
                let allowed = self.state.allows(&intent);
                let mut decision_group = v_flex().min_w_0().gap_1().child(
                    Button::new(SharedString::from(format!("approval-{request_id}-{n}")))
                        .ghost()
                        .label(option["label"].as_str().unwrap_or(decision).to_owned())
                        .disabled(disabled || !allowed)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.dispatch(intent.clone(), window, cx)
                        })),
                );
                if let Some(warning) = option["warning"].as_str() {
                    decision_group =
                        decision_group.child(div().whitespace_normal().child(warning.to_owned()));
                }
                buttons = buttons.child(decision_group);
            }
            card = card.child(buttons);
        } else if let Some(questions) = request.item["questions"].as_array() {
            for question in questions {
                let Some(id) = question["id"].as_str() else {
                    continue;
                };
                card = card.child(
                    div()
                        .whitespace_normal()
                        .child(question["question"].as_str().unwrap_or("").to_owned()),
                );
                let key = (request_id.clone(), id.to_owned());
                if let Some(options) = question["options"].as_array() {
                    let mut buttons = h_flex().flex_wrap().gap_1();
                    for (n, option) in options.iter().enumerate() {
                        let Some(value) = option["value"].as_str().or(option["label"].as_str())
                        else {
                            continue;
                        };
                        let key = key.clone();
                        let value = value.to_owned();
                        let multi = question["multiSelect"] == true;
                        let selected = self
                            .selections
                            .get(&key)
                            .is_some_and(|s| s.contains(&value));
                        buttons = buttons.child(
                            Button::new(SharedString::from(format!(
                                "answer-{}-{}-{n}",
                                request_id, id
                            )))
                            .ghost()
                            .label(format!(
                                "{}{}",
                                if selected { "✓ " } else { "" },
                                option["label"].as_str().unwrap_or(&value)
                            ))
                            .tooltip(option["description"].as_str().unwrap_or("").to_owned())
                            .disabled(disabled)
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    let choices = this.selections.entry(key.clone()).or_default();
                                    if choices.contains(&value) {
                                        choices.retain(|x| x != &value);
                                    } else {
                                        if !multi {
                                            choices.clear();
                                        }
                                        choices.push(value.clone());
                                    }
                                    if let Some(input) = this.inputs.get(&key) {
                                        input.update(cx, |input, cx| {
                                            input.set_value("", window, cx)
                                        });
                                    }
                                    cx.notify();
                                },
                            )),
                        );
                    }
                    card = card.child(buttons);
                }
                if question["allowCustomAnswer"] != false {
                    if let Some(input) = self.inputs.get(&key) {
                        card = card.child(Input::new(input).disabled(disabled).w_full());
                    }
                }
            }
            let intent = self.intent(&request_id, cx);
            card = card.child(
                Button::new(SharedString::from(format!("respond-{request_id}")))
                    .ghost()
                    .label("Send answers")
                    .disabled(disabled || !self.state.allows(&intent))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.dispatch(intent.clone(), window, cx)
                    })),
            );
            if request.dismissible {
                let id = request_id.clone();
                card = card.child(
                    Button::new(SharedString::from(format!("dismiss-{request_id}")))
                        .ghost()
                        .label("Dismiss")
                        .disabled(disabled)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.dispatch(
                                Intent::Dismiss {
                                    request_id: id.clone(),
                                },
                                window,
                                cx,
                            )
                        })),
                );
            }
        }
        if retained {
            card = card.child("Answer retained. Waiting for confirmation in the conversation.");
        } else if !request.answerable {
            card = card.child(
                request
                    .reason
                    .clone()
                    .unwrap_or_else(|| "This request cannot be answered in this session.".into()),
            );
        }
        card.into_any_element()
    }
}
impl Render for Actions {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut panel = v_flex()
            .id("project-native-actions")
            .w_full()
            .min_w_0()
            .max_h(px(280.))
            .overflow_y_scroll()
            .gap_2();
        if let Some(run_id) = &self.state.active_run {
            let run_id = run_id.clone();
            panel = panel.child(
                Button::new("project-interrupt")
                    .ghost()
                    .label("Stop")
                    .debug_selector(|| "project-interrupt".into())
                    .disabled(self.busy || !self.enabled || !self.state.ready || self.records.iter().any(|r| matches!(&r.command.intent, Intent::Interrupt { run_id: id } if id == &run_id)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.dispatch(
                            Intent::Interrupt {
                                run_id: run_id.clone(),
                            },
                            window,
                            cx,
                        )
                    })),
            );
        }
        for record in self.records.iter().filter(|r| r.sequence.is_none()) {
            let retry = record.clone();
            let parent = self.parent.clone();
            let mut row = v_flex().w_full().min_w_0().gap_1().child(format!(
                "{} — delivery unconfirmed",
                record.command.intent.label()
            ));
            if let Intent::Message { text, .. } = &record.command.intent {
                row = row.child(div().whitespace_normal().child(text.clone()));
            }
            panel = panel.child(
                row.child(
                    Button::new(SharedString::from(format!(
                        "retry-{}",
                        record.command.command_id
                    )))
                    .ghost()
                    .label("Retry same action")
                    .disabled(
                        self.busy
                            || !self.enabled
                            || !self.state.ready
                            || !self.retry_allowed(record),
                    )
                    .on_click(move |_, window, cx| {
                        let _ = parent.update(cx, |view, cx| {
                            view.native_command(None, Some(retry.clone()), None, window, cx)
                        });
                    }),
                ),
            );
        }
        if let Some(error) = &self.error {
            panel = panel.child(
                div()
                    .whitespace_normal()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        panel
    }
}
impl ProjectView {
    pub(crate) fn native_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || !self.can_send() {
            return;
        }
        let draft = self.compose.read(cx).value().to_string();
        if let Some(error) = okilum_t3::actions::prompt_length_validation(&draft) {
            self.error = Some(error);
            cx.notify();
            return;
        }
        let attachments = match self.prepared_attachments() {
            Ok(attachments) => attachments,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        let state = &self.native.actions.read(cx).state;
        let mode = match self.composer.read(cx).mode {
            super::super::composer::Mode::Steer => match &state.active_run {
                Some(run) => DispatchMode::SteerActive {
                    target_run_id: run.clone(),
                },
                None => return,
            },
            super::super::composer::Mode::Queue => DispatchMode::QueueAfterActive,
            super::super::composer::Mode::Auto if state.active_run.is_some() => {
                DispatchMode::QueueAfterActive
            }
            _ => DispatchMode::StartImmediately,
        };
        self.native_command(
            Some(Intent::message_with_attachments(
                draft.clone(),
                mode,
                attachments,
            )),
            None,
            Some(draft),
            window,
            cx,
        );
    }
    pub(super) fn native_command(
        &mut self,
        intent: Option<Intent>,
        retry: Option<Record>,
        draft: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.active
            || !reader_features::enabled(reader_features::Feature::Agents, cx)
            || self.busy
            || !self.native.active()
            || self.native.deleted()
            || !self.native.stream_error_is_clear()
            || !self.native.actions.read(cx).state.ready
        {
            return;
        }
        let Some(client) = self.native.client.clone().filter(|c| c.can_operate()) else {
            return;
        };
        if intent
            .as_ref()
            .is_some_and(|i| !self.native.actions.read(cx).state.allows(i))
            || retry
                .as_ref()
                .is_some_and(|r| !self.native.actions.read(cx).retry_allowed(r))
        {
            return;
        }
        let Some(journal) = self.native_journal() else {
            return;
        };
        self.busy = true;
        self.native.actions.update(cx, |a, cx| {
            a.busy = true;
            a.revision = a.revision.wrapping_add(1);
            a.error = None;
            cx.notify();
        });
        let sent_snapshot = draft.as_ref().map(|_| self.draft_sent(cx));
        let generation = self.generation;
        cx.spawn_in(window, async move |this,cx| {
            let (result, records, retained) = cx.background_executor().spawn(async move {
                let mut retained = false;
                let result = (|| {
                    let record = match retry { Some(r) => r, None => journal.prepare(intent.context("missing action")?)? };
                    retained = true;
                    journal.send(&record, |command| client.dispatch(command))
                })();
                (result, journal.records(), retained)
            }).await;
            let _ = this.update_in(cx, |this,window,cx| {
                if generation != this.generation { return; }
                this.busy = false;
                if retained { if let Some(sent) = &sent_snapshot { this.draft_clear_after_send(sent,window,cx); } }
                this.native.actions.update(cx, |a,cx| {
                    a.busy=false;
                    match records { Ok(records) => a.records=records, Err(e) => a.error=Some(format!("{e:#}")) }
                    if let Err(e) = result { a.error=Some(format!("{e:#}. Delivery may be unconfirmed; retry the retained action or check in T3.")); }
                    cx.notify();
                });
                this.feed.update(cx, |feed, cx| feed.requests_changed(cx));
                cx.notify();
            });
        }).detach();
        cx.notify();
    }
    fn native_journal(&self) -> Option<Journal> {
        Some(Journal::new(
            self.state.as_ref()?,
            self.environment.as_ref()?,
            &self.chat.as_ref()?.thread_id,
        ))
    }
    pub(super) fn native_load_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(journal) = self.native_journal() else {
            return;
        };
        let generation = self.generation;
        let revision = self.native.actions.read(cx).revision;
        cx.spawn_in(window, async move |this, cx| {
            let records = cx
                .background_executor()
                .spawn(async move { journal.records() })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                if this.generation != generation {
                    return;
                }
                this.native.actions.update(cx, |a, cx| {
                    if a.revision != revision {
                        return;
                    }
                    match records {
                        Ok(r) => a.records = r,
                        Err(e) => a.error = Some(format!("{e:#}")),
                    }
                    cx.notify();
                });
                this.feed.update(cx, |feed, cx| feed.requests_changed(cx));
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        thread,
    };

    fn inert_native(view: &mut ProjectView) {
        let environment = Environment::new("fixture", "http://127.0.0.1:9").unwrap();
        let token = NativeToken::from_secret(&environment, &serde_json::to_vec(&json!({
            "origin":environment.origin(),"access_token":"fixture","expires_at":chrono::Utc::now().timestamp() as u64+3600,
            "scope":"orchestration:read orchestration:operate"
        })).unwrap()).unwrap();
        view.native.client = Some(Arc::new(
            NativeClient::new(environment.clone(), token, Duration::from_secs(1)).unwrap(),
        ));
        view.native.stream_ready = true;
        view.environment = Some(environment);
        view.threads.enabled = true;
    }

    #[gpui::test]
    fn composer_uses_native_start_queue_and_pinned_steer_without_mcp_fallback(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::test_init);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let environment = Environment::new(
            "fixture",
            &format!("http://{}", listener.local_addr().unwrap()),
        )
        .unwrap();
        let expected = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = expected.clone();
        let server = thread::spawn(move || {
            let accept = || {
                let started = std::time::Instant::now();
                loop {
                    if let Ok((s, _)) = listener.accept() {
                        s.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
                        return s;
                    }
                    assert!(
                        started.elapsed() < Duration::from_secs(8),
                        "native action never reached fixture"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
            };
            for sequence in 1..=3 {
                let mut stream = accept();
                let mut reader = BufReader::new(&mut stream);
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                }
                drop(reader);
                let body = json!({"ticket":"fixture"}).to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
                drop(stream);
                let mut ws = tungstenite::accept(accept()).unwrap();
                let wire: Value =
                    serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
                assert_eq!(wire["tag"], "orchestration.dispatchCommand");
                observed.lock().unwrap().push(wire["payload"].clone());
                ws.send(tungstenite::Message::Text(json!({"_tag":"Exit","requestId":"1","exit":{"_tag":"Success","value":{"sequence":sequence}}}).to_string().into())).unwrap();
            }
        });
        let token=NativeToken::from_secret(&environment,&serde_json::to_vec(&json!({"origin":environment.origin(),"access_token":"test","expires_at":chrono::Utc::now().timestamp() as u64+3600,"scope":"orchestration:read orchestration:operate"})).unwrap()).unwrap();
        let client = Arc::new(
            NativeClient::new(environment.clone(), token, Duration::from_secs(3)).unwrap(),
        );
        let temp = tempfile::tempdir().unwrap();
        let fake = Arc::new(super::super::super::fixture::Fake::default());
        let (root, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = super::super::super::fixture::view(
                    temp.path().to_owned(),
                    fake.clone(),
                    window,
                    cx,
                );
                view.active = false;
                view.environment = Some(environment.clone());
                view.native.client = Some(client);
                view.native.stream_ready = true; // This fixture supplies a synchronized action state.
                view.threads.enabled = true; // Do not start legacy MCP history behind WS actions.
                view.active = true;
                view.native.actions.update(cx, |a, cx| {
                    a.set_state(
                        State {
                            ready: true,
                            ..Default::default()
                        },
                        window,
                        cx,
                    )
                });
                view
            });
            Root::new(view, window, cx)
        });
        visual.simulate_resize(size(px(760.), px(1000.)));
        visual.run_until_parked();
        let view = root.read_with(visual, |r, _| {
            r.view().clone().downcast::<ProjectView>().unwrap()
        });
        for (index, mode) in [
            super::super::super::composer::Mode::Auto,
            super::super::super::composer::Mode::Queue,
            super::super::super::composer::Mode::Steer,
        ]
        .into_iter()
        .enumerate()
        {
            view.update_in(visual, |v, window, cx| {
                if index > 0 {
                    v.native.actions.update(cx, |a, cx| {
                        a.set_state(
                            State {
                                ready: true,
                                active_run: Some("exact-run".into()),
                                can_steer: true,
                                requests: vec![],
                            },
                            window,
                            cx,
                        )
                    });
                }
                v.composer.update(cx, |c, _| c.mode = mode);
                v.compose.update(cx, |i, cx| {
                    i.set_value(format!("message-{index}"), window, cx)
                });
                v.send(None, window, cx);
            });
            visual.run_until_parked();
            assert!(
                fake.sends.lock().unwrap().is_empty(),
                "NATIVE1787: native composer fell back to MCP"
            );
            view.read_with(visual, |v, cx| {
                assert!(!v.busy);
                assert!(v.compose.read(cx).value().is_empty());
                let panel = v.native.actions.read(cx);
                assert!(panel.error.is_none(), "{:?}", panel.error);
                assert_eq!(panel.records.len(), index + 1);
                assert!(panel.records.iter().all(|r| r.sequence.is_some()));
            });
        }
        server.join().unwrap();
        let wire = expected.lock().unwrap();
        assert_eq!(wire[0]["dispatchMode"]["type"], "start_immediately");
        assert_eq!(wire[1]["dispatchMode"]["type"], "queue_after_active");
        assert_eq!(
            wire[2]["dispatchMode"],
            json!({"type":"steer_active","targetRunId":"exact-run"})
        );
        assert_ne!(wire[0]["commandId"], wire[1]["commandId"]);
    }

    #[test]
    fn question_initial_values_and_capability_checks_never_auto_accept_approvals() {
        let state = State {
            ready: true,
            requests: vec![okilum_t3::actions::Request {
                id: "approval".into(),
                item: json!({"type":"approval_request","options":[{"decision":"decline","label":"Decline"}]}),
                answerable: true,
                dismissible: false,
                response_mode: Some("live".into()),
                reason: None,
            }],
            ..Default::default()
        };
        assert!(!state.allows(&Intent::Respond {
            request_id: "approval".into(),
            decision: Some("accept".into()),
            answers: None
        }));
        assert!(state.allows(&Intent::Respond {
            request_id: "approval".into(),
            decision: Some("decline".into()),
            answers: None
        }));
    }
    #[gpui::test]
    fn inline_question_keystrokes_keep_history_work_bounded_and_parent_silent(
        cx: &mut TestAppContext,
    ) {
        use std::{cell::Cell, rc::Rc};
        cx.update(|cx| {
            crate::test_init(cx);
            cx.set_reduce_motion(true);
        });
        let temp = tempfile::tempdir().unwrap();
        let (root,visual)=cx.add_window_view(|window,cx| {
            let view=cx.new(|cx| {
                let mut view=super::super::super::fixture::view(temp.path().to_owned(),Arc::new(super::super::super::fixture::Fake::default()),window,cx);
                view.active=false;
                inert_native(&mut view);
                let question = Arc::new(json!({"id":"request-row","threadId":"thread","ordinal":2,"status":"pending","type":"user_input_request","requestId":"r","questions":[{"id":"q","question":"Your choice","options":[]}]}));
                view.native.actions.update(cx,|a,cx| a.set_state(State{ready:true,requests:vec![okilum_t3::actions::Request{id:"r".into(),item:question.as_ref().clone(),answerable:true,dismissible:true,response_mode:Some("message".into()),reason:None}],..Default::default()},window,cx));
                // Native history must contain real unrelated messages: the
                // first native projection deliberately replaces any MCP rows.
                let mut rows:Vec<_>=(0..1000).map(|n| Arc::new(okilum_t3::projection::TurnRow {
                    key:okilum_t3::projection::RowKey{thread:"thread".into(),item:format!("history-{n}")},local:true,
                    item:Arc::new(json!({"id":format!("history-{n}"),"threadId":"thread","ordinal":n,"status":"completed","type":"assistant_message","text":format!("Retained message {n}: **history stays available**.")}))
                })).collect();
                rows.push(Arc::new(okilum_t3::projection::TurnRow{key:okilum_t3::projection::RowKey{thread:"thread".into(),item:"request-row".into()},local:true,item:question}));
                let order=rows.iter().map(|r|r.key.clone()).collect();
                view.feed.update(cx,|feed,cx|feed.apply_native(okilum_t3::projection::Changes { rows,order:Some(order),..Default::default()}, &[],cx));
                view
            });Root::new(view,window,cx)
        });
        visual.simulate_resize(size(px(760.), px(1000.)));
        visual.run_until_parked();
        let view = root.read_with(visual, |r, _| {
            r.view().clone().downcast::<ProjectView>().unwrap()
        });
        let (feed, actions, input) = view.read_with(visual, |v, cx| {
            let a = v.native.actions.clone();
            let input = a.read(cx).inputs[&("r".into(), "q".into())].clone();
            (v.feed.clone(), a, input)
        });
        input.update_in(visual, |i, window, cx| i.focus_handle(cx).focus(window, cx));
        visual.run_until_parked();
        visual.simulate_keystrokes("left");
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("project-request-r").is_some(),
            "positive control: inline request is visible"
        );
        assert!(
            visual.debug_bounds("project-message-1000").is_some(),
            "positive control: real unrelated native history is visible"
        );
        feed.read_with(visual, |f, _| f.rendered_positions.borrow_mut().clear());
        let counts = Rc::new([Cell::new(0), Cell::new(0), Cell::new(0)]);
        let _observer: Entity<()> = visual.update(|_, cx| {
            cx.new(|cx| {
                let c = counts.clone();
                cx.observe(&view, move |_, _, _| c[0].set(c[0].get() + 1))
                    .detach();
                let c = counts.clone();
                cx.observe(&feed, move |_, _, _| c[1].set(c[1].get() + 1))
                    .detach();
                let c = counts.clone();
                cx.observe(&actions, move |_, _, _| c[2].set(c[2].get() + 1))
                    .detach();
            })
        });
        visual.simulate_input("answer");
        visual.run_until_parked();
        assert_eq!(
            input.read_with(visual, |i, _| i.value().to_string()),
            "answer"
        );
        // GPUI marks rendered ancestors dirty when an inline Input changes.
        // Its list may repaint visible rows; it must not traverse all history
        // or emit model notifications/remeasure requests for each character.
        feed.read_with(visual, |f, _| {
            let positions = f.rendered_positions.borrow();
            let unique: std::collections::HashSet<_> = positions.iter().copied().collect();
            assert!(
                !unique.is_empty(),
                "positive control: interactive list repainted"
            );
            assert!(
                unique.len() <= 32,
                "typing visited {} distinct rows",
                unique.len()
            );
            assert!(
                unique.iter().all(|position| *position > 900),
                "typing rendered offscreen history: {unique:?}"
            );
        });
        assert_eq!(counts[0].get(), 0);
        assert_eq!(counts[1].get(), 0);
        assert!(
            counts[2].get() > 0,
            "positive control: action draft changed"
        );
    }
    #[gpui::test]
    fn answer_choices_keep_exact_values_and_message_mode_is_string_only(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let temp = tempfile::tempdir().unwrap();
        let (view, visual) = cx.add_window_view(|window, cx| {
            super::super::super::fixture::view(
                temp.path().to_owned(),
                Arc::new(super::super::super::fixture::Fake::default()),
                window,
                cx,
            )
        });
        view.update_in(visual, |view, window, cx| {
            let item = json!({"id":"row","type":"user_input_request","questions":[
                {"id":"one","question":"Choose","initialAnswer":" exact ","options":[{"label":"Label","value":" exact "}],"allowCustomAnswer":false},
                {"id":"many","question":"Several","multiSelect":true,"options":[{"label":"A"},{"label":"B"}],"allowCustomAnswer":false},
                {"id":"custom","question":"Why","options":[]}
            ]});
            view.native.actions.update(cx, |a,cx| {
                let request=okilum_t3::actions::Request{id:"r".into(),item:item.clone(),answerable:true,dismissible:true,response_mode:Some("live".into()),reason:None};
                a.set_state(State{ready:true,requests:vec![request],..Default::default()},window,cx);
                assert_eq!(a.selections[&("r".into(),"one".into())],vec![" exact "]);
                a.selections.insert(("r".into(),"many".into()),vec!["A".into(),"B".into()]);
                a.inputs[&("r".into(),"custom".into())].update(cx,|i,cx|i.set_value("Because",window,cx));
                let live=a.intent("r",cx);
                assert!(a.state.allows(&live));
                if let Intent::Respond{answers:Some(answers),..}=&live {
                    assert_eq!(answers["one"],json!(" exact "));
                    assert_eq!(answers["many"],json!(["A","B"]));
                    assert_eq!(answers["custom"],json!("Because"));
                } else {panic!("missing answers")}
                a.state.requests[0].response_mode=Some("message".into());
                let message=a.intent("r",cx);
                assert!(a.state.allows(&message));
                if let Intent::Respond{answers:Some(answers),..}=message {
                    assert_eq!(answers["many"],json!("[\"A\",\"B\"]"));
                } else {panic!("missing message answers")}
                let draft=a.inputs[&("r".into(),"custom".into())].clone();
                a.set_state(State::default(),window,cx);
                assert_eq!(a.inputs[&("r".into(),"custom".into())],draft,"unsynchronized reconnect retains answers");
                a.set_state(State{ready:true,..Default::default()},window,cx);
                assert!(a.inputs.is_empty(),"confirmed resolution removes controls/drafts");
            });
        });
    }
    #[gpui::test]
    fn accepted_requests_latch_and_stale_retries_never_change_target(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let temp = tempfile::tempdir().unwrap();
        let (view, visual) = cx.add_window_view(|window, cx| {
            super::super::super::fixture::view(
                temp.path().to_owned(),
                Arc::new(super::super::super::fixture::Fake::default()),
                window,
                cx,
            )
        });
        view.update_in(visual,|v,_,cx|v.native.actions.update(cx,|a,_| {
            a.state=State{ready:true,active_run:Some("new-run".into()),..Default::default()};
            let response=Record{command:okilum_t3::actions::Command::new("thread",Intent::Respond{request_id:"resolved".into(),decision:Some("accept".into()),answers:None}),sequence:Some(17)};
            a.records=vec![response.clone()];
            assert!(a.request_retained("resolved"));
            assert!(a.retry_allowed(&response),"acknowledged receipt is local only");
            let mut uncertain=response;
            uncertain.sequence=None;
            assert!(!a.retry_allowed(&uncertain),"resolved requests cannot be re-sent");
            uncertain.command.intent=Intent::Interrupt{run_id:"old-run".into()};
            assert!(!a.retry_allowed(&uncertain),"Stop cannot move to the newer run");
            uncertain.command.intent=Intent::message("Clarification".into(),DispatchMode::SteerActive{target_run_id:"old-run".into()});
            assert!(a.retry_allowed(&uncertain),"message retries query the original command receipt even after run state changes");
            assert_eq!(uncertain.command.wire().unwrap()["dispatchMode"]["targetRunId"],"old-run");
        }));
    }
    #[gpui::test]
    fn request_callback_releases_its_entity_before_parent_authority_checks(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::test_init);
        let temp = tempfile::tempdir().unwrap();
        let (view, visual) = cx.add_window_view(|window, cx| {
            let mut view = super::super::super::fixture::view(
                temp.path().to_owned(),
                Arc::new(super::super::super::fixture::Fake::default()),
                window,
                cx,
            );
            inert_native(&mut view);
            view.native.actions.update(cx, |a, cx| {
                a.set_state(
                    State {
                        ready: true,
                        active_run: Some("exact-run".into()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            view
        });
        view.update_in(visual, |v, window, cx| {
            v.native.actions.update(cx, |a, cx| {
                a.dispatch(
                    Intent::Interrupt {
                        run_id: "exact-run".into(),
                    },
                    window,
                    cx,
                )
            });
        });
        visual.run_until_parked();
        view.read_with(visual, |v, cx| {
            assert!(!v.busy);
            let a = v.native.actions.read(cx);
            assert_eq!(
                a.records.len(),
                1,
                "positive control: button path prepared the exact operation"
            );
            assert_eq!(
                a.records[0].command.intent,
                Intent::Interrupt {
                    run_id: "exact-run".into()
                }
            );
            assert!(
                a.records[0].sequence.is_none(),
                "refused local transport retains recoverable operation"
            );
        });
    }
}

#[cfg(test)]
mod composer_limit_tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn oversized_native_prompt_is_not_retained_and_keeps_the_draft(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let temp = tempfile::tempdir().unwrap();
        let env = Environment::new("fixture", "http://127.0.0.1:1").unwrap();
        let token = NativeToken::from_secret(&env, &serde_json::to_vec(&json!({"origin":env.origin(),"access_token":"fixture","expires_at":chrono::Utc::now().timestamp() as u64+3600,"scope":"orchestration:read orchestration:operate"})).unwrap()).unwrap();
        let client =
            Arc::new(NativeClient::new(env.clone(), token, Duration::from_millis(10)).unwrap());
        let (root, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = super::super::super::fixture::view(
                    temp.path().to_owned(),
                    Arc::new(super::super::super::fixture::Fake::default()),
                    window,
                    cx,
                );
                view.active = false;
                view.environment = Some(env);
                view.native.client = Some(client);
                view.native.stream_ready = true;
                view.compose.update(cx, |input, cx| {
                    input.set_value("x".repeat(120_000), window, cx)
                });
                assert!(
                    view.can_send(),
                    "positive control: valid prompt reaches send preflight"
                );
                view
            });
            Root::new(view, window, cx)
        });
        let view = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<ProjectView>().unwrap()
        });
        view.update_in(visual, |view, window, cx| {
            let draft = "x".repeat(120_001);
            view.compose.update(cx, |input, cx| input.set_value(draft.clone(), window, cx));
            view.native_send(window, cx);
            assert_eq!(view.compose.read(cx).value().as_str(), draft);
            assert_eq!(view.error.as_deref(), Some("Prompt is 1 character over the 120,000-character limit. Shorten or split it before sending."));
            assert!(!view.busy);
            assert!(view.native.actions.read(cx).records.is_empty());
            assert!(!temp.path().join("t3-native-commands").exists());
        });
        visual.run_until_parked();
    }
}
