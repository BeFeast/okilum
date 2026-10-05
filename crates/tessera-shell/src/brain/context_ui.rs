//! Context retrieval and review share the existing goal and source editor.
//! Search never changes the user's chosen citations; guidance is editable,
//! source excerpts are not. Each goal keeps its own in-progress selection.
use super::*;
use std::path::PathBuf;
mod search_passage;
mod source_selection;
pub(super) use search_passage::ContextOrigin;

fn brief_source_selected(chosen: &BTreeMap<String, Value>, citation: &Value) -> bool {
    chosen.values().any(|selected| {
        selected["path"] == citation["path"] && selected["revision"] == citation["revision"]
    })
}

fn brief_omission_reason(code: &Value) -> &'static str {
    match code.as_str().unwrap_or("") {
        "source_unavailable_or_oversized" => "Source is missing, unreadable, or too large",
        "invalid_record" => "Source metadata could not be read",
        "unknown_owner" => "The source could not be associated with a goal",
        "source_unavailable_oversized_or_invalid" => "Source is missing, too large, or invalid",
        "latest_stage_unavailable_or_invalid" => "The latest stage could not be read",
        "latest_result_unavailable_oversized_or_invalid" => {
            "The latest outcome could not be included: it exceeds the excerpt limit, or could not be read or validated"
        }
        "criterion_evidence_unavailable" => "Completion evidence is unavailable",
        "brief_input_budget" => "This source exceeds the brief size limit",
        _ => "This source could not be included",
    }
}

fn retrieval_notice(search: &Value) -> Option<&'static str> {
    if search["mode_used"] == "lexical" && search["mode_requested"] != "lexical" {
        Some("Meaning search is unavailable right now. These results match words only.")
    } else if !array(&search["warnings"]).is_empty()
        || !array(&search["index"]["warnings"]).is_empty()
    {
        Some(
            "Some source passages could not be included. You can narrow the search or inspect the original notes.",
        )
    } else if search["index"]["status"] == "indexing" {
        Some("Saved notes are being indexed. Search again shortly for the latest changes.")
    } else {
        None
    }
}

pub(super) struct GoalContext {
    _subscriptions: Vec<Subscription>,
    query: Entity<InputState>,
    scope: Entity<InputState>,
    guidance: Entity<TextareaState>,
    mode: String,
    scope_mode: String,
    search: Value,
    search_request: Value,
    chosen: BTreeMap<String, Value>,
    pinned: BTreeSet<String>,
    packet: Value,
    selection_changed: bool,
    pending_text: Option<String>,
    export: Value,
    saved_path: Option<String>,
    brief: Value,
    brief_error: Option<String>,
}
impl GoalContext {
    fn new(window: &mut Window, cx: &mut Context<BrainView>) -> Self {
        let guidance = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(6)
                .placeholder("Add the decisions, constraints and next step to carry forward…")
        });
        let scope = cx.new(|cx| {
            InputState::new(window, cx).placeholder("All brain notes · or limit to a folder")
        });
        let subscription = cx.subscribe(&guidance, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.cancel_context_passage_input();
            }
            cx.notify();
        });
        let scope_subscription = cx.subscribe(&scope, |this, entity, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.context_search_input_changed(entity.entity_id());
            }
            cx.notify();
        });
        let query = cx.new(|cx| {
            InputState::new(window, cx).placeholder("What context do you need for this goal?")
        });
        let query_subscription = cx.subscribe(&query, |this, entity, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.context_search_input_changed(entity.entity_id());
            }
            cx.notify();
        });
        // Keep draft-state-dependent controls in sync while the goal is retained.
        Self {
            _subscriptions: vec![subscription, scope_subscription, query_subscription],
            query,
            scope,
            guidance,
            mode: "hybrid".into(),
            scope_mode: "project".into(),
            search: Value::Null,
            search_request: Value::Null,
            chosen: BTreeMap::new(),
            pinned: BTreeSet::new(),
            packet: Value::Null,
            selection_changed: false,
            pending_text: None,
            export: Value::Null,
            saved_path: None,
            brief: Value::Null,
            brief_error: None,
        }
    }
    fn scope_changed(&self, cx: &App) -> bool {
        self.packet.is_object()
            && (self.scope_mode != self.packet["scope"]["mode"].as_str().unwrap_or("project")
                || self.scope.read(cx).value().trim()
                    != text(&self.packet["scope"]["path_prefix"]).trim())
    }
    fn dirty(&self, cx: &App) -> bool {
        self.selection_changed
            || self.scope_changed(cx)
            || (self.packet.is_object()
                && self.guidance.read(cx).value().as_ref() != text(&self.packet["text"]))
    }
    fn reviewed(&self, cx: &App) -> bool {
        self.packet["reviewed"] == true && self.packet["stale"] != true && !self.dirty(cx)
    }
}
struct ProposalContext {
    detail: Value,
    base: Value,
    prior: Option<GoalContext>,
    loading: bool,
    waiting_selection: bool,
    choice: bool,
    error: Option<String>,
    restored_fields: Option<Value>,
}

fn proposal_guidance(generated: &Value) -> String {
    let mut parts = vec![format!(
        "## Suggested next action (unverified)\n{}",
        text(&generated["title"])
    )];
    for (field, heading) in [
        ("criteria", "Proposed criteria"),
        ("open_questions", "Open questions"),
    ] {
        let values: Vec<_> = array(&generated[field])
            .iter()
            .map(|v| format!("- {}", text(v)))
            .collect();
        if !values.is_empty() {
            parts.push(format!("### {heading}\n{}", values.join("\n")));
        }
    }
    if let Some(rationale) = generated["rationale"].as_str().filter(|s| !s.is_empty()) {
        parts.push(format!("### Rationale\n{rationale}"));
    }
    parts.join("\n\n")
}

