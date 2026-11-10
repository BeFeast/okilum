//! Action drafts invalidate this entity, never the virtualized history.
use super::*;
use okilum_t3::actions::{
    DispatchMode, Intent, Journal, Record, Request, State, APPROVAL_FALLBACK_OPTIONS,
};
use serde_json::json;
#[cfg(test)]
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
mod approval;
mod question_progress;
use question_progress::{QuestionProgress, Transition};
mod question_attachments;
mod question_keys;
use question_keys::QuestionKeys;

#[derive(Clone)]
struct QuestionControl {
    conversation: Option<(String, String)>,
    lease: Option<Arc<()>>,
    transition: Transition,
}

/// A request row observes only action state; editing an answer never rebuilds
/// the virtualized feed. The source row and live authority must both match.
pub(in crate::project) struct RequestCard {
    actions: Entity<Actions>,
    item: Arc<serde_json::Value>,
    local: bool,
    request_revision: u64,
    _actions: Subscription,
}
impl RequestCard {
    pub(in crate::project) fn new(
        actions: Entity<Actions>,
        item: Arc<serde_json::Value>,
        local: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let request_revision = actions.read(cx).request_revision;
        let observation = cx.observe(&actions, |this, actions, cx| {
            let actions = actions.read(cx);
            #[cfg(test)]
            if let Some(observations) = &actions.request_observations {
                observations.borrow_mut().push((
                    this.request_revision,
                    actions.request_revision,
                    actions.pinned,
                    !actions.pinned || this.request_revision != actions.request_revision,
                ));
            }
            // A pinned history summary has no draft controls. Only request
            // presentation changes invalidate it, never answer keystrokes.
            if !actions.pinned || this.request_revision != actions.request_revision {
                this.request_revision = actions.request_revision;
                cx.notify();
            }
        });
        Self {
            actions,
            item,
            local,
            request_revision,
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
        // A render can consume a presentation change before its notification.
        self.request_revision = self.actions.read(cx).request_revision;
        let id = self.item["requestId"].as_str().unwrap_or("");
        let pending = self.local
            && self
                .actions
                .read(cx)
                .state
                .requests
                .iter()
                .any(|r| r.id == id && r.item["id"] == self.item["id"]);
        let pinned = self.local
            && self.actions.read(cx).pinned
            && self
                .actions
                .read(cx)
                .display_requests
                .iter()
                .any(|request| {
                    request.id == id
                        && request.item["id"] == self.item["id"]
                        && (request.answerable || request.dismissible)
                });
        let current = self
            .actions
            .read(cx)
            .current_request()
            .is_some_and(|(request, _)| request.id == id && request.item["id"] == self.item["id"]);
        let mut row = v_flex().w_full().min_w_0().gap_1();
        if pinned {
            let id = id.to_owned();
            row = row.debug_selector(move || format!("project-request-history-summary-{id}"));
        }
        if pending && !pinned {
            row = row.child(
                self.actions
                    .update(cx, |actions, cx| actions.render_request(id, &self.item, cx)),
            );
        } else {
            if self.item["type"] == "approval_request" {
                for field in ["prompt", "reason", "decision"] {
                    if field == "reason" && self.item["reason"] == self.item["prompt"] {
                        continue;
                    }
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
            row = row.child(
                div()
                    .text_color(brand::palette(cx).text_muted)
                    .child(if pinned {
                        if !current {
                            "Waiting in the request queue.".to_owned()
                        } else if self.actions.read(cx).state.ready {
                            "Respond above the composer.".to_owned()
                        } else {
                            "Answer draft retained above the composer while reconnecting."
                                .to_owned()
                        }
                    } else if self.local && self.item["status"] == "pending" {
                        "Waiting for supported response controls.".to_owned()
                    } else {
                        self.item["status"].as_str().unwrap_or("Request").to_owned()
                    }),
            );
        }
        row
    }
}

#[cfg(test)]
type RequestObservations = std::cell::RefCell<Vec<(u64, u64, bool, bool)>>;

pub(crate) struct Actions {
    parent: WeakEntity<ProjectView>,
    pub(crate) state: State,
    // Presentation survives an unsynchronized reconnect, never grants authority.
    display_requests: Vec<Request>,
    pinned: bool,
    conversation: Option<(String, String)>,
    lease: Option<Arc<()>>,
    #[cfg(test)]
    deferred_dispatches: std::cell::Cell<usize>,
    #[cfg(test)]
    request_observations: Option<RequestObservations>,
    records: Vec<Record>,
    inputs: HashMap<(String, String), Entity<InputState>>,
    selections: HashMap<(String, String), Vec<String>>,
    subscriptions: HashMap<(String, String), Subscription>,
    question_progress: HashMap<String, QuestionProgress>,
    question_timers: HashMap<String, Task<()>>,
    question_keys: Option<QuestionKeys>,
    question_files: question_attachments::Runtime,
    request_revision: u64,
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
            display_requests: Vec::new(),
            pinned: false,
            conversation: None,
            lease: None,
            #[cfg(test)]
            deferred_dispatches: std::cell::Cell::new(0),
            #[cfg(test)]
            request_observations: None,
            records: Vec::new(),
            inputs: HashMap::new(),
            selections: HashMap::new(),
            subscriptions: HashMap::new(),
            question_progress: HashMap::new(),
            question_timers: HashMap::new(),
            question_keys: None,
            question_files: question_attachments::Runtime::default(),
            request_revision: 0,
            busy: false,
            enabled: false,
            revision: 0,
            error: None,
        }
    }
    pub(super) fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.enabled != enabled {
            self.enabled = enabled;
            if !enabled {
                self.cancel_question_transitions();
                self.question_files.invalidate_jobs();
            }
            cx.notify();
        }
    }
    pub(in crate::project) fn set_pinned(&mut self, pinned: bool, cx: &mut Context<Self>) -> bool {
        if self.pinned == pinned {
            return false;
        }
        self.pinned = pinned;
        self.cancel_question_transitions();
        self.request_revision = self.request_revision.wrapping_add(1);
        cx.notify();
        true
    }
    fn set_scope(
        &mut self,
        conversation: Option<(String, String)>,
        lease: &Arc<()>,
        cx: &mut Context<Self>,
    ) -> bool {
        let changed = self.lease.as_ref().is_some_and(|previous| {
            !Arc::ptr_eq(previous, lease) || self.conversation != conversation
        });
        self.conversation = conversation;
        self.lease = Some(lease.clone());
        if !changed {
            return false;
        }
        self.state = State::default();
        self.display_requests.clear();
        self.inputs.clear();
        self.selections.clear();
        self.subscriptions.clear();
        self.question_timers.clear();
        self.question_keys = None;
        self.question_progress.clear();
        self.question_files = question_attachments::Runtime::default();
        self.records.clear();
        self.error = None;
        self.busy = false;
        self.request_revision = self.request_revision.wrapping_add(1);
        self.revision = self.revision.wrapping_add(1);
        cx.notify();
        true
    }
    pub(super) fn set_state(&mut self, state: State, window: &mut Window, cx: &mut Context<Self>) {
        if self.state == state {
            return;
        }
        let previous_current = self
            .current_request()
            .map(|(request, _)| request.id.clone());
        if !state.ready {
            self.cancel_question_transitions();
            self.question_files.invalidate_jobs();
        }
        self.question_files.reconcile(&state);
        let mut keep = std::collections::HashSet::new();
        let mut progress_keep = std::collections::HashSet::new();
        for request in &state.requests {
            if request.item["type"] == "user_input_request" {
                progress_keep.insert(request.id.clone());
                let changed = if let Some(progress) = self.question_progress.get_mut(&request.id) {
                    progress.reconcile(request)
                } else {
                    self.question_progress
                        .insert(request.id.clone(), QuestionProgress::new(request));
                    false
                };
                if changed || !request.answerable {
                    self.cancel_question_transition(&request.id);
                }
            }
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
                let changed_key = key.clone();
                let allow_custom = question["allowCustomAnswer"] != false;
                self.subscriptions.insert(
                    key.clone(),
                    cx.subscribe(&input, move |this, input, event: &InputEvent, cx| {
                        if matches!(event, InputEvent::Change) {
                            // Programmatic choice clears do not emit Change.
                            this.cancel_question_transition(&changed_key.0);
                        }
                        if allow_custom
                            && matches!(event, InputEvent::Change)
                            && !input.read(cx).value().trim().is_empty()
                        {
                            this.selections.remove(&changed_key);
                        }
                        cx.notify();
                    }),
                );
                self.inputs.insert(key, input);
            }
        }
        // An unsynchronized reconnect cannot resolve a user's pending draft.
        if state.ready {
            self.inputs.retain(|key, _| keep.contains(key));
            self.selections.retain(|key, _| keep.contains(key));
            self.subscriptions.retain(|key, _| keep.contains(key));
            self.question_progress
                .retain(|id, _| progress_keep.contains(id));
            self.question_timers
                .retain(|id, _| progress_keep.contains(id));
        }
        if state.ready || self.display_requests.is_empty() {
            self.display_requests = state.requests.clone();
        }
        self.state = state;
        if previous_current
            != self
                .current_request()
                .map(|(request, _)| request.id.clone())
        {
            self.cancel_question_transitions();
        }
        self.request_revision = self.request_revision.wrapping_add(1);
        cx.notify();
    }
    fn current_request(&self) -> Option<(&Request, usize)> {
        // T3 prioritizes approvals; ordinal order within each kind is retained.
        for kind in ["approval_request", "user_input_request"] {
            let mut requests = self.display_requests.iter().filter(|request| {
                request.item["type"] == kind && (request.answerable || request.dismissible)
            });
            if let Some(first) = requests.next() {
                return Some((first, 1 + requests.count()));
            }
        }
        None
    }
    fn request_retained(&self, id: &str) -> bool {
        self.records.iter().any(|r| matches!(&r.command.intent, Intent::Respond { request_id, .. } | Intent::Dismiss { request_id } if request_id == id))
    }
    fn retry_allowed(&self, record: &Record) -> bool {
        if matches!(&record.command.intent, Intent::Respond { attachments_by_question_id, .. } if !attachments_by_question_id.is_empty())
            && !self.question_files.capabilities().is_some_and(
                okilum_t3::stream::NativeEnvironmentCapabilities::question_attachments_available,
            )
        {
            return false;
        }
        record.sequence.is_some()
            || (self.state.ready && matches!(record.command.intent, Intent::Message { .. }))
            || self.state.allows(&record.command.intent)
    }
    fn intent(&self, request: &str, cx: &App) -> Intent {
        let mut answers = BTreeMap::new();
        let attachments_by_question_id = self.question_files.ready_references(request);
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
                } else if question["allowCustomAnswer"] != false
                    && attachments_by_question_id
                        .get(id)
                        .is_some_and(|files| !files.is_empty())
                {
                    answers.insert(id.to_owned(), json!(""));
                }
            }
        }
        Intent::Respond {
            request_id: request.to_owned(),
            decision: None,
            answers: Some(answers),
            attachments_by_question_id,
        }
    }
    fn cancel_question_transition(&mut self, request: &str) {
        if let Some(progress) = self.question_progress.get_mut(request) {
            progress.invalidate();
        }
        self.question_timers.remove(request);
    }
    fn cancel_question_transitions(&mut self) {
        for progress in self.question_progress.values_mut() {
            progress.invalidate();
        }
        self.question_timers.clear();
    }
    fn finish_question_submission(&mut self, transition: &Transition) {
        if let Some(progress) = self.question_progress.get_mut(&transition.request) {
            if progress.same_instance(transition) {
                progress.finish_submission();
            }
        }
    }
    fn cancel_retained_transitions(&mut self) {
        let requests = self
            .records
            .iter()
            .filter_map(|record| match &record.command.intent {
                Intent::Respond { request_id, .. } | Intent::Dismiss { request_id } => {
                    Some(request_id.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for request in requests {
            self.cancel_question_transition(&request);
        }
    }
    fn question_enabled(&self, request: &str) -> bool {
        !self.busy
            && self.enabled
            && self.state.ready
            && self.lease.is_some()
            && !self.request_retained(request)
            && self
                .state
                .requests
                .iter()
                .any(|candidate| candidate.id == request && candidate.answerable)
            && self
                .question_progress
                .get(request)
                .is_some_and(|progress| progress.position().is_some() && !progress.submitting)
    }
    fn question_control_current(&self, control: &QuestionControl) -> bool {
        self.conversation == control.conversation
            && self
                .lease
                .as_ref()
                .zip(control.lease.as_ref())
                .is_some_and(|(current, rendered)| Arc::ptr_eq(current, rendered))
            && (!self.pinned
                || self
                    .current_request()
                    .is_some_and(|(request, _)| request.id == control.transition.request))
            && self
                .question_progress
                .get(&control.transition.request)
                .is_some_and(|progress| progress.interaction_current(&control.transition))
    }
    fn question_complete(&self, request: &str, question: &str, cx: &App) -> bool {
        let Intent::Respond {
            answers: Some(answers),
            attachments_by_question_id,
            ..
        } = self.intent(request, cx)
        else {
            return false;
        };
        self.question_files
            .references(request, Some(question))
            .is_ok()
            && self
                .state
                .requests
                .iter()
                .find(|candidate| candidate.id == request)
                .is_some_and(|candidate| {
                    question_progress::question_complete_with_attachments(
                        candidate,
                        question,
                        &answers,
                        &attachments_by_question_id,
                    )
                })
    }
    fn advance_question(
        &mut self,
        request: &str,
        question: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.question_enabled(request)
            || self
                .question_progress
                .get(request)
                .and_then(QuestionProgress::active)
                != Some(question)
            || !self.question_complete(request, question, cx)
        {
            return;
        }
        let Some((index, count)) = self.question_progress[request].position() else {
            return;
        };
        let intent = self.intent(request, cx);
        if index + 1 == count
            && (!self.state.allows(&intent)
                || self.question_files.references(request, None).is_err())
        {
            return;
        }
        self.cancel_question_transition(request);
        let progress = self.question_progress.get_mut(request).unwrap();
        if index + 1 < count {
            progress.next();
        } else {
            // Lock this request before the deferred parent dispatch starts.
            progress.begin_submission();
            self.dispatch(intent, window, cx);
        }
        cx.notify();
    }
    fn previous_question(&mut self, request: &str, question: &str, cx: &mut Context<Self>) {
        if !self.question_enabled(request)
            || self
                .question_progress
                .get(request)
                .and_then(QuestionProgress::active)
                != Some(question)
        {
            return;
        }
        self.cancel_question_transition(request);
        if self
            .question_progress
            .get_mut(request)
            .is_some_and(QuestionProgress::previous)
        {
            cx.notify();
        }
    }
    fn collapse_question(&mut self, request: &str, question: &str, cx: &mut Context<Self>) {
        if self
            .question_progress
            .get(request)
            .and_then(QuestionProgress::active)
            != Some(question)
        {
            return;
        }
        self.cancel_question_transition(request);
        self.question_progress
            .get_mut(request)
            .unwrap()
            .toggle_collapsed();
        cx.notify();
    }
    fn select_question_option(
        &mut self,
        request: &str,
        question: &str,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.question_enabled(request)
            || self
                .question_progress
                .get(request)
                .and_then(QuestionProgress::active)
                != Some(question)
            || self.question_progress[request].collapsed()
        {
            return;
        }
        let Some(item) = self
            .state
            .requests
            .iter()
            .find(|candidate| candidate.id == request)
            .and_then(|request| request.item["questions"].as_array())
            .and_then(|questions| {
                questions
                    .iter()
                    .find(|candidate| candidate["id"] == question)
            })
        else {
            return;
        };
        if !item["options"].as_array().is_some_and(|options| {
            options
                .iter()
                .any(|option| option["value"].as_str().or(option["label"].as_str()) == Some(value))
        }) {
            return;
        }
        let multi = item["multiSelect"] == true;
        self.cancel_question_transition(request);
        let key = (request.to_owned(), question.to_owned());
        let choices = self.selections.entry(key.clone()).or_default();
        if multi && choices.iter().any(|choice| choice == value) {
            choices.retain(|choice| choice != value);
        } else {
            if !multi {
                choices.clear();
            }
            choices.push(value.to_owned());
        }
        if let Some(input) = self.inputs.get(&key) {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        if !multi
            && self.pinned
            && self
                .current_request()
                .is_some_and(|(current, _)| current.id == request)
        {
            self.schedule_question_transition(request, window, cx);
        }
        cx.notify();
    }
    fn schedule_question_transition(
        &mut self,
        request: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(transition) = self
            .question_progress
            .get(request)
            .and_then(|progress| progress.transition(request))
        else {
            return;
        };
        let Some(lease) = self.lease.clone() else {
            return;
        };
        let conversation = self.conversation.clone();
        let parent = self.parent.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(question_progress::AUTO_ADVANCE_DELAY)
                .await;
            let _ = this.update_in(cx, |_, window, cx| {
                // Release Actions before entering the parent's authority guard.
                window.defer(cx, move |window, cx| {
                    let _ = parent.update(cx, |view, cx| {
                        if !view.active
                            || view.busy
                            || view.native.deleted()
                            || !reader_features::enabled(reader_features::Feature::Agents, cx)
                            || !view.native.stream_error_is_clear()
                            || !view.native.client_can_operate()
                            || !Arc::ptr_eq(&lease, &view.native.request_lease)
                            || conversation != view.native_action_conversation()
                        {
                            return;
                        }
                        view.native.actions.update(cx, |actions, cx| {
                            if actions.pinned
                                && actions.question_enabled(&transition.request)
                                && actions
                                    .current_request()
                                    .is_some_and(|(current, _)| current.id == transition.request)
                                && actions
                                    .question_progress
                                    .get(&transition.request)
                                    .is_some_and(|progress| progress.accepts(&transition))
                            {
                                actions.advance_question(
                                    &transition.request,
                                    &transition.question,
                                    window,
                                    cx,
                                );
                            }
                        });
                    });
                });
            });
        });
        self.question_timers.insert(request.to_owned(), task);
    }
    fn dispatch(&self, intent: Intent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lease) = self.lease.clone() else {
            return;
        };
        let conversation = self.conversation.clone();
        let submission = match &intent {
            Intent::Respond {
                request_id,
                decision: None,
                ..
            } => self
                .question_progress
                .get(request_id)
                .filter(|progress| progress.submitting)
                .and_then(|progress| progress.transition(request_id)),
            _ => None,
        };
        let parent = self.parent.clone();
        #[cfg(test)]
        self.deferred_dispatches
            .set(self.deferred_dispatches.get() + 1);
        // Release the request entity before the parent reads its live authority
        // and marks the operation busy. A button callback otherwise re-borrows
        // the Actions entity through native_command.
        window.defer(cx, move |window, cx| {
            let _ = parent.update(cx, |view, cx| {
                if !Arc::ptr_eq(&lease, &view.native.request_lease)
                    || conversation != view.native_action_conversation()
                {
                    return;
                }
                let eligible = submission.as_ref().is_none_or(|submission| {
                    view.native
                        .actions
                        .read(cx)
                        .question_progress
                        .get(&submission.request)
                        .is_some_and(|progress| progress.same_submission(submission))
                });
                let was_busy = view.busy;
                if eligible {
                    view.native_command(Some(intent), None, None, window, cx);
                }
                if !eligible || was_busy || !view.busy {
                    if let Some(submission) = &submission {
                        view.native.actions.update(cx, |actions, cx| {
                            actions.finish_question_submission(submission);
                            cx.notify();
                        });
                    }
                }
            });
        });
    }
}
impl Actions {
    fn render_question_step(
        &mut self,
        request: &Request,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(progress) = self.question_progress.get(&request.id) else {
            return div()
                .child("These questions cannot be identified safely. Open T3 to answer.")
                .into_any_element();
        };
        let Some((index, count)) = progress.position() else {
            return div()
                .child("These questions cannot be identified safely. Open T3 to answer.")
                .into_any_element();
        };
        let question_id = progress.active().unwrap().to_owned();
        let control = QuestionControl {
            conversation: self.conversation.clone(),
            lease: self.lease.clone(),
            transition: progress.transition(&request.id).unwrap(),
        };
        let Some(question) = request.item["questions"].as_array().and_then(|questions| {
            questions
                .iter()
                .find(|question| question["id"] == question_id)
        }) else {
            return div()
                .child("These questions cannot be identified safely. Open T3 to answer.")
                .into_any_element();
        };
        let collapsed = progress.collapsed();
        let submitting = progress.submitting;
        let disabled = disabled || submitting;
        let request_id = request.id.clone();
        let question_text = question["question"].as_str().unwrap_or("").to_owned();
        let request_for_header = request_id.clone();
        let question_for_header = question_id.clone();
        let header_control = control.clone();
        let mut header = h_flex()
            .w_full()
            .min_w_0()
            .gap_1()
            .items_center()
            .child(
                Button::new(SharedString::from(format!(
                    "question-header-{request_id}-{question_id}"
                )))
                .debug_selector({
                    let request = request_id.clone();
                    let question = question_id.clone();
                    move || format!("question-header-{request}-{question}")
                })
                .ghost()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .icon(if collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                })
                .label(
                    question["header"]
                        .as_str()
                        .filter(|header| !header.trim().is_empty())
                        .unwrap_or("Question")
                        .to_owned(),
                )
                .tooltip(question_text.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.question_control_current(&header_control) {
                        this.collapse_question(&request_for_header, &question_for_header, cx);
                    }
                })),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .debug_selector({
                        let request = request_id.clone();
                        move || format!("question-progress-{request}-{}-{count}", index + 1)
                    })
                    .child(format!("{}/{count}", index + 1)),
            );
        if request.dismissible {
            let id = request_id.clone();
            let dismiss = Intent::Dismiss {
                request_id: id.clone(),
            };
            let allowed = self.state.allows(&dismiss);
            let dismiss_control = control.clone();
            header = header.child(
                Button::new(SharedString::from(format!("dismiss-{request_id}")))
                    .debug_selector({
                        let request = request_id.clone();
                        move || format!("dismiss-{request}")
                    })
                    .ghost()
                    .icon(IconName::Close)
                    .label("Dismiss")
                    .tooltip("Dismiss question without answering")
                    .disabled(
                        self.busy
                            || submitting
                            || !self.enabled
                            || self.request_retained(&request_id)
                            || !allowed,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        if !this.question_control_current(&dismiss_control) {
                            return;
                        }
                        this.cancel_question_transition(&id);
                        this.dispatch(dismiss.clone(), window, cx);
                    })),
            );
        }
        let mut form = v_flex().w_full().min_w_0().gap_1().child(header);
        if collapsed {
            form = form.child(
                div()
                    .w_full()
                    .min_w_0()
                    .truncate()
                    .debug_selector({
                        let request = request_id.clone();
                        move || format!("question-collapsed-{request}")
                    })
                    .child(question_text.lines().next().unwrap_or("").to_owned()),
            );
        } else {
            form = form.child(div().whitespace_normal().child(question_text));
            if question["multiSelect"] == true {
                form = form.child(
                    div()
                        .text_xs()
                        .text_color(brand::palette(cx).text_muted)
                        .debug_selector({
                            let request = request_id.clone();
                            move || format!("question-multi-guidance-{request}")
                        })
                        .child("Select one or more options."),
                );
            }
            if let Some(options) = question["options"].as_array() {
                let shortcuts = self.pinned
                    && self
                        .current_request()
                        .is_some_and(|(current, _)| current.id == request_id);
                let keys = QuestionKeys::new(control.clone(), options);
                let mut buttons = v_flex().w_full().min_w_0().gap_1();
                for (n, option) in options.iter().enumerate() {
                    let Some(value) = option["value"].as_str().or(option["label"].as_str()) else {
                        continue;
                    };
                    let key = (request_id.clone(), question_id.clone());
                    let selected = self
                        .selections
                        .get(&key)
                        .is_some_and(|selected| selected.iter().any(|selected| selected == value));
                    let request = request_id.clone();
                    let question = question_id.clone();
                    let value = value.to_owned();
                    let option_control = control.clone();
                    let label = option["label"].as_str().unwrap_or(&value).to_owned();
                    let description = option["description"]
                        .as_str()
                        .filter(|description| !description.is_empty() && *description != label);
                    let mut text = v_flex().flex_1().min_w_0().gap_1().child(
                        div()
                            .w_full()
                            .whitespace_normal()
                            .text_sm()
                            .debug_selector({
                                let request = request_id.clone();
                                let question = question_id.clone();
                                move || format!("answer-label-{request}-{question}-{n}")
                            })
                            .child(label.clone()),
                    );
                    if let Some(description) = description {
                        text = text.child(
                            div()
                                .w_full()
                                .whitespace_normal()
                                .text_xs()
                                .text_color(brand::palette(cx).text_muted)
                                .debug_selector({
                                    let request = request_id.clone();
                                    let question = question_id.clone();
                                    move || format!("answer-description-{request}-{question}-{n}")
                                })
                                .child(description.to_owned()),
                        );
                    }
                    let mut content = h_flex().w_full().min_w_0().gap_2().child(text);
                    if selected || (shortcuts && n < 9) {
                        content = content.child(
                            div()
                                .flex_shrink_0()
                                .text_sm()
                                .debug_selector({
                                    let request = request_id.clone();
                                    let question = question_id.clone();
                                    move || {
                                        let kind = if selected { "check" } else { "key" };
                                        format!("answer-{kind}-{request}-{question}-{n}")
                                    }
                                })
                                .child(if selected {
                                    "✓".into()
                                } else {
                                    (n + 1).to_string()
                                }),
                        );
                    }
                    buttons = buttons.child(
                        Button::new(SharedString::from(format!(
                            "answer-{request_id}-{question_id}-{n}"
                        )))
                        .debug_selector({
                            let request = request_id.clone();
                            let question = question_id.clone();
                            move || format!("answer-{request}-{question}-{n}")
                        })
                        .ghost()
                        .w_full()
                        .h_auto()
                        .py_2()
                        .px_3()
                        .accessibility_label(label)
                        .child(content)
                        .disabled(disabled)
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                if this.question_control_current(&option_control) {
                                    this.select_question_option(
                                        &request, &question, &value, window, cx,
                                    );
                                }
                            },
                        )),
                    );
                }
                if shortcuts {
                    self.question_keys = Some(keys);
                }
                form = form.child(buttons);
            }
            if question["allowCustomAnswer"] != false {
                if let Some(input) = self.inputs.get(&(request_id.clone(), question_id.clone())) {
                    form = form.child(
                        div()
                            .debug_selector({
                                let request = request_id.clone();
                                let question = question_id.clone();
                                move || format!("answer-custom-{request}-{question}")
                            })
                            .capture_action({
                                let paste_control = control.clone();
                                cx.listener(
                                    move |this, _: &gpui_component::input::Paste, window, cx| {
                                        if this.question_paste_files(
                                            paste_control.clone(),
                                            window,
                                            cx,
                                        ) {
                                            cx.stop_propagation();
                                        } else {
                                            cx.propagate();
                                        }
                                    },
                                )
                            })
                            .child(Input::new(input).disabled(disabled).w_full()),
                    );
                }
            }
            if let Some(files) = self.render_question_files(&control, disabled, cx) {
                form = form.child(files);
            }
        }
        let mut controls = h_flex().gap_1();
        if index > 0 {
            let request = request_id.clone();
            let question = question_id.clone();
            let previous_control = control.clone();
            controls = controls.child(
                Button::new(SharedString::from(format!(
                    "question-previous-{request_id}"
                )))
                .debug_selector({
                    let request = request_id.clone();
                    move || format!("question-previous-{request}")
                })
                .ghost()
                .label("Previous")
                .disabled(disabled)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.question_control_current(&previous_control) {
                        this.previous_question(&request, &question, cx);
                    }
                })),
            );
        }
        let last = index + 1 == count;
        let complete = self.question_complete(&request_id, &question_id, cx);
        let can_submit = !last
            || (self.state.allows(&self.intent(&request_id, cx))
                && self.question_files.references(&request_id, None).is_ok());
        let request = request_id.clone();
        let question = question_id;
        let submit_control = control.clone();
        controls = controls.child(
            Button::new(SharedString::from(if last {
                format!("respond-{request_id}")
            } else {
                format!("question-next-{request_id}")
            }))
            .debug_selector({
                let request = request_id;
                move || {
                    if last {
                        format!("respond-{request}")
                    } else {
                        format!("question-next-{request}")
                    }
                }
            })
            .ghost()
            .label(if submitting {
                "Submitting…"
            } else if !last {
                "Next"
            } else if count == 1 {
                "Submit answer"
            } else {
                "Submit answers"
            })
            .disabled(disabled || !complete || !can_submit)
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.question_control_current(&submit_control) {
                    this.advance_question(&request, &question, window, cx);
                }
            })),
        );
        if !collapsed {
            controls = controls.child(self.render_question_file_controls(&control, disabled, cx));
        }
        form.child(controls).into_any_element()
    }
    pub(super) fn render_request(
        &mut self,
        id: &str,
        item: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let requests = if self.state.ready {
            &self.state.requests
        } else {
            &self.display_requests
        };
        let Some(request) = requests
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
            card = card.child(self.render_approval(&request, disabled, cx));
        } else if request.item["type"] == "user_input_request" {
            card = card.child(self.render_question_step(&request, disabled, cx));
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
impl Actions {
    /// Match the actual action surface, including the session-only dismissal
    /// mask. An idle entity must not reserve a blank strip above the composer.
    pub(in crate::project) fn has_visible_content(&self, cx: &App) -> bool {
        self.state.active_run.is_some()
            || (self.pinned && self.current_request().is_some())
            || self.records.iter().any(|record| record.sequence.is_none())
            || self.error.as_ref().is_some_and(|error| {
                let key = self.conversation.as_ref().map(|(origin, thread)| {
                    super::recovery_state::ErrorDismissKey::new(
                        origin,
                        thread,
                        super::recovery_state::ErrorOrigin::Action,
                        error,
                    )
                });
                !key.as_ref().is_some_and(|key| {
                    cx.try_global::<super::recovery_state::SessionDismissals>()
                        .is_some_and(|session| session.is_dismissed(key))
                })
            })
    }

    fn render_run_header(
        &self,
        label: &str,
        count: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = brand::palette(cx);
        let mut row = h_flex()
            .debug_selector(|| "project-run-controls".into())
            .w_full()
            .min_w_0()
            .min_h(px(32.))
            .flex_shrink_0()
            .items_center()
            .gap(px(crate::project::design::GAP))
            .child(
                div()
                    .debug_selector(|| "project-run-indicator".into())
                    .size(px(6.))
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(if label == "Working" {
                        palette.accent
                    } else {
                        palette.warning
                    }),
            )
            .child(
                div()
                    .debug_selector(|| "project-run-status".into())
                    .text_size(px(crate::project::design::CHROME))
                    .line_height(px(crate::project::design::LINE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(palette.text)
                    .child(label.to_owned()),
            )
            .when_some(count.filter(|count| *count > 1), |row, count| {
                row.child(
                    div()
                        .debug_selector(move || format!("project-current-request-count-{count}"))
                        .text_size(px(crate::project::design::LABEL))
                        .line_height(px(crate::project::design::LINE))
                        .text_color(palette.text_muted)
                        .child(format!("1/{count}")),
                )
            })
            .child(div().flex_1());
        if let Some(run_id) = &self.state.active_run {
            let run_id = run_id.clone();
            row = row.child(
                Button::new("project-interrupt")
                    .ghost()
                    .small()
                    .h(px(28.))
                    .px_2()
                    .rounded(px(6.))
                    .bg(palette.surface)
                    .border_1()
                    .border_color(palette.border_subtle)
                    .icon(Icon::default().path("icons/square.svg").size(px(12.)).text_color(palette.danger))
                    .label("Stop")
                    .accessibility_label("Stop")
                    .tooltip("Stop")
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
        row.into_any_element()
    }
}
impl Render for Actions {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.question_keys = None;
        let current = self
            .current_request()
            .filter(|_| self.pinned)
            .map(|(request, count)| (request.clone(), count));
        let mut panel = v_flex()
            .id(SharedString::from(format!(
                "project-native-actions-{}",
                current
                    .as_ref()
                    .map_or("controls", |(request, _)| request.id.as_str())
            )))
            .debug_selector(|| "project-native-actions".into())
            .w_full()
            .min_w_0()
            .max_h(px(280.))
            .overflow_y_scroll()
            .text_size(px(crate::project::design::CHROME))
            .line_height(px(crate::project::design::LINE))
            .gap(px(crate::project::design::GAP));
        if current.is_none() && self.state.active_run.is_some() {
            panel = panel.child(self.render_run_header("Working", None, cx));
        }
        // Recovery details must not bury the current actionable request.
        if let Some((request, count)) = current {
            let label = if request.item["type"] == "approval_request" {
                "Approval requested"
            } else {
                "Answer requested"
            };
            let form = self.render_request(&request.id, &request.item, cx);
            panel = panel.child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .debug_selector(|| "project-current-request".into())
                    .child(self.render_run_header(label, Some(count), cx))
                    .child(form),
            );
        }
        if let Some(error) = &self.error {
            if let Some(banner) = super::recovery::error_banner(
                self.conversation.clone(),
                super::recovery_state::ErrorOrigin::Action,
                error.clone(),
                None,
                "project-action-error",
                window,
                cx,
            ) {
                panel = panel.child(banner);
            }
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

        panel
    }
}
impl ProjectView {
    fn native_action_conversation(&self) -> Option<(String, String)> {
        self.environment
            .as_ref()
            .zip(self.chat.as_ref())
            .map(|(environment, chat)| (environment.origin(), chat.thread_id.clone()))
    }

    pub(in crate::project) fn sync_native_action_surface(&self, cx: &mut Context<Self>) {
        let conversation = self.native_action_conversation();
        let lease = self.native.request_lease.clone();
        let pinned = self.native_actions_pinned();
        let attachment_context = self.native.model_client().and_then(|client| {
            Some(question_attachments::AttachmentContext {
                origin: self.environment.as_ref()?.origin(),
                project: self
                    .chat
                    .as_ref()?
                    .project_id
                    .clone()
                    .or_else(|| self.binding.as_ref()?.manager.project_id.clone())?,
                thread: self.chat.as_ref()?.thread_id.clone(),
                lease: lease.clone(),
                client,
                confirmed_environment: self
                    .attachment_identity()
                    .map(|identity| identity.environment_id.clone()),
            })
        });
        let changed = self.native.actions.update(cx, |actions, cx| {
            let changed = actions.set_scope(conversation, &lease, cx);
            actions.question_files.set_context(attachment_context);
            actions.set_pinned(pinned, cx) || changed
        });
        if changed {
            self.feed.update(cx, |feed, cx| feed.requests_changed(cx));
        }
    }

    pub(in crate::project) fn native_actions_pinned(&self) -> bool {
        self.local_setup.is_none()
            && self.binding.is_some()
            && self.native.active()
            && !self.executors_open
    }

    pub(crate) fn native_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || !self.can_send() {
            return;
        }
        let draft = self.composer.read(cx).typed_source_text(cx);
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
        if intent.as_ref().is_some_and(|intent| matches!(intent,
            Intent::Respond { request_id, decision: None, .. }
                if self.native.actions.read(cx).question_files.references(request_id, None).is_err())) { return; }
        let question_capabilities = self
            .native
            .actions
            .read(cx)
            .question_files
            .capabilities()
            .cloned();
        let Some(journal) = self.native_journal() else {
            return;
        };
        let question_submission = match &intent {
            Some(Intent::Respond {
                request_id,
                decision: None,
                ..
            }) => self
                .native
                .actions
                .read(cx)
                .question_progress
                .get(request_id)
                .filter(|progress| progress.submitting)
                .and_then(|progress| progress.transition(request_id)),
            _ => None,
        };
        self.typed_sync(cx);
        let typed_capture = match (&intent, &retry, &draft) {
            (Some(Intent::Message { text, .. }), None, Some(_)) => {
                match self.typed_send_type(text, cx) {
                    Ok(capture) => capture,
                    Err(error) => {
                        self.error = Some(error);
                        cx.notify();
                        return;
                    }
                }
            }
            _ => None,
        };
        self.busy = true;
        self.native.actions.update(cx, |a, cx| {
            a.busy = true;
            a.cancel_question_transitions();
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
                    if let (Some(capture), Some(Intent::Message { message_id, .. })) = (&typed_capture, &intent) {
                        capture.retain(message_id).map_err(anyhow::Error::msg)?;
                    }
                    let record = match retry { Some(r) => r, None => journal.prepare(intent.context("missing action")?)? };
                    retained = true;
                    journal.send(&record, |command| {
                        if matches!(&command.intent, Intent::Respond { attachments_by_question_id, .. } if !attachments_by_question_id.is_empty()) {
                            let capabilities = question_capabilities.as_ref().ok_or(StreamError::Protocol("Confirm question attachment support before retrying"))?;
                            client.dispatch_with_question_attachments(command, capabilities)
                        } else { client.dispatch(command) }
                    })
                })();
                (result, journal.records(), retained)
            }).await;
            let _ = this.update_in(cx, |this,window,cx| {
                if generation != this.generation { return; }
                this.busy = false;
                if retained { if let Some(sent) = &sent_snapshot { this.draft_clear_after_send(sent,window,cx); } }
                if retained { this.typed_reload_annotations(cx); }
                this.native.actions.update(cx, |a,cx| {
                    a.busy=false;
                    match records { Ok(records) => a.records=records, Err(e) => a.error=Some(format!("{e:#}")) }
                    if let Err(e) = result { a.error=Some(format!("{e:#}. Delivery may be unconfirmed; retry the retained action or check in T3.")); }
                    a.cancel_retained_transitions();
                    if let Some(submission) = &question_submission { a.finish_question_submission(submission); }
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
                    a.cancel_retained_transitions();
                    cx.notify();
                });
                this.feed.update(cx, |feed, cx| feed.requests_changed(cx));
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod current_request_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        thread,
    };

    #[gpui::test]
    fn action_surface_is_empty_at_idle_and_stop_shares_the_running_or_request_header(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::test_init);
        let state = tempfile::tempdir().unwrap();
        let mut owner = None;
        let (root, visual) = cx.add_window_view(|window, cx| {
            let project = cx.new(|cx| {
                let mut project = super::super::super::fixture::view(
                    state.path().into(),
                    Arc::new(super::super::super::fixture::Fake::default()),
                    window,
                    cx,
                );
                project.active = false;
                project
            });
            let actions = project.read(cx).native.actions.clone();
            owner = Some(project);
            Root::new(actions, window, cx)
        });
        visual.simulate_resize(size(px(320.), px(700.)));
        visual.run_until_parked();
        let actions = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<Actions>().unwrap()
        });
        actions.read_with(visual, |actions, cx| {
            assert!(!actions.has_visible_content(cx))
        });
        assert!(
            visual.debug_bounds("project-run-controls").is_none(),
            "idle has no blank run row"
        );
        assert!(visual.debug_bounds("project-interrupt").is_none());
        actions.update_in(visual, |actions, window, cx| {
            actions.set_state(
                State {
                    ready: true,
                    active_run: Some("exact-current-run".into()),
                    ..Default::default()
                },
                window,
                cx,
            );
            assert!(actions.has_visible_content(cx));
        });
        visual.run_until_parked();
        let running_header = visual.debug_bounds("project-run-controls").unwrap();
        let status = visual.debug_bounds("project-run-status").unwrap();
        let stop = visual.debug_bounds("project-interrupt").unwrap();
        assert!(status.left() < stop.left());
        assert_eq!(
            stop.right(),
            running_header.right(),
            "Stop has a stable right-aligned action position"
        );
        assert!(stop.top() >= running_header.top() && stop.bottom() <= running_header.bottom());
        actions.update_in(visual, |actions, window, cx| {
            actions.pinned = true;
            actions.set_state(State { ready:true, active_run:Some("exact-current-run".into()), requests:vec![Request {
                approval_detail:None, id:"current-question".into(), item:json!({"id":"current-question-row","type":"user_input_request","status":"pending","questions":[{"id":"q","question":"Confirm the visible result","options":[{"label":"Yes","value":"yes"}]}]}), answerable:true, dismissible:true, response_mode:Some("message".into()),reason:None,
            }], ..Default::default() }, window, cx);
        });
        visual.run_until_parked();
        let request = visual.debug_bounds("project-current-request").unwrap();
        let header = visual.debug_bounds("project-run-controls").unwrap();
        let stop = visual.debug_bounds("project-interrupt").unwrap();
        assert_eq!(
            request.top(),
            header.top(),
            "request and Stop share one header rather than an orphan action row"
        );
        assert_eq!(stop.right(), header.right());
        assert!(stop.bottom() <= header.bottom());
        actions.update_in(visual, |actions, window, cx| {
            actions.set_state(
                State {
                    ready: true,
                    ..Default::default()
                },
                window,
                cx,
            );
            actions.error = Some("Retained action detail".into());
            actions.conversation = Some(("https://t3.example".into(), "exact-thread".into()));
            assert!(
                actions.has_visible_content(cx),
                "errors remain reachable while idle"
            );
            let key = super::super::recovery_state::ErrorDismissKey::new(
                "https://t3.example",
                "exact-thread",
                super::super::recovery_state::ErrorOrigin::Action,
                "Retained action detail",
            );
            cx.default_global::<super::super::recovery_state::SessionDismissals>()
                .dismiss(key);
            assert!(
                !actions.has_visible_content(cx),
                "a dismissed error cannot leave an empty action strip"
            );
            actions.error = None;
            actions.records.push(Record {
                command: okilum_t3::actions::Command::new(
                    "exact-thread",
                    Intent::Interrupt {
                        run_id: "old-retained-run".into(),
                    },
                ),
                sequence: None,
            });
            assert!(
                actions.has_visible_content(cx),
                "unconfirmed actions retain recovery controls"
            );
            actions.records[0].sequence = Some(1);
            assert!(
                !actions.has_visible_content(cx),
                "acknowledged records alone do not reserve a row"
            );
        });
        drop(owner);
    }

    pub(super) fn inert_native(view: &mut ProjectView) {
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
                        // BSD/macOS inherits the listener's nonblocking mode.
                        s.set_nonblocking(false).unwrap();
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
                approval_detail: None,
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
            attachments_by_question_id: Default::default(),
            request_id: "approval".into(),
            decision: Some("accept".into()),
            answers: None
        }));
        assert!(state.allows(&Intent::Respond {
            attachments_by_question_id: Default::default(),
            request_id: "approval".into(),
            decision: Some("decline".into()),
            answers: None
        }));
    }
    #[gpui::test]
    fn pinned_question_keystrokes_keep_history_work_bounded_and_parent_silent(
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
                view.native.actions.update(cx,|a,cx| a.set_state(State{ready:true,requests:vec![okilum_t3::actions::Request { approval_detail: None,id:"r".into(),item:question.as_ref().clone(),answerable:true,dismissible:true,response_mode:Some("message".into()),reason:None}],..Default::default()},window,cx));
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
        actions.update(visual, |actions, _| {
            actions.request_observations = Some(Default::default());
        });
        // GPUI 0.3.3 clears debug_bounds every frame but does not replay it
        // for cached views. Check history before an Actions-only focus frame.
        assert!(
            visual.debug_bounds("project-native-actions").is_some(),
            "positive control: the request surface is pinned above the composer"
        );
        assert!(
            visual.debug_bounds("project-request-r").is_some(),
            "positive control: pinned current request is visible"
        );
        let history_diagnostics = feed.read_with(visual, |feed, cx| {
            (
                feed.test_scroll_anchor(cx),
                feed.rendered_positions.borrow().clone(),
            )
        });
        assert!(
            visual.debug_bounds("project-message-1000").is_some(),
            "positive control: real unrelated native history is visible; anchor/rendered={history_diagnostics:?}; history={:?}; pinned={:?}; request={:?}",
            visual.debug_bounds("project-history"),
            visual.debug_bounds("project-native-actions"),
            visual.debug_bounds("project-request-r"),
        );
        let history_anchor = history_diagnostics
            .0
            .expect("positive control: actual source-keyed history anchor before focus");
        input.update_in(visual, |i, window, cx| i.focus_handle(cx).focus(window, cx));
        visual.run_until_parked();
        visual.simulate_keystrokes("left");
        visual.run_until_parked();
        visual.update(|window, cx| {
            assert!(
                input.read(cx).focus_handle(cx).is_focused(window),
                "positive control: keystrokes target the pinned answer Input"
            );
        });
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
        let diagnostic = |stage: &str, visual: &mut VisualTestContext| {
            let history = feed.read_with(visual, |feed, cx| {
                (
                    feed.test_scroll_anchor(cx),
                    feed.rendered_positions.borrow().clone(),
                    feed.render_calls.get(),
                )
            });
            let requests = actions.read_with(visual, |actions, _| {
                (
                    actions.request_revision,
                    std::mem::take(
                        &mut *actions.request_observations.as_ref().unwrap().borrow_mut(),
                    ),
                )
            });
            diagnostic_error!("typing probe {stage}: history(anchor,rows,renders)={history:?}; requests(revision,observations[cached,current,pinned,notify])={requests:?}; notifications={:?}; panel={:?}; history_bounds={:?}",
                counts.iter().map(Cell::get).collect::<Vec<_>>(),
                visual.debug_bounds("project-native-actions"),
                visual.debug_bounds("project-history"));
        };
        diagnostic("before observer setup flush", visual);
        visual.run_until_parked();
        diagnostic("after observer setup flush", visual);
        assert!(
            input.read_with(visual, |input, _| input.value().is_empty()),
            "positive control: setup flush precedes all answer input"
        );
        feed.read_with(visual, |feed, _| {
            feed.rendered_positions.borrow_mut().clear()
        });
        for count in counts.iter() {
            count.set(0);
        }
        visual.simulate_input("a");
        visual.run_until_parked();
        diagnostic("after first character", visual);
        visual.simulate_input("nswer");
        visual.run_until_parked();
        diagnostic("after remaining characters", visual);
        assert_eq!(
            input.read_with(visual, |i, _| i.value().to_string()),
            "answer"
        );
        // Editing the pinned Input invalidates only Actions. The history must
        // reuse its cache without rendering rows or emitting notifications.
        feed.read_with(visual, |f, _| {
            let positions = f.rendered_positions.borrow();
            assert!(
                positions.is_empty(),
                "editing the pinned answer must not render history rows: {positions:?}"
            );
        });
        assert_eq!(counts[0].get(), 0);
        assert_eq!(counts[1].get(), 0);
        assert_eq!(
            feed.read_with(visual, |feed, cx| feed.test_scroll_anchor(cx)),
            Some(history_anchor),
            "focus and typing preserve the actual source key, item index and intra-row offset"
        );
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
                let request=okilum_t3::actions::Request { approval_detail: None,id:"r".into(),item:item.clone(),answerable:true,dismissible:true,response_mode:Some("live".into()),reason:None};
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
            let response=Record{command:okilum_t3::actions::Command::new("thread",Intent::Respond{ attachments_by_question_id: Default::default(),request_id:"resolved".into(),decision:Some("accept".into()),answers:None}),sequence:Some(17)};
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

#[cfg(test)]
mod recovery_tests;

#[cfg(test)]
mod question_progress_tests;

#[cfg(test)]
mod question_options_tests;

#[cfg(test)]
mod image_delivery_tests;
