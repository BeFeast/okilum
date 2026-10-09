//! Proposal adoption reuses existing forms and retains delivery independently of navigation.
use super::proposal_adoption_outbox::{AdoptionJournal, Pending};
use super::*;
#[derive(Default)]
pub(super) struct AdoptionUi {
    journal: Option<AdoptionJournal>,
    pub pending: Vec<Pending>,
    pub detail: Value,
    pub active: bool,
    pub error: Option<String>,
    pub notice: Option<String>,
    receipt: Value,
    receipt_detail: Value,
    notice_detail: Value,
    retired: Vec<Pending>,
    show_history: bool,
}
impl AdoptionUi {
    fn matches_origin(origin: &Value, selected: &Value) -> bool {
        origin["record"]["id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
            && origin["record"]["id"] == selected["record"]["id"]
            && origin["record"].get("goal_id") == selected["record"].get("goal_id")
    }
    fn feedback_origin<'a>(&'a self, visible: Option<&'a Value>) -> &'a Value {
        visible.unwrap_or_else(|| {
            if self.notice.is_some() {
                &self.notice_detail
            } else {
                &self.receipt_detail
            }
        })
    }
    fn feedback_for(&self, selected: &Value) -> (Option<&str>, Value) {
        let notice = Self::matches_origin(&self.notice_detail, selected)
            .then_some(self.notice.as_deref())
            .flatten();
        let target = if Self::matches_origin(&self.receipt_detail, selected) {
            self.receipt["result"]["receipt"]["target"].clone()
        } else {
            Value::Null
        };
        (notice, target)
    }
    fn delivery_origin(&self) -> &Value {
        self.pending.first().map_or(&self.detail, |p| &p.detail)
    }
}
impl BrainView {
    pub(super) fn ensure_adoption(&mut self) {
        if self.adoption.journal.is_some() {
            return;
        }
        let Some(workspace) = self.expected_workspace.as_ref() else {
            return;
        };
        match AdoptionJournal::open(workspace).and_then(|j| Ok((j.pending()?, j.completed()?, j))) {
            Ok((pending, completed, journal)) => {
                self.adoption.retired = completed
                    .iter()
                    .filter(|(_, r)| r["result"]["outcome"] == "not_applied")
                    .map(|(p, _)| p.clone())
                    .collect();
                if let Some((p, r)) = completed.last() {
                    self.adoption.receipt = r.clone();
                    self.adoption.receipt_detail = p.detail.clone();
                }
                self.adoption.pending = pending;
                self.adoption.journal = Some(journal);
            }
            Err(e) => self.adoption.error = Some(e),
        }
    }
    pub(super) fn adoption_pending_for(&self, id: &str) -> bool {
        self.adoption
            .pending
            .iter()
            .any(|p| p.request["proposal_id"] == id)
    }
    pub(super) fn begin_proposal_adoption(
        &mut self,
        detail: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.capabilities["proposal_adopt"] != true {
            return;
        }
        self.ensure_adoption();
        if self.adoption.active || !self.adoption.pending.is_empty() {
            self.adoption.error = Some(
                "Keep or cancel the open suggestion form, or recover its original delivery first."
                    .into(),
            );
            cx.notify();
            return;
        }
        if self.dirty(cx) {
            self.error =
                Some("Save or discard the source edit before opening a suggestion form.".into());
            return;
        }
        self.adoption.detail = detail.clone();
        self.adoption.active = true;
        self.adoption.error = None;
        self.adoption.notice = None;
        if detail["record"]["goal_id"].is_string() {
            self.proposals.leave();
            self.collection = Collection::Goals;
            self.show_capture = false;
            self.begin_proposal_context(detail, window, cx);
        } else {
            self.begin_proposal_inbox(detail, window, cx);
        }
        cx.notify();
    }
    pub(super) fn submit_proposal_adoption(
        &mut self,
        detail: Value,
        fields: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.capabilities["proposal_adopt"] != true {
            return;
        }
        self.ensure_adoption();
        match self
            .adoption
            .journal
            .as_ref()
            .ok_or("Adoption journal unavailable".to_string())
            .and_then(|j| j.pending())
        {
            Ok(p) => self.adoption.pending = p,
            Err(e) => {
                self.adoption.error = Some(e);
                return;
            }
        }
        if !self.adoption.pending.is_empty() {
            self.adoption.error =
                Some("Recover the retained adoption before submitting another.".into());
            return;
        }
        let result = self
            .adoption
            .journal
            .as_ref()
            .ok_or("Adoption journal unavailable".to_string())
            .and_then(|j| {
                j.prepare(
                    &detail,
                    fields,
                    &text(&self.capabilities["actor"]),
                    self.goal_id(),
                )
            })
            .and_then(|p| {
                self.adoption.journal.as_ref().unwrap().retain(&p)?;
                Ok(p)
            });
        match result {
            Ok(p) => {
                self.adoption.pending.push(p.clone());
                self.adoption.error = None;
                self.batch(vec![p.wire()], window, cx);
            }
            Err(e) => {
                self.adoption.error = Some(e);
                if let Some(j) = &self.adoption.journal {
                    if let Ok(pending) = j.pending() {
                        self.adoption.pending = pending;
                    }
                }
            }
        }
        cx.notify();
    }
    pub(super) fn recover_proposal_adoption(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.ensure_adoption();
        if let Some(p) = self.adoption.pending.first().cloned() {
            self.adoption.detail = p.detail.clone();
            self.batch(vec![p.wire()], window, cx);
        }
    }
    pub(super) fn adoption_reply(
        &mut self,
        data: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pending = &data["_client_proposal_request"]["_proposal_pending"];
        let Some(i) = self
            .adoption
            .pending
            .iter()
            .position(|p| serde_json::to_value(p).ok().as_ref() == Some(pending))
        else {
            return;
        };
        let p = self.adoption.pending[i].clone();
        if self.expected_workspace.as_ref() != Some(&p.workspace) {
            return;
        }
        if data["_proposal_error"].is_object() {
            self.adoption.error = Some(format!(
                "{} The original adoption delivery remains retained.",
                text(&data["_proposal_error"]["message"])
            ));
            return;
        }
        if let Err(e) = self
            .adoption
            .journal
            .as_ref()
            .ok_or("Adoption journal unavailable".to_string())
            .and_then(|j| j.acknowledge(&p, &data))
        {
            self.adoption.error = Some(e);
            return;
        }
        self.adoption.pending.remove(i);
        self.adoption.error = None;
        if data["result"]["outcome"] == "not_applied" {
            self.adoption.retired.push(p.clone());
        } else {
            if p.request["destination"] == "context" {
                self.finish_proposal_context(&text(&p.request["goal_id"]), window, cx);
            } else {
                self.finish_proposal_inbox(window, cx);
            }
            self.adoption.active = false;
        }
        self.adoption.notice = Some(if data["result"]["outcome"] == "not_applied" {
            format!("Adoption was not applied: {}. The original delivery is closed; inspect the source before another action.",text(&data["result"]["reason"]))
        } else {
            "Suggestion adopted. Its original destination is saved and remains unverified; no work has started.".into()
        });
        self.adoption.receipt = data;
        self.adoption.receipt_detail = p.detail.clone();
        self.adoption.notice_detail = p.detail;
        cx.notify();
    }
    fn reopen_retired_adoption(&mut self, p: Pending, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.adoption.active || !self.adoption.pending.is_empty() {
            return;
        }
        self.begin_proposal_adoption(p.detail.clone(), window, cx);
        if !self.adoption.active || self.adoption.detail["record"]["id"] != p.request["proposal_id"]
        {
            return;
        }
        if p.request["destination"] == "context" {
            self.restore_proposal_context_fields(
                &text(&p.request["goal_id"]),
                p.request,
                window,
                cx,
            );
        } else {
            self.restore_proposal_inbox_fields(&p.request, window, cx);
        }
        self.adoption.notice_detail = p.detail;
        self.adoption.notice=Some("Retained submitted draft. Its original source identity is preserved; inspect current sources before submitting a new operation.".into());
    }
    pub(super) fn adoption_origin_banner(
        &mut self,
        visible: Option<&Value>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // The caller identifies the actually rendered proposal/form. An active
        // form retained elsewhere cannot lend its feedback to another surface.
        // Global summaries identify their last update's original owner explicitly.
        let feedback_origin = self.adoption.feedback_origin(visible);
        let (notice, mut target) = self.adoption.feedback_for(feedback_origin);
        // Canonical disposition and a just-arrived receipt share one destination
        // action. Keep receipt fallback while the selected page is refreshing.
        if let Some(detail) = visible {
            let disposition = &detail["record"]["disposition"];
            if matches!(
                disposition["kind"].as_str(),
                Some("adopted" | "adopted_inbox_goal")
            ) && disposition["target"]["path"].is_string()
            {
                target = disposition["target"].clone();
            }
        }
        let mut panel = v_flex().gap_2();
        if self.adoption.active || !self.adoption.pending.is_empty() {
            let origin = self.adoption.delivery_origin();
            panel = panel.child(format!(
                "Unverified suggestion · {} · {} · original owner {}",
                text(&origin["record"]["id"]),
                text(&origin["source"]["revision"]),
                origin["record"]["goal_id"].as_str().unwrap_or("Inbox")
            ));
        }
        if let Some(e) = &self.adoption.error {
            panel = panel.child(e.clone());
        }
        if visible.is_none() && (notice.is_some() || target["path"].is_string()) {
            panel = panel.child(format!(
                "Last adoption update · original owner {} · suggestion {}",
                feedback_origin["record"]["goal_id"]
                    .as_str()
                    .unwrap_or("Inbox"),
                text(&feedback_origin["record"]["id"])
            ));
        }
        if let Some(n) = notice {
            panel = panel.child(n.to_owned());
        }
        if !self.adoption.pending.is_empty() {
            panel = panel.child(
                super::super::brand::control("proposal-adoption-recover", cx)
                    .label("Recover adoption delivery")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.recover_proposal_adoption(window, cx)
                    })),
            );
        }
        let history: Vec<_> = self
            .adoption
            .retired
            .iter()
            .filter(|p| visible.is_none_or(|detail| AdoptionUi::matches_origin(&p.detail, detail)))
            .cloned()
            .collect();
        if !history.is_empty() {
            panel = panel.child(
                super::super::brand::control("proposal-adoption-history", cx)
                    .label(format!(
                        "{} earlier submissions ({})",
                        if self.adoption.show_history {
                            "Hide"
                        } else {
                            "Show"
                        },
                        history.len()
                    ))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.adoption.show_history = !this.adoption.show_history;
                        cx.notify();
                    })),
            );
            if self.adoption.show_history {
                panel = panel.child(
                    "These earlier submissions were not applied. Their edited drafts remain available for inspection.",
                );
                for p in history {
                    let id = format!("adoption-retired-{}", text(&p.request["operation_id"]));
                    panel = panel
                        .child(div().text_sm().child(format!(
                            "Earlier submission · {} · suggestion {}",
                            text(&p.request["operation_id"]),
                            text(&p.request["proposal_id"])
                        )))
                        .child(
                            super::super::brand::control(SharedString::from(id), cx)
                                .label("Inspect earlier submitted draft")
                                .disabled(
                                    self.busy
                                        || self.adoption.active
                                        || !self.adoption.pending.is_empty(),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.reopen_retired_adoption(p.clone(), window, cx)
                                })),
                        );
                }
            }
        }
        if target["path"].is_string() {
            panel = panel.child(
                super::super::brand::control("proposal-adoption-open-target", cx)
                    .label("Open original adopted destination")
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_source(text(&target["path"]), window, cx);
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

    #[test]
    fn completed_adoption_feedback_follows_original_proposal_across_navigation() {
        let inbox = json!({"record":{"id":"inbox-proposal","goal_id":null}});
        let amber = json!({"record":{"id":"amber-proposal","goal_id":"amber",
            "attempt":{"state":"failed"},"disposition":{"kind":"unreviewed"}}});
        let target = json!({"path":"records/goal-from-inbox.md"});
        let mut ui = AdoptionUi {
            notice: Some("Suggestion adopted".into()),
            notice_detail: inbox.clone(),
            receipt_detail: inbox.clone(),
            receipt: json!({"result":{"receipt":{"target":target}}}),
            ..Default::default()
        };
        assert_eq!(
            ui.feedback_for(&inbox),
            (Some("Suggestion adopted"), target.clone())
        );
        // Navigating to the unrelated failed attempt must not claim its success
        // or offer the prior Inbox destination. Returning preserves the receipt.
        ui.active = true;
        ui.detail = inbox.clone(); // A retained form may still be active elsewhere.
        assert_eq!(ui.feedback_origin(Some(&amber)), &amber);
        assert_eq!(
            ui.feedback_for(ui.feedback_origin(Some(&amber))),
            (None, Value::Null)
        );
        assert_eq!(ui.feedback_for(&Value::Null), (None, Value::Null));
        // A global summary still acknowledges completion under its actual owner,
        // including after finish clears active forms and proposal inspection.
        ui.active = false;
        assert_eq!(ui.feedback_origin(None), &inbox);
        assert_eq!(
            ui.feedback_for(ui.feedback_origin(None)),
            (Some("Suggestion adopted"), target.clone())
        );
        let mut wrong_owner = inbox.clone();
        wrong_owner["record"]["goal_id"] = json!("amber");
        assert_eq!(ui.feedback_for(&wrong_owner), (None, Value::Null));
        assert_eq!(
            ui.feedback_for(&inbox),
            (Some("Suggestion adopted"), target.clone())
        );
        // Desktop restoration retains the destination binding without inventing
        // a transient success notice for whatever happens to be selected.
        ui.notice = None;
        assert_eq!(ui.feedback_for(&amber), (None, Value::Null));
        assert_eq!(ui.feedback_for(&inbox), (None, target));
    }
}
