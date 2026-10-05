//! Linked Maestro observation and explicit, exact guarded approval decisions.
use super::native_outbox::InboxJournal;
use super::*;

#[derive(Default)]
pub(super) struct MaestroUi {
    journal: Option<InboxJournal>,
    pending: Vec<Value>,
    discovery: Value,
    discovery_goal: String,
    selected: Option<(Value, Value)>,
    sequence: u64,
    pub(super) error: Option<String>,
    notice: Option<String>,
    history: bool,
    approval_review: Option<Value>,
    approval_reason: Option<Entity<InputState>>,
    pub(super) backend_unavailable: bool,
}

fn value_label(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|n| n.to_string()))
        .unwrap_or_else(|| "unknown".into())
}

fn attempt_is_current(view: &Value, attempt: &Value, backend_available: bool) -> bool {
    backend_available
        && view["recovery_required"] != true
        && view["link"]["status"] == "observed"
        && view["link"]["error"].is_null()
        && attempt["live"] == true
}
fn last_paused(link: &Value) -> &'static str {
    match link["latest"]["paused"].as_bool() {
        Some(true) => "paused at last observation",
        Some(false) => "not paused at last observation",
        None => "pause state unknown",
    }
}

fn safe_url(value: &Value) -> Option<String> {
    let url = value.as_str()?;
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    (!authority.is_empty() && !authority.contains('@') && !url.chars().any(char::is_whitespace))
        .then(|| url.to_owned())
}

fn link_request(
    journal: &InboxJournal,
    goal: &str,
    project: &Value,
    choice: &Value,
) -> Result<Value, String> {
    Uuid::parse_str(goal).map_err(|_| "Select an existing goal first.")?;
    Uuid::parse_str(&text(&project["project_id"]))
        .map_err(|_| "Project identity is unavailable.")?;
    if project["stale"] != false
        || !text(&choice["selection_guard"]).starts_with("sha256:")
        || choice["issue"]["number"].as_u64().is_none()
        || text(&project["name"]).is_empty()
        || text(&project["repo"]).is_empty()
    {
        return Err("This selection is stale or incomplete. Discover current work again.".into());
    }
    Ok(
        json!({"op":"maestro_link","operation_id":uuid(),"goal_id":goal,
        "project_id":project["project_id"],"project_name":project["name"],"repo":project["repo"],
        "issue_number":choice["issue"]["number"],"selection_guard":choice["selection_guard"],
        "source":{"instance_id":journal.instance}}),
    )
}

fn typed_request(request: &Value) -> Value {
    let mut value = request.clone();
    if let Some(object) = value.as_object_mut() {
        object.remove("op");
        object.remove("source");
    }
    value
}
pub(super) fn matching_disposition(request: &Value, disposition: &Value) -> bool {
    disposition["schema"] == "tessera-maestro-operation/v1"
        && disposition["operation_id"] == request["operation_id"]
        && disposition["goal_id"] == request["goal_id"]
        && disposition["kind"]
            == match request["op"].as_str() {
                Some("maestro_link") => "link",
                Some("maestro_unlink") => "unlink",
                Some("maestro_approval_decision") => "approval_decision",
                _ => return false,
            }
        && disposition["request"] == typed_request(request)
}

