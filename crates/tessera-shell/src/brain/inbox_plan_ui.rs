//! Explicit goal planning from an immutable saved thought. No provider or engine
//! action is part of this command; an uncertain reply retains the exact request.
use super::native_outbox::InboxJournal;
use super::*;

fn plan_request(
    journal: &InboxJournal,
    origin: &Value,
    title: &str,
    criteria: &str,
    human: bool,
    actor: &str,
    current_goal: &str,
) -> Result<Value, String> {
    if !origin["text"].is_string()
        || origin["item"]["capture_id"]
            .as_str()
            .and_then(|v| Uuid::parse_str(v).ok())
            .is_none()
        || origin["source"]["brain_id"] != journal.workspace["brain_id"]
        || origin["source"]["path"] != origin["item"]["path"]
        || origin["source"]["revision"] != origin["item"]["revision"]
        || !text(&origin["item"]["revision"]).starts_with("sha256:")
    {
        return Err("Open the exact saved thought before planning it.".into());
    }
    let title = title.trim();
    let descriptions: Vec<&str> = criteria
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if title.is_empty() || descriptions.is_empty() {
        return Err("Add a goal title and at least one observable result criterion.".into());
    }
    // A locally invalid draft must stay editable; retaining it would turn a
    // correct backend rejection into an unrecoverable immutable request.
    if title.len() > 512 {
        return Err("Keep the goal title within 512 UTF-8 bytes.".into());
    }
    if descriptions.len() > 50
        || descriptions
            .iter()
            .any(|description| description.len() > 4096)
    {
        return Err("Use at most 50 criteria, each within 4,096 UTF-8 bytes.".into());
    }
    let criteria: Vec<Value> = descriptions
        .into_iter()
        .map(|description| json!({"id":uuid(),"description":description,"requires_human":human}))
        .collect();
    let mut request = journal.request(title, actor)?;
    request["op"] = json!("inbox_plan");
    request.as_object_mut().unwrap().remove("text");
    request["capture_id"] = origin["item"]["capture_id"].clone();
    request["expected_capture_revision"] = origin["item"]["revision"].clone();
    request["title"] = json!(title.trim());
    request["criteria"] = json!(criteria);
    // These local fields are retained for restart/review, stripped before TCP.
    request["_inbox_plan_origin"] = origin.clone();
    request["_inbox_plan_goal_at_submit"] = json!(current_goal);
    Ok(request)
}