fn validate_proposal_context_form(fields: &Value, base: &Value) -> Result<(), String> {
    let query = fields["query"].as_str().unwrap_or("");
    let guidance = fields["guidance"].as_str().unwrap_or("");
    let citations = array(&fields["citations"]);
    let pins = array(&fields["pinned_citation_ids"]);
    if query.trim().is_empty() || query.len() > 2048 {
        return Err("Add a context query of at most 2048 bytes.".into());
    }
    if guidance.trim().is_empty()
        || citations.len() > 20
        || guidance.len()
            + citations
                .iter()
                .map(|c| text(&c["excerpt"]).len())
                .sum::<usize>()
            > 64 * 1024
    {
        return Err("Context must contain guidance and fit twenty excerpts and 64 KiB. Adjust the form; no request has been sent.".into());
    }
    let mut ids = BTreeSet::new();
    for citation in &citations {
        let id = text(&citation["citation_id"]);
        let path = text(&citation["path"]);
        if id.is_empty()
            || !ids.insert(id)
            || path.is_empty()
            || path.starts_with('/')
            || path.contains(['\\', '\0'])
            || path.split('/').any(|p| p.is_empty() || p.starts_with('.'))
            || text(&citation["excerpt"]).len() > 8192
        {
            return Err("A selected excerpt has an invalid identity, path or size. Inspect its original source.".into());
        }
        let prefix = fields["scope"]["path_prefix"]
            .as_str()
            .unwrap_or("")
            .trim_end_matches('/');
        let include = array(&fields["scope"]["include_paths"]);
        let exclude = array(&fields["scope"]["exclude_paths"]);
        if (!prefix.is_empty() && path != prefix && !path.starts_with(&format!("{prefix}/")))
            || (!include.is_empty() && !include.contains(&json!(path)))
            || exclude.contains(&json!(path))
        {
            return Err("The edited scope excludes a selected source. Adjust the scope or selection explicitly.".into());
        }
        if fields["scope"]["mode"] == "goal"
            && citation["metadata"]["owner_goal_id"]
                .as_str()
                .is_some_and(|owner| fields["goal_id"] != owner)
        {
            return Err("A selected excerpt belongs to another goal.".into());
        }
    }
    let pinned: BTreeSet<_> = pins.iter().filter_map(Value::as_str).collect();
    if pinned.len() != pins.len() || pinned.iter().any(|id| !ids.contains(*id)) {
        return Err("Pins must refer to selected exact excerpts.".into());
    }
    if base.is_object() {
        if !guidance.contains(&text(&base["text"])) {
            return Err(
                "Keep the saved manual guidance verbatim; add the suggestion alongside it.".into(),
            );
        }
        for pin in array(&base["pinned_citation_ids"]) {
            if !pins.contains(&pin)
                || !array(&base["citations"])
                    .iter()
                    .find(|c| c["citation_id"] == pin)
                    .is_some_and(|c| citations.contains(c))
            {
                return Err("Keep the saved pins with their original excerpt and revision.".into());
            }
        }
    }
    if serde_json::to_vec(fields).map_err(|e| e.to_string())?.len() > 240 * 1024 {
        return Err("The complete context form is too large to retain.".into());
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct ContextUi {
    selection_preview: source_selection::SelectionUi,
    goals: BTreeMap<String, GoalContext>,
    proposals: BTreeMap<String, ProposalContext>,
}
impl ContextUi {
    pub(super) fn running(&self, goal: &str) -> bool {
        self.goals
            .get(goal)
            .is_some_and(|g| g.export["status"] == "running")
    }
    pub(super) fn poll(&self, goal: &str) -> Option<Value> {
        self.goals
            .get(goal)
            .filter(|g| g.export["status"] == "running")
            .map(|g| json!({"op":"context_export_get","goal_id":goal,"job_id":g.export["job_id"]}))
    }
    pub(super) fn unchanged(&self, goal: &str, data: &Value) -> bool {
        let mut data = data.clone();
        if let Some(object) = data.as_object_mut() {
            object.remove("_client_context_goal_id");
        }
        self.goals.get(goal).is_some_and(|g| g.export == data)
    }
}
impl BrainView {
    fn context_form_locked(&self) -> bool {
        self.busy
            || self
                .context_ui
                .proposals
                .get(&self.goal_id())
                .is_some_and(|p| self.adoption_pending_for(&text(&p.detail["record"]["id"])))
    }
    fn reload_proposal_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let goal = self.goal_id();
        let Some(proposal) = self.context_ui.proposals.get_mut(&goal) else {
            return;
        };
        if proposal.prior.is_some() {
            return;
        }
        proposal.loading = false;
        proposal.waiting_selection = true;
        proposal.error = None;
        self.resume_proposal_context(window, cx);
    }
    pub(super) fn begin_proposal_context(
        &mut self,
        detail: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(goal) = detail["record"]["goal_id"].as_str().map(str::to_owned) else {
            return;
        };
        if self.context_ui.proposals.contains_key(&goal) {
            return;
        }
        self.context_ui.proposals.insert(
            goal.clone(),
            ProposalContext {
                detail,
                base: Value::Null,
                prior: None,
                loading: false,
                waiting_selection: true,
                choice: false,
                error: None,
                restored_fields: None,
            },
        );
        if self.goal_id() != goal {
            self.select_goal(goal, window, cx);
        } else {
            self.resume_proposal_context(window, cx);
        }
        cx.notify();
    }
    pub(super) fn resume_proposal_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let goal = self.goal_id();
        let Some(proposal) = self.context_ui.proposals.get_mut(&goal) else {
            return;
        };
        if !proposal.waiting_selection {
            return;
        }
        proposal.waiting_selection = false;
        proposal.loading = true;
        self.ensure_context(window, cx);
        self.surface = Surface::Context;
        self.batch(
            vec![json!({"op":"context_get", "goal_id":goal})],
            window,
            cx,
        );
    }
    pub(super) fn restore_proposal_context_fields(
        &mut self,
        goal: &str,
        fields: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(proposal) = self.context_ui.proposals.get_mut(goal) else {
            return;
        };
        if fields["destination"] != "context" || fields["goal_id"] != goal {
            return;
        }
        proposal.restored_fields = Some(fields.clone());
        if proposal.prior.is_none() {
            return;
        }
        let Some(state) = self.context_ui.goals.get_mut(goal) else {
            return;
        };
        state.query.update(cx, |input, cx| {
            input.set_value(text(&fields["query"]), window, cx)
        });
        state.scope.update(cx, |input, cx| {
            input.set_value(text(&fields["scope"]["path_prefix"]), window, cx)
        });
        state.guidance.update(cx, |input, cx| {
            input.set_value(text(&fields["guidance"]), window, cx)
        });
        state.scope_mode = fields["scope"]["mode"].as_str().unwrap_or("project").into();
        state.chosen = array(&fields["citations"])
            .into_iter()
            .map(|c| (text(&c["citation_id"]), c))
            .collect();
        state.pinned = array(&fields["pinned_citation_ids"])
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        state.selection_changed = true;
        cx.notify();
    }
    pub(super) fn finish_proposal_context(
        &mut self,
        goal: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(proposal) = self.context_ui.proposals.remove(goal) {
            if let Some(prior) = proposal.prior {
                self.context_ui.goals.insert(goal.into(), prior);
            }
        }
        cx.notify();
    }
    pub(super) fn cancel_proposal_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let goal = self.goal_id();
        let Some(proposal) = self.context_ui.proposals.get(&goal) else {
            return;
        };
        if self.adoption_pending_for(&text(&proposal.detail["record"]["id"])) {
            return;
        }
        self.finish_proposal_context(&goal, window, cx);
        self.adoption.active = false;
        self.adoption.error = None;
        self.surface = Surface::Context;
    }
    fn accept_proposal_context_choice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let goal = self.goal_id();
        let Some(proposal) = self.context_ui.proposals.get(&goal) else {
            return;
        };
        if proposal.loading || proposal.waiting_selection || proposal.prior.is_some() {
            return;
        }
        let detail = proposal.detail.clone();
        let base = proposal.base.clone();
        if let Some(fields) = proposal.restored_fields.clone() {
            // A retired submission is editable evidence, even when its original
            // sources are now stale. Restore it without merging refreshed sources
            // or changing the retained expectation; keep the manual editor whole.
            self.ensure_context(window, cx);
            let mut working = GoalContext::new(window, cx);
            working.mode = self.context_ui.goals[&goal].mode.clone();
            working.packet = base.clone();
            let prior = self.context_ui.goals.insert(goal.clone(), working).unwrap();
            let proposal = self.context_ui.proposals.get_mut(&goal).unwrap();
            proposal.prior = Some(prior);
            proposal.choice = false;
            if base["stale"] == true {
                proposal.error = Some("The submitted draft is restored for editing. Its saved context is stale; saving is unavailable until you inspect a current suggestion. Original revision expectations are retained.".into());
            }
            self.restore_proposal_context_fields(&goal, fields, window, cx);
            return;
        }
        if base["stale"] == true || (base.is_object() && base["goal_id"] != goal) {
            self.context_ui.proposals.get_mut(&goal).unwrap().error = Some("The saved context is stale or belongs to another goal. Cancel and inspect the saved context first.".into());
            return;
        }
        self.ensure_context(window, cx);
        let prior = &self.context_ui.goals[&goal];
        let mut selected: BTreeMap<String, Value> = array(&base["citations"])
            .into_iter()
            .map(|c| (text(&c["citation_id"]), c))
            .collect();
        for c in prior
            .chosen
            .values()
            .cloned()
            .chain(array(&detail["record"]["captured"]["citations"]))
        {
            let id = text(&c["citation_id"]);
            if selected.get(&id).is_some_and(|old| old != &c) {
                self.context_ui.proposals.get_mut(&goal).unwrap().error = Some("A retained excerpt conflicts with the manual selection. Both are preserved; cancel and inspect their revisions.".into());
                return;
            }
            selected.insert(id, c);
        }
        let mut pins: BTreeSet<String> = array(&base["pinned_citation_ids"])
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        pins.extend(prior.pinned.iter().cloned());
        let manual = prior.guidance.read(cx).value().to_string();
        let saved = text(&base["text"]);
        let mut guidance = if manual.is_empty() {
            saved.clone()
        } else if saved.is_empty() || manual.contains(&saved) {
            manual
        } else {
            format!("{saved}\n\n{manual}")
        };
        if !guidance.is_empty() {
            guidance.push_str("\n\n");
        }
        guidance.push_str(&proposal_guidance(&detail["record"]["generated"]));
        let manual_query = prior.query.read(cx).value().to_string();
        let query = if manual_query.trim().is_empty() {
            text(&detail["record"]["generated"]["title"])
        } else {
            manual_query
        };
        let prefix = if prior.packet.is_object() || !prior.scope.read(cx).value().is_empty() {
            prior.scope.read(cx).value().to_string()
        } else {
            text(&base["scope"]["path_prefix"])
        };
        let mode = prior.mode.clone();
        let scope_mode = if prior.packet.is_object() || prior.scope_mode != "project" {
            prior.scope_mode.clone()
        } else {
            base["scope"]["mode"].as_str().unwrap_or("project").into()
        };
        let search = prior.search.clone();
        let brief = prior.brief.clone();
        let brief_error = prior.brief_error.clone();
        let mut working = GoalContext::new(window, cx);
        working
            .query
            .update(cx, |input, cx| input.set_value(query, window, cx));
        working
            .scope
            .update(cx, |input, cx| input.set_value(prefix, window, cx));
        working
            .guidance
            .update(cx, |input, cx| input.set_value(guidance, window, cx));
        working.mode = mode;
        working.scope_mode = scope_mode;
        working.search = search;
        working.chosen = selected;
        working.pinned = pins;
        working.packet = base;
        working.brief = brief;
        working.brief_error = brief_error;
        working.selection_changed = true;
        let prior = self.context_ui.goals.insert(goal.clone(), working).unwrap();
        let proposal = self.context_ui.proposals.get_mut(&goal).unwrap();
        proposal.prior = Some(prior);
        proposal.choice = false;
        proposal.error = None;
        if let Some(fields) = proposal.restored_fields.clone() {
            self.restore_proposal_context_fields(&goal, fields, window, cx);
        }
        cx.notify();
    }
    fn proposal_context_fields(&self, cx: &App) -> Result<(Value, Value), String> {
        let goal = self.goal_id();
        let proposal = self
            .context_ui
            .proposals
            .get(&goal)
            .ok_or("Suggestion form is unavailable")?;
        if proposal.prior.is_none() || proposal.loading || proposal.waiting_selection {
            return Err("Inspect the original context before submitting.".into());
        }
        if proposal.restored_fields.is_some() && proposal.base["stale"] == true {
            return Err("The restored submission remains editable, but its saved context is stale. Inspect a current suggestion before saving.".into());
        }
        let state = &self.context_ui.goals[&goal];
        let prefix = state.scope.read(cx).value().trim().to_string();
        let mut scope = if let Some(fields) = &proposal.restored_fields {
            fields["scope"].clone()
        } else if proposal.base["scope"].is_object() {
            proposal.base["scope"].clone()
        } else {
            json!({"include_paths":[],"exclude_paths":[]})
        };
        scope["goal_id"] = json!(goal);
        scope["mode"] = json!(state.scope_mode);
        scope["path_prefix"] = if prefix.is_empty() {
            Value::Null
        } else {
            json!(prefix)
        };
        let mut fields = json!({"destination":"context","goal_id":goal,
            "expected_goal_revision":proposal.detail["record"]["captured"]["goal_revision"],
            "expected_base_packet":proposal.base["id"],"expected_base_revision":proposal.base["revision"],
            "query":state.query.read(cx).value().to_string(),"scope":scope,
            "citations":state.chosen.values().collect::<Vec<_>>(),"pinned_citation_ids":state.pinned,
            "guidance":state.guidance.read(cx).value().to_string()});
        if let Some(restored) = &proposal.restored_fields {
            for key in [
                "expected_goal_revision",
                "expected_base_packet",
                "expected_base_revision",
            ] {
                fields[key] = restored[key].clone();
            }
        }
        validate_proposal_context_form(&fields, &proposal.base)?;
        Ok((proposal.detail.clone(), fields))
    }
    fn submit_proposal_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.proposal_context_fields(cx) {
            Ok((detail, fields)) => self.submit_proposal_adoption(detail, fields, window, cx),
            Err(error) => {
                self.adoption.error = Some(error);
                cx.notify();
            }
        }
    }
    fn ensure_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.context_ui
            .goals
            .entry(self.goal_id())
            .or_insert_with(|| GoalContext::new(window, cx));
    }
    pub(super) fn open_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_context(window, cx);
        self.surface = Surface::Context;
        let goal = self.goal_id();
        if self.context_ui.proposals.contains_key(&goal) {
            cx.notify();
            return;
        }
        if !self.busy {
            let mut requests = vec![
                json!({"op":"context_get","goal_id":goal}),
                json!({"op":"brain_index_status"}),
            ];
            if self.capabilities["goal_context_brief"] == true {
                requests.push(json!({"op":"goal_context_brief","goal_id":goal}));
            }
            self.batch(requests, window, cx);
        }
        cx.notify();
    }
    pub(super) fn context_reply(
        &mut self,
        op: &str,
        mut data: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let search_request = data
            .as_object_mut()
            .and_then(|o| o.remove("_client_search_request"))
            .unwrap_or(Value::Null);
        let origin = data
            .as_object_mut()
            .and_then(|o| o.remove("_client_context_goal_id"))
            .and_then(|v| v.as_str().map(str::to_owned));
        if origin.as_ref().is_some_and(|id| id != &self.goal_id()) {
            return;
        }
        self.ensure_context(window, cx);
        let goal = self.goal_id();
        if self.context_ui.proposals.contains_key(&goal)
            && matches!(op, "context_get" | "context_prepare" | "context_revise")
        {
            let proposal = self.context_ui.proposals.get_mut(&goal).unwrap();
            if op == "context_get" && proposal.loading {
                proposal.loading = false;
                proposal.base = data["packet"].clone();
                let state = &self.context_ui.goals[&goal];
                proposal.choice = state.dirty(cx)
                    || (!state.packet.is_object()
                        && (!state.guidance.read(cx).value().is_empty()
                            || !state.query.read(cx).value().is_empty()
                            || !state.scope.read(cx).value().is_empty()
                            || !state.chosen.is_empty()
                            || !state.pinned.is_empty()
                            || state.mode != "hybrid"
                            || state.scope_mode != "project"));
                if !proposal.choice {
                    self.accept_proposal_context_choice(window, cx);
                }
                cx.notify();
            }
            return;
        }
        let state = self.context_ui.goals.get_mut(&goal).unwrap();
        match op {
            "goal_context_brief" => {
                if data["schema"] != "tessera-goal-brief/v1" || data["goal_id"] != goal {
                    state.brief_error = Some(
                        "Saved inputs could not be matched to this goal. Refresh saved inputs."
                            .into(),
                    );
                    return;
                }
                state.brief = data;
                state.brief_error = None;
            }
            "brain_search" => {
                state.search = data;
                state.search_request = search_request;
            }
            "brain_index_status" => {}
            "context_prepare" | "context_get" | "context_revise" => {
                if op == "context_get" && data["export_job"].is_object() {
                    state.export = data["export_job"].clone();
                }
                let packet = &data["packet"];
                if !packet.is_object() {
                    return;
                }
                if packet["goal_id"] != goal {
                    self.error = Some(
                        "Context belongs to a different goal. Reopen this goal's context.".into(),
                    );
                    return;
                }
                let pending = state.pending_text.take();
                let preserve_build_text = if op == "context_prepare" {
                    pending.clone().filter(|s| !s.is_empty())
                } else {
                    None
                };
                let replace_text = !matches!(op, "context_prepare" | "context_revise")
                    || pending.is_none_or(|sent| state.guidance.read(cx).value().as_ref() == sent);
                if op == "context_get" && state.dirty(cx) {
                    // Retain local guidance, selections and scope while still
                    // reflecting that their saved source approval is obsolete.
                    if packet["stale"] == true {
                        state.packet["stale"] = json!(true);
                    }
                    return;
                }
                state.packet = packet.clone();
                state.selection_changed = false;
                if op == "context_get" {
                    state.query.update(cx, |input, cx| {
                        input.set_value(text(&packet["query"]), window, cx)
                    });
                    state.scope_mode = packet["scope"]["mode"].as_str().unwrap_or("project").into();
                    state.scope.update(cx, |input, cx| {
                        input.set_value(text(&packet["scope"]["path_prefix"]), window, cx)
                    });
                }
                if replace_text {
                    state.guidance.update(cx, |input, cx| {
                        input.set_value(
                            preserve_build_text.unwrap_or_else(|| text(&packet["text"])),
                            window,
                            cx,
                        )
                    });
                }
                state.chosen = array(&packet["citations"])
                    .into_iter()
                    .map(|c| (text(&c["citation_id"]), c))
                    .collect();
                state.pinned = array(&packet["pinned_citation_ids"])
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .filter(|id| state.chosen.contains_key(id))
                    .collect();
                self.notice = if packet["reviewed"] == true {
                    "Reviewed context saved"
                } else {
                    "Context ready to review"
                }
                .into();
            }
            "context_export_start" | "context_export_get" | "context_export_cancel" => {
                state.export = data;
            }
            _ => {}
        }
    }
    pub(super) fn context_brief_failed(
        &mut self,
        error: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_context(window, cx);
        self.context_ui
            .goals
            .get_mut(&self.goal_id())
            .unwrap()
            .brief_error = Some(error);
    }
    fn refresh_goal_brief(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.capabilities["goal_context_brief"] != true {
            return;
        }
        self.batch(
            vec![json!({"op":"goal_context_brief","goal_id":self.goal_id()})],
            window,
            cx,
        );
    }
    pub(super) fn include_manual_decision(&mut self, view: Value, cx: &mut Context<Self>) {
        if self.context_form_locked()
            || view["state"] != "manual_only"
            || view["goal_id"] != self.goal_id()
        {
            return;
        }
        let goal = self.goal_id();
        let Some(state) = self.context_ui.goals.get_mut(&goal) else {
            return;
        };
        let citation = view["citation"].clone();
        if !array(&state.brief["manual_only"])
            .iter()
            .any(|row| row["id"] == view["decision_id"] && row["path"] == citation["path"])
            || state.brief_error.is_some()
        {
            return;
        }
        if citation["revision"] != view["source"]["revision"]
            || decode_source(&view["source"]).ok().as_deref() != citation["excerpt"].as_str()
        {
            return;
        }
        let id = text(&citation["citation_id"]);
        if id.is_empty() || state.chosen.contains_key(&id) {
            return;
        }
        state.chosen.insert(id, citation);
        state.selection_changed = true;
        cx.notify();
    }
    fn include_brief_citation(&mut self, citation: Value, cx: &mut Context<Self>) {
        if self.context_form_locked() || self.capabilities["goal_context_brief"] != true {
            return;
        }
        let goal = self.goal_id();
        let Some(state) = self.context_ui.goals.get_mut(&goal) else {
            return;
        };
        if state.brief_error.is_some()
            || state.brief["goal_id"] != goal
            || !array(&state.brief["inputs"])
                .iter()
                .any(|input| input["citation"] == citation)
            || brief_source_selected(&state.chosen, &citation)
        {
            return;
        }
        let id = text(&citation["citation_id"]);
        if id.is_empty() || state.chosen.contains_key(&id) {
            return;
        }
        state.chosen.insert(id, citation);
        state.selection_changed = true;
        cx.notify();
    }
    fn source_context_form(&self, goal: &str, cx: &App) -> Option<Value> {
        self.context_ui.goals.get(goal).map(|s| json!({
            "query":s.query.read(cx).value().to_string(),"guidance":s.guidance.read(cx).value().to_string(),
            "mode":s.mode,"scope_mode":s.scope_mode,"prefix":s.scope.read(cx).value().to_string(),
            "chosen":s.chosen.values().collect::<Vec<_>>(),"pinned":s.pinned,"packet":s.packet,"selection_changed":s.selection_changed
        }))
    }
    fn source_context_ready(&self, cx: &App) -> Result<(), String> {
        if self.goal_id().is_empty() {
            return Err("Select a goal before using this note in Context.".into());
        }
        if self.source_loading
            || self.editor_closing()
            || !self.editor_can_begin_criteria()
            || self.source_conflict.is_some()
            || self.dirty(cx)
        {
            return Err("Save or discard Source changes and resolve recovery before using this note in Context.".into());
        }
        if self.context_ui.proposals.contains_key(&self.goal_id())
            || self.adoption.active
            || self.adoption.pending.iter().any(|pending| {
                self.expected_workspace.as_ref() == Some(&pending.workspace)
                    && pending.request["destination"] == "context"
                    && pending.request["goal_id"] == self.goal_id()
            })
        {
            return Err(
                "Finish the current Context adoption or recovery before adding a Source note."
                    .into(),
            );
        }
        if self.expected_workspace.is_none()
            || !self.source_editable
            || self.capabilities["reviewed_context"] != true
        {
            return Err("Choose a saved note in a managed goal with Context available.".into());
        }
        Ok(())
    }
    pub(super) fn use_source_in_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Err(e) = self.source_context_ready(cx) {
            self.error = Some(e);
            cx.notify();
            return;
        }
        let goal = self.goal_id();
        let workspace = self.expected_workspace.clone().unwrap();
        let Some(source) = self.source_snapshot.clone() else {
            self.error = Some("Choose a saved Source note first.".into());
            cx.notify();
            return;
        };
        let history = match source_context::observation_history(
            &source,
            &workspace,
            &goal,
            &self.snapshot["maestro"],
        ) {
            Ok(history) => history,
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        };
        let default_scope = json!({"goal_id":goal,"mode":"project","path_prefix":null});
        if let Err(e) = source_context::citation_with_history(
            &source,
            &workspace,
            &goal,
            &default_scope,
            &history,
        ) {
            self.error = Some(e);
            cx.notify();
            return;
        }
        let form = self.source_context_form(&goal, cx);
        let hydrate = form.as_ref().is_none_or(|s| !s["packet"].is_object());
        let stamp = self.source.stamp(cx);
        let endpoint = self.endpoint;
        self.busy = true;
        self.source_loading = true;
        self.sync_source_policy(cx);
        cx.spawn_in(window,async move |this,cx| {
            let owner=workspace.clone();let expected=source.clone();let requested_goal=goal.clone();
            let result=cx.background_executor().spawn(async move {
                let current=rpc_guarded(endpoint,json!({"op":"source_read","path":expected["path"]}),Some(&owner))?;
                if current!=expected { return Err("Source changed on disk. Reload and inspect it, then choose Use in Context again.".to_string()); }
                let packet=if hydrate {
                    let reply=rpc_guarded(endpoint,json!({"op":"context_get","goal_id":requested_goal}),Some(&owner))?;
                    reply.get("packet").filter(|p| p.is_null() || p.is_object()).cloned().ok_or("Saved Context response is incomplete. Open Context to inspect it before adding this note.")?
                }else{Value::Null};
                Ok((current,packet))
            }).await;
            let _=this.update_in(cx,|this,window,cx| {
                this.busy=false;this.source_loading=false;this.sync_source_policy(cx);
                if this.endpoint!=endpoint || this.expected_workspace.as_ref()!=Some(&workspace)
                    || this.goal_id()!=goal || this.source_snapshot.as_ref()!=Some(&source)
                    || this.source.stamp(cx)!=stamp || this.source_context_form(&goal,cx)!=form
                    || this.source_context_ready(cx).is_err()
                    || source_context::observation_history(&source,&workspace,&goal,&this.snapshot["maestro"]).as_ref()!=Ok(&history) {
                    this.error=Some("The goal, Source or Context draft changed while loading. Both drafts were preserved; choose Use in Context again.".into());cx.notify();return;
                }
                let prepared=result.and_then(|(current,packet)| {
                    let form=source_context::hydrate(form.as_ref(),&packet,&goal)?;
                    let scope=source_context::form_scope(&form,&goal);
                    let citation=source_context::citation_with_history(&current,&workspace,&goal,&scope,&history)?;
                    let action=source_context::preflight(&citation,&array(&form["chosen"]),&text(&form["guidance"]))?;
                    Ok((form,citation,action))
                });
                match prepared {
                    Ok((form,citation,action))=>{
                        this.apply_source_context(&goal, form, citation, action, window, cx);
                    }
                    Err(e)=>this.error=Some(e),
                }
                cx.notify();
            });
        }).detach();
    }
    fn apply_source_context(
        &mut self,
        goal: &str,
        form: Value,
        citation: Value,
        action: source_context::Addition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_context(window, cx);
        let state = self.context_ui.goals.get_mut(goal).unwrap();
        state.query.update(cx, |input, cx| {
            input.set_value(text(&form["query"]), window, cx)
        });
        state.guidance.update(cx, |input, cx| {
            input.set_value(text(&form["guidance"]), window, cx)
        });
        state.scope.update(cx, |input, cx| {
            input.set_value(text(&form["prefix"]), window, cx)
        });
        state.mode = text(&form["mode"]);
        state.scope_mode = text(&form["scope_mode"]);
        state.packet = form["packet"].clone();
        state.chosen = array(&form["chosen"])
            .into_iter()
            .map(|c| (text(&c["citation_id"]), c))
            .collect();
        state.pinned = array(&form["pinned"])
            .iter()
            .filter_map(|p| p.as_str().map(str::to_owned))
            .collect();
        state.selection_changed = form["selection_changed"] == true;
        if action == source_context::Addition::Add {
            state
                .chosen
                .insert(text(&citation["citation_id"]), citation);
            state.selection_changed = true;
        }
        self.notice = if action == source_context::Addition::Add {
            "Added the saved Source to Context. Build and review it before use."
        } else {
            "Already included in Context"
        }
        .into();
        self.error = None;
        self.surface = Surface::Context;
    }
    fn search_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_context(window, cx);
        let goal = self.goal_id();
        let state = &self.context_ui.goals[&goal];
        let query = state.query.read(cx).value().trim().to_string();
        if query.is_empty() {
            self.error = Some("Describe what you want to find.".into());
            cx.notify();
            return;
        }
        let scope = state.scope.read(cx).value().trim().to_string();
        self.batch(vec![json!({"op":"brain_search","_context_search_id":uuid(),"query":query,"mode":state.mode,"scope":{"mode":state.scope_mode,"goal_id":goal,"path_prefix":if scope.is_empty(){None}else{Some(scope)}},"limit":12,"max_excerpt_bytes":4096})],window,cx);
    }
    fn change_citation(&mut self, citation: Value, pin: bool, cx: &mut Context<Self>) {
        if self.context_form_locked() {
            return;
        }
        let goal = self.goal_id();
        let Some(state) = self.context_ui.goals.get_mut(&goal) else {
            return;
        };
        let id = text(&citation["citation_id"]);
        if pin {
            if !state.pinned.remove(&id) {
                state.pinned.insert(id.clone());
                state.chosen.insert(id, citation);
            }
        } else if state.chosen.remove(&id).is_some() {
            state.pinned.remove(&id);
        } else {
            state.chosen.insert(id, citation);
        }
        state.selection_changed = true;
        cx.notify();
    }
    fn build_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.context_ui.proposals.contains_key(&self.goal_id()) {
            return;
        }
        let goal = self.goal_id();
        let Some(state) = self.context_ui.goals.get_mut(&goal) else {
            return;
        };
        let citations: Vec<_> = state.chosen.values().cloned().collect();
        let query = state.query.read(cx).value().to_string();
        let pins = state.pinned.clone();
        let form = json!({"packet":state.packet,"scope_mode":state.scope_mode,"prefix":state.scope.read(cx).value().to_string()});
        let scope = source_context::form_scope(&form, &goal);
        state.pending_text = Some(state.guidance.read(cx).value().to_string());
        self.batch(vec![json!({"op":"context_prepare","goal_id":goal,"query":query,"citations":citations,"pinned_citation_ids":pins,"scope":scope})],window,cx);
    }
    fn save_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.context_ui.proposals.contains_key(&self.goal_id()) {
            return;
        }
        let goal = self.goal_id();
        let Some(state) = self.context_ui.goals.get_mut(&goal) else {
            return;
        };
        if state.selection_changed || state.scope_changed(cx) {
            self.error = Some(
                "Rebuild context from the updated selection and scope before reviewing it.".into(),
            );
            cx.notify();
            return;
        }
        if state.packet["stale"] == true {
            self.error = Some(
                "A saved source changed. Replace its excerpt, then rebuild and review the context."
                    .into(),
            );
            cx.notify();
            return;
        }
        let value = state.guidance.read(cx).value().to_string();
        if value.trim().is_empty() {
            self.error = Some("Add guidance for the next stage before saving your review.".into());
            cx.notify();
            return;
        }
        state.pending_text = Some(value.clone());
        let request = json!({"op":"context_revise","goal_id":goal,"packet_id":state.packet["id"],"expected_revision":state.packet["revision"],"text":value});
        self.batch(vec![request], window, cx);
    }
    pub(super) fn reviewed_context(&self, cx: &App) -> Result<Option<Value>, String> {
        if self.context_ui.proposals.contains_key(&self.goal_id()) {
            return Err(
                "Finish or cancel the suggestion form before using reviewed context.".into(),
            );
        }
        let Some(state) = self.context_ui.goals.get(&self.goal_id()) else {
            return if self.capabilities["reviewed_context"] == true {
                Err(
                    "Find relevant sources and save the reviewed context before preparing a stage."
                        .into(),
                )
            } else {
                Ok(None)
            };
        };
        if state.packet.is_null()
            && state.chosen.is_empty()
            && self.capabilities["reviewed_context"] != true
        {
            return Ok(None);
        }
        if !state.reviewed(cx) {
            return Err("Open Context and save your reviewed guidance and source selection before preparing this stage.".into());
        }
        Ok(Some(
            json!({"id":state.packet["id"],"revision":state.packet["revision"]}),
        ))
    }
    fn generate_context_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.context_ui.proposals.contains_key(&self.goal_id()) {
            return;
        }
        let goal = self.goal_id();
        let Some(state) = self.context_ui.goals.get(&goal) else {
            return;
        };
        if !state.reviewed(cx) || array(&state.packet["citations"]).is_empty() {
            self.error = Some(
                "Save the reviewed context and select at least one source for AI export.".into(),
            );
            cx.notify();
            return;
        }
        self.batch(vec![json!({"op":"context_export_start","goal_id":goal,"packet_id":state.packet["id"],"packet_revision":state.packet["revision"]})],window,cx);
    }
    fn download_context_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.context_ui.proposals.contains_key(&self.goal_id()) {
            return;
        }
        if self.busy {
            return;
        }
        let goal = self.goal_id();
        let Some(state) = self.context_ui.goals.get(&goal) else {
            return;
        };
        if state.export["status"] != "complete" || !state.reviewed(cx) {
            return;
        }
        let job = state.export["job_id"].clone();
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let downloads = home.join("Downloads");
        let directory = if downloads.is_dir() { downloads } else { home };
        let receiver = cx.prompt_for_new_path(&directory, Some("tessera-context.tar"));
        let endpoint = self.endpoint;
        let workspace = self.expected_workspace.clone();
        self.busy = true;
        self.sync_source_policy(cx);
        self.notice = "Choose where to save the AI context package…".into();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let selected = match receiver.await {
                Ok(Ok(path)) => Ok(path),
                Ok(Err(_)) => Err("The save dialog could not be opened.".to_string()),
                Err(_) => Err("The save dialog closed unexpectedly.".to_string()),
            };
            let result = match selected {
                Ok(Some(path)) => {
                    let goal = goal.clone();
                    let saved = path.display().to_string();
                    cx.background_executor()
                        .spawn(async move {
                            super::super::export::download_context(
                                endpoint,
                                workspace.as_ref(),
                                &path,
                                &goal,
                                &job,
                            )
                            .map(|_| Some(saved))
                        })
                        .await
                }
                Ok(None) => Ok(None),
                Err(error) => Err(error),
            };
            let _ = this.update_in(cx, |this, _, cx| {
                this.busy = false;
                this.sync_source_policy(cx);
                match result {
                    Ok(Some(path)) => {
                        if let Some(state) = this.context_ui.goals.get_mut(&goal) {
                            state.saved_path = Some(path);
                        }
                        this.notice = "AI context package saved".into();
                    }
                    Ok(None) => {
                        this.notice =
                            "Save cancelled; the generated package is still available".into()
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn citation_card(
        &self,
        citation: Value,
        index: usize,
        chosen: bool,
        pinned: bool,
        search_hit: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = super::super::brand::palette(cx);
        let path = text(&citation["path"]);
        let open = path.clone();
        let passage = citation.clone();
        let include = citation.clone();
        let pin = citation.clone();
        let excerpt = text(&citation["excerpt"]);
        let preview = if citation["start_line"].as_u64() == Some(1) {
            tessera_core::render::without_frontmatter(&excerpt)
        } else {
            &excerpt
        };
        let metadata = &citation["metadata"];
        let provenance = [
            text(&metadata["record_type"]),
            text(&metadata["status"]),
            text(&metadata["verification"]),
            text(&metadata["observed_at"]),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
        let match_label = match (
            citation["lexical_score"].is_number(),
            citation["semantic_score"].is_number(),
        ) {
            (true, true) => "Words + meaning",
            (true, false) => "Matched words",
            (false, true) => "Related meaning",
            _ => "Selected source",
        };
        v_flex()
            .gap_2()
            .p_3()
            .border_1()
            .border_color(if chosen {
                colors.link
            } else {
                colors.border_subtle
            })
            .rounded_md()
            .bg(colors.surface)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(path),
                    )
                    .child(
                        super::super::brand::control(("context-original", index), cx)
                            .small()
                            .label(if search_hit {
                                "Open source passage"
                            } else {
                                "Open original"
                            })
                            .debug_selector(move || {
                                format!(
                                    "context-open-{}-{index}",
                                    if search_hit { "passage" } else { "original" }
                                )
                            })
                            .disabled(self.context_form_locked())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if search_hit {
                                    this.open_context_search_passage(passage.clone(), window, cx);
                                } else {
                                    this.open_source(open.clone(), window, cx);
                                }
                            })),
                    ),
            )
            .child(div().text_xs().text_color(colors.text_muted).child(format!(
                "{match_label} · lines {}–{}",
                citation["start_line"].as_u64().unwrap_or(0),
                citation["end_line"].as_u64().unwrap_or(0)
            )))
            .when(!provenance.is_empty(), |card| {
                card.child(div().text_xs().child(provenance))
            })
            .child(
                TextView::markdown(("context-excerpt", index), preview.to_owned())
                    .selectable(true)
                    .style(super::super::reader_text_style(cx.theme())),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        super::super::brand::control(("context-include", index), cx)
                            .small()
                            .label(if chosen { "Exclude" } else { "Include" })
                            .disabled(self.context_form_locked())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.change_citation(include.clone(), false, cx)
                            })),
                    )
                    .child(
                        super::super::brand::control(("context-pin", index), cx)
                            .small()
                            .label(if pinned { "Unpin" } else { "Pin" })
                            .disabled(self.context_form_locked())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.change_citation(pin.clone(), true, cx)
                            })),
                    ),
            )
            .into_any_element()
    }
    fn goal_brief_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let state = &self.context_ui.goals[&self.goal_id()];
        let colors = super::super::brand::palette(cx);
        let brief = &state.brief;
        let mut panel = v_flex().gap_3().child(
            h_flex().gap_2().child(
                div().flex_1().text_lg().font_weight(FontWeight::SEMIBOLD).child("Saved goal context")
            ).child(
                super::super::brand::control("goal-brief-refresh", cx)
                    .small().label("Refresh saved inputs").disabled(self.context_form_locked())
                    .on_click(cx.listener(|this, _, window, cx| this.refresh_goal_brief(window, cx)))
            )
        ).child(div().text_sm().text_color(colors.text_muted).child(
            "Saved replies and the latest outcome. Choose sources to carry into the next reviewed context."
        ));
        if let Some(error) = &state.brief_error {
            panel = panel.child(div().text_sm().child(format!(
                "Saved inputs could not be refreshed: {error}. Previously shown inputs are retained; refresh before including them."
            )));
        }
        if !brief.is_object() {
            return panel
                .child(
                    div()
                        .text_sm()
                        .child("Saved inputs have not been loaded yet."),
                )
                .into_any_element();
        }
        if brief["complete"] == false {
            panel = panel.child(div().text_sm().child("Some saved inputs or completion evidence could not be included. This brief is incomplete."));
        }
        for omission in array(&brief["omissions"]) {
            panel = panel.child(div().text_xs().child(format!(
                "{} · {}",
                text(&omission["path"]),
                brief_omission_reason(&omission["code"])
            )));
        }
        if array(&brief["omissions"])
            .iter()
            .any(|omission| omission["code"] == "latest_result_unavailable_oversized_or_invalid")
        {
            panel = panel.child(
                super::super::brand::control("goal-brief-outcome", cx)
                    .small()
                    .label("Open Outcome")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.surface = Surface::Outcome;
                        cx.notify();
                    })),
            );
        }
        let criteria = array(&brief["remaining_criteria"]);
        if !criteria.is_empty() {
            panel = panel.child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Remaining criteria"),
            );
            for criterion in criteria {
                panel = panel.child(div().text_sm().child(format!(
                    "{} · {}",
                    text(&criterion["id"]),
                    text(&criterion["description"])
                )));
            }
        }
        let inputs = array(&brief["inputs"]);
        if inputs.is_empty() {
            panel = panel.child(
                div()
                    .text_sm()
                    .child("No saved inputs can be included in this context right now."),
            );
        }
        for (index, input) in inputs.into_iter().enumerate() {
            let citation = input["citation"].clone();
            let original = text(&citation["path"]);
            let included = brief_source_selected(&state.chosen, &citation);
            let actor = input["actor_id"].as_str().unwrap_or("Engine result");
            let provenance = format!(
                "{} · {} · {}",
                actor,
                text(&input["received_at"]),
                text(&input["verification"])
            );
            let reuse_id = text(&input["id"]);
            if input["kind"] == "discussion-decision"
                && self.capabilities["discussion_decision_reuse"] == true
            {
                panel = panel.child(
                    super::super::brand::control(("brief-reuse", index), cx)
                        .small()
                        .label("Reuse settings")
                        .disabled(self.context_form_locked())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_decision_reuse(reuse_id.clone(), window, cx)
                        })),
                );
            }
            panel = panel.child(
                v_flex()
                    .gap_2()
                    .p_3()
                    .border_1()
                    .border_color(colors.border_subtle)
                    .rounded_md()
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(text(&input["title"])),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors.text_muted)
                            .child(provenance),
                    )
                    .child(
                        TextView::markdown(("goal-brief-body", index), text(&input["body"]))
                            .selectable(true)
                            .style(super::super::reader_text_style(cx.theme())),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                super::super::brand::control(("goal-brief-original", index), cx)
                                    .small()
                                    .label("Open original")
                                    .disabled(self.context_form_locked())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_source(original.clone(), window, cx)
                                    })),
                            )
                            .child(
                                super::super::brand::control(("goal-brief-include", index), cx)
                                    .small()
                                    .label(if included {
                                        "Already included"
                                    } else {
                                        "Include in context"
                                    })
                                    .disabled(self.busy || included || state.brief_error.is_some())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.include_brief_citation(citation.clone(), cx)
                                    })),
                            ),
                    ),
            );
        }
        for (index, entry) in array(&brief["manual_only"]).into_iter().enumerate() {
            let id = text(&entry["id"]);
            let path = text(&entry["path"]);
            panel = panel.child(
                h_flex()
                    .gap_2()
                    .child("Saved decision · Manual selection only")
                    .child(
                        super::super::brand::control(("manual-decision-inspect", index), cx)
                            .small()
                            .label("Inspect")
                            .disabled(self.context_form_locked())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_decision_reuse(id.clone(), window, cx)
                            })),
                    )
                    .child(
                        super::super::brand::control(("manual-decision-original", index), cx)
                            .small()
                            .label("Open original")
                            .disabled(self.context_form_locked())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_source(path.clone(), window, cx)
                            })),
                    ),
            );
        }
        if brief["manual_only_truncated"] == true {
            panel =
                panel.child("More manual-only decisions remain available in Source and search.");
        }
        panel.into_any_element()
    }
    pub(super) fn context_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.ensure_context(window, cx);
        let goal = self.goal_id();
        let proposal_active = self.context_ui.proposals.contains_key(&goal);
        let proposal_waiting = self
            .context_ui
            .proposals
            .get(&goal)
            .is_some_and(|p| p.prior.is_none());
        let restored_stale = self
            .context_ui
            .proposals
            .get(&goal)
            .is_some_and(|p| p.restored_fields.is_some() && p.base["stale"] == true);
        let proposal_loading = self
            .context_ui
            .proposals
            .get(&goal)
            .is_some_and(|p| p.loading || p.waiting_selection);
        let proposal_error = self
            .context_ui
            .proposals
            .get(&goal)
            .and_then(|p| p.error.clone());
        let state = &self.context_ui.goals[&goal];
        let colors = super::super::brand::palette(cx);
        let query = state.query.clone();
        let scope = state.scope.clone();
        let guidance = state.guidance.clone();
        let mode = state.mode.clone();
        let scope_mode = state.scope_mode.clone();
        let search = state.search.clone();
        let chosen = state.chosen.clone();
        let pinned = state.pinned.clone();
        let packet = state.packet.clone();
        let dirty = state.dirty(cx);
        let needs_rebuild = state.selection_changed || state.scope_changed(cx);
        let stale = state.packet["stale"] == true;
        let reviewed = state.reviewed(cx);
        let job = state.export.clone();
        let saved = state.saved_path.clone();
        let mut panel = v_flex()
            .id("brain-context-panel")
            .debug_selector(|| "brain-context-panel".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_4();
        if proposal_active {
            let detail = self.context_ui.proposals[&goal].detail.clone();
            panel = panel.child(self.adoption_origin_banner(Some(&detail), cx));
            if let Some(error) = proposal_error {
                panel = panel.child(div().text_sm().child(error));
            }
            if proposal_waiting {
                panel = panel.child(div().text_sm().child(if proposal_loading {
                    "Loading the original goal's saved context. Your manual draft is retained."
                } else {
                    "A manual context draft is open. Keep it, or edit this suggestion separately; cancelling restores the complete manual draft."
                })).child(h_flex().gap_2().child(
                    super::super::brand::control("proposal-context-keep-manual",cx).label("Keep manual draft").disabled(self.busy)
                        .on_click(cx.listener(|this,_,window,cx|this.cancel_proposal_context(window,cx)))
                ).child(
                    super::super::brand::control("proposal-context-open-separate",cx).label("Edit suggestion separately").disabled(self.busy || proposal_loading)
                        .on_click(cx.listener(|this,_,window,cx|this.accept_proposal_context_choice(window,cx)))
                ).child(
                    super::super::brand::control("proposal-context-reload",cx).label("Reload original context").disabled(self.busy)
                        .on_click(cx.listener(|this,_,window,cx|this.reload_proposal_context(window,cx)))
                ));
                return panel.into_any_element();
            }
            panel = panel.child(div().text_sm().child("Edit the suggested context. Saving creates an unreviewed packet; the previous manual form is retained.")).child(
                h_flex().gap_2().child(
                    super::super::brand::control("proposal-context-submit",cx).primary().label("Save suggested context").disabled(self.context_form_locked() || restored_stale)
                        .on_click(cx.listener(|this,_,window,cx|this.submit_proposal_context(window,cx)))
                ).child(
                    super::super::brand::control("proposal-context-cancel",cx).label("Cancel suggestion").disabled(self.context_form_locked())
                        .on_click(cx.listener(|this,_,window,cx|this.cancel_proposal_context(window,cx)))
                ));
        }
        if self.capabilities["goal_context_brief"] == true {
            panel = panel.child(self.goal_brief_panel(cx));
        }
        panel = panel
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Find context for the next step"),
            )
            .child(Input::new(&query).disabled(self.context_form_locked()))
            .child(Input::new(&scope).disabled(self.context_form_locked()));
        let mut scope_controls = h_flex().gap_2();
        for (value, label) in [("project", "Project knowledge"), ("goal", "This goal")] {
            scope_controls = scope_controls.child(
                super::super::brand::control(
                    SharedString::from(format!("context-scope-{value}")),
                    cx,
                )
                .small()
                .label(label)
                .when(scope_mode == value, |b| b.primary())
                .disabled(self.context_form_locked())
                .on_click(cx.listener(move |this, _, _, cx| {
                    let goal = this.goal_id();
                    if let Some(state) = this.context_ui.goals.get_mut(&goal) {
                        state.scope_mode = value.into();
                        state.search_request = Value::Null;
                    }
                    cx.notify();
                })),
            );
        }
        panel = panel.child(scope_controls);
        let mut modes = h_flex().gap_2();
        for (value, label) in [
            ("hybrid", "Words + meaning"),
            ("lexical", "Words"),
            ("semantic", "Meaning"),
        ] {
            modes = modes.child(
                super::super::brand::control(
                    SharedString::from(format!("context-mode-{value}")),
                    cx,
                )
                .small()
                .label(label)
                .when(mode == value, |b| b.primary())
                .disabled(self.context_form_locked())
                .on_click(cx.listener(move |this, _, _, cx| {
                    let goal = this.goal_id();
                    if let Some(state) = this.context_ui.goals.get_mut(&goal) {
                        state.mode = value.into();
                        state.search_request = Value::Null;
                    }
                    cx.notify();
                })),
            );
        }
        panel = panel.child(
            modes.child(
                super::super::brand::control("context-search", cx)
                    .label("Find sources")
                    .disabled(self.context_form_locked())
                    .on_click(cx.listener(|this, _, window, cx| this.search_context(window, cx))),
            ),
        );
        if search.is_object() {
            let hits = array(&search["hits"]);
            panel = panel.child(div().text_sm().text_color(colors.text_muted).child(
                if hits.is_empty() {
                    "No matching sources. Try another description or a broader folder.".into()
                } else {
                    format!("{} matches · choose what belongs in this goal", hits.len())
                },
            ));
            if let Some(message) = retrieval_notice(&search) {
                panel = panel.child(div().text_sm().child(message));
            }
            for (i, hit) in hits.into_iter().enumerate() {
                let id = text(&hit["citation_id"]);
                panel = panel.child(self.citation_card(
                    hit,
                    i,
                    chosen.contains_key(&id),
                    pinned.contains(&id),
                    true,
                    cx,
                ));
            }
        }
        panel = panel.child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child(format!("Selected context · {} excerpts", chosen.len())),
        );
        if chosen.is_empty() {
            panel = panel.child(div().text_sm().text_color(colors.text_muted).child(
                "Include relevant excerpts, or build guidance-only context for T3. Pins stay selected when you search again.",
            ));
        }
        for (i, (id, citation)) in chosen.into_iter().enumerate() {
            panel = panel.child(self.citation_card(
                citation,
                1000 + i,
                true,
                pinned.contains(&id),
                false,
                cx,
            ));
        }
        panel = panel.child(
            super::super::brand::control("context-build", cx)
                .label(if packet.is_object() {
                    "Rebuild selected context"
                } else {
                    "Build context"
                })
                .disabled(self.context_form_locked() || proposal_active)
                .on_click(cx.listener(|this, _, window, cx| this.build_context(window, cx))),
        );
        if packet.is_object() || proposal_active {
            if stale {
                panel=panel.child(div().text_sm().text_color(colors.warning).child("A saved source changed. Find sources again, replace the outdated excerpt, then rebuild and review this context."));
            } else if needs_rebuild && !proposal_active {
                panel = panel.child(div().text_sm().child(
                    "Your selection or scope changed. Rebuild context before saving its review.",
                ));
            }
            panel =
                panel
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Guidance to carry forward"),
                    )
                    .child(div().text_sm().text_color(colors.text_muted).child(format!(
                        "Saved context scope: {}. Edit guidance below; source excerpts stay exact.",
                        if packet["scope"]["mode"] == "goal" {
                            "This goal"
                        } else {
                            "Project knowledge"
                        }
                    )))
                    .child(
                        Textarea::new(&guidance)
                            .disabled(self.context_form_locked())
                            .h(px(180.))
                            .flex_shrink_0(),
                    )
                    .child(div().text_sm().child(if proposal_active {
                        "This suggestion will be saved as unreviewed context."
                    } else if dirty {
                        "Changes need review before use."
                    } else if reviewed {
                        "Reviewed context · ready for T3 or an AI package."
                    } else {
                        "Review the guidance and excerpts, then save your review."
                    }))
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                super::super::brand::control("context-review", cx)
                                    .primary()
                                    .label("Save reviewed context")
                                    .disabled(
                                        self.context_form_locked()
                                            || proposal_active
                                            || stale
                                            || needs_rebuild,
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save_context(window, cx)
                                    })),
                            )
                            .child(
                                super::super::brand::control("context-execution", cx)
                                    .label("Use in T3 stage")
                                    .disabled(
                                        self.context_form_locked() || proposal_active || !reviewed,
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.surface = Surface::Execution;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                super::super::brand::control("context-ai-export", cx)
                                    .label("Generate AI package")
                                    .disabled(
                                        self.context_form_locked()
                                            || proposal_active
                                            || !reviewed
                                            || job["status"] == "running"
                                            || self.capabilities["chat"] != true
                                            || array(&packet["citations"]).is_empty(),
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.generate_context_export(window, cx)
                                    })),
                            ),
                    );
        }
        if packet.is_object() && array(&packet["citations"]).is_empty() {
            panel=panel.child(div().text_sm().text_color(colors.text_muted).child("Select at least one source for AI export. Guidance-only context can be used in T3."));
        }
        if job.is_object() {
            let status = text(&job["status"]);
            panel = panel.child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("AI context package"),
            );
            if status == "running" {
                let id = job["job_id"].clone();
                panel=panel.child(div().text_sm().child("Generating a sourced summary… You can keep working or close Tessera."))
                .child(super::super::brand::control("context-export-cancel",cx).label("Cancel generation").disabled(self.context_form_locked()).on_click(cx.listener(move|this,_,window,cx|this.batch(vec![json!({"op":"context_export_cancel","goal_id":this.goal_id(),"job_id":id})],window,cx))));
            } else if status == "complete" {
                if !reviewed {
                    panel=panel.child(div().text_sm().child("This package uses the last saved review. Save your edits and generate again to include them."));
                }
                panel=panel.child(div().text_sm().text_color(colors.text_muted).child("AI-generated · unverified. Review claims against the sources before sharing."))
                .child(TextView::markdown("context-export-preview",text(&job["markdown"])).selectable(true).style(super::super::reader_text_style(cx.theme())))
                .child(super::super::brand::control("context-export-save",cx).primary().label("Save AI package…").disabled(self.context_form_locked() || proposal_active || !reviewed).on_click(cx.listener(|this,_,window,cx|this.download_context_export(window,cx))));
            } else {
                panel=panel.child(div().text_sm().child(match status.as_str(){"stale"=>"A source changed. Rebuild and review the context before generating again.","interrupted"=>"Generation was interrupted. No incomplete package was published.",_=>"Generation could not finish. Your reviewed context is preserved; you can try again."}));
            }
        }
        if let Some(path) = saved {
            panel = panel.child(
                div()
                    .text_sm()
                    .child(format!("Saved on this computer: {path}")),
            );
        }
        panel.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    pub(super) fn citation(id: &str) -> Value {
        json!({"citation_id":id,"path":format!("notes/{id}.md"),"revision":"sha256:source","start_line":1,"end_line":1,"excerpt":"Exact original."})
    }
    pub(super) fn packet(goal: &str, reviewed: bool) -> Value {
        json!({"id":"packet","revision":"sha256:packet","goal_id":goal,"goal_revision":"sha256:goal","query":"context","text":"Saved guidance","citations":[citation("one")],"pinned_citation_ids":["one"],"reviewed":reviewed,"scope":{"mode":"project","goal_id":goal}})
    }
    fn proposal_detail(goal: &str) -> Value {
        json!({"record":{"id":"proposal","goal_id":goal,"captured":{"goal_revision":"sha256:goal","citations":[citation("one")]},
            "generated":{"title":"Suggested next step","criteria":["Observe the outcome"],"rationale":"Retained evidence","open_questions":["Which date?"]}},"source":{"revision":"sha256:proposal"}})
    }
    fn pending_proposal(view: &mut BrainView, goal: &str) {
        view.context_ui.proposals.insert(
            goal.into(),
            ProposalContext {
                detail: proposal_detail(goal),
                base: Value::Null,
                prior: None,
                loading: true,
                waiting_selection: false,
                choice: false,
                error: None,
                restored_fields: None,
            },
        );
        view.adoption.active = true;
    }
    #[gpui::test]
    fn proposal_context_preserves_full_empty_packet_draft_through_choice_and_cancel(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.snapshot = json!({"goal":{"id":"goal"}});
            view.ensure_context(window, cx);
            let state = view.context_ui.goals.get_mut("goal").unwrap();
            state
                .query
                .update(cx, |input, cx| input.set_value("Manual query", window, cx));
            state
                .scope
                .update(cx, |input, cx| input.set_value("notes", window, cx));
            state.guidance.update(cx, |input, cx| {
                input.set_value("Unsent manual guidance", window, cx)
            });
            state.mode = "lexical".into();
            state.scope_mode = "goal".into();
            state.chosen.insert("one".into(), citation("one"));
            state.pinned.insert("one".into());
            pending_proposal(&mut view, "goal");
            view.context_reply(
                "context_get",
                json!({"packet":packet("goal",true),"_client_context_goal_id":"goal"}),
                window,
                cx,
            );
            assert!(view.context_ui.proposals["goal"].choice);
            assert!(view.context_ui.proposals["goal"].prior.is_none());
            assert_eq!(
                view.context_ui.goals["goal"]
                    .guidance
                    .read(cx)
                    .value()
                    .as_ref(),
                "Unsent manual guidance"
            );
            view.accept_proposal_context_choice(window, cx);
            let state = &view.context_ui.goals["goal"];
            assert_eq!(state.query.read(cx).value().as_ref(), "Manual query");
            assert_eq!(state.scope.read(cx).value().as_ref(), "notes");
            assert_eq!(state.mode, "lexical");
            assert_eq!(state.scope_mode, "goal");
            assert_eq!(state.pinned, BTreeSet::from(["one".into()]));
            assert!(state.guidance.read(cx).value().contains("Saved guidance"));
            assert!(state
                .guidance
                .read(cx)
                .value()
                .contains("Unsent manual guidance"));
            assert!(state
                .guidance
                .read(cx)
                .value()
                .contains("Suggested next step"));
            assert!(view.reviewed_context(cx).is_err());
            view.context_reply(
                "context_get",
                json!({"packet":packet("goal",true),"_client_context_goal_id":"goal"}),
                window,
                cx,
            );
            assert!(view.context_ui.goals["goal"]
                .guidance
                .read(cx)
                .value()
                .contains("Suggested next step"));
            view.cancel_proposal_context(window, cx);
            let state = &view.context_ui.goals["goal"];
            assert!(state.packet.is_null());
            assert_eq!(
                state.guidance.read(cx).value().as_ref(),
                "Unsent manual guidance"
            );
            assert_eq!(state.query.read(cx).value().as_ref(), "Manual query");
            assert_eq!(state.scope.read(cx).value().as_ref(), "notes");
            assert_eq!(state.mode, "lexical");
            assert_eq!(state.scope_mode, "goal");
            assert_eq!(
                state.chosen,
                BTreeMap::from([("one".into(), citation("one"))])
            );
            assert_eq!(state.pinned, BTreeSet::from(["one".into()]));
            assert!(!view.adoption.active);
            view
        });
    }
    #[gpui::test]
    fn proposal_context_owner_loading_and_receipt_restore_do_not_touch_other_goal(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.snapshot = json!({"goal":{"id":"other"}});
            view.selected_goal_id = Some("other".into());
            view.ensure_context(window, cx);
            view.context_ui.goals["other"]
                .guidance
                .update(cx, |input, cx| {
                    input.set_value("Other goal draft", window, cx)
                });
            view.begin_proposal_context(proposal_detail("original"), window, cx);
            assert_eq!(view.goal_id(), "other");
            assert!(view.context_ui.proposals["original"].waiting_selection);
            assert!(!view.context_ui.goals.contains_key("original"));
            view.context_reply(
                "context_get",
                json!({"packet":packet("original",true),"_client_context_goal_id":"original"}),
                window,
                cx,
            );
            assert!(!view.context_ui.goals.contains_key("original"));
            view.snapshot = json!({"goal":{"id":"original"}});
            view.selected_goal_id = Some("original".into());
            view.busy = false;
            view.resume_proposal_context(window, cx);
            assert!(view.context_ui.proposals["original"].loading);
            view.context_reply(
                "context_get",
                json!({"packet":packet("original",true),"_client_context_goal_id":"original"}),
                window,
                cx,
            );
            assert!(view.context_ui.proposals["original"].prior.is_some());
            let (_, fields) = view.proposal_context_fields(cx).unwrap();
            assert_eq!(fields["goal_id"], "original");
            assert_eq!(fields["expected_base_packet"], "packet");
            assert_eq!(fields["pinned_citation_ids"], json!(["one"]));
            view.selected_goal_id = Some("other".into());
            view.snapshot = json!({"goal":{"id":"other"}});
            view.finish_proposal_context("original", window, cx);
            assert_eq!(view.goal_id(), "other");
            assert_eq!(
                view.context_ui.goals["other"]
                    .guidance
                    .read(cx)
                    .value()
                    .as_ref(),
                "Other goal draft"
            );
            assert!(view.context_ui.goals["original"].packet.is_null());
            assert!(view.context_ui.proposals.is_empty());
            view
        });
    }
    #[gpui::test]
    fn proposal_context_restores_retired_form_without_refreshing_submitted_identity(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window,cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);
            view.snapshot = json!({"goal":{"id":"goal"}});
            view.ensure_context(window,cx);
            pending_proposal(&mut view,"goal");
            let fields=json!({"destination":"context","goal_id":"goal","query":"Original submitted query",
                "expected_goal_revision":"sha256:original-goal","expected_base_packet":"original-packet","expected_base_revision":"sha256:original-packet",
                "guidance":"Saved guidance\nEdited submitted action","scope":{"mode":"project","goal_id":"goal","path_prefix":"notes","include_paths":["notes/one.md"],"exclude_paths":["notes/private.md"]},
                "citations":[citation("one")],"pinned_citation_ids":["one"]});
            view.restore_proposal_context_fields("goal",fields.clone(),window,cx);
            view.context_reply("context_get",json!({"packet":packet("goal",true),"_client_context_goal_id":"goal"}),window,cx);
            let (_,restored)=view.proposal_context_fields(cx).unwrap();
            assert_eq!(restored,fields);
            assert_eq!(view.context_ui.goals["goal"].guidance.read(cx).value().as_ref(),"Saved guidance\nEdited submitted action");
            view.finish_proposal_context("goal",window,cx);
            assert!(view.context_ui.goals["goal"].guidance.read(cx).value().is_empty());
            assert!(view.context_ui.goals["goal"].packet.is_null());
            view
        });
    }
    #[gpui::test]
    fn proposal_context_retired_fields_remain_editable_when_saved_base_is_stale(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window,cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);
            view.snapshot=json!({"goal":{"id":"goal"}});view.ensure_context(window,cx);
            view.context_ui.goals["goal"].guidance.update(cx,|input,cx|input.set_value("Unsent manual draft",window,cx));
            pending_proposal(&mut view,"goal");
            let fields=json!({"destination":"context","goal_id":"goal","query":"Retired query","guidance":"Retained edited submission","scope":{"goal_id":"goal","mode":"project"},"citations":[],"pinned_citation_ids":[],"expected_goal_revision":"old-goal","expected_base_packet":"old-packet","expected_base_revision":"old-revision"});
            view.restore_proposal_context_fields("goal",fields.clone(),window,cx);
            let mut stale=packet("goal",true);stale["stale"]=json!(true);
            stale["citations"][0]["excerpt"]=json!("A changed current excerpt must not replace the retained submission");
            view.context_reply("context_get",json!({"packet":stale,"_client_context_goal_id":"goal"}),window,cx);
            assert!(view.context_ui.proposals["goal"].choice);
            view.accept_proposal_context_choice(window,cx);
            assert_eq!(view.context_ui.goals["goal"].guidance.read(cx).value().as_ref(),"Retained edited submission");
            assert!(view.context_ui.goals["goal"].chosen.is_empty());
            assert_eq!(view.context_ui.proposals["goal"].restored_fields.as_ref(),Some(&fields));
            assert!(view.proposal_context_fields(cx).is_err());
            assert!(!view.context_form_locked());
            view.context_ui.goals["goal"].guidance.update(cx,|input,cx|input.set_value("Further local edits",window,cx));
            view.cancel_proposal_context(window,cx);
            assert_eq!(view.context_ui.goals["goal"].guidance.read(cx).value().as_ref(),"Unsent manual draft");
            view
        });
    }
    #[test]
    fn proposal_context_validation_preserves_saved_pins_guidance_and_bounds() {
        let base = packet("goal", true);
        let fields = json!({"goal_id":"goal","query":"Next step","guidance":"Saved guidance\nSuggested action","scope":{"mode":"project","goal_id":"goal","path_prefix":"notes"},"citations":[citation("one")],"pinned_citation_ids":["one"]});
        assert!(validate_proposal_context_form(&fields, &base).is_ok());
        for (field, value) in [
            ("guidance", json!("Suggestion replaced manual text")),
            ("guidance", json!("x".repeat(65537))),
            ("pinned_citation_ids", json!([])),
            ("citations", json!([])),
        ] {
            let mut bad = fields.clone();
            bad[field] = value;
            assert!(validate_proposal_context_form(&bad, &base).is_err());
        }
        let mut bad = fields.clone();
        bad["scope"]["path_prefix"] = json!("different-folder");
        assert!(validate_proposal_context_form(&bad, &base).is_err());
        let mut bad = fields;
        bad["citations"][0]["revision"] = json!("sha256:new");
        assert!(validate_proposal_context_form(&bad, &base).is_err());
    }
    #[gpui::test]
    fn saved_brief_is_explicit_and_preserves_reviewed_text_and_manual_pins(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.snapshot = json!({"goal":{"id":"goal"}});
            view.capabilities["goal_context_brief"] = json!(true);
            view.context_reply("context_get", json!({"packet":packet("goal",true)}), window, cx);
            let original = view.context_ui.goals["goal"].chosen.clone();
            let mut duplicate = citation("different-excerpt-id");
            duplicate["path"] = json!("notes/one.md");
            let fresh = citation("new-decision");
            let brief = json!({"schema":"tessera-goal-brief/v1","goal_id":"goal","inputs":[{"citation":duplicate},{"citation":fresh}]});
            view.context_reply("goal_context_brief", brief.clone(), window, cx);
            assert_eq!(view.context_ui.goals["goal"].chosen, original, "Reading a brief is not selection");
            assert!(view.context_ui.goals["goal"].reviewed(cx));
            view.include_brief_citation(duplicate, cx);
            assert_eq!(view.context_ui.goals["goal"].chosen, original, "Exact path/revision keeps the existing manual excerpt");
            view.include_brief_citation(fresh.clone(), cx);
            view.include_brief_citation(fresh, cx);
            let state = &view.context_ui.goals["goal"];
            assert_eq!(state.chosen.len(), 2);
            assert_eq!(state.pinned, BTreeSet::from(["one".into()]));
            assert_eq!(state.guidance.read(cx).value().as_ref(), "Saved guidance");
            assert!(!state.reviewed(cx), "New sources require the existing rebuild/review step");
            view.context_reply("goal_context_brief", brief, window, cx);
            assert_eq!(view.context_ui.goals["goal"].chosen.len(), 2);
            assert_eq!(view.context_ui.goals["goal"].guidance.read(cx).value().as_ref(), "Saved guidance");
            view
        });
    }
    #[gpui::test]
    fn saved_brief_rejects_old_goal_and_retained_unavailable_selection(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.snapshot = json!({"goal":{"id":"goal"}});
            view.capabilities["goal_context_brief"] = json!(true);
            let input = citation("decision");
            let mut brief = json!({"schema":"tessera-goal-brief/v1","goal_id":"goal","inputs":[{"citation":input}]});
            view.ensure_context(window, cx);
            brief["_client_context_goal_id"] = json!("other");
            view.context_reply("goal_context_brief", brief.clone(), window, cx);
            assert!(view.context_ui.goals["goal"].brief.is_null());
            brief["_client_context_goal_id"] = json!("goal");
            view.context_reply("goal_context_brief", brief.clone(), window, cx);
            view.context_brief_failed("offline".into(), window, cx);
            view.include_brief_citation(input.clone(), cx);
            assert!(view.context_ui.goals["goal"].chosen.is_empty());
            brief["inputs"] = json!([]);
            view.context_reply("goal_context_brief", brief, window, cx);
            view.include_brief_citation(input, cx);
            assert!(view.context_ui.goals["goal"].chosen.is_empty(), "A removed input is not selected by an old click");
            view
        });
    }
    #[gpui::test]
    fn late_search_and_export_are_bound_to_the_requested_goal(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window,cx|{
            let mut view=BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);
            view.snapshot=json!({"goal":{"id":"first"}});view.selected_goal_id=Some("first".into());view.ensure_context(window,cx);
            view.context_reply("context_get",json!({"packet":packet("first",true),"_client_context_goal_id":"first"}),window,cx);
            view.snapshot=json!({"goal":{"id":"second"}});view.selected_goal_id=Some("second".into());view.ensure_context(window,cx);
            view.context_ui.goals["second"].guidance.update(cx,|input,cx|input.set_value("Second goal draft",window,cx));
            view.context_reply("brain_search",json!({"hits":[citation("late")],"_client_context_goal_id":"first"}),window,cx);
            view.context_reply("context_export_get",json!({"job_id":"oldjob","status":"complete","markdown":"Old goal result","_client_context_goal_id":"first"}),window,cx);
            assert!(view.context_ui.goals["second"].search.is_null());assert!(view.context_ui.goals["second"].export.is_null());
            assert_eq!(view.context_ui.goals["second"].guidance.read(cx).value().as_ref(),"Second goal draft");
            assert_eq!(view.context_ui.goals["first"].packet["goal_id"],"first");view
        });
    }
    #[gpui::test]
    fn searching_preserves_pins_and_exclusions_without_automatic_selection(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window,cx|{
            let mut view=BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);view.snapshot=json!({"goal":{"id":"goal"}});view.ensure_context(window,cx);
            view.change_citation(citation("one"),true,cx);view.change_citation(citation("two"),false,cx);view.change_citation(citation("two"),false,cx);
            view.context_reply("brain_search",json!({"hits":[citation("two"),citation("three")],"_client_context_goal_id":"goal"}),window,cx);
            let state=&view.context_ui.goals["goal"];assert_eq!(state.chosen.len(),1);assert!(state.pinned.contains("one"));assert!(!state.chosen.contains_key("two"));assert!(!state.chosen.contains_key("three"));view
        });
    }
    #[gpui::test]
    fn review_is_required_and_late_save_preserves_newer_guidance(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.snapshot = json!({"goal":{"id":"goal"}});
            view.ensure_context(window, cx);
            view.context_reply(
                "context_prepare",
                json!({"packet":packet("goal",false)}),
                window,
                cx,
            );
            assert!(view.reviewed_context(cx).is_err());
            view.context_reply(
                "context_revise",
                json!({"packet":packet("goal",true)}),
                window,
                cx,
            );
            assert!(view.reviewed_context(cx).unwrap().is_some());
            let state = view.context_ui.goals.get_mut("goal").unwrap();
            state.pending_text = Some("Sent guidance".into());
            state.guidance.update(cx, |input, cx| {
                input.set_value("Newer local edit", window, cx)
            });
            let mut response = packet("goal", true);
            response["text"] = json!("Sent guidance");
            response["revision"] = json!("sha256:replacement");
            view.context_reply("context_revise", json!({"packet":response}), window, cx);
            assert_eq!(
                view.context_ui.goals["goal"]
                    .guidance
                    .read(cx)
                    .value()
                    .as_ref(),
                "Newer local edit"
            );
            assert!(view.reviewed_context(cx).is_err());
            view.context_reply(
                "context_get",
                json!({"packet":packet("goal",true)}),
                window,
                cx,
            );
            assert_eq!(
                view.context_ui.goals["goal"]
                    .guidance
                    .read(cx)
                    .value()
                    .as_ref(),
                "Newer local edit"
            );
            view
        });
    }
    #[gpui::test]
    fn restored_export_job_resumes_observation_and_never_replaces_guidance(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window,cx|{
            let mut view=BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);view.snapshot=json!({"goal":{"id":"goal"}});view.ensure_context(window,cx);
            view.context_reply("context_get",json!({"packet":packet("goal",true),"export_job":{"job_id":"job","status":"running"}}),window,cx);assert!(view.context_ui.running("goal"));
            assert_eq!(view.context_ui.poll("goal").unwrap()["job_id"],"job");
            view.context_ui.goals["goal"].guidance.update(cx,|input,cx|input.set_value("Edited while generation runs",window,cx));
            view.context_reply("context_export_get",json!({"job_id":"job","status":"complete","markdown":"Generated package","_client_context_goal_id":"goal"}),window,cx);
            assert!(!view.context_ui.running("goal"));assert_eq!(view.context_ui.goals["goal"].guidance.read(cx).value().as_ref(),"Edited while generation runs");assert!(view.reviewed_context(cx).is_err());view
        });
    }
    #[gpui::test]
    fn new_backend_requires_restored_review_but_guidance_only_can_prepare_t3(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.snapshot = json!({"goal":{"id":"goal"}});
            view.capabilities["reviewed_context"] = json!(true);
            assert!(
                view.reviewed_context(cx).is_err(),
                "no silent legacy fallback before context restoration"
            );
            let mut guidance_only = packet("goal", true);
            guidance_only["citations"] = json!([]);
            guidance_only["pinned_citation_ids"] = json!([]);
            view.context_reply("context_get", json!({"packet":guidance_only}), window, cx);
            assert!(view.reviewed_context(cx).unwrap().is_some());
            view.generate_context_export(window, cx);
            assert!(!view.busy);
            assert!(view.error.as_ref().unwrap().contains("at least one source"));
            view.snapshot["dispatch"]["packet"]["reviewed_packet"] =
                json!({"id":"packet","revision":"sha256:packet"});
            view.begin_prepared_edit(false, window, cx);
            assert!(view.prepared_edits.is_empty());
            assert!(view
                .error
                .as_ref()
                .unwrap()
                .contains("Discard the prepared stage"));
            view
        });
    }
    #[test]
    fn actual_search_fallback_is_explicit_even_when_the_index_has_ready_vectors() {
        assert!(retrieval_notice(&json!({"mode_requested":"hybrid","mode_used":"lexical","index":{"semantic_status":"ready"},"warnings":["query embedding unavailable"]})).unwrap().contains("words only"));
        assert!(retrieval_notice(&json!({"mode_requested":"hybrid","mode_used":"hybrid","index":{"semantic_status":"ready"},"warnings":[]})).is_none());
    }
    #[gpui::test]
    fn stale_or_narrowed_scope_cannot_approve_the_previous_packet(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.snapshot = json!({"goal":{"id":"goal"}});
            view.ensure_context(window, cx);
            let mut stale = packet("goal", true);
            stale["stale"] = json!(true);
            view.context_reply("context_get", json!({"packet":stale}), window, cx);
            assert!(view.reviewed_context(cx).is_err());
            view.context_reply(
                "context_get",
                json!({"packet":packet("goal",true)}),
                window,
                cx,
            );
            assert!(view.reviewed_context(cx).unwrap().is_some());
            view.context_ui.goals.get_mut("goal").unwrap().scope_mode = "goal".into();
            assert!(view.reviewed_context(cx).is_err());
            view.save_context(window, cx);
            assert!(!view.busy);
            assert!(view.error.as_ref().unwrap().contains("scope"));
            view.context_ui.goals.get_mut("goal").unwrap().scope_mode = "project".into();
            view.context_ui.goals["goal"].scope.update(cx, |input, cx| {
                input.set_value("narrower-folder", window, cx)
            });
            assert!(view.reviewed_context(cx).is_err());
            view
        });
    }
    #[gpui::test]
    fn rejected_export_refreshes_packet_and_preserves_error(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.snapshot = json!({"goal":{"id":"goal"}});
            let generation = view.request_generation;
            view.finish_batch(
                vec![("stage_prepare".into(), Err("Legacy stage error".into()))],
                false,
                generation,
                window,
                cx,
            );
            // Legacy stage errors retain their existing snapshot reconciliation.
            let legacy_generation = view.request_generation;
            view.finish_batch(
                vec![(
                    "context_export_start".into(),
                    Err("citation source revision changed".into()),
                )],
                false,
                legacy_generation,
                window,
                cx,
            );
            assert!(view.busy, "Rejected export must refresh the saved packet");
            assert_eq!(
                view.error.as_deref(),
                Some("citation source revision changed")
            );
            view
        });
    }
    #[gpui::test]
    fn reopening_context_refreshes_staleness_without_losing_local_guidance(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.snapshot = json!({"goal":{"id":"goal"}});
            view.context_reply(
                "context_get",
                json!({"packet":packet("goal",true)}),
                window,
                cx,
            );
            view.context_ui.goals["goal"]
                .guidance
                .update(cx, |input, cx| {
                    input.set_value("Unsent local draft", window, cx)
                });
            view.open_context(window, cx);
            assert!(
                view.busy,
                "An existing packet must be revalidated when Context opens"
            );
            let mut stale = packet("goal", true);
            stale["stale"] = json!(true);
            view.context_reply("context_get", json!({"packet":stale}), window, cx);
            assert_eq!(
                view.context_ui.goals["goal"]
                    .guidance
                    .read(cx)
                    .value()
                    .as_ref(),
                "Unsent local draft"
            );
            assert_eq!(view.context_ui.goals["goal"].packet["stale"], true);
            assert!(view.reviewed_context(cx).is_err());
            view
        });
    }
    pub(super) fn source_context_test_server(
        source: Value,
        packet: Value,
    ) -> (
        SocketAddr,
        Arc<std::sync::atomic::AtomicBool>,
        std::thread::JoinHandle<Vec<Value>>,
    ) {
        source_context_changing_test_server(
            Arc::new(std::sync::Mutex::new(source)),
            Arc::new(std::sync::Mutex::new(packet)),
        )
    }
    pub(super) fn source_context_changing_test_server(
        source: Arc<std::sync::Mutex<Value>>,
        packet: Arc<std::sync::Mutex<Value>>,
    ) -> (
        SocketAddr,
        Arc<std::sync::atomic::AtomicBool>,
        std::thread::JoinHandle<Vec<Value>>,
    ) {
        source_context_server_with_preview(source, packet, None)
    }
    fn source_context_server_with_preview(
        source: Arc<std::sync::Mutex<Value>>,
        packet: Arc<std::sync::Mutex<Value>>,
        preview: Option<Value>,
    ) -> (
        SocketAddr,
        Arc<std::sync::atomic::AtomicBool>,
        std::thread::JoinHandle<Vec<Value>>,
    ) {
        use std::sync::atomic::{AtomicBool, Ordering};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(15);
            let mut requests = vec![];
            while !stopped.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(c) => c,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
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
                    "source_read" => source.lock().unwrap().clone(),
                    "context_get" => json!({"packet":packet.lock().unwrap().clone()}),
                    "brain_index_status" if preview.is_some() => json!({}),
                    "source_preview" => preview
                        .as_ref()
                        .filter(|p| p["path"] == request["path"])
                        .cloned()
                        .unwrap_or_else(|| json!({"markdown":"Preview","assets":[]})),
                    op => panic!("Staging issued a forbidden operation: {op}"),
                };
                writeln!(
                    stream,
                    "{}",
                    json!({"schema":request["schema"],"id":request["id"],"ok":true,"data":data})
                )
                .unwrap();
                requests.push(request);
            }
            requests
        });
        (endpoint, stop, worker)
    }
    pub(super) fn source_context_test_source(workspace: &Value) -> Value {
        let raw = "---\ntype: Note\nverification: unverified\n---\nExact Source λ\r\n";
        json!({"schema":SCHEMA,"brain_id":workspace["brain_id"],"path":"notes/source.md","revision":tessera_core::decision_reuse::revision(raw.as_bytes()),"content_base64":STANDARD.encode(raw),"media_type":"text/markdown"})
    }
    #[gpui::test]
    fn source_context_hydrates_then_merges_exact_source_and_duplicate_is_noop(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let goal = uuid();
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/source-context","records_dir":"records","managed":true});
        let directory = std::env::temp_dir().join(format!("source-context265-{}", uuid()));
        let store = editor_recovery::EditorRecovery::at(directory.clone(), &workspace).unwrap();
        let source = source_context_test_source(&workspace);
        let mut saved = packet(&goal, true);
        saved["scope"]["include_paths"] = json!(["notes/source.md", "notes/one.md"]);
        saved["scope"]["exclude_paths"] = json!(["notes/private.md"]);
        let (endpoint, stop, server) = source_context_test_server(source.clone(), saved.clone());
        let (view, visual) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&workspace, &store, window, cx)
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            v.endpoint = endpoint;
            v.busy = false;
            v.snapshot = json!({"goal":{"id":goal}});
            v.selected_goal_id = Some(goal.clone());
            v.capabilities["reviewed_context"] = json!(true);
            v.load_source(source.clone(), window, cx);
            v.ensure_context(window, cx);
            let state = v.context_ui.goals.get_mut(&goal).unwrap();
            state.guidance.update(cx, |i, cx| {
                i.set_value("Retain unsent guidance", window, cx)
            });
            state
                .query
                .update(cx, |i, cx| i.set_value("Retain query", window, cx));
            state.chosen.insert("local".into(), citation("local"));
            state.pinned.insert("local".into());
            state.selection_changed = true;
            v.use_source_in_context(window, cx);
            assert!(v.busy);
            assert!(v.source_loading);
        });
        visual.run_until_parked();
        let before = view.update_in(visual, |v, window, cx| {
            assert!(v.error.is_none(), "{:?}", v.error);
            assert!(matches!(v.surface, Surface::Context));
            assert_eq!(v.source_snapshot.as_ref(), Some(&source));
            let state = &v.context_ui.goals[&goal];
            assert_eq!(
                state.guidance.read(cx).value().as_ref(),
                "Retain unsent guidance"
            );
            assert_eq!(state.query.read(cx).value().as_ref(), "Retain query");
            assert!(state.pinned.contains("local"));
            assert_eq!(state.packet, saved);
            assert!(state.chosen.values().any(|c| c["path"] == source["path"]));
            let form = v.source_context_form(&goal, cx).unwrap();
            v.use_source_in_context(window, cx);
            form
        });
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            assert_eq!(v.source_context_form(&goal, cx), Some(before));
            assert_eq!(v.notice, "Already included in Context");
        });
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let requests = server.join().unwrap();
        assert_eq!(
            requests.iter().filter(|r| r["op"] == "source_read").count(),
            2
        );
        assert_eq!(
            requests.iter().filter(|r| r["op"] == "context_get").count(),
            1
        );
        assert!(requests
            .iter()
            .all(|r| r["expected_workspace"] == workspace));
        assert!(!directory.exists());
    }
    #[gpui::test]
    fn source_context_historical_observation_hydrates_exactly_once(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let goal = uuid();
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/source-context","records_dir":"records","managed":true});
        let directory = std::env::temp_dir().join(format!("source-context265-{}", uuid()));
        let store = editor_recovery::EditorRecovery::at(directory.clone(), &workspace).unwrap();
        let (source, history) = source_context::tests::observation_fixture(&workspace, &goal);
        let mut saved = packet(&goal, true);
        saved["scope"]["include_paths"] = json!([source["path"], "notes/one.md"]);
        saved["scope"]["exclude_paths"] = json!(["notes/private.md"]);
        let (endpoint, stop, server) = source_context_test_server(source.clone(), saved.clone());
        let (view, visual) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&workspace, &store, window, cx)
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            v.endpoint = endpoint;
            v.busy = false;
            v.snapshot = json!({"goal":{"id":goal},"maestro":history});
            v.selected_goal_id = Some(goal.clone());
            v.capabilities["reviewed_context"] = json!(true);
            v.load_source(source.clone(), window, cx);
            v.ensure_context(window, cx);
            let state = v.context_ui.goals.get_mut(&goal).unwrap();
            state.guidance.update(cx, |i, cx| {
                i.set_value("Retain unsent guidance", window, cx)
            });
            state
                .query
                .update(cx, |i, cx| i.set_value("Retain query", window, cx));
            state.chosen.insert("local".into(), citation("local"));
            state.pinned.insert("local".into());
            state.selection_changed = true;
            v.use_source_in_context(window, cx);
            assert!(v.busy);
            assert!(v.source_loading);
        });
        visual.run_until_parked();
        let before = view.update_in(visual, |v, window, cx| {
            assert!(v.error.is_none(), "{:?}", v.error);
            assert!(matches!(v.surface, Surface::Context));
            assert_eq!(v.source_snapshot.as_ref(), Some(&source));
            let state = &v.context_ui.goals[&goal];
            assert_eq!(
                state.guidance.read(cx).value().as_ref(),
                "Retain unsent guidance"
            );
            assert_eq!(state.query.read(cx).value().as_ref(), "Retain query");
            assert!(state.pinned.contains("local"));
            assert_eq!(state.packet, saved);
            assert!(state.chosen.values().any(|c| c["path"] == source["path"]));
            let form = v.source_context_form(&goal, cx).unwrap();
            v.use_source_in_context(window, cx);
            form
        });
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            assert_eq!(v.source_context_form(&goal, cx), Some(before));
            assert_eq!(v.notice, "Already included in Context");
        });
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let requests = server.join().unwrap();
        assert_eq!(
            requests.iter().filter(|r| r["op"] == "source_read").count(),
            2
        );
        assert_eq!(
            requests.iter().filter(|r| r["op"] == "context_get").count(),
            1
        );
        assert!(requests
            .iter()
            .all(|r| r["expected_workspace"] == workspace));
        assert!(!directory.exists());
    }
    #[gpui::test]
    fn source_context_retained_adoption_owns_only_its_goal(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let goal = uuid();
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/source-context","records_dir":"records","managed":true});
        let directory = std::env::temp_dir().join(format!("source-context265-{}", uuid()));
        let store = editor_recovery::EditorRecovery::at(directory.clone(), &workspace).unwrap();
        let source = source_context_test_source(&workspace);
        let (endpoint, stop, server) =
            source_context_test_server(source.clone(), packet(&goal, true));
        let (view, visual) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&workspace, &store, window, cx)
        });
        visual.run_until_parked();
        let pending = view.update_in(visual, |v, window, cx| {
            v.endpoint = endpoint;
            v.busy = false;
            v.snapshot = json!({"goal":{"id":goal}});
            v.selected_goal_id = Some(goal.clone());
            v.capabilities["reviewed_context"] = json!(true);
            v.load_source(source.clone(), window, cx);
            v.ensure_context(window, cx);
            let mut pending: proposal_adoption_outbox::Pending =
                serde_json::from_str(include_str!("fixtures/adoption-context-pending.json"))
                    .unwrap();
            pending.workspace = workspace.clone();
            pending.request["goal_id"] = json!(goal);
            pending.goal_at_submit = goal.clone();
            v.adoption.pending = vec![pending.clone()];
            v.adoption.active = false;
            assert!(v.context_ui.proposals.is_empty());
            let before = v.source_context_form(&goal, cx);
            v.use_source_in_context(window, cx);
            assert!(!v.busy && !v.source_loading);
            assert!(v.error.as_ref().unwrap().contains("adoption or recovery"));
            assert_eq!(v.source_context_form(&goal, cx), before);
            assert_eq!(v.source_snapshot.as_ref(), Some(&source));
            assert_eq!(v.adoption.pending, vec![pending.clone()]);
            pending
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert_eq!(v.adoption.pending, vec![pending.clone()]);
            // Retained delivery for another goal cannot own this local form.
            v.adoption.pending[0].request["goal_id"] = json!(uuid());
            v.use_source_in_context(window, cx);
            assert!(v.busy && v.source_loading);
        });
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            assert!(v.error.is_none(), "{:?}", v.error);
            assert!(v.context_ui.goals[&goal]
                .chosen
                .values()
                .any(|c| c["path"] == source["path"]));
            assert_eq!(v.adoption.pending.len(), 1);
            assert_eq!(v.source_snapshot.as_ref(), Some(&source));
            assert_eq!(v.source.value(cx).as_ref(), v.source_original);
        });
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let requests = server.join().unwrap();
        assert_eq!(
            requests.iter().filter(|r| r["op"] == "source_read").count(),
            1
        );
        assert_eq!(
            requests.iter().filter(|r| r["op"] == "context_get").count(),
            1
        );
        assert!(!directory.exists());
    }
    #[gpui::test]
    fn source_context_late_owner_or_dirty_source_never_changes_either_draft(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        for change in ["goal", "workspace", "source", "context", "disk"] {
            let goal = uuid();
            let workspace = json!({"brain_id":uuid(),"root":"/isolated/source-context","records_dir":"records","managed":true});
            let dir = std::env::temp_dir().join(format!("source-context265-{}", uuid()));
            let store = editor_recovery::EditorRecovery::at(dir.clone(), &workspace).unwrap();
            let source = source_context_test_source(&workspace);
            let mut current = source.clone();
            if change == "disk" {
                current["revision"] = json!("changed");
            }
            let (endpoint, stop, server) = source_context_test_server(current, packet(&goal, true));
            let (view, visual) = cx.add_window_view(|window, cx| {
                BrainView::new_managed_test(&workspace, &store, window, cx)
            });
            visual.run_until_parked();
            view.update_in(visual, |v, window, cx| {
                v.endpoint = endpoint;
                v.busy = false;
                v.snapshot = json!({"goal":{"id":goal}});
                v.selected_goal_id = Some(goal.clone());
                v.capabilities["reviewed_context"] = json!(true);
                v.load_source(source.clone(), window, cx);
                v.ensure_context(window, cx);
                v.use_source_in_context(window, cx);
                assert!(v.busy);
                match change {
                    "goal" => v.snapshot["goal"]["id"] = json!(uuid()),
                    "workspace" => {
                        v.expected_workspace.as_mut().unwrap()["brain_id"] = json!(uuid())
                    }
                    "source" => v.source.reset("New local draft", window, cx),
                    "context" => v.context_ui.goals[&goal]
                        .guidance
                        .update(cx, |i, cx| i.set_value("Later guidance", window, cx)),
                    _ => {}
                }
            });
            visual.run_until_parked();
            view.update_in(visual, |v, _, cx| {
                assert!(v.error.is_some());
                assert!(v.context_ui.goals[&goal].chosen.is_empty());
                assert!(v.context_ui.goals[&goal].packet.is_null());
                assert_eq!(v.source_snapshot.as_ref(), Some(&source));
                if change == "source" {
                    assert_eq!(v.source.value(cx).as_ref(), "New local draft");
                }
                if change == "context" {
                    assert_eq!(
                        v.context_ui.goals[&goal].guidance.read(cx).value().as_ref(),
                        "Later guidance"
                    );
                }
            });
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let requests = server.join().unwrap();
            assert_eq!(
                requests.iter().filter(|r| r["op"] == "source_read").count(),
                1
            );
        }
    }
    #[gpui::test]
    fn source_context_late_observation_history_preserves_both_drafts(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        for change in [
            "goal",
            "workspace",
            "source",
            "context",
            "disk",
            "history",
            "recovery",
            "provenance",
        ] {
            let goal = uuid();
            let workspace = json!({"brain_id":uuid(),"root":"/isolated/source-context","records_dir":"records","managed":true});
            let dir = std::env::temp_dir().join(format!("source-context265-{}", uuid()));
            let store = editor_recovery::EditorRecovery::at(dir.clone(), &workspace).unwrap();
            let (source, history) = source_context::tests::observation_fixture(&workspace, &goal);
            let mut current = source.clone();
            if change == "disk" {
                current["revision"] = json!("changed");
            }
            let (endpoint, stop, server) = source_context_test_server(current, packet(&goal, true));
            let (view, visual) = cx.add_window_view(|window, cx| {
                BrainView::new_managed_test(&workspace, &store, window, cx)
            });
            visual.run_until_parked();
            view.update_in(visual, |v, window, cx| {
                v.endpoint = endpoint;
                v.busy = false;
                v.snapshot = json!({"goal":{"id":goal},"maestro":history});
                v.selected_goal_id = Some(goal.clone());
                v.capabilities["reviewed_context"] = json!(true);
                v.load_source(source.clone(), window, cx);
                v.ensure_context(window, cx);
                v.use_source_in_context(window, cx);
                assert!(v.busy);
                match change {
                    "goal" => v.snapshot["goal"]["id"] = json!(uuid()),
                    "workspace" => {
                        v.expected_workspace.as_mut().unwrap()["brain_id"] = json!(uuid())
                    }
                    "source" => v.source.reset("New local draft", window, cx),
                    "context" => v.context_ui.goals[&goal]
                        .guidance
                        .update(cx, |i, cx| i.set_value("Later guidance", window, cx)),
                    "history" => v.snapshot["maestro"]["history"][0]["observation_ids"] = json!([]),
                    "recovery" => v.snapshot["maestro"]["recovery_required"] = json!(true),
                    "provenance" => {
                        v.snapshot["maestro"]["history"][0]["repo"] = json!("other/repo")
                    }
                    _ => {}
                }
            });
            visual.run_until_parked();
            view.update_in(visual, |v, _, cx| {
                assert!(v.error.is_some());
                assert!(v.context_ui.goals[&goal].chosen.is_empty());
                assert!(v.context_ui.goals[&goal].packet.is_null());
                assert_eq!(v.source_snapshot.as_ref(), Some(&source));
                if change == "source" {
                    assert_eq!(v.source.value(cx).as_ref(), "New local draft");
                }
                if change == "context" {
                    assert_eq!(
                        v.context_ui.goals[&goal].guidance.read(cx).value().as_ref(),
                        "Later guidance"
                    );
                }
            });
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let requests = server.join().unwrap();
            assert_eq!(
                requests.iter().filter(|r| r["op"] == "source_read").count(),
                1
            );
        }
    }
    #[gpui::test]
    fn reuse_refresh_updates_manual_summary_without_replacing_dirty_guidance_or_pins(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.snapshot = json!({"goal":{"id":"goal"}});
            view.context_reply("context_get", json!({"packet":packet("goal",true)}),window,cx);
            let chosen = view.context_ui.goals["goal"].chosen.clone();
            let pinned = view.context_ui.goals["goal"].pinned.clone();
            view.context_ui.goals["goal"].guidance.update(cx,|input,cx| input.set_value("Keep unsent guidance",window,cx));
            let brief=json!({"schema":"tessera-goal-brief/v1","goal_id":"goal","inputs":[],"manual_only":[{"id":"decision","path":"records/decision.md","status":"manual_only"}]});
            view.context_reply("goal_context_brief",brief.clone(),window,cx);
            let mut stale=packet("goal",true);stale["stale"]=json!(true);stale["text"]=json!("Saved text from server");stale["citations"]=json!([]);
            view.context_reply("context_get",json!({"packet":stale}),window,cx);
            let state=&view.context_ui.goals["goal"];
            assert_eq!(state.brief,brief);
            assert!(state.brief["inputs"].as_array().unwrap().is_empty());
            assert_eq!(state.guidance.read(cx).value().as_ref(),"Keep unsent guidance");
            assert_eq!(state.chosen,chosen);
            assert_eq!(state.pinned,pinned);
            assert_eq!(state.packet["stale"],true);
            view
        });
    }
    #[gpui::test]
    fn export_is_polled_on_a_goal_without_chat_or_stage_results(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.snapshot = json!({"goal":{"id":"goal"}});
            view.context_reply("context_get", json!({"packet":packet("goal",true),"export_job":{"job_id":"job","status":"running"}}), window, cx);
            assert!(view.conversation_id.is_none());
            let generation = view.request_generation;
            view.finish_batch(vec![("snapshot".into(), Ok(view.snapshot.clone()))], true, generation, window, cx);
            assert!(view.poll_in_flight, "An unchanged snapshot must still trigger export detail polling");
            view
        });
    }
    #[gpui::test]
    fn late_build_preserves_guidance_edited_during_request(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.snapshot = json!({"goal":{"id":"goal"}});
            view.ensure_context(window, cx);
            let state = view.context_ui.goals.get_mut("goal").unwrap();
            state.pending_text = Some("Earlier draft".into());
            state
                .guidance
                .update(cx, |input, cx| input.set_value("Newer draft", window, cx));
            view.context_reply(
                "context_prepare",
                json!({"packet":packet("goal",false)}),
                window,
                cx,
            );
            assert_eq!(
                view.context_ui.goals["goal"]
                    .guidance
                    .read(cx)
                    .value()
                    .as_ref(),
                "Newer draft"
            );
            assert!(view.reviewed_context(cx).is_err());
            assert_eq!(view.context_ui.goals["goal"].packet["id"], "packet");
            view
        });
    }
    #[gpui::test]
    fn taskless_planning_dispatches_only_with_reviewed_context(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.snapshot = json!({"goal":{"id":"goal","criteria":[]},"task":null});
            view.next_step.update(cx, |input, cx| {
                input.set_value("Return a sourced plan only", window, cx)
            });
            view.prepare(window, cx);
            assert!(!view.busy);
            assert!(view
                .error
                .as_ref()
                .unwrap()
                .contains("Create or link a task"));
            view.error = None;
            view.capabilities["reviewed_context"] = json!(true);
            view.context_reply(
                "context_get",
                json!({"packet":packet("goal",true)}),
                window,
                cx,
            );
            view.prepare(window, cx);
            assert!(
                view.busy,
                "reviewed taskless planning reaches stage_prepare"
            );
            assert!(view.error.is_none());
            view
        });
    }
    #[gpui::test]
    fn source_back_actual_rpc_keeps_existing_context_draft_and_pins(cx: &mut TestAppContext) {
        use std::sync::atomic::Ordering;
        cx.update(gpui_component::init);
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/back283-rpc","records_dir":"records","managed":true});
        let goal = uuid();
        let directory = std::env::temp_dir().join(format!("back283-rpc-{}", uuid()));
        let store = editor_recovery::EditorRecovery::at(directory.clone(), &workspace).unwrap();
        let a = source_context_test_source(&workspace);
        let mut b = a.clone();
        b["path"] = json!("notes/linked.md");
        let disk = Arc::new(std::sync::Mutex::new(b.clone()));
        let saved = Arc::new(std::sync::Mutex::new(packet(&goal, true)));
        let (endpoint, stop, server) = source_context_changing_test_server(disk.clone(), saved);
        let (view, visual) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&workspace, &store, window, cx)
        });
        visual.run_until_parked();
        let form=view.update_in(visual,|v,window,cx| {
            v.endpoint=endpoint;v.busy=false;v.surface=Surface::Source;v.collection=Collection::Sources;
            v.snapshot=json!({"goal":{"id":goal}});v.load_source(a.clone(),window,cx);
            v.ensure_context(window,cx);
            let context=v.context_ui.goals.get_mut(&goal).unwrap();
            context.guidance.update(cx,|i,cx| i.set_value("Unsent guidance",window,cx));
            context.query.update(cx,|i,cx| i.set_value("Unsent query",window,cx));
            context.chosen.insert("manual".into(),citation("manual"));context.pinned.insert("manual".into());
            let form=v.source_context_form(&goal,cx);
            v.preview_loading=false;
            v.preview=json!({"document_links_version":1,"path":a["path"],"revision":a["revision"],"preview_revision":note_link::digest(v.source.value(cx).as_bytes()),"links":[{"url":"tessera://linked","status":"resolved","candidates":[{"path":"notes/linked.md"}]}]});
            v.preview_link("tessera://linked",window,cx);
            form
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert_eq!(v.source_snapshot.as_ref(), Some(&b));
            assert_eq!(v.source_back_path(), Some("notes/source.md"));
            assert_eq!(v.source_context_form(&goal, cx), form);
            *disk.lock().unwrap() = a.clone();
            v.back_source(window, cx);
        });
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            assert_eq!(v.source_snapshot.as_ref(), Some(&a));
            assert!(v.source_back_path().is_none());
            assert_eq!(v.source_context_form(&goal, cx), form);
        });
        stop.store(true, Ordering::Relaxed);
        let requests = server.join().unwrap();
        assert_eq!(
            requests.iter().filter(|r| r["op"] == "source_read").count(),
            2,
            "positive control: actual reads in both directions"
        );
        assert!(requests.iter().all(|r| matches!(
            r["op"].as_str(),
            Some("source_read" | "source_preview" | "context_get")
        )));
        if directory.exists() {
            std::fs::remove_dir_all(directory).unwrap();
        }
    }
    #[gpui::test]
    fn open_link_actual_button_rpc_and_back_preserve_lp_context(cx: &mut TestAppContext) {
        use std::sync::atomic::Ordering;
        cx.update(gpui_component::init);
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/open285-rpc","records_dir":"records","managed":true});
        let goal = uuid();
        let directory = std::env::temp_dir().join(format!("open285-rpc-{}", uuid()));
        let store = editor_recovery::EditorRecovery::at(directory.clone(), &workspace).unwrap();
        let raw = "# Origin\n\n[[notes/linked.md|Linked 😀]]\n";
        let a = json!({"schema":SCHEMA,"brain_id":workspace["brain_id"],"path":"notes/source.md","revision":note_link::digest(raw.as_bytes()),"content_base64":STANDARD.encode(raw),"media_type":"text/markdown"});
        let mut b = a.clone();
        b["path"] = json!("notes/linked.md");
        let disk = Arc::new(std::sync::Mutex::new(b.clone()));
        let (endpoint, stop, server) = source_context_changing_test_server(
            disk.clone(),
            Arc::new(std::sync::Mutex::new(packet(&goal, true))),
        );
        let (view, visual) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&workspace, &store, window, cx)
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            v.endpoint = endpoint;
            v.busy = false;
            v.surface = Surface::Source;
            v.collection = Collection::Sources;
            v.snapshot = json!({"goal":{"id":goal}});
            v.load_source(a.clone(), window, cx);
            v.ensure_context(window, cx);
            v.source_projection.live = true;
            v.schedule_source_projection(window, cx);
            let start = raw.find("Linked").unwrap();
            v.source
                .managed()
                .unwrap()
                .update(cx, |s, cx| s.set_selected_range(start..start, cx));
            let c = v.context_ui.goals.get_mut(&goal).unwrap();
            c.query
                .update(cx, |i, cx| i.set_value("Local query", window, cx));
            c.chosen.insert("manual".into(), citation("manual"));
            c.pinned.insert("manual".into());
        });
        visual.run_until_parked();
        let form=view.update_in(visual,|v,_,cx| {
            v.preview_loading=false;v.preview_error=None;v.preview=json!({"document_links_version":1,"path":a["path"],"revision":a["revision"],"preview_revision":note_link::digest(raw.as_bytes()),"links":[{"wiki":true,"target":"notes/linked.md","authored_target":"notes/linked.md","url":"tessera://open/notes/linked.md","status":"resolved","candidates":[{"path":"notes/linked.md"}]}]});
            v.accept_open_link_preview(v.open_link_preview_owner(cx),v.preview_generation,cx);v.apply_source_projection(cx);cx.notify();v.source_context_form(&goal,cx)
        });
        visual.run_until_parked();
        let button = visual
            .debug_bounds("source-open-link")
            .expect("actual Open link action painted");
        visual.simulate_click(button.center(), Modifiers::default());
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert_eq!(v.source_snapshot.as_ref(), Some(&b));
            assert_eq!(v.source_back_path(), Some("notes/source.md"));
            assert_eq!(v.source_context_form(&goal, cx), form);
            *disk.lock().unwrap() = a.clone();
            v.back_source(window, cx);
        });
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            assert_eq!(v.source_snapshot.as_ref(), Some(&a));
            assert!(v.source_projection.live);
            assert_eq!(v.source_context_form(&goal, cx), form);
            assert!(v.source_back_path().is_none());
        });
        stop.store(true, Ordering::Relaxed);
        let requests = server.join().unwrap();
        assert_eq!(
            requests.iter().filter(|r| r["op"] == "source_read").count(),
            2,
            "actual bidirectional RPC positive control"
        );
        assert!(requests.iter().all(|r| matches!(
            r["op"].as_str(),
            Some("source_read" | "source_preview" | "context_get")
        )));
        if directory.exists() {
            std::fs::remove_dir_all(directory).unwrap();
        }
    }
    #[gpui::test]
    fn source_return_renews_preview_and_actual_open_preserves_lp_context(cx: &mut TestAppContext) {
        source_return_open_case(cx, false);
    }
    #[gpui::test]
    fn heading_link_actual_open_refuses_missing_duplicates_then_lands_and_back(
        cx: &mut TestAppContext,
    ) {
        source_return_open_case(cx, true);
    }
    #[gpui::test]
    fn markdown_heading_actual_open_refuses_then_lands_and_back(cx: &mut TestAppContext) {
        source_return_open_case_kind(cx, true, true);
    }
    fn source_return_open_case(cx: &mut TestAppContext, heading: bool) {
        source_return_open_case_kind(cx, heading, false);
    }
    fn source_return_open_case_kind(cx: &mut TestAppContext, heading: bool, markdown: bool) {
        use std::sync::atomic::Ordering;
        cx.update(gpui_component::init);
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/source-return289-rpc","records_dir":"records","managed":true});
        let goal = uuid();
        let directory = std::env::temp_dir().join(format!("source-return289-rpc-{}", uuid()));
        let store = editor_recovery::EditorRecovery::at(directory.clone(), &workspace).unwrap();
        let raw = if markdown {
            "# Origin\n\n[Linked 😀](linked.md#Decision%20C%23%20😀)\n"
        } else if heading {
            "# Origin\n\n[[notes/linked.md#Decision C# 😀|Linked 😀]]\n"
        } else {
            "# Origin\n\n[[notes/linked.md|Linked 😀]]\n"
        };
        let a = json!({"schema":SCHEMA,"brain_id":workspace["brain_id"],"path":"notes/source.md","revision":note_link::digest(raw.as_bytes()),"content_base64":STANDARD.encode(raw),"media_type":"text/markdown"});
        let mut b = a.clone();
        b["path"] = json!("notes/linked.md");
        let target_raw = format!(
            "# Target\r\n\r\n{}## **Decision** `C#` 😀 ###\r\nExact section\r\n{}",
            "Paragraph before destination.\r\n\r\n".repeat(60),
            "Following section content.\r\n\r\n".repeat(20)
        );
        let target_offset = target_raw.find("## **Decision").unwrap();
        if heading {
            b["content_base64"] = json!(STANDARD.encode(&target_raw));
            b["revision"] = json!(note_link::digest(target_raw.as_bytes()));
        }
        let disk = Arc::new(std::sync::Mutex::new(b.clone()));
        let preview = json!({"document_links_version":1,"path":a["path"],"revision":a["revision"],"preview_revision":note_link::digest(raw.as_bytes()),"markdown":raw,"assets":[],"links":[{"wiki":!markdown,"target":if markdown { "linked" } else { "notes/linked.md" },"authored_target":if markdown { "linked.md#Decision%20C%23%20😀" } else if heading { "notes/linked.md#Decision C# 😀" } else { "notes/linked.md" },"heading":if heading { Some("Decision C# 😀") } else { None },"url":if heading { "tessera://open/notes/linked.md#Decision%20C%23%20😀" } else { "tessera://open/notes/linked.md" },"status":if heading { "resolved_heading" } else { "resolved" },"candidates":[{"path":"notes/linked.md"}]}]});
        let (endpoint, stop, server) = source_context_server_with_preview(
            disk.clone(),
            Arc::new(std::sync::Mutex::new(packet(&goal, true))),
            Some(preview),
        );
        let (view, visual) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&workspace, &store, window, cx)
        });
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(250));
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            v.endpoint = endpoint;
            v.busy = false;
            v.surface = Surface::Source;
            v.collection = Collection::Sources;
            v.snapshot = json!({"goal":{"id":goal}});
            v.load_source(a.clone(), window, cx);
            v.ensure_context(window, cx);
            v.source_projection.live = true;
            v.schedule_source_projection(window, cx);
            let start = raw.find("Linked").unwrap();
            v.source
                .managed()
                .unwrap()
                .update(cx, |s, cx| s.set_selected_range(start..start, cx));
            let c = v.context_ui.goals.get_mut(&goal).unwrap();
            c.query
                .update(cx, |i, cx| i.set_value("Local query", window, cx));
            c.chosen.insert("manual".into(), citation("manual"));
            c.pinned.insert("manual".into());
            c.selection_changed = true;
        });
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(250));
        visual.run_until_parked();
        let (form, stamp, selection, entity) = view.update_in(visual, |v, window, cx| {
            assert!(!v.preview_loading);
            assert!(v.preview_error.is_none(), "{:?}", v.preview_error);
            v.apply_source_projection(cx);
            let before = (
                v.source_context_form(&goal, cx),
                v.source.stamp(cx),
                v.source.managed().unwrap().read(cx).selected_range(),
                v.source.managed().unwrap().entity_id(),
            );
            v.note_navigate_surface(Surface::Context, window, cx);
            before
        });
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(250));
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert!(matches!(v.surface, Surface::Context));
            v.note_navigate_surface(Surface::Source, window, cx);
        });
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(250));
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            assert_eq!(v.source_snapshot.as_ref(), Some(&a));
            assert_eq!(v.source.stamp(cx), stamp);
            assert_eq!(
                v.source.managed().unwrap().read(cx).selected_range(),
                selection
            );
            assert_eq!(v.source.managed().unwrap().entity_id(), entity);
            assert!(v.source_projection.live);
            assert_eq!(v.source_context_form(&goal, cx), form);
        });
        if heading {
            for invalid in [
                "# No matching section\n",
                "# Decision C# 😀\n\n## **decision** `c#` 😀\n",
            ] {
                let mut refused = b.clone();
                refused["content_base64"] = json!(STANDARD.encode(invalid));
                refused["revision"] = json!(note_link::digest(invalid.as_bytes()));
                *disk.lock().unwrap() = refused;
                let button = visual.debug_bounds("source-open-link").unwrap();
                visual.simulate_click(button.center(), Modifiers::default());
                visual.run_until_parked();
                view.update_in(visual, |v, _, cx| {
                    assert_eq!(v.source_snapshot.as_ref(), Some(&a));
                    assert_eq!(
                        v.source.managed().unwrap().read(cx).selected_range(),
                        selection
                    );
                    assert_eq!(v.source_context_form(&goal, cx), form);
                    assert!(v.source_back_path().is_none());
                    assert!(v.error.is_some());
                });
            }
            *disk.lock().unwrap() = b.clone();
        }
        let button = visual
            .debug_bounds("source-open-link")
            .expect("actual Open link action painted");
        visual.simulate_click(button.center(), Modifiers::default());
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(250));
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert_eq!(v.source_snapshot.as_ref(), Some(&b), "{:?}", v.error);
            if heading {
                assert!(v.source_projection.live);
                assert_eq!(
                    v.source.managed().unwrap().read(cx).selected_range(),
                    target_offset..target_offset
                );
                let state = v.source.managed().unwrap().read(cx);
                let (caret, line_height) = state.cursor_layout().unwrap();
                let painted_y = caret.origin.y + state.scroll_offset().y;
                let top = state.input_bounds().origin.y;
                assert!((painted_y - top - line_height * 2.).abs() < px(1.),
                    "after-paint placement must execute, not merely ordinary reveal: y={painted_y:?}, top={top:?}");
                assert_ne!(
                    v.source.managed().unwrap().read(cx).scroll_offset().y,
                    px(0.)
                );
            }
            assert_eq!(v.source_back_path(), Some("notes/source.md"));
            assert_eq!(v.source_context_form(&goal, cx), form);
            *disk.lock().unwrap() = a.clone();
            v.back_source(window, cx);
        });
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(250));
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            assert_eq!(v.source_snapshot.as_ref(), Some(&a));
            assert!(v.source_projection.live);
            assert_eq!(v.source_context_form(&goal, cx), form);
            assert!(v.source_back_path().is_none());
        });
        stop.store(true, Ordering::Relaxed);
        let requests = server.join().unwrap();
        assert_eq!(
            requests.iter().filter(|r| r["op"] == "source_read").count(),
            if heading { 4 } else { 2 },
            "actual bidirectional RPC and heading refusal controls"
        );
        assert_eq!(
            requests
                .iter()
                .filter(|r| r["op"] == "source_preview" && r["path"] == a["path"])
                .count(),
            3,
            "initial load, real Source return and Back each request their own preview"
        );
        assert!(requests.iter().all(|r| matches!(
            r["op"].as_str(),
            Some("source_read" | "source_preview" | "context_get" | "brain_index_status")
        )));
        if directory.exists() {
            std::fs::remove_dir_all(directory).unwrap();
        }
    }
}