pub(super) fn matching_decision_receipt(request: &Value, receipt: &Value) -> bool {
    let proof = &receipt["decision_receipt"];
    receipt["operation_id"] == request["operation_id"]
        && receipt["goal_id"] == request["goal_id"]
        && receipt["link_id"] == request["expected_link_id"]
        && receipt["instance"] == request["instance"]
        && proof["expected"] == request["review"]["expected"]
        && proof["decision"] == request["decision"]
        && proof["actor"].as_str().is_some_and(|s| !s.is_empty())
        && proof["reason"].is_string()
        && proof["at"].as_str().is_some_and(|s| {
            time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).is_ok()
        })
}
fn approval_scope(review: &Value, goal: &str, link: &Value) -> bool {
    let exact = &review["view"]["review"];
    review["goal_id"] == goal
        && review["link_id"] == link["id"]
        && link["active"] == true
        && review["instance"] == link["instance"]
        && exact["expected"]["project_id"] == link["project_id"]
        && exact["expected"]["project_name"] == link["project_name"]
        && exact["expected"]["project_repo"] == link["repo"]
        && exact["target"]["issue"] == link["issue_number"]
        && exact["action"] == "merge_pr"
}
impl BrainView {
    pub(super) fn ensure_maestro(&mut self) {
        if self.maestro_ui.journal.is_some() {
            return;
        }
        let Some(workspace) = &self.expected_workspace else {
            return;
        };
        match InboxJournal::open_maestro(workspace).and_then(|j| Ok((j.pending()?, j))) {
            Ok((pending, journal)) => {
                self.maestro_ui.pending = pending;
                self.maestro_ui.journal = Some(journal);
            }
            Err(error) => self.maestro_ui.error = Some(error),
        }
    }
    fn discover_maestro(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy
            || self.goal_id().is_empty()
            || self.capabilities["maestro_observation"] != true
        {
            return;
        }
        self.maestro_ui.sequence += 1;
        self.maestro_ui.selected = None;
        self.maestro_ui.discovery = Value::Null;
        self.maestro_ui.discovery_goal = self.goal_id();
        self.maestro_ui.error = None;
        self.batch(vec![json!({"op":"maestro_discover","_maestro_goal":self.goal_id(),
            "_maestro_workspace":self.expected_workspace,"_maestro_sequence":self.maestro_ui.sequence})],window,cx);
    }
    fn deliver_maestro(&mut self, request: Value, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if request["op"] == "maestro_approval_decision"
            && (self.capabilities["maestro_approval_send"] != true
                || request["goal_id"] != self.goal_id()
                || request["expected_link_id"] != self.snapshot["maestro"]["link"]["id"]
                || request["instance"] != self.snapshot["maestro"]["link"]["instance"])
        {
            self.maestro_ui.error = Some("Retry requires the original active goal, link and provider. Use Check request status to reconcile retained evidence.".into());
            cx.notify();
            return;
        }
        self.ensure_maestro();
        // Preserve the chosen UUID in memory before a possible post-publication fsync error.
        if !self.maestro_ui.pending.iter().any(|r| r == &request) {
            self.maestro_ui.pending.push(request.clone());
        }
        let retained = self
            .maestro_ui
            .journal
            .as_ref()
            .ok_or("Local link recovery is unavailable.".into())
            .and_then(|j| j.retain(&request));
        match retained {
            Ok(()) => {
                self.maestro_ui.error = None;
                self.maestro_ui.notice = None;
                self.batch(vec![request], window, cx);
            }
            Err(error) => self.maestro_ui.error = Some(error),
        }
        cx.notify();
    }
    fn link_selected_maestro(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy
            || !self.maestro_ui.pending.is_empty()
            || self.capabilities["maestro_link"] != true
            || self.maestro_ui.discovery_goal != self.goal_id()
            || self.snapshot["maestro"]["link"].is_object()
        {
            return;
        }
        let Some((project, choice)) = self.maestro_ui.selected.clone() else {
            return;
        };
        self.ensure_maestro();
        let request = self
            .maestro_ui
            .journal
            .as_ref()
            .ok_or("Local link recovery is unavailable.".into())
            .and_then(|j| link_request(j, &self.goal_id(), &project, &choice));
        match request {
            Ok(r) => self.deliver_maestro(r, window, cx),
            Err(e) => {
                self.maestro_ui.error = Some(e);
                cx.notify();
            }
        }
    }
    fn unlink_maestro(&mut self, link: Value, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy
            || self
                .maestro_ui
                .pending
                .iter()
                .any(|r| r["op"] != "maestro_approval_decision")
            || link["id"] != self.snapshot["maestro"]["link"]["id"]
            || link["goal_id"] != self.snapshot["goal"]["id"]
            || self.capabilities["maestro_observation"] != true
        {
            return;
        }
        self.ensure_maestro();
        let Some(journal) = &self.maestro_ui.journal else {
            return;
        };
        self.deliver_maestro(
            json!({"op":"maestro_unlink","operation_id":uuid(),"goal_id":link["goal_id"],
            "expected_link_id":link["id"],"source":{"instance_id":journal.instance}}),
            window,
            cx,
        );
    }
    fn recover_maestro_status(
        &mut self,
        request: Value,
        abandon: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || !self.maestro_ui.pending.iter().any(|r| r == &request) {
            return;
        }
        if abandon && request["op"] == "maestro_approval_decision" {
            return;
        }
        let command = if request["op"] == "maestro_approval_decision" {
            json!({"op":"maestro_approval_reconcile","operation_id":request["operation_id"],"goal_id":request["goal_id"],"_maestro_original":request})
        } else if abandon {
            json!({"op":"maestro_operation_abandon","kind":if request["op"]=="maestro_link" {"link"}else{"unlink"},
                "request":typed_request(&request),"_maestro_original":request})
        } else {
            json!({"op":"maestro_operation_get","operation_id":request["operation_id"],"goal_id":request["goal_id"],"_maestro_original":request})
        };
        self.batch(vec![command], window, cx);
    }
    pub(super) fn finish_maestro(
        &mut self,
        op: &str,
        data: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let client_request = &data["_client_maestro_request"];
        let request = client_request
            .get("_maestro_original")
            .unwrap_or(client_request);
        if matches!(
            op,
            "maestro_link"
                | "maestro_approval_decision"
                | "maestro_approval_reconcile"
                | "maestro_unlink"
                | "maestro_operation_get"
                | "maestro_operation_abandon"
        ) {
            if !self.maestro_ui.pending.iter().any(|r| r == request) {
                return;
            }
            let disposition = if matches!(
                op,
                "maestro_operation_get"
                    | "maestro_operation_abandon"
                    | "maestro_approval_reconcile"
            ) && data["_maestro_error"].is_null()
                && data["schema"] == "tessera-maestro-operation/v1"
            {
                &data
            } else {
                &data["_maestro_error"]["maestro_operation"]
            };
            let receipt = if matching_disposition(request, disposition) {
                match disposition["status"].as_str() {
                    Some("committed") => &disposition["receipt"],
                    Some("rejected") => {
                        if request["op"] == "maestro_approval_decision"
                            && disposition["rejection"]["never_sent"] != true
                        {
                            self.maestro_ui.error = Some("Remote decision remains uncertain; a local rejection cannot prove its outcome.".into());
                            cx.notify();
                            return;
                        }
                        let result = self
                            .maestro_ui
                            .journal
                            .as_ref()
                            .ok_or("Local link recovery is unavailable.".into())
                            .and_then(|j| {
                                if request["op"] == "maestro_approval_decision" {
                                    j.archive_never_sent(request, disposition)
                                } else {
                                    j.archive_rejected(request)
                                }
                            });
                        match result {
                            Ok(()) => {
                                self.maestro_ui.pending.retain(|r| r != request);
                                self.maestro_ui.selected = None;
                                self.maestro_ui.discovery = Value::Null;
                                self.maestro_ui.error = None;
                                self.maestro_ui.notice=Some(format!("Request not applied: {}. Its exact identity is archived; discover current work to choose again.",text(&disposition["rejection"]["message"])));
                            }
                            Err(error) => self.maestro_ui.error = Some(error),
                        }
                        cx.notify();
                        return;
                    }
                    _ => {
                        self.maestro_ui.notice=Some(if request["op"] == "maestro_approval_decision" { "Decision remains uncertain. Check the provider receipt or explicitly retry the identical decision in its original active scope. Leaving this view does not revoke a remote decision." } else { "The exact request is still pending. Retry it or abandon this local request." }.into());
                        cx.notify();
                        return;
                    }
                }
            } else if data["_maestro_error"].is_object()
                || matches!(op, "maestro_operation_get" | "maestro_operation_abandon")
                || (op == "maestro_approval_reconcile"
                    && !matching_decision_receipt(request, &data))
            {
                self.maestro_ui.error=Some(format!("{} No matching terminal receipt is available. The exact request remains retained.",text(&data["_maestro_error"]["message"])));
                cx.notify();
                return;
            } else {
                &data
            };
            let result = self
                .maestro_ui
                .journal
                .as_ref()
                .ok_or("Local link recovery is unavailable.".into())
                .and_then(|j| j.acknowledge(request, receipt));
            match result {
                Ok(()) => {
                    self.maestro_ui.pending.retain(|r| r != request);
                    if request["op"] == "maestro_approval_decision"
                        && self
                            .maestro_ui
                            .approval_review
                            .as_ref()
                            .is_some_and(|review| {
                                review["goal_id"] == request["goal_id"]
                                    && review["link_id"] == request["expected_link_id"]
                                    && review["instance"] == request["instance"]
                                    && review["view"]["review"] == request["review"]
                            })
                    {
                        self.maestro_ui.approval_review = None;
                    }
                    self.maestro_ui.selected = None;
                    self.maestro_ui.discovery = Value::Null;
                    self.maestro_ui.error = None;
                    self.maestro_ui.notice = Some(
                        if request["op"] == "maestro_approval_decision" {
                            "Exact decision recorded in Maestro. Execution status is separate; no Tessera stage or goal was completed."
                        } else if request["op"] == "maestro_link" {
                            "Existing work linked. Maestro execution continues independently."
                        } else {
                            "Observation stopped. Maestro work and saved history are preserved."
                        }
                        .into(),
                    );
                    // Refresh only the selected goal. A late receipt cannot switch navigation.
                    let goal = self.goal_id();
                    if !goal.is_empty() {
                        self.batch(vec![json!({"op":"snapshot","goal_id":goal})], window, cx);
                    }
                }
                Err(error) => self.maestro_ui.error = Some(error),
            }
        } else if op == "maestro_approval_review" {
            if request["_maestro_workspace"]
                == self.expected_workspace.clone().unwrap_or(Value::Null)
                && request["goal_id"] == self.goal_id()
                && request["expected_link_id"] == self.snapshot["maestro"]["link"]["id"]
            {
                if data["_maestro_error"].is_object() {
                    self.maestro_ui.error = Some(text(&data["_maestro_error"]["message"]));
                } else {
                    self.maestro_ui.approval_review = Some(data);
                    self.maestro_ui.approval_reason =
                        Some(cx.new(|cx| {
                            InputState::new(window, cx).placeholder("Reason (optional)")
                        }));
                    self.maestro_ui.error = None;
                }
            }
        } else if op == "maestro_discover"
            && request["_maestro_workspace"]
                == self.expected_workspace.clone().unwrap_or(Value::Null)
            && request["_maestro_sequence"].as_u64() == Some(self.maestro_ui.sequence)
            && request["_maestro_goal"] == self.goal_id()
        {
            if data["_maestro_error"].is_object() {
                self.maestro_ui.error = Some(text(&data["_maestro_error"]["message"]));
            } else if data["schema"] == "tessera-maestro-observation/v1"
                && data["controls_enabled"] == false
            {
                self.maestro_ui.discovery = data;
                self.maestro_ui.error = None;
            } else {
                self.maestro_ui.error = Some("Unsupported Maestro discovery response.".into());
            }
        }
        cx.notify();
    }
    fn maestro_external(
        &self,
        id: impl Into<ElementId>,
        label: &'static str,
        url: &Value,
        cx: &Context<Self>,
    ) -> AnyElement {
        let url = safe_url(url);
        super::super::brand::control(id, cx)
            .label(label)
            .small()
            .disabled(url.is_none())
            .on_click(move |_, _, cx| {
                if let Some(url) = &url {
                    cx.open_url(url);
                }
            })
            .into_any_element()
    }
    fn send_approval_decision(
        &mut self,
        decision: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(review) = self.maestro_ui.approval_review.clone() else {
            return;
        };
        if self.busy
            || !self.maestro_ui.pending.is_empty()
            || self.capabilities["maestro_approval_send"] != true
            || !approval_scope(&review, &self.goal_id(), &self.snapshot["maestro"]["link"])
            || review["view"]["supported"] != true
            || review["view"]["status"] != "pending"
        {
            return;
        }
        self.ensure_maestro();
        let Some(journal) = &self.maestro_ui.journal else {
            return;
        };
        let reason = self
            .maestro_ui
            .approval_reason
            .as_ref()
            .map(|i| i.read(cx).value().to_string())
            .unwrap_or_default();
        let request = json!({"op":"maestro_approval_decision","operation_id":uuid(),"goal_id":self.goal_id(),"expected_link_id":review["link_id"],"instance":review["instance"],"review":review["view"]["review"],"decision":decision,"actor":self.capabilities["actor"],"reason":reason,"source":{"instance_id":journal.instance}});
        self.deliver_maestro(request, window, cx);
    }
    fn maestro_approval_panel(&self, cx: &Context<Self>) -> AnyElement {
        let mut panel = v_flex().gap_2();
        if let Some(review) = &self.maestro_ui.approval_review {
            if review["goal_id"] == self.goal_id() {
                let exact = &review["view"]["review"];
                let enabled =
                    approval_scope(review, &self.goal_id(), &self.snapshot["maestro"]["link"])
                        && review["view"]["supported"] == true
                        && review["view"]["status"] == "pending"
                        && self.capabilities["maestro_approval_send"] == true
                        && self.maestro_ui.pending.is_empty()
                        && !self.busy;
                panel = panel.child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .child("Review exact Maestro decision"),
                );
                if exact.is_object() {
                    panel =
                        panel
                            .child(div().child(format!(
                                "{} · issue {} · PR {}",
                                text(&exact["expected"]["project_repo"]),
                                value_label(&exact["target"]["issue"]),
                                value_label(&exact["target"]["pr"])
                            )))
                            .child(div().text_sm().child(format!(
                                "Full head: {}",
                                text(&exact["target"]["head_sha"])
                            )))
                            .child(div().text_sm().child(format!(
                                "Approval {} · created {}",
                                text(&exact["expected"]["approval_id"]),
                                text(&exact["expected"]["created_at"])
                            )))
                            .child(div().text_sm().child(format!(
                                "{} · risk: {}",
                                text(&exact["summary"]),
                                text(&exact["risk"])
                            )))
                            .child(div().text_xs().child(format!(
                                "Revision: {}",
                                text(&exact["expected"]["decision_revision"])
                            )));
                    for evidence in array(&exact["evidence"]) {
                        panel = panel.child(div().text_sm().child(text(&evidence)));
                    }
                } else {
                    panel=panel.child(div().child("Exact guarded review is unavailable. Continue observation or open this approval in Maestro."));
                }
                panel=panel.child(div().text_sm().child("This records a decision. Maestro executes separately and may update a behind-base branch, then skip it for revalidation. This does not prove the reviewed head merged or complete a Tessera goal."));
                if !enabled {
                    panel=panel.child(div().text_sm().child("New decisions are unavailable for this review or scope. Existing receipt recovery remains available."));
                }
                if let Some(reason) = &self.maestro_ui.approval_reason {
                    panel = panel.child(Input::new(reason).disabled(!enabled));
                }
                panel = panel.child(
                    h_flex()
                        .gap_2()
                        .child(
                            super::super::brand::control("maestro-approve-exact", cx)
                                .label("Approve exact decision")
                                .disabled(!enabled)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.send_approval_decision("approved", window, cx)
                                })),
                        )
                        .child(
                            super::super::brand::control("maestro-reject-exact", cx)
                                .label("Reject exact decision")
                                .disabled(!enabled)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.send_approval_decision("rejected", window, cx)
                                })),
                        )
                        .child(
                            super::super::brand::control("maestro-close-review", cx)
                                .label("Close review")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.maestro_ui.approval_review = None;
                                    cx.notify();
                                })),
                        ),
                );
                let receipt = &review["view"]["decision_receipt"];
                if receipt.is_object() {
                    panel = panel.child(div().text_sm().child(format!(
                        "Recorded decision: {} · actor {} · {}. Original reason: {}. Execution status: {}",
                        text(&receipt["decision"]),
                        text(&receipt["actor"]),
                        text(&receipt["at"]),
                        text(&receipt["reason"]),
                        text(&review["view"]["status"])
                    )));
                }
            }
        }
        for record in array(&self.snapshot["maestro"]["decisions"]) {
            if record["status"] == "committed" {
                let receipt = &record["receipt"]["decision_receipt"];
                panel = panel.child(div().text_sm().child(format!(
                    "Saved decision {} for PR {} · head {} · actor {} · {}. Original reason: {}. Execution observed: {}",
                    text(&receipt["decision"]),
                    value_label(&record["request"]["review"]["target"]["pr"]),
                    text(&record["request"]["review"]["target"]["head_sha"]),
                    text(&receipt["actor"]),
                    text(&receipt["at"]),
                    text(&receipt["reason"]),
                    text(&record["receipt"]["execution_status"])
                )));
            }
        }
        panel.into_any_element()
    }
    pub(super) fn maestro_panel(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let view = &self.snapshot["maestro"];
        if self.capabilities["maestro_observation"] != true
            && !view["link"].is_object()
            && array(&view["history"]).is_empty()
            && self.maestro_ui.pending.is_empty()
            && self.maestro_ui.error.is_none()
        {
            return div().into_any_element();
        }
        let mut panel=v_flex().id("maestro-linked-work").gap_2().p_3().border_1().border_color(theme.border).rounded_md()
            .child(div().font_weight(FontWeight::BOLD).child("Linked Maestro work"))
            .child(div().text_sm().text_color(theme.muted_foreground).child("Observe an existing issue. Maestro owns its workers; outcome review stays with this goal."));
        if let Some(error) = &self.maestro_ui.error {
            panel = panel.child(div().text_sm().child(error.clone()));
        }
        if let Some(notice) = &self.maestro_ui.notice {
            panel = panel.child(div().text_sm().child(notice.clone()));
        }
        if view["recovery_required"] == true {
            panel=panel.child(div().child("Maestro history needs recovery. Retained evidence remains available; new linking is unavailable."));
        }
        for (index, request) in self.maestro_ui.pending.iter().enumerate() {
            let r = request.clone();
            let query = request.clone();
            let abandon = request.clone();
            panel = panel.child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child(format!(
                        "Unconfirmed {} · goal {}",
                        if request["op"] == "maestro_approval_decision" {
                            "approval decision"
                        } else if request["op"] == "maestro_link" {
                            "link"
                        } else {
                            "stop observation"
                        },
                        text(&request["goal_id"])
                    )))
                    .child(div().text_xs().child(if request["op"] == "maestro_approval_decision" {format!("Retained {} · {} / PR {} / head {}. Reason: {}. No automatic retry; leaving this view cannot revoke it.",text(&request["decision"]),text(&request["review"]["expected"]["project_repo"]),value_label(&request["review"]["target"]["pr"]),text(&request["review"]["target"]["head_sha"]),text(&request["reason"]))} else {String::new()}))
                    .child(
                        super::super::brand::control(("maestro-recover", index), cx)
                            .label("Recover same request")
                            .disabled(self.busy || (request["op"] == "maestro_approval_decision" && (self.capabilities["maestro_approval_send"] != true || request["goal_id"] != self.goal_id() || request["expected_link_id"] != self.snapshot["maestro"]["link"]["id"])))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.deliver_maestro(r.clone(), window, cx)
                            })),
                    )
                    .child(
                        super::super::brand::control(("maestro-operation-query", index), cx)
                            .label("Check request status")
                            .small()
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.recover_maestro_status(query.clone(), false, window, cx)
                            })),
                    )
                    .child(
                        super::super::brand::control(("maestro-operation-abandon", index), cx)
                            .label("Abandon this local request")
                            .small()
                            .disabled(self.busy || request["op"] == "maestro_approval_decision")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.recover_maestro_status(abandon.clone(), true, window, cx)
                            })),
                    ),
            );
        }
        panel = panel.child(self.maestro_approval_panel(cx));
        let link = &view["link"];
        if link.is_object() {
            let issue = &link["latest"]["issue"];
            panel = panel
                .child(div().child(format!(
                    "{} · {} #{}",
                    text(&link["project_name"]),
                    text(&link["repo"]),
                    value_label(&link["issue_number"])
                )))
                .child(div().text_sm().child(text(&issue["title"])))
                .child(div().text_sm().child(format!(
                    "Connection: {} · Project {}",
                    if self.maestro_ui.backend_unavailable {
                        "backend unavailable; previous evidence retained".into()
                    } else {
                        text(&link["status"])
                    },
                    last_paused(link)
                )))
                .child(div().text_xs().child(format!(
                    "Last observed: {} · Provider snapshot: {}",
                    text(&link["last_seen"]),
                    text(&link["last_remote"])
                )));
            if let Some(error) = link["error"].as_str() {
                panel = panel.child(div().text_sm().child(format!(
                    "Current state unknown: {error}. Previous evidence is retained."
                )));
            }
            panel = panel.child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(self.maestro_external(
                        "maestro-open-issue",
                        "Open issue",
                        &issue["url"],
                        cx,
                    ))
                    .child(self.maestro_external(
                        "maestro-open-project",
                        "Open in Maestro",
                        &link["project_url"],
                        cx,
                    )),
            );
            for (index, attempt) in array(&issue["attempts"]).iter().enumerate() {
                panel = panel.child(
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().child(format!(
                            "Attempt {} · generation {} · {}{}",
                            text(&attempt["slot"]),
                            value_label(&attempt["generation"]),
                            text(&attempt["status"]),
                            if attempt_is_current(
                                view,
                                attempt,
                                !self.maestro_ui.backend_unavailable
                            ) {
                                " · live"
                            } else {
                                " · retained evidence"
                            }
                        )))
                        .child(div().text_xs().child(text(&attempt["reason"])))
                        .child(self.maestro_external(
                            ("maestro-open-pr", index),
                            "Open PR",
                            &attempt["pr_url"],
                            cx,
                        )),
                );
            }
            for (index, approval) in array(&issue["approvals"]).iter().enumerate() {
                let review_request = json!({"op":"maestro_approval_review","goal_id":link["goal_id"],"expected_link_id":link["id"],"approval_id":approval["id"],"_maestro_workspace":self.expected_workspace});
                panel = panel.child(
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().child(format!(
                            "Decision in Maestro · {} · {}",
                            text(&approval["action"]),
                            text(&approval["status"])
                        )))
                        .child(div().text_sm().child(text(&approval["summary"])))
                        .child(self.maestro_external(
                            ("maestro-open-approval", index),
                            "Review in Maestro",
                            &approval["dashboard_url"],
                            cx,
                        ))
                        .child(
                            super::super::brand::control(("maestro-review-exact", index), cx)
                                .label("Review exact decision")
                                .small()
                                .disabled(self.busy || self.capabilities["maestro_control"] != true)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.maestro_ui.approval_review = None;
                                    this.batch(vec![review_request.clone()], window, cx);
                                })),
                        ),
                );
            }
            panel = panel.child(div().text_xs().text_color(theme.muted_foreground).child(
                "Observed evidence · unverified. A worker status does not complete this goal.",
            ));
            if let Some(path) = view["source_paths"]["link"].as_str() {
                let path = path.to_owned();
                panel = panel.child(
                    super::super::brand::control("maestro-source", cx)
                        .label("Open saved link source")
                        .small()
                        .disabled(self.busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_source(path.clone(), window, cx)
                        })),
                );
            }
            let retained = link.clone();
            panel = panel.child(
                super::super::brand::control("maestro-unlink", cx)
                    .label("Stop observing")
                    .small()
                    .disabled(
                        self.busy
                            || self
                                .maestro_ui
                                .pending
                                .iter()
                                .any(|r| r["op"] != "maestro_approval_decision"),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.unlink_maestro(retained.clone(), window, cx)
                    })),
            );
        } else {
            panel = panel.child(
                super::super::brand::control("maestro-discover", cx)
                    .label("Find existing Maestro work")
                    .disabled(
                        self.busy
                            || self.goal_id().is_empty()
                            || self.capabilities["maestro_observation"] != true,
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.discover_maestro(window, cx))),
            );
            if self.maestro_ui.discovery_goal == self.goal_id()
                && self.maestro_ui.discovery.is_object()
            {
                let projects = array(&self.maestro_ui.discovery["projects"]);
                let mut count = 0;
                for (pi, project) in projects.iter().enumerate() {
                    panel =
                        panel.child(div().text_sm().font_weight(FontWeight::BOLD).child(format!(
                            "{} · {}{}{}",
                            text(&project["name"]),
                            text(&project["repo"]),
                            if project["paused"] == true {
                                " · paused"
                            } else {
                                ""
                            },
                            if project["stale"] == true {
                                " · stale"
                            } else {
                                ""
                            }
                        )));
                    for (ii, choice) in array(&project["issues"]).iter().enumerate() {
                        count += 1;
                        let p = project.clone();
                        let c = choice.clone();
                        let goal = self.goal_id();
                        let sequence = self.maestro_ui.sequence;
                        panel = panel.child(
                            super::super::brand::control(
                                SharedString::from(format!("maestro-choice-{pi}-{ii}")),
                                cx,
                            )
                            .label(format!(
                                "#{} {}",
                                value_label(&choice["issue"]["number"]),
                                text(&choice["issue"]["title"])
                            ))
                            .disabled(
                                self.busy
                                    || project["stale"] != false
                                    || !self.maestro_ui.pending.is_empty(),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if this.goal_id() == goal
                                        && this.maestro_ui.sequence == sequence
                                    {
                                        this.maestro_ui.selected = Some((p.clone(), c.clone()));
                                        cx.notify();
                                    }
                                },
                            )),
                        );
                    }
                }
                if count == 0 {
                    panel = panel.child(
                        div()
                            .text_sm()
                            .child("No supported issues in the current Maestro snapshot."),
                    );
                }
                if let Some((project, choice)) = &self.maestro_ui.selected {
                    panel = panel
                        .child(div().text_sm().child(format!(
                            "Selected: {} / {} #{}",
                            text(&project["name"]),
                            text(&project["repo"]),
                            value_label(&choice["issue"]["number"])
                        )))
                        .child(
                            super::super::brand::control("maestro-link-selected", cx)
                                .label("Link selected issue to this goal")
                                .primary()
                                .disabled(
                                    self.busy
                                        || self.capabilities["maestro_link"] != true
                                        || !self.maestro_ui.pending.is_empty()
                                        || view["recovery_required"] == true,
                                )
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.link_selected_maestro(window, cx)
                                })),
                        );
                }
            }
        }
        if !array(&view["history"]).is_empty() {
            panel = panel.child(
                super::super::brand::control("maestro-history", cx)
                    .label(if self.maestro_ui.history {
                        "Hide observation history"
                    } else {
                        "Show observation history"
                    })
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.maestro_ui.history = !this.maestro_ui.history;
                        cx.notify();
                    })),
            );
            if self.maestro_ui.history {
                for (li, old) in array(&view["history"]).iter().enumerate() {
                    panel = panel.child(div().text_sm().child(format!(
                        "{} #{} · {} · {}",
                        text(&old["repo"]),
                        value_label(&old["issue_number"]),
                        if old["active"] == true {
                            "observing"
                        } else {
                            "observation stopped"
                        },
                        text(&old["created_at"])
                    )));
                    for (oi, id) in array(&old["observation_ids"]).iter().enumerate() {
                        if let Some(path) = view["source_paths"]["observations"][text(id)].as_str()
                        {
                            let path = path.to_owned();
                            panel = panel.child(
                                super::super::brand::control(
                                    SharedString::from(format!("maestro-observation-{li}-{oi}")),
                                    cx,
                                )
                                .label(format!("Open saved observation {}", oi + 1))
                                .small()
                                .disabled(self.busy)
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.open_source(path.clone(), window, cx)
                                    },
                                )),
                            );
                        }
                    }
                }
            }
        }
        panel.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    fn workspace() -> Value {
        json!({"brain_id":"cc000000-0000-4000-8000-000000000152","root":"/fixture/maestro-ui","records_dir":"records","managed":true})
    }
    fn temp() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tessera-maestro-ui-{}", uuid()))
    }
    fn journal(dir: &std::path::Path) -> InboxJournal {
        InboxJournal::at(dir.to_owned(), &workspace())
            .unwrap()
            .for_maestro()
    }
    fn project() -> Value {
        json!({"project_id":"aa000000-0000-4000-8000-000000000152","name":"Example","repo":"example/project","stale":false,"paused":true})
    }
    fn choice() -> Value {
        json!({"issue":{"number":42,"title":"Existing work"},"selection_guard":"sha256:exact"})
    }
    const GOAL: &str = "ee000000-0000-4000-8000-000000000152";
    fn receipt(request: &Value) -> Value {
        json!({"operation_id":request["operation_id"],"goal_id":request["goal_id"],"link_id":"bb000000-0000-4000-8000-000000000152","linked":true,"_client_maestro_request":request})
    }
    fn decision_request(j: &InboxJournal) -> Value {
        json!({"op":"maestro_approval_decision","operation_id":uuid(),"goal_id":GOAL,"expected_link_id":"bb000000-0000-4000-8000-000000000152","instance":{"base_url":"http://localhost:1234/","instance_id":uuid()},"review":{"expected":{"version":"v1","project_id":project()["project_id"],"project_name":"Example","project_repo":"example/project","approval_id":"approval-1","created_at":"2026-09-08T06:00:00Z","decision_revision":"v1:exact"},"decision_id":"decision-1","action":"merge_pr","target":{"issue":42,"pr":9,"head_sha":"a".repeat(40)},"summary":"Merge reviewed head","risk":"low","evidence":[]},"decision":"approved","actor":"operator","reason":"Exact head reviewed","source":{"instance_id":j.instance}})
    }
    fn decision_receipt(r: &Value) -> Value {
        json!({"operation_id":r["operation_id"],"goal_id":r["goal_id"],"link_id":r["expected_link_id"],"instance":r["instance"],"decision_receipt":{"expected":r["review"]["expected"],"decision":r["decision"],"actor":"original-author","reason":"Original decision reason","at":"2026-09-08T06:01:00Z"},"execution_status":"execution_failed"})
    }
    #[test]
    fn uncertain_decision_survives_restart_and_cannot_be_abandoned_or_retargeted() {
        let dir = temp();
        let j = journal(&dir);
        let request = decision_request(&j);
        j.retain(&request).unwrap();
        assert_eq!(journal(&dir).pending().unwrap(), vec![request.clone()]);
        assert!(j.archive_rejected(&request).is_err());
        let mut changed = request.clone();
        changed["reason"] = json!("Changed after send");
        assert!(j.retain(&changed).is_err());
        let mut wrong = decision_receipt(&request);
        wrong["decision_receipt"]["expected"]["created_at"] = json!("2026-09-09T06:00:00Z");
        assert!(j.acknowledge(&request, &wrong).is_err());
        let mut wrong = decision_receipt(&request);
        wrong["decision_receipt"]["decision"] = json!("rejected");
        assert!(j.acknowledge(&request, &wrong).is_err());
        assert_eq!(journal(&dir).pending().unwrap(), vec![request.clone()]);
        j.acknowledge(&request, &decision_receipt(&request))
            .unwrap();
        assert!(journal(&dir).pending().unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn only_exact_never_sent_proof_retires_a_decision_without_receipt() {
        let dir = temp();
        let j = journal(&dir);
        let r = decision_request(&j);
        j.retain(&r).unwrap();
        let mut d = json!({"schema":"tessera-maestro-operation/v1","operation_id":r["operation_id"],"goal_id":r["goal_id"],"kind":"approval_decision","request":typed_request(&r),"status":"rejected","rejection":{"code":"approval_not_sent","never_sent":false}});
        assert!(j.archive_never_sent(&r, &d).is_err());
        d["rejection"]["never_sent"] = json!(true);
        d["request"]["reason"] = json!("Different");
        assert!(j.archive_never_sent(&r, &d).is_err());
        d["request"] = typed_request(&r);
        j.archive_never_sent(&r, &d).unwrap();
        assert!(journal(&dir).pending().unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn exact_review_scope_includes_linked_issue_and_provider() {
        let dir = temp();
        let j = journal(&dir);
        let r = decision_request(&j);
        let link = json!({"id":r["expected_link_id"],"goal_id":GOAL,"active":true,"instance":r["instance"],"project_id":r["review"]["expected"]["project_id"],"project_name":"Example","repo":"example/project","issue_number":42});
        let v = json!({"goal_id":GOAL,"link_id":link["id"],"instance":link["instance"],"view":{"review":r["review"],"supported":true,"status":"pending"}});
        assert!(approval_scope(&v, GOAL, &link));
        let mut wrong = link.clone();
        wrong["issue_number"] = json!(43);
        assert!(!approval_scope(&v, GOAL, &wrong));
        let mut wrong = link.clone();
        wrong["instance"]["base_url"] = json!("http://localhost:9999/");
        assert!(!approval_scope(&v, GOAL, &wrong));
        assert!(!approval_scope(&v, &uuid(), &link));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn retained_link_is_exact_across_restart_and_wrong_receipts_cannot_clear_it() {
        let dir = temp();
        let j = journal(&dir);
        let request = link_request(&j, GOAL, &project(), &choice()).unwrap();
        j.retain(&request).unwrap();
        assert_eq!(journal(&dir).pending().unwrap(), vec![request.clone()]);
        let mut changed = request.clone();
        changed["issue_number"] = json!(43);
        assert!(j.retain(&changed).is_err());
        let mut wrong = receipt(&request);
        wrong["goal_id"] = json!(uuid());
        assert!(j.acknowledge(&request, &wrong).is_err());
        assert_eq!(journal(&dir).pending().unwrap(), vec![request.clone()]);
        let inbox = InboxJournal::at(dir.clone(), &workspace()).unwrap();
        assert!(inbox.pending().unwrap().is_empty());
        j.acknowledge(&request, &receipt(&request)).unwrap();
        assert!(journal(&dir).pending().unwrap().is_empty());
        let mut wrong_workspace = workspace();
        wrong_workspace["root"] = json!("/another");
        assert!(InboxJournal::at(dir.clone(), &wrong_workspace)
            .unwrap()
            .for_maestro()
            .pending()
            .is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn disconnected_or_stale_observation_cannot_claim_old_attempt_is_live() {
        let attempt = json!({"live":true});
        let mut view = json!({"link":{"status":"observed","error":null},"recovery_required":false});
        assert!(attempt_is_current(&view, &attempt, true));
        assert!(!attempt_is_current(&view, &attempt, false));
        for status in ["disconnected", "unknown", "unlinked"] {
            view["link"]["status"] = json!(status);
            assert!(!attempt_is_current(&view, &attempt, true));
        }
        view["link"]["status"] = json!("observed");
        view["recovery_required"] = json!(true);
        assert!(!attempt_is_current(&view, &attempt, true));
        assert_eq!(last_paused(&Value::Null), "pause state unknown");
        assert_eq!(
            last_paused(&json!({"latest":{"paused":true}})),
            "paused at last observation"
        );
    }
    #[test]
    fn picker_refuses_stale_missing_identity_and_never_invents_a_desktop_url() {
        let dir = temp();
        let j = journal(&dir);
        let mut stale = project();
        stale["stale"] = json!(true);
        assert!(link_request(&j, GOAL, &stale, &choice()).is_err());
        assert!(link_request(&j, "", &project(), &choice()).is_err());
        let mut no_guard = choice();
        no_guard["selection_guard"] = Value::Null;
        assert!(link_request(&j, GOAL, &project(), &no_guard).is_err());
        for bad in [
            Value::Null,
            json!("/project/Example"),
            json!("javascript:bad"),
            json!("https://secret@example.test/path"),
            json!("http://"),
        ] {
            assert!(safe_url(&bad).is_none());
        }
        assert_eq!(
            safe_url(&json!("https://maestro.example.test/project/Example")),
            Some("https://maestro.example.test/project/Example".into())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[gpui::test]
    fn late_discovery_and_link_receipt_preserve_selected_goal_and_drafts(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let dir = temp();
        let cleanup = dir.clone();
        cx.add_window_view(move|window,cx|{
            let mut v=BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);v.busy=true;v.expected_workspace=Some(workspace());
            v.snapshot=json!({"goal":{"id":"other-goal","status":"running"}});let snapshot=v.snapshot.clone();v.selected_goal_id=Some("other-goal".into());
            v.source.reset("editor draft", window, cx);v.compose.update(cx,|i,cx|i.set_value("conversation draft",window,cx));
            v.maestro_ui.sequence=2;
            let discovery=json!({"schema":"tessera-maestro-observation/v1","controls_enabled":false,"projects":[project()],"_client_maestro_request":{"op":"maestro_discover","_maestro_goal":GOAL,"_maestro_sequence":2,"_maestro_workspace":workspace()}});
            v.finish_maestro("maestro_discover",discovery,window,cx);assert!(v.maestro_ui.discovery.is_null());
            let j=journal(&dir);let request=link_request(&j,GOAL,&project(),&choice()).unwrap();j.retain(&request).unwrap();v.maestro_ui.journal=Some(j);v.maestro_ui.pending=vec![request.clone()];
            v.finish_maestro("maestro_link",receipt(&request),window,cx);assert!(v.maestro_ui.pending.is_empty());
            assert_eq!(v.snapshot,snapshot);assert_eq!(v.selected_goal_id.as_deref(),Some("other-goal"));assert_eq!(v.source.value(cx).as_ref(),"editor draft");assert_eq!(v.compose.read(cx).value().as_ref(),"conversation draft");v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
    #[gpui::test]
    fn post_publication_failure_and_unknown_reply_keep_the_same_link_request(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = temp();
        let cleanup = dir.clone();
        cx.add_window_view(move|window,cx|{
            let mut v=BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);v.expected_workspace=Some(workspace());
            v.snapshot=json!({"goal":{"id":GOAL}});let j=journal(&dir);let r=link_request(&j,GOAL,&project(),&choice()).unwrap();j.fail_retain_after_publish.set(true);v.maestro_ui.journal=Some(j);
            v.deliver_maestro(r.clone(),window,cx);assert!(!v.busy);assert_eq!(v.maestro_ui.pending,vec![r.clone()]);assert_eq!(journal(&dir).pending().unwrap(),vec![r.clone()]);
            v.finish_maestro("maestro_link",json!({"_client_maestro_request":r,"_maestro_error":{"message":"Request interrupted"}}),window,cx);
            assert_eq!(v.maestro_ui.pending,vec![r.clone()]);assert_eq!(journal(&dir).pending().unwrap(),vec![r]);v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
    #[gpui::test]
    fn committed_decision_closes_only_its_exact_review(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let dir = temp();
        let cleanup = dir.clone();
        cx.add_window_view(move |window, cx| {
            let mut v = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            v.expected_workspace = Some(workspace());
            let j = journal(&dir);
            let request = decision_request(&j);
            let review = json!({"goal_id":GOAL,"link_id":request["expected_link_id"],"instance":request["instance"],"view":{"supported":true,"status":"pending","review":request["review"]}});
            j.retain(&request).unwrap();
            v.maestro_ui.journal = Some(j);
            v.maestro_ui.pending = vec![request.clone()];
            v.maestro_ui.approval_review = Some(review.clone());
            let mut reply = decision_receipt(&request);
            reply["_client_maestro_request"] = request.clone();
            v.finish_maestro("maestro_approval_decision", reply, window, cx);
            assert!(v.maestro_ui.approval_review.is_none());
            assert!(v.maestro_ui.pending.is_empty());
            // A late receipt must not close a newer review from another scope,
            // or a changed review under the same goal/link identity.
            for field in ["goal_id", "link_id", "instance", "review"] {
                let mut late = request.clone();
                late["operation_id"] = json!(uuid());
                v.maestro_ui.journal.as_ref().unwrap().retain(&late).unwrap();
                v.maestro_ui.pending = vec![late.clone()];
                let mut current = review.clone();
                if field == "review" {
                    current["view"]["review"]["summary"] = json!("Newer captured review");
                } else {
                    current[field] = json!("different-current-scope");
                }
                v.snapshot = json!({"goal":{"id":"other-selected-goal"}});
                v.selected_goal_id = Some("other-selected-goal".into());
                v.maestro_ui.approval_review = Some(current.clone());
                let mut reply = decision_receipt(&late);
                reply["_client_maestro_request"] = late;
                v.finish_maestro("maestro_approval_decision", reply, window, cx);
                assert_eq!(v.maestro_ui.approval_review, Some(current));
                assert_eq!(v.selected_goal_id.as_deref(), Some("other-selected-goal"));
                assert!(v.maestro_ui.pending.is_empty());
            }
            v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
    #[gpui::test]
    fn unlink_handler_retains_uncertain_decision_and_creates_local_unlink(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let dir = temp();
        let cleanup = dir.clone();
        cx.add_window_view(move |window, cx| {
            let mut v = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            v.expected_workspace = Some(workspace());
            let j = journal(&dir);
            let request = decision_request(&j);
            j.retain(&request).unwrap();
            // Stop after durable publication: no asynchronous network is needed
            // to prove that the actual unlink handler crossed its scope guards.
            j.fail_retain_after_publish.set(true);
            let link = json!({"id":request["expected_link_id"],"goal_id":GOAL});
            v.snapshot = json!({"goal":{"id":GOAL},"maestro":{"link":link}});
            v.capabilities = json!({"maestro_observation":true});
            v.maestro_ui.journal = Some(j);
            v.maestro_ui.pending = vec![request.clone()];
            v.unlink_maestro(link, window, cx);
            let pending = journal(&dir).pending().unwrap();
            assert_eq!(pending.len(), 2);
            assert!(pending.contains(&request));
            let unlink = pending
                .iter()
                .find(|r| r["op"] == "maestro_unlink")
                .unwrap();
            assert_eq!(unlink["goal_id"], GOAL);
            assert_eq!(unlink["expected_link_id"], request["expected_link_id"]);
            assert!(v.maestro_ui.pending.contains(&request));
            assert!(!v.busy);
            v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
    #[gpui::test]
    fn only_exact_terminal_disposition_can_retire_a_pending_request(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let dir = temp();
        let cleanup = dir.clone();
        cx.add_window_view(move|window,cx|{
            let mut v=BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);v.busy=true;v.expected_workspace=Some(workspace());
            let j=journal(&dir);let r=link_request(&j,GOAL,&project(),&choice()).unwrap();j.retain(&r).unwrap();v.maestro_ui.journal=Some(j);v.maestro_ui.pending=vec![r.clone()];
            let command=json!({"op":"maestro_operation_get","operation_id":r["operation_id"],"goal_id":GOAL,"_maestro_original":r});
            let mut d=json!({"schema":"tessera-maestro-operation/v1","operation_id":r["operation_id"],"goal_id":GOAL,"kind":"link","request":typed_request(&r),"status":"unknown","receipt":null,"rejection":null,"_client_maestro_request":command});
            v.finish_maestro("maestro_operation_get",d.clone(),window,cx);assert_eq!(v.maestro_ui.pending,vec![r.clone()]);
            d["status"]=json!("rejected");d["rejection"]=json!({"code":"abandoned","message":"Local request abandoned"});
            d["request"]["issue_number"]=json!(43);v.finish_maestro("maestro_operation_get",d.clone(),window,cx);assert_eq!(v.maestro_ui.pending,vec![r.clone()]);
            d["request"]=typed_request(&r);v.finish_maestro("maestro_operation_get",d,window,cx);assert!(v.maestro_ui.pending.is_empty());assert_eq!(journal(&dir).rejected().unwrap(),vec![r]);v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
}