pub(super) struct InboxPlanUi {
    journal: Option<InboxJournal>,
    pending: Vec<Value>,
    archived: Vec<Value>,
    origin: Value,
    title: Entity<InputState>,
    criteria: Entity<TextareaState>,
    human: bool,
    drafts: BTreeMap<String, (String, String, bool)>,
    pub(super) form: bool,
    pub(super) error: Option<String>,
    rejected_operation: Option<String>,
    created: Option<Value>,
    adoption_shelf: super::proposal_form_state::DraftShelf<super::proposal_form_state::InboxDraft>,
    adoption_origin: Option<Value>,
    adoption_detail: Value,
    adoption_choice: bool,
    adoption_previous_form: bool,
}
impl InboxPlanUi {
    pub(super) fn new(window: &mut Window, cx: &mut Context<BrainView>) -> Self {
        Self {
            journal: None,
            pending: vec![],
            archived: vec![],
            origin: Value::Null,
            title: cx.new(|cx| {
                InputState::new(window, cx).placeholder("What would you like to achieve?")
            }),
            criteria: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .rows(4)
                    .placeholder("Observable outcome — one criterion per line")
            }),
            human: false,
            drafts: BTreeMap::new(),
            form: false,
            error: None,
            rejected_operation: None,
            created: None,
            adoption_shelf: Default::default(),
            adoption_origin: None,
            adoption_detail: Value::Null,
            adoption_choice: false,
            adoption_previous_form: false,
        }
    }
    pub(super) fn remember(&mut self, cx: &Context<BrainView>) {
        if self.adoption_origin.is_some() {
            let _ = self
                .adoption_shelf
                .edit(super::proposal_form_state::InboxDraft {
                    title: self.title.read(cx).value().to_string(),
                    criteria: self.criteria.read(cx).value().to_string(),
                    human: self.human,
                });
            return;
        }
        if let Some(id) = self.origin["item"]["capture_id"].as_str() {
            self.drafts.insert(
                id.into(),
                (
                    self.title.read(cx).value().to_string(),
                    self.criteria.read(cx).value().to_string(),
                    self.human,
                ),
            );
        }
    }
    fn restore(&mut self, request: &Value, window: &mut Window, cx: &mut Context<BrainView>) {
        self.origin = request["_inbox_plan_origin"].clone();
        self.title.update(cx, |input, cx| {
            input.set_value(text(&request["title"]), window, cx)
        });
        let body = array(&request["criteria"])
            .iter()
            .map(|c| text(&c["description"]))
            .collect::<Vec<_>>()
            .join("\n");
        self.criteria
            .update(cx, |input, cx| input.set_value(body, window, cx));
        self.human = array(&request["criteria"])
            .iter()
            .any(|c| c["requires_human"] == true);
    }
}
impl BrainView {
    pub(super) fn begin_proposal_inbox(
        &mut self,
        detail: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_inbox_plan(window, cx);
        if !self.inbox_plan.pending.is_empty() {
            self.adoption.error =
                Some("Recover the existing manual goal creation before using a suggestion.".into());
            self.adoption.active = false;
            return;
        }
        let Some(workspace) = self.expected_workspace.clone() else {
            return;
        };
        let manual = super::proposal_form_state::InboxDraft {
            title: self.inbox_plan.title.read(cx).value().to_string(),
            criteria: self.inbox_plan.criteria.read(cx).value().to_string(),
            human: self.inbox_plan.human,
        };
        let prefill =
            match super::proposal_form_state::inbox_prefill(&detail, &workspace, manual.human) {
                Ok(v) => v,
                Err(e) => {
                    self.adoption.error = Some(e);
                    self.adoption.active = false;
                    return;
                }
            };
        let original_text = match decode_source(&detail["record"]["captured"]["trigger_source"]) {
            Ok(s) => s,
            Err(e) => {
                self.adoption.error = Some(e.into());
                self.adoption.active = false;
                return;
            }
        };
        self.inbox_plan.remember(cx);
        let dirty = !manual.criteria.is_empty() || !manual.title.is_empty();
        let key = format!(
            "{}:{}",
            text(&detail["record"]["id"]),
            text(&detail["source"]["revision"])
        );
        let baseline = json!({"workspace":workspace,"manual_origin":self.inbox_plan.origin,"capture":detail["record"]["trigger"]});
        let opened = self
            .inbox_plan
            .adoption_shelf
            .open(key, baseline, manual, prefill, dirty);
        let form = match opened {
            Ok(v) => v,
            Err(e) => {
                self.adoption.error = Some(e);
                self.adoption.active = false;
                return;
            }
        };
        self.inbox_plan.adoption_previous_form = self.inbox_plan.form;
        self.inbox_plan.adoption_origin = Some(self.inbox_plan.origin.clone());
        self.inbox_plan.adoption_detail = detail.clone();
        self.inbox_plan.adoption_choice = form.is_none();
        let trigger = &detail["record"]["trigger"];
        self.inbox_plan.origin = json!({"text":original_text,"item":{"capture_id":trigger["identity"]["record_id"],"path":trigger["source_path"],"revision":trigger["identity"]["source_revision"]},"source":{"brain_id":workspace["brain_id"],"path":trigger["source_path"],"revision":trigger["identity"]["source_revision"]}});
        self.collection = Collection::Inbox;
        self.proposals.leave();
        self.inbox_plan.form = true;
        self.inbox_plan.error = None;
        if let Some(form) = form {
            self.apply_proposal_inbox(form, window, cx);
        }
    }
    fn apply_proposal_inbox(
        &mut self,
        form: super::proposal_form_state::InboxDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.inbox_plan
            .title
            .update(cx, |i, cx| i.set_value(form.title, window, cx));
        self.inbox_plan
            .criteria
            .update(cx, |i, cx| i.set_value(form.criteria, window, cx));
        self.inbox_plan.human = form.human;
    }
    fn choose_proposal_inbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(form) = self.inbox_plan.adoption_shelf.choose_suggestion() {
            self.apply_proposal_inbox(form, window, cx);
            self.inbox_plan.adoption_choice = false;
            cx.notify();
        }
    }
    pub(super) fn finish_proposal_inbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inbox_plan.adoption_origin.is_none() {
            return;
        }
        self.inbox_plan.remember(cx);
        if let Some(form) = self.inbox_plan.adoption_shelf.cancel() {
            self.apply_proposal_inbox(form, window, cx);
        }
        self.inbox_plan.origin = self.inbox_plan.adoption_origin.take().unwrap();
        self.inbox_plan.adoption_detail = Value::Null;
        self.inbox_plan.adoption_choice = false;
        self.inbox_plan.form = self.inbox_plan.adoption_previous_form;
    }
    pub(super) fn restore_proposal_inbox_fields(
        &mut self,
        fields: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.inbox_plan.adoption_origin.is_none() {
            return;
        }
        let form = super::proposal_form_state::InboxDraft {
            title: text(&fields["title"]),
            criteria: array(&fields["criteria"])
                .iter()
                .map(|c| text(&c["description"]))
                .collect::<Vec<_>>()
                .join("\n"),
            human: array(&fields["criteria"])
                .iter()
                .any(|c| c["requires_human"] == true),
        };
        self.inbox_plan.adoption_shelf.choose_suggestion();
        let _ = self.inbox_plan.adoption_shelf.edit(form.clone());
        self.inbox_plan.adoption_choice = false;
        self.apply_proposal_inbox(form, window, cx);
    }
    fn submit_proposal_inbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inbox_plan.adoption_choice {
            return;
        }
        self.inbox_plan.remember(cx);
        let prepared = self
            .inbox_plan
            .journal
            .as_ref()
            .ok_or("Planning journal unavailable".to_string())
            .and_then(|j| {
                plan_request(
                    j,
                    &self.inbox_plan.origin,
                    self.inbox_plan.title.read(cx).value().as_ref(),
                    self.inbox_plan.criteria.read(cx).value().as_ref(),
                    self.inbox_plan.human,
                    &text(&self.capabilities["actor"]),
                    &self.goal_id(),
                )
            });
        match prepared {
            Ok(r) => {
                let fields = json!({"destination":"inbox","capture_id":r["capture_id"],"expected_capture_revision":r["expected_capture_revision"],"title":r["title"],"criteria":r["criteria"]});
                self.submit_proposal_adoption(
                    self.inbox_plan.adoption_detail.clone(),
                    fields,
                    window,
                    cx,
                );
            }
            Err(e) => self.adoption.error = Some(e),
        }
        cx.notify();
    }
    pub(super) fn ensure_inbox_plan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inbox_plan.journal.is_some() {
            return;
        }
        let Some(workspace) = self.expected_workspace.as_ref() else {
            return;
        };
        match InboxJournal::open_planning(workspace)
            .and_then(|j| Ok((j.pending()?, j.rejected()?, j)))
        {
            Ok((pending, archived, journal)) => {
                self.inbox_plan.archived = archived;
                if let Some(first) = pending.first() {
                    self.inbox_plan.restore(first, window, cx);
                }
                self.inbox_plan.pending = pending;
                self.inbox_plan.journal = Some(journal);
            }
            Err(error) => self.inbox_plan.error = Some(error),
        }
    }
    pub(super) fn begin_inbox_plan(
        &mut self,
        origin: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.inbox_plan.adoption_origin.is_some() {
            self.adoption.error =
                Some("Keep or cancel the suggestion form before opening another thought.".into());
            cx.notify();
            return;
        }
        if self.busy || self.capabilities["inbox_plan"] != true {
            return;
        }
        self.ensure_inbox_plan(window, cx);
        if !self.inbox_plan.pending.is_empty() {
            self.recover_inbox_plan_form(window, cx);
            return;
        }
        if self.dirty(cx) {
            self.error =
                Some("Save or discard the source draft before planning another goal.".into());
            cx.notify();
            return;
        }
        self.inbox_plan.remember(cx);
        let fields = self
            .inbox_plan
            .drafts
            .get(&text(&origin["item"]["capture_id"]))
            .cloned()
            .unwrap_or_else(|| (text(&origin["item"]["title"]), String::new(), false));
        self.inbox_plan.origin = origin;
        self.inbox_plan.human = fields.2;
        self.inbox_plan
            .title
            .update(cx, |input, cx| input.set_value(fields.0, window, cx));
        self.inbox_plan
            .criteria
            .update(cx, |input, cx| input.set_value(fields.1, window, cx));
        self.inbox_plan.form = true;
        self.inbox_plan.created = None;
        self.inbox_plan.error = None;
        self.inbox_plan
            .title
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }
    fn recover_inbox_plan_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Some(request) = self.inbox_plan.pending.first().cloned() {
            self.inbox_plan.remember(cx);
            self.inbox_plan.restore(&request, window, cx);
            self.inbox_plan.form = true;
            self.collection = Collection::Inbox;
            cx.notify();
        }
    }
    fn submit_inbox_plan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inbox_plan.adoption_origin.is_some() {
            self.submit_proposal_inbox(window, cx);
            return;
        }
        if self.busy {
            return;
        }
        self.ensure_inbox_plan(window, cx);
        let request = if let Some(pending) = self.inbox_plan.pending.first() {
            Ok(pending.clone())
        } else if self.capabilities["inbox_plan"] != true {
            Err("Planning is unavailable on this backend; your draft stays here.".into())
        } else if self.dirty(cx) {
            Err("Save or discard the source draft before creating another goal.".into())
        } else if let Some(journal) = &self.inbox_plan.journal {
            plan_request(
                journal,
                &self.inbox_plan.origin,
                self.inbox_plan.title.read(cx).value().as_ref(),
                self.inbox_plan.criteria.read(cx).value().as_ref(),
                self.inbox_plan.human,
                &text(&self.capabilities["actor"]),
                &self.goal_id(),
            )
        } else {
            Err("Reconnect the saved workspace before planning this thought.".into())
        };
        match request {
            Ok(request) => {
                if self.inbox_plan.pending.is_empty() {
                    self.inbox_plan.pending.push(request.clone());
                }
                if let Err(error) = self
                    .inbox_plan
                    .journal
                    .as_ref()
                    .ok_or("Planning recovery is unavailable.".to_string())
                    .and_then(|j| j.retain(&request))
                {
                    self.inbox_plan.error = Some(error);
                    cx.notify();
                    return;
                }
                self.inbox_plan.error = None;
                self.inbox_plan.rejected_operation = None;
                self.batch(vec![request], window, cx);
            }
            Err(error) => {
                self.inbox_plan.error = Some(error);
                cx.notify();
            }
        }
    }
    pub(super) fn inbox_plan_reply(
        &mut self,
        data: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let request = &data["_client_inbox_request"];
        let Some(index) = self.inbox_plan.pending.iter().position(|p| p == request) else {
            return;
        };
        if self.inbox_plan.journal.as_ref().map(|j| &j.workspace)
            != self.expected_workspace.as_ref()
        {
            return;
        }
        if data["_inbox_plan_error"].is_object() {
            let error = &data["_inbox_plan_error"];
            self.inbox_plan.rejected_operation = if error["code"] == "inbox_plan_source_changed" {
                Some(text(&request["operation_id"]))
            } else {
                None
            };
            self.inbox_plan.error = Some(format!(
                "{} The exact planning request and draft are retained.",
                text(&error["message"])
            ));
            return;
        }
        let pending = self.inbox_plan.pending[index].clone();
        if let Err(error) = self
            .inbox_plan
            .journal
            .as_ref()
            .unwrap()
            .acknowledge(&pending, &data)
        {
            self.inbox_plan.error = Some(error);
            return;
        }
        self.inbox_plan.pending.remove(index);
        self.inbox_plan.error = None;
        self.inbox_plan.rejected_operation = None;
        self.inbox_plan.created = Some(data.clone());
        // Changing collection/goal or editing a source while the reply travels
        // prevents surprise navigation. The committed goal remains accessible.
        let navigate = self.inbox_plan.form
            && self.collection == Collection::Inbox
            && !self.dirty(cx)
            && self.goal_id() == text(&pending["_inbox_plan_goal_at_submit"])
            && self.inbox_plan.origin["item"]["capture_id"] == pending["capture_id"];
        if navigate {
            self.inbox_plan.form = false;
            self.select_collection(Collection::Goals, window, cx);
            self.select_goal(text(&data["goal_id"]), window, cx);
        }
        self.notice =
            "Goal created from the original thought. Discussion is ready; no work has started."
                .into();
    }
    fn archive_inbox_plan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(request) = self.inbox_plan.pending.first().cloned() else {
            return;
        };
        if self.inbox_plan.rejected_operation.as_deref() != request["operation_id"].as_str() {
            return;
        }
        match self
            .inbox_plan
            .journal
            .as_ref()
            .ok_or("Planning recovery is unavailable.".to_string())
            .and_then(|j| j.archive_rejected(&request))
        {
            Ok(()) => {
                self.inbox_plan.remember(cx);
                self.inbox_plan.archived.push(request.clone());
                self.inbox_plan.pending.remove(0);
                self.inbox_plan.form = false;
                self.inbox_plan.rejected_operation = None;
                self.inbox_plan.error = None;
                self.open_inbox(text(&request["capture_id"]), window, cx);
            }
            Err(error) => self.inbox_plan.error = Some(error),
        }
        cx.notify();
    }
    pub(super) fn inbox_plan_summary(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut panel = v_flex()
            .gap_2()
            .child(self.adoption_origin_banner(None, cx));
        if !self.inbox_plan.pending.is_empty() {
            panel = panel
                .child("A goal planning request is waiting for exact delivery recovery.")
                .child(
                    super::super::brand::control("inbox-plan-recovery-open", cx)
                        .label("Review pending goal")
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.recover_inbox_plan_form(window, cx)
                        })),
                );
        }
        if let Some(created) = &self.inbox_plan.created {
            let goal = text(&created["goal_id"]);
            panel = panel.child(
                super::super::brand::control("inbox-plan-created-open", cx)
                    .label("Open created goal")
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.inbox_plan.form = false;
                        this.select_collection(Collection::Goals, window, cx);
                        this.select_goal(goal.clone(), window, cx);
                    })),
            );
        }
        if let Some(error) = &self.inbox_plan.error {
            panel = panel.child(error.clone());
        }
        for (index, request) in self.inbox_plan.archived.iter().enumerate() {
            let request = request.clone();
            let title = text(&request["title"]);
            panel = panel
                .child(format!("Retained planning draft: {title}"))
                .child(
                    super::super::brand::control(
                        SharedString::from(format!("inbox-plan-archive-{index}")),
                        cx,
                    )
                    .label("Review current thought with this draft")
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.inbox_plan.restore(&request, window, cx);
                        this.inbox_plan.remember(cx);
                        this.open_inbox(text(&request["capture_id"]), window, cx);
                    })),
                );
        }
        panel.into_any_element()
    }
    pub(super) fn inbox_plan_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let proposal = self.inbox_plan.adoption_origin.is_some();
        let original_thought = text(&self.inbox_plan.origin["text"]);
        let original_thought = if proposal {
            tessera_core::render::without_frontmatter(&original_thought).to_owned()
        } else {
            original_thought
        };
        let pending = !self.inbox_plan.pending.is_empty()
            || self.adoption_pending_for(&text(&self.inbox_plan.adoption_detail["record"]["id"]));
        let blocked = self.inbox_plan.adoption_choice;
        let rejected = self.inbox_plan.pending.first().is_some_and(|p| {
            self.inbox_plan.rejected_operation.as_deref() == p["operation_id"].as_str()
        });
        let mut panel=v_flex().id("inbox-plan-panel").debug_selector(||"inbox-plan-panel".into()).h_full().flex_1().min_w_0().min_h_0().whitespace_normal().overflow_y_scroll().p_6().gap_4()
            .child(div().debug_selector(||"inbox-plan-heading".into()).text_2xl().child("Plan this thought"))
            .child("Original thought — operator input, unverified")
            .child(original_thought)
            .child("The original Inbox record will remain unchanged. Add an observable outcome before creating the goal.")
            .child(Input::new(&self.inbox_plan.title).disabled(self.busy||pending||blocked))
            .child(Textarea::new(&self.inbox_plan.criteria).h(px(120.)).flex_shrink_0().disabled(self.busy||pending||blocked))
            .child(super::super::brand::control("inbox-plan-human",cx).ghost().label(if self.inbox_plan.human{"Requires my confirmation: yes"}else{"Requires my confirmation: no"}).disabled(self.busy||pending||blocked)
                .on_click(cx.listener(|this,_,_,cx|{this.inbox_plan.human = !this.inbox_plan.human;cx.notify();})))
            .child("Creating a goal opens its discussion. It does not send a message, create a task or start work.")
            .child(super::super::brand::control("inbox-plan-submit",cx).primary().label(if proposal{"Adopt into new goal"}else if pending{"Recover goal creation"}else{"Create goal and open discussion"})
                .disabled(self.busy||blocked||(proposal&&pending)||(!pending&&self.capabilities[if proposal{"proposal_adopt"}else{"inbox_plan"}]!=true)).on_click(cx.listener(|this,_,window,cx|this.submit_inbox_plan(window,cx))))
            .child(super::super::brand::control("inbox-plan-close",cx).ghost().label(if proposal{"Cancel suggestion form"}else{"Keep for later"}).disabled(proposal&&pending).on_click(cx.listener(|this,_,window,cx|{if this.inbox_plan.adoption_origin.is_some(){this.finish_proposal_inbox(window,cx);this.adoption.active=false;}else{this.inbox_plan.remember(cx);this.inbox_plan.form=false;}cx.notify();})));
        if proposal {
            let detail = self.inbox_plan.adoption_detail.clone();
            panel = panel.child(self.adoption_origin_banner(Some(&detail), cx));
            if blocked {
                panel = panel
                    .child("An existing manual draft is preserved. Choose which form to edit.")
                    .child(
                        super::super::brand::control("proposal-inbox-choose", cx)
                            .label("Edit suggestion; keep manual draft")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose_proposal_inbox(window, cx)
                            })),
                    );
            }
        }
        if pending {
            panel=panel.child("Delivery is retained. Recovery reuses the same goal request and criteria; no new goal identity is generated.");
        }
        if let Some(error) = &self.inbox_plan.error {
            panel = panel.child(error.clone());
        }
        if rejected {
            panel = panel.child(
                super::super::brand::control("inbox-plan-archive", cx)
                    .label("Keep draft and review the changed thought")
                    .disabled(self.busy)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.archive_inbox_plan(window, cx)),
                    ),
            );
        }
        panel.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    fn workspace() -> Value {
        json!({"brain_id":"cc000000-0000-4000-8000-000000000140","root":"/synthetic/brain","records_dir":"records","managed":true})
    }
    fn origin() -> Value {
        json!({"item":{"capture_id":"dd000000-0000-4000-8000-000000000140","title":"Original title","path":"records/inbox-original.md","revision":"sha256:original"},"text":"  Exact thought\r\nwith trailing spaces  ","source":{"schema":SCHEMA,"brain_id":workspace()["brain_id"],"path":"records/inbox-original.md","revision":"sha256:original","content_base64":STANDARD.encode("original canonical bytes"),"media_type":"text/markdown"},"planned_goals":[]})
    }
    fn receipt(request: &Value) -> Value {
        json!({"goal_id":"ee000000-0000-4000-8000-000000000140","receipt":{"operation_id":request["operation_id"],"status":"committed","request_sha256":"sha256:request","replayed":false},"origin":{"schema":"ai-brain/inbox-origin-v1","brain_id":workspace()["brain_id"],"capture_id":request["capture_id"],"path":request["_inbox_plan_origin"]["item"]["path"],"revision":request["expected_capture_revision"],"operation_id":request["operation_id"],"text":request["_inbox_plan_origin"]["text"],"source_snapshot":request["_inbox_plan_origin"]["source"],"planned_by":request["source"],"planned_at":"2026-09-07T00:00:00Z"},"_client_inbox_request":request})
    }
    #[gpui::test]
    fn proposal_inbox_uses_frozen_source_and_cancel_restores_manual_form(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let dir = std::env::temp_dir().join(uuid());
        let cleanup = dir.clone();
        cx.add_window_view(move|window,cx|{
            let mut v=BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);v.busy=false;
            let workspace=super::super::proposal_outbox::tests::workspace();v.expected_workspace=Some(workspace.clone());
            v.inbox_plan.journal=Some(InboxJournal::at(dir,&workspace).unwrap().for_planning());
            v.inbox_plan.origin=origin();let manual_origin=v.inbox_plan.origin.clone();
            v.inbox_plan.title.update(cx,|i,cx|i.set_value("Manual 雪",window,cx));
            v.inbox_plan.criteria.update(cx,|i,cx|i.set_value("Exact manual criterion  ",window,cx));v.inbox_plan.human=true;
            let mut d=super::super::proposal_outbox::tests::detail();
            let capture="dd000000-0000-4000-8000-000000000194";let path=format!("records/inbox-{capture}.md");let revision=format!("sha256:{}","a".repeat(64));
            d["record"]["trigger"]=json!({"identity":{"kind":"inbox_saved","record_id":capture,"source_revision":revision},"source_path":path});
            d["record"]["generated"]=json!({"title":"Suggested goal","criteria":["Observable result"],"rationale":"test","open_questions":[],"citation_ids":[]});
            d["record"]["captured"]=json!({"trigger_source":{"schema":SCHEMA,"brain_id":workspace["brain_id"],"path":path,"revision":revision,"content_base64":STANDARD.encode("Frozen source 雪  \n")},"citations":[]});
            v.adoption.active=true;v.begin_proposal_inbox(d.clone(),window,cx);
            assert_eq!(v.inbox_plan.origin["text"],"Frozen source 雪  \n");assert!(v.inbox_plan.adoption_choice);
            assert_eq!(v.inbox_plan.title.read(cx).value().as_ref(),"Manual 雪");
            v.choose_proposal_inbox(window,cx);assert_eq!(v.inbox_plan.title.read(cx).value().as_ref(),"Suggested goal");assert!(v.inbox_plan.human);
            v.inbox_plan.title.update(cx,|i,cx|i.set_value("Edited suggestion",window,cx));
            let prepared=plan_request(v.inbox_plan.journal.as_ref().unwrap(),&v.inbox_plan.origin,"Edited suggestion","Observable result",true,"actor","").unwrap();assert_eq!(prepared["capture_id"],capture);
            v.finish_proposal_inbox(window,cx);assert_eq!(v.inbox_plan.origin,manual_origin);assert_eq!(v.inbox_plan.title.read(cx).value().as_ref(),"Manual 雪");assert_eq!(v.inbox_plan.criteria.read(cx).value().as_ref(),"Exact manual criterion  ");
            v.begin_proposal_inbox(d,window,cx);v.choose_proposal_inbox(window,cx);assert_eq!(v.inbox_plan.title.read(cx).value().as_ref(),"Edited suggestion");
            v.finish_proposal_inbox(window,cx);assert!(v.inbox_plan.journal.as_ref().unwrap().pending().unwrap().is_empty());v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
    #[test]
    fn planning_requires_reviewed_criteria_and_preserves_exact_origin_authority() {
        let dir = std::env::temp_dir().join(uuid());
        let journal = InboxJournal::at(dir.clone(), &workspace())
            .unwrap()
            .for_planning();
        assert!(plan_request(&journal, &origin(), "Title", " \n", false, "actor", "old").is_err());
        assert!(plan_request(&journal, &origin(), " ", "Done", false, "actor", "old").is_err());
        let mut wrong = origin();
        wrong["source"]["brain_id"] = json!(uuid());
        assert!(plan_request(&journal, &wrong, "Title", "Done", false, "actor", "old").is_err());
        let request = plan_request(
            &journal,
            &origin(),
            " Reviewed title ",
            "Outcome one\n\nOutcome two",
            true,
            "actor",
            "old",
        )
        .unwrap();
        assert_eq!(request["title"], "Reviewed title");
        assert_eq!(request["_inbox_plan_origin"], origin());
        assert_eq!(request["source"]["actor_id"], "actor");
        assert_eq!(request["source"]["message_id"], request["operation_id"]);
        assert_eq!(array(&request["criteria"]).len(), 2);
        assert_ne!(request["criteria"][0]["id"], request["criteria"][1]["id"]);
        assert_eq!(request["criteria"][0]["requires_human"], true);
        for key in ["goal_id", "task", "engine", "provider", "text", "path"] {
            assert!(request.get(key).is_none(), "unexpected authority: {key}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[gpui::test]
    fn planning_form_stays_within_viewport_and_long_original_can_scroll(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            v.busy = true;
            v.collection = Collection::Inbox;
            v.inbox_plan.form = true;
            v.inbox_plan.origin = origin();
            v.inbox_plan.origin["text"] =
                json!("Original thought line with retained detail.\n".repeat(150));
            v
        });
        cx.run_until_parked();
        let panel = cx
            .debug_bounds("inbox-plan-panel")
            .expect("planning panel is rendered");
        assert!(
            panel.bottom() <= cx.update(|window, _| window.viewport_size().height),
            "long origin must not grow the panel beyond the viewport"
        );
        let heading = cx
            .debug_bounds("inbox-plan-heading")
            .expect("positive control: initial heading is visible");
        cx.simulate_event(ScrollWheelEvent {
            position: panel.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(-300.))),
            ..Default::default()
        });
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("inbox-plan-heading")
                .is_none_or(|after| after.top() < heading.top()),
            "wheel input must scroll the original rather than leaving lower controls unreachable"
        );
    }
    #[gpui::test]
    fn oversized_planning_drafts_remain_editable_without_retaining_an_identity(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = std::env::temp_dir().join(uuid());
        let cleanup = dir.clone();
        cx.add_window_view(move |window, cx| {
            let mut v = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            v.busy = false;
            v.expected_workspace = Some(workspace());
            v.capabilities = json!({"inbox_plan":true,"actor":"actor"});
            v.inbox_plan.journal =
                Some(InboxJournal::at(dir, &workspace()).unwrap().for_planning());
            v.inbox_plan.origin = origin();
            let too_many = vec!["Observable outcome"; 51].join("\n");
            for (title, criteria) in [
                ("é".repeat(257), "Valid result".into()),
                ("Title".into(), "é".repeat(2049)),
                ("Title".into(), too_many),
            ] {
                v.inbox_plan
                    .title
                    .update(cx, |i, cx| i.set_value(title.clone(), window, cx));
                v.inbox_plan
                    .criteria
                    .update(cx, |i, cx| i.set_value(criteria.clone(), window, cx));
                v.submit_inbox_plan(window, cx);
                assert!(!v.busy);
                assert!(v.inbox_plan.pending.is_empty());
                assert!(v.inbox_plan.error.is_some());
                assert_eq!(v.inbox_plan.title.read(cx).value().as_ref(), title);
                assert_eq!(v.inbox_plan.criteria.read(cx).value().as_ref(), criteria);
                assert!(v
                    .inbox_plan
                    .journal
                    .as_ref()
                    .unwrap()
                    .pending()
                    .unwrap()
                    .is_empty());
            }
            let boundary = plan_request(
                v.inbox_plan.journal.as_ref().unwrap(),
                &origin(),
                &"é".repeat(256),
                &vec!["é".repeat(2048); 50].join("\n"),
                false,
                "actor",
                "",
            )
            .unwrap();
            assert_eq!(
                array(&boundary["criteria"]).len(),
                50,
                "positive control: exact UTF-8/count boundaries accepted"
            );
            v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
    #[test]
    fn planning_restart_receipt_binding_and_interrupted_publication_keep_one_identity() {
        let dir = std::env::temp_dir().join(uuid());
        let j = InboxJournal::at(dir.clone(), &workspace())
            .unwrap()
            .for_planning();
        let request = plan_request(
            &j,
            &origin(),
            "Title",
            "Observable outcome",
            false,
            "actor",
            "old",
        )
        .unwrap();
        j.fail_retain_after_publish.set(true);
        assert!(j.retain(&request).is_err());
        let j = InboxJournal::at(dir.clone(), &workspace())
            .unwrap()
            .for_planning();
        assert_eq!(j.pending().unwrap(), vec![request.clone()]);
        j.retain(&request).unwrap();
        assert!(InboxJournal::at(dir.clone(), &workspace())
            .unwrap()
            .pending()
            .unwrap()
            .is_empty());
        assert!(InboxJournal::at(dir.clone(), &workspace())
            .unwrap()
            .for_attention()
            .pending()
            .unwrap()
            .is_empty());
        for path in [
            "brain_id",
            "capture_id",
            "revision",
            "operation_id",
            "text",
            "source_snapshot",
            "planned_by",
        ] {
            let mut bad = receipt(&request);
            bad["origin"][path] = json!("mismatch");
            assert!(
                j.acknowledge(&request, &bad).is_err(),
                "accepted wrong {path}"
            );
            assert_eq!(j.pending().unwrap(), vec![request.clone()]);
        }
        j.acknowledge(&request, &receipt(&request)).unwrap();
        assert!(j.pending().unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[gpui::test]
    fn planning_late_receipt_preserves_navigation_and_unrelated_drafts(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let dir = std::env::temp_dir().join(uuid());
        let cleanup = dir.clone();
        cx.add_window_view(move |window, cx| {
            let mut v = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            v.busy = true;
            v.expected_workspace = Some(workspace());
            v.snapshot = json!({"goal":{"id":"new-navigation"}});
            v.selected_goal_id = Some("new-navigation".into());
            v.compose
                .update(cx, |i, cx| i.set_value("conversation draft", window, cx));
            v.title
                .update(cx, |i, cx| i.set_value("unrelated goal draft", window, cx));
            v.source.reset("editor draft", window, cx);
            v.source_editable = true;
            let j = InboxJournal::at(dir, &workspace()).unwrap().for_planning();
            let request = plan_request(
                &j,
                &origin(),
                "Title",
                "Observable result",
                false,
                "actor",
                "old-navigation",
            )
            .unwrap();
            j.retain(&request).unwrap();
            v.inbox_plan.restore(&request, window, cx);
            v.inbox_plan.pending = vec![request.clone()];
            v.inbox_plan.journal = Some(j);
            v.inbox_plan.form = true;
            v.collection = Collection::Inbox;
            let mut wrong = receipt(&request);
            wrong["_client_inbox_request"]["criteria"][0]["description"] = json!("changed");
            v.inbox_plan_reply(wrong, window, cx);
            assert_eq!(
                v.inbox_plan.pending.len(),
                1,
                "unmatched receipt cannot acknowledge"
            );
            v.inbox_plan_reply(receipt(&request), window, cx);
            assert!(v.inbox_plan.pending.is_empty());
            assert!(v.inbox_plan.created.is_some());
            assert_eq!(v.goal_id(), "new-navigation");
            assert!(v.collection == Collection::Inbox);
            assert_eq!(v.compose.read(cx).value().as_ref(), "conversation draft");
            assert_eq!(v.title.read(cx).value().as_ref(), "unrelated goal draft");
            assert_eq!(v.source.value(cx).as_ref(), "editor draft");
            v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
    #[gpui::test]
    fn matching_planning_receipt_opens_new_goal_and_keeps_old_context_drafts(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = std::env::temp_dir().join(uuid());
        let cleanup = dir.clone();
        cx.add_window_view(move |window, cx| {
            let mut v = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            v.busy = false;
            v.expected_workspace = Some(workspace());
            v.snapshot = json!({"goal":{"id":"old"}});
            v.selected_goal_id = Some("old".into());
            v.compose.update(cx, |i,cx|i.set_value("old conversation draft",window,cx));
            v.next_step.update(cx, |i,cx|i.set_value("old stage draft",window,cx));
            v.title.update(cx, |i,cx|i.set_value("independent goal form",window,cx));
            let j = InboxJournal::at(dir, &workspace()).unwrap().for_planning();
            let request = plan_request(&j, &origin(), "Title", "Observable result", false, "actor", "old").unwrap();
            j.retain(&request).unwrap();
            v.inbox_plan.restore(&request,window,cx);
            v.inbox_plan.pending = vec![request.clone()];
            v.inbox_plan.journal = Some(j);
            v.inbox_plan.form = true;
            v.collection = Collection::Inbox;
            let reply = receipt(&request);
            v.inbox_plan_reply(reply.clone(),window,cx);
            assert!(v.inbox_plan.pending.is_empty());
            assert!(v.collection == Collection::Goals);
            assert!(!v.inbox_plan.form);
            assert!(v.busy,"positive control: exact created goal snapshot requested");
            assert_eq!(v.goal_id(),"old","receipt does not fabricate a goal snapshot");
            let snapshot=json!({"goal":{"id":reply["goal_id"],"title":"Title","origin_inbox":reply["origin"]}});
            v.finish_batch(vec![("snapshot".into(),Ok(snapshot))],false,v.request_generation,window,cx);
            assert_eq!(v.goal_id(),text(&reply["goal_id"]));
            assert_eq!(v.goal_drafts["old"],("old conversation draft".into(),"old stage draft".into()));
            assert_eq!(v.title.read(cx).value().as_ref(),"independent goal form");
            assert!(v.surface == Surface::Conversation);
            assert_eq!(v.snapshot["goal"]["origin_inbox"]["text"],origin()["text"]);
            v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
    #[gpui::test]
    fn only_exact_source_changed_allows_archive_and_new_attempt_requires_review(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = std::env::temp_dir().join(uuid());
        let cleanup = dir.clone();
        cx.add_window_view(move|window,cx|{
            let mut v=BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);v.busy=true;v.expected_workspace=Some(workspace());
            let j=InboxJournal::at(dir,&workspace()).unwrap().for_planning();let request=plan_request(&j,&origin(),"Title","Observable result",false,"actor","").unwrap();j.retain(&request).unwrap();
            v.inbox_plan.restore(&request,window,cx);v.inbox_plan.pending=vec![request.clone()];v.inbox_plan.journal=Some(j);
            for code in ["inbox_plan_operation_conflict","inbox_plan_recovery_required","inbox_plan_invalid_request"] {
                v.inbox_plan_reply(json!({"_client_inbox_request":request,"_inbox_plan_error":{"code":code,"message":"Retained"}}),window,cx);
                assert!(v.inbox_plan.rejected_operation.is_none());v.busy=false;v.archive_inbox_plan(window,cx);assert_eq!(v.inbox_plan.pending.len(),1);v.busy=true;
            }
            v.inbox_plan_reply(json!({"_client_inbox_request":request,"_inbox_plan_error":{"code":"inbox_plan_source_changed","message":"Review current source"}}),window,cx);
            assert_eq!(v.inbox_plan.rejected_operation.as_deref(),request["operation_id"].as_str());
            v.busy=false;v.archive_inbox_plan(window,cx);assert!(v.inbox_plan.pending.is_empty());assert_eq!(v.inbox_plan.archived,vec![request.clone()]);assert!(!v.inbox_plan.form);
            assert_eq!(v.inbox_plan.title.read(cx).value().as_ref(),"Title");assert_eq!(v.inbox_plan.criteria.read(cx).value().as_ref(),"Observable result");
            assert!(v.inbox_plan.journal.as_ref().unwrap().pending().unwrap().is_empty());v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
    #[gpui::test]
    fn disabled_planning_blocks_new_identity_but_recovery_reuses_exact_retained_request(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = std::env::temp_dir().join(uuid());
        let cleanup = dir.clone();
        cx.add_window_view(move|window,cx|{
            let mut v=BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);v.busy=false;v.expected_workspace=Some(workspace());v.capabilities=json!({"inbox_plan":false,"actor":"actor"});
            let j=InboxJournal::at(dir,&workspace()).unwrap().for_planning();let request=plan_request(&j,&origin(),"Title","Observable result",false,"actor","").unwrap();v.inbox_plan.journal=Some(j);
            v.submit_inbox_plan(window,cx);assert!(!v.busy);assert!(v.inbox_plan.pending.is_empty());
            v.inbox_plan.pending=vec![request.clone()];v.inbox_plan.rejected_operation=Some(text(&request["operation_id"]));
            v.submit_inbox_plan(window,cx);assert!(v.busy,"positive control: retained recovery attempted despite missing new-write capability");assert_eq!(v.inbox_plan.pending,vec![request.clone()]);assert!(v.inbox_plan.rejected_operation.is_none());assert_eq!(v.inbox_plan.journal.as_ref().unwrap().pending().unwrap(),vec![request]);v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
}
