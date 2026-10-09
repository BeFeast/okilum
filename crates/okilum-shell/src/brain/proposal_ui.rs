//! Owner-scoped, read-only inspection and separately retained disposition delivery.
use super::proposal_outbox::{Disposition, Pending, ProposalJournal};
use super::proposal_retry_outbox::{self, Pending as PendingRetry, RetryJournal};
use super::*;
use sha2::{Digest, Sha256};
fn valid_uuid(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| Uuid::parse_str(s).is_ok_and(|id| id.to_string() == s))
}
fn valid_proposal_id(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}
fn generation_identity(identity: &Value) -> Option<String> {
    let version = identity["policy_version"].as_u64()?;
    if version == 0 || version > u32::MAX as u64 {
        return None;
    }
    // The versioned proposal identity uses length-prefixed UTF-8 fields.
    let version = version.to_string();
    let mut hash = Sha256::new();
    for field in [
        identity["brain_id"].as_str()?,
        identity["kind"].as_str()?,
        identity["record_id"].as_str()?,
        identity["source_revision"].as_str()?,
        &version,
    ] {
        hash.update(field.len().to_string().as_bytes());
        hash.update(b":");
        hash.update(field.as_bytes());
    }
    Some(format!("{:x}", hash.finalize()))
}
fn valid_generation(row: &Value, workspace: &Value, owner: &Value) -> bool {
    let trigger = &row["trigger"];
    let identity = &trigger["identity"];
    let source_kind = match identity["kind"].as_str() {
        Some("inbox_saved") if owner.is_null() => "inbox",
        Some("decision_saved") if valid_uuid(owner) => "decision",
        Some("result_saved") if valid_uuid(owner) => "result",
        _ => return false,
    };
    trigger.get("goal_id") == Some(owner)
        && identity["brain_id"] == workspace["brain_id"]
        && valid_uuid(&identity["brain_id"])
        && valid_uuid(&identity["record_id"])
        && valid_uuid(&row["attempt"]["id"])
        && row["attempt"].get("input") == Some(&Value::Null)
        && match (row["attempt"]["state"].as_str(), row.get("issue")) {
            (Some("queued"), Some(Value::Null)) => true,
            (Some("failed"), Some(Value::String(s))) => s == "input_unavailable",
            (Some("stale"), Some(Value::String(s))) => s == "source_changed",
            _ => false,
        }
        && generation_identity(identity).as_deref() == row["proposal_id"].as_str()
        && trigger["source_path"]
            == format!(
                "{}/{}-{}.md",
                text(&workspace["records_dir"]),
                source_kind,
                text(&identity["record_id"])
            )
}
fn merge_proposal_page(
    previous: &Value,
    data: &Value,
    request: &Value,
    workspace: &Value,
    owner: &Value,
) -> Result<Value, String> {
    let items = data["items"].as_array().ok_or("Invalid proposal page.")?;
    let generation = match data.get("generation") {
        Some(value) => value.as_array().ok_or("Invalid generation page.")?.clone(),
        None => vec![],
    };
    if items.iter().any(|d| !valid_detail(d, workspace, owner))
        || generation
            .iter()
            .any(|r| !valid_generation(r, workspace, owner))
    {
        return Err("Proposal list ownership or identity mismatch.".into());
    }
    let limit = request["limit"].as_u64().unwrap_or(20);
    if items.len() + generation.len() > limit as usize {
        return Err("Proposal page exceeds the requested limit.".into());
    }
    let mut ids = std::collections::BTreeSet::new();
    for id in items
        .iter()
        .map(|d| &d["record"]["id"])
        .chain(generation.iter().map(|r| &r["proposal_id"]))
    {
        if !valid_proposal_id(id)
            || !ids.insert(text(id))
            || request["cursor"]
                .as_str()
                .is_some_and(|cursor| text(id).as_str() <= cursor)
        {
            return Err("Proposal page contains duplicate or out-of-order identities.".into());
        }
    }
    if !data["next_cursor"].is_null()
        && (!valid_proposal_id(&data["next_cursor"])
            || ids.last().map(String::as_str) != data["next_cursor"].as_str())
    {
        return Err("Proposal page cursor does not match its last identity.".into());
    }
    let mut merged = data.clone();
    let (mut retained_items, mut retained_generation) = if request["cursor"].is_null() {
        (vec![], vec![])
    } else {
        if request["cursor"] != previous["next_cursor"] {
            return Err("Proposal page no longer follows the displayed page.".into());
        }
        (array(&previous["items"]), array(&previous["generation"]))
    };
    retained_items.extend(items.iter().cloned());
    retained_generation.extend(generation);
    merged["items"] = json!(retained_items);
    merged["generation"] = json!(retained_generation);
    Ok(merged)
}
fn owner_key(owner: &Value) -> String {
    owner.as_str().unwrap_or("inbox").to_owned()
}
fn proposal_key(detail: &Value) -> String {
    format!(
        "{}:{}",
        owner_key(&detail["record"]["goal_id"]),
        text(&detail["record"]["id"])
    )
}
pub(super) fn valid_detail(detail: &Value, workspace: &Value, owner: &Value) -> bool {
    let record = &detail["record"];
    let source = &detail["source"];
    record.get("goal_id") == Some(owner)
        && (owner.is_null() || owner.as_str().is_some_and(|v| Uuid::parse_str(v).is_ok()))
        && record["brain_id"] == workspace["brain_id"]
        && source["brain_id"] == workspace["brain_id"]
        && record["schema"] == "tessera-proposal/v1"
        && record["record_type"] == "proposal"
        && record["id"]
            .as_str()
            .is_some_and(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        && source["path"]
            == format!(
                "{}/proposal-{}.md",
                text(&workspace["records_dir"]),
                text(&record["id"])
            )
        && source["revision"].as_str().is_some_and(|v| {
            v.len() == 71
                && v.starts_with("sha256:")
                && v[7..].bytes().all(|b| b.is_ascii_hexdigit())
        })
}
fn parse_deadline(value: &str) -> Result<String, String> {
    let date =
        time::OffsetDateTime::parse(value.trim(), &time::format_description::well_known::Rfc3339)
            .map_err(|_| {
            "Enter a full date and time with an explicit offset, e.g. 2026-09-09T09:00:00+03:00."
                .to_string()
        })?;
    if date <= time::OffsetDateTime::now_utc() {
        return Err("Choose a future snooze deadline.".into());
    }
    date.to_offset(time::UtcOffset::UTC)
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|e| e.to_string())
}
fn display_deadline(value: &str) -> String {
    let Ok(date) =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
    else {
        return value.into();
    };
    match time::UtcOffset::local_offset_at(date) {
        Ok(offset) => format!(
            "{} (local offset {})",
            date.to_offset(offset)
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
            offset
        ),
        Err(_) => format!(
            "{} (UTC; local zone unavailable)",
            date.to_offset(time::UtcOffset::UTC)
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default()
        ),
    }
}
fn snoozed(detail: &Value) -> bool {
    detail["record"]["disposition"]["kind"] == "snoozed"
        && detail["record"]["disposition"]["until"]
            .as_str()
            .and_then(|v| {
                time::OffsetDateTime::parse(v, &time::format_description::well_known::Rfc3339).ok()
            })
            .is_some_and(|v| v > time::OffsetDateTime::now_utc())
}
pub(super) struct ProposalUi {
    journal: Option<ProposalJournal>,
    pub pending: Vec<Pending>,
    retry_journal: Option<RetryJournal>,
    pub retry_pending: Vec<PendingRetry>,
    owner: Value,
    page: Value,
    selected: Value,
    pub active: bool,
    sequence: u64,
    deadline: Entity<InputState>,
    drafts: BTreeMap<String, String>,
    show_retained: bool,
    show_frozen_source: bool,
    pub error: Option<String>,
    notice: Option<String>,
    _deadline_subscription: Subscription,
    deadline_wake: Option<String>,
}
impl ProposalUi {
    pub fn new(window: &mut Window, cx: &mut Context<BrainView>) -> Self {
        let deadline =
            cx.new(|cx| InputState::new(window, cx).placeholder("YYYY-MM-DDTHH:MM:SS+03:00"));
        let subscription = cx.subscribe(&deadline, |_, _, _: &InputEvent, cx| cx.notify());
        Self {
            journal: None,
            pending: vec![],
            retry_journal: None,
            retry_pending: vec![],
            owner: Value::Null,
            page: Value::Null,
            selected: Value::Null,
            active: false,
            sequence: 0,
            deadline,
            _deadline_subscription: subscription,
            deadline_wake: None,
            drafts: BTreeMap::new(),
            show_retained: false,
            show_frozen_source: false,
            error: None,
            notice: None,
        }
    }
    pub fn leave(&mut self) {
        self.sequence += 1;
        self.active = false;
        self.deadline_wake = None;
    }
    fn remember(&mut self, cx: &Context<BrainView>) {
        if self.selected.is_object() {
            self.drafts.insert(
                proposal_key(&self.selected),
                self.deadline.read(cx).value().to_string(),
            );
        }
    }
    fn select(&mut self, detail: Value, window: &mut Window, cx: &mut Context<BrainView>) {
        self.remember(cx);
        let deadline = self
            .drafts
            .get(&proposal_key(&detail))
            .cloned()
            .unwrap_or_else(|| text(&detail["record"]["disposition"]["until"]));
        self.deadline
            .update(cx, |i, cx| i.set_value(deadline, window, cx));
        self.show_frozen_source = false;
        self.selected = detail;
        self.active = true;
        self.notice = None;
    }
    fn read_matches(&self, request: &Value, workspace: Option<&Value>) -> bool {
        Some(&request["_proposal_workspace"]) == workspace
            && request["_proposal_sequence"].as_u64() == Some(self.sequence)
    }
}
impl BrainView {
    fn schedule_proposal_deadline(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.collection != Collection::Attention {
            return;
        }
        let now = time::OffsetDateTime::now_utc();
        let earliest = array(&self.proposals.page["items"])
            .iter()
            .filter_map(|d| {
                d["record"]["disposition"]["until"].as_str().and_then(|v| {
                    time::OffsetDateTime::parse(v, &time::format_description::well_known::Rfc3339)
                        .ok()
                })
            })
            .filter(|at| *at > now)
            .min();
        let Some(at) = earliest else {
            self.proposals.deadline_wake = None;
            return;
        };
        let key = format!(
            "{}:{}",
            owner_key(&self.proposals.owner),
            at.unix_timestamp()
        );
        if self.proposals.deadline_wake.as_ref() == Some(&key) {
            return;
        }
        self.proposals.deadline_wake = Some(key.clone());
        let seconds = ((at - now).whole_seconds() + 1).clamp(1, 60) as u64;
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(seconds))
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.proposals.deadline_wake.as_ref() != Some(&key) {
                    return;
                }
                this.proposals.deadline_wake = None;
                if this.collection == Collection::Attention {
                    cx.notify();
                    this.schedule_proposal_deadline(window, cx);
                }
            });
        })
        .detach();
    }
    pub fn ensure_proposals(&mut self) {
        if self.proposals.retry_journal.is_none() {
            if let Some(workspace) = &self.expected_workspace {
                match RetryJournal::open(workspace).and_then(|j| Ok((j.pending()?, j))) {
                    Ok((pending, journal)) => {
                        self.proposals.retry_pending = pending;
                        self.proposals.retry_journal = Some(journal);
                    }
                    Err(error) => self.proposals.error = Some(error),
                }
            }
        }
        if self.proposals.journal.is_some() {
            return;
        }
        let Some(workspace) = &self.expected_workspace else {
            return;
        };
        match ProposalJournal::open(workspace).and_then(|j| Ok((j.pending()?, j))) {
            Ok((pending, j)) => {
                self.proposals.pending = pending;
                self.proposals.journal = Some(j);
            }
            Err(e) => self.proposals.error = Some(e),
        }
    }
    pub fn proposal_list_request(&mut self, more: bool) -> Option<Value> {
        if self.capabilities["proposal_read"] != true {
            return None;
        }
        self.ensure_proposals();
        self.proposals.sequence += 1;
        Some(
            json!({"op":"proposal_list","goal_id":self.proposals.owner,"limit":20,"cursor":if more{self.proposals.page["next_cursor"].clone()}else{Value::Null},
            "_proposal_workspace":self.expected_workspace,"_proposal_sequence":self.proposals.sequence}),
        )
    }
    fn refresh_proposals(&mut self, more: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Some(r) = self.proposal_list_request(more) {
            self.batch(vec![r], window, cx);
        }
    }
    fn inspect_proposal(&mut self, detail: Value, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.proposals.sequence += 1;
        self.proposals.select(detail.clone(), window, cx);
        self.batch(vec![json!({"op":"proposal_get","proposal_id":detail["record"]["id"],"goal_id":detail["record"]["goal_id"],
            "_proposal_workspace":self.expected_workspace,"_proposal_sequence":self.proposals.sequence})],window,cx);
    }
    fn disposition_proposal(&mut self, action: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.ensure_proposals();
        let result = (|| -> Result<Pending, String> {
            if action == "recover" {
                return self
                    .proposals
                    .pending
                    .first()
                    .cloned()
                    .ok_or("No retained proposal delivery.".into());
            }
            if !self.proposals.pending.is_empty() || !self.proposals.retry_pending.is_empty() {
                return Err("Recover the retained delivery before another disposition.".into());
            }
            if self.proposals.retry_journal.is_none() {
                return Err(
                    "Retry delivery storage is unavailable. Resolve it before a new decision."
                        .into(),
                );
            }
            if self.capabilities["proposal_disposition"] != true {
                return Err("Proposal disposition is unavailable on this connection.".into());
            }
            let disposition = if action == "reject" {
                Disposition::Rejected
            } else {
                Disposition::Snoozed {
                    until: parse_deadline(self.proposals.deadline.read(cx).value().as_ref())?,
                }
            };
            self.proposals
                .journal
                .as_ref()
                .ok_or("Proposal recovery is unavailable.")?
                .prepare(
                    &self.proposals.selected,
                    disposition,
                    &text(&self.capabilities["actor"]),
                )
        })();
        match result {
            Ok(pending) => {
                if !self.proposals.pending.contains(&pending) {
                    self.proposals.pending.push(pending.clone());
                }
                // Retain before every send, including recovery of a failed local publication.
                if let Err(e) = self
                    .proposals
                    .journal
                    .as_ref()
                    .ok_or("Proposal recovery unavailable.".to_string())
                    .and_then(|j| j.retain(&pending))
                {
                    self.proposals.error = Some(e);
                    cx.notify();
                    return;
                }
                self.proposals.error = None;
                self.proposals.notice = None;
                self.batch(vec![pending.wire()], window, cx);
            }
            Err(e) => {
                self.proposals.error = Some(e);
                cx.notify();
            }
        }
    }
    fn retry_proposal(&mut self, recover: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.ensure_proposals();
        let result = (|| -> Result<PendingRetry, String> {
            if recover {
                return self
                    .proposals
                    .retry_pending
                    .first()
                    .cloned()
                    .ok_or("No retained Retry delivery.".into());
            }
            if !self.proposals.pending.is_empty() || !self.proposals.retry_pending.is_empty() {
                return Err("Recover the retained proposal delivery before Retry.".into());
            }
            if self.proposals.journal.is_none() {
                return Err(
                    "Proposal delivery storage is unavailable. Resolve it before a new Retry."
                        .into(),
                );
            }
            if self.capabilities["proposal_retry"] != true {
                return Err("New Retry is unavailable on this connection.".into());
            }
            self.proposals
                .retry_journal
                .as_ref()
                .ok_or("Retry recovery is unavailable.")?
                .prepare(&self.proposals.selected, &text(&self.capabilities["actor"]))
        })();
        match result {
            Ok(pending) => {
                if !self.proposals.retry_pending.contains(&pending) {
                    self.proposals.retry_pending.push(pending.clone());
                }
                if let Err(error) = self
                    .proposals
                    .retry_journal
                    .as_ref()
                    .ok_or("Retry recovery unavailable.".to_string())
                    .and_then(|j| j.retain(&pending))
                {
                    self.proposals.error = Some(error);
                    cx.notify();
                    return;
                }
                self.proposals.error = None;
                self.proposals.notice = None;
                self.batch(vec![pending.wire()], window, cx);
            }
            Err(error) => {
                self.proposals.error = Some(error);
                cx.notify();
            }
        }
    }
    fn proposal_retry_reply(&mut self, data: Value, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(pending) = serde_json::from_value::<PendingRetry>(
            data["_client_proposal_request"]["_proposal_pending"].clone(),
        ) else {
            return;
        };
        let Some(index) = self
            .proposals
            .retry_pending
            .iter()
            .position(|p| p == &pending)
        else {
            return;
        };
        if self.expected_workspace.as_ref() != Some(&pending.workspace) {
            return;
        }
        if data["_proposal_error"].is_object() {
            self.proposals.error = Some(format!("{} The original Retry request remains retained. Recover Retry delivery sends that same operation.", text(&data["_proposal_error"]["message"])));
            return;
        }
        if let Err(error) = self
            .proposals
            .retry_journal
            .as_ref()
            .ok_or("Retry recovery unavailable.".to_string())
            .and_then(|j| j.acknowledge(&pending, &data))
        {
            self.proposals.error = Some(error);
            return;
        }
        self.proposals.retry_pending.remove(index);
        self.proposals.error = None;
        self.proposals.notice = Some(if data["result"]["outcome"] == "not_applied" {
            format!("Retry was not applied: {}. The original delivery is closed. Inspect the refreshed suggestion before another action.",text(&data["result"]["reason"]))
        } else {
            "Retry accepted. The previous attempt remains in history; completion follows separately.".into()
        });
        // A delivery receipt never changes navigation or fabricates the current projection.
        if self.collection == Collection::Attention
            && self.proposals.active
            && self.proposals.selected["record"]["id"] == pending.request.proposal_id
            && self.proposals.selected["record"]["goal_id"] == json!(pending.request.goal_id)
            && self.proposals.selected["source"]["revision"] == pending.request.expected_revision
        {
            self.proposals.selected["projection_pending"] = json!(true);
            let detail = self.proposals.selected.clone();
            let notice = self.proposals.notice.clone();
            self.inspect_proposal(detail, window, cx);
            self.proposals.notice = notice;
        }
    }
    pub fn proposal_reply(
        &mut self,
        op: &str,
        data: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let request = &data["_client_proposal_request"];
        if op == "proposal_retry" {
            self.proposal_retry_reply(data, window, cx);
            return;
        }
        if op == "proposal_disposition" {
            let Ok(pending) =
                serde_json::from_value::<Pending>(request["_proposal_pending"].clone())
            else {
                return;
            };
            let Some(index) = self.proposals.pending.iter().position(|p| p == &pending) else {
                return;
            };
            if self.expected_workspace.as_ref() != Some(&pending.workspace) {
                return;
            }
            if data["_proposal_error"].is_object() {
                self.proposals.error=Some(format!("{} The original request remains retained. Inspect current sources; Recover delivery never creates another operation.",text(&data["_proposal_error"]["message"])));
                return;
            }
            if data["outcome"] == "not_applied"
                && self.capabilities["proposal_disposition_terminal"] != true
            {
                self.proposals.error = Some("Terminal delivery acknowledgement is unavailable on this connection; retain the original request.".into());
                return;
            }
            if let Err(e) = self
                .proposals
                .journal
                .as_ref()
                .ok_or("Proposal recovery unavailable.".to_string())
                .and_then(|j| j.acknowledge(&pending, &data))
            {
                self.proposals.error = Some(e);
                return;
            }
            let terminal = data["outcome"] == "not_applied";
            self.proposals.pending.remove(index);
            self.proposals.error = None;
            self.proposals.notice = Some(if terminal {
                format!("Nothing applied: the Snooze deadline elapsed. Original delivery {} is closed. Inspect the refreshed proposal before choosing a new deadline or Reject.", pending.request.operation_id)
            } else {
                format!(
                    "{} saved. Receipt {}",
                    text(&data["disposition"]["kind"]),
                    pending.request.operation_id
                )
            });
            // The receipt reconciles its own outbox. A late reply cannot select a different proposal or goal.
            if self.collection == Collection::Attention
                && self.proposals.active
                && self.proposals.selected["record"]["id"] == pending.request.proposal_id
                && self.proposals.selected["record"]["goal_id"] == json!(pending.request.goal_id)
                && self.proposals.selected["source"]["revision"]
                    == pending.request.expected_revision
            {
                if !terminal {
                    self.proposals.selected["record"]["disposition"] = data["disposition"].clone();
                }
                // The full canonical snapshot must come from get, never a fabricated source revision.
                self.proposals.selected["projection_pending"] = json!(true);
                self.proposals
                    .drafts
                    .remove(&proposal_key(&self.proposals.selected));
                self.proposals.deadline.update(cx, |i, cx| {
                    i.set_value(
                        if terminal {
                            String::new()
                        } else {
                            text(&data["disposition"]["until"])
                        },
                        window,
                        cx,
                    )
                });
                let detail = self.proposals.selected.clone();
                let notice = self.proposals.notice.clone();
                self.inspect_proposal(detail, window, cx);
                self.proposals.notice = notice;
            }
            return;
        }
        if self.collection != Collection::Attention
            || !self
                .proposals
                .read_matches(request, self.expected_workspace.as_ref())
        {
            return;
        }
        if data["_proposal_error"].is_object() {
            self.proposals.error = Some(text(&data["_proposal_error"]["message"]));
            return;
        }
        if op == "proposal_list" && request["goal_id"] == self.proposals.owner {
            let page = merge_proposal_page(
                &self.proposals.page,
                &data,
                request,
                self.expected_workspace.as_ref().unwrap(),
                &self.proposals.owner,
            );
            let Ok(page) = page else {
                self.proposals.error = page.err();
                return;
            };
            self.proposals.page = page;
            self.proposals.error = None;
        } else if op == "proposal_get"
            && self.proposals.active
            && valid_detail(
                &data,
                self.expected_workspace.as_ref().unwrap(),
                &request["goal_id"],
            )
            && data["record"]["id"] == request["proposal_id"]
            && data["record"]["goal_id"] == request["goal_id"]
            && self.proposals.selected["record"]["id"] == request["proposal_id"]
            && self.proposals.selected["record"]["goal_id"] == request["goal_id"]
        {
            if let Some(items) = self.proposals.page["items"].as_array_mut() {
                for item in items {
                    if proposal_key(item) == proposal_key(&data) {
                        *item = data.clone();
                    }
                }
            }
            self.proposals.selected = data;
            self.proposals.error = None;
        }
        self.schedule_proposal_deadline(window, cx);
    }
    fn proposal_owner_title(&self, owner: &Value) -> String {
        if owner.is_null() {
            return "Inbox · no goal".into();
        }
        let title = array(&self.snapshot["goals"])
            .into_iter()
            .find(|g| g["id"] == *owner)
            .map(|g| text(&g["title"]))
            .unwrap_or_else(|| "Goal".into());
        format!("{} · {}", title, text(owner))
    }
    pub fn proposals_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut rows = v_flex().gap_2();
        if self.capabilities["proposal_read"] != true
            && self.proposals.pending.is_empty()
            && self.proposals.retry_pending.is_empty()
        {
            return rows.into_any_element();
        }
        rows = rows.child(div().text_lg().child("Suggestions"));
        let mut owners = vec![(Value::Null, "Inbox · no goal".to_string())];
        owners.extend(array(&self.snapshot["goals"]).iter().map(|g| {
            (
                g["id"].clone(),
                format!("{} · {}", text(&g["title"]), text(&g["id"])),
            )
        }));
        for (owner, label) in owners {
            let id = format!("proposal-owner-{}", owner_key(&owner));
            rows = rows.child(
                super::super::brand::control(SharedString::from(id.clone()), cx)
                    .debug_selector(move || id.clone())
                    .label(label)
                    .w_full()
                    .h_auto()
                    .whitespace_normal()
                    .disabled(self.busy || self.capabilities["proposal_read"] != true)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.proposals.remember(cx);
                        this.proposals.owner = owner.clone();
                        this.proposals.page = Value::Null;
                        this.proposals.active = false;
                        this.refresh_proposals(false, window, cx);
                    })),
            );
        }
        rows = rows.child(
            super::super::brand::control("proposal-show-retained", cx)
                .label(if self.proposals.show_retained {
                    "Hide rejected / snoozed"
                } else {
                    "Show rejected / snoozed"
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.proposals.show_retained = !this.proposals.show_retained;
                    cx.notify();
                })),
        );
        if let Some(backlog) = self.proposals.page["backlog"].as_str() {
            rows = rows.child(backlog.to_owned());
        }
        for pending in array(&self.proposals.page["generation"]) {
            rows = rows.child(format!(
                "{} · {} · {}{}",
                text(&pending["trigger"]["source_path"]),
                text(&pending["attempt"]["state"]),
                text(&pending["proposal_id"]),
                if pending["issue"].is_null() {
                    ""
                } else {
                    " · Input changed, incomplete or exceeds limit; no provider request sent."
                }
            ));
        }
        for detail in array(&self.proposals.page["items"]) {
            let record = &detail["record"];
            if !self.proposals.show_retained
                && (record["disposition"]["kind"] == "rejected" || snoozed(&detail))
            {
                continue;
            }
            let title = record["generated"]["title"]
                .as_str()
                .unwrap_or("Pending proposal");
            if !title
                .to_lowercase()
                .contains(&self.filter.read(cx).value().to_lowercase())
            {
                continue;
            }
            let id = format!("proposal-item-{}", text(&record["id"]));
            let label = format!(
                "{}\n{} · {}\nUnverified · {}",
                title,
                text(&record["attempt"]["state"]),
                text(&record["disposition"]["kind"]),
                text(&record["trigger"]["identity"]["kind"])
            );
            rows = rows.child(
                super::super::brand::control(SharedString::from(id.clone()), cx)
                    .debug_selector(move || id.clone())
                    .label(label)
                    .w_full()
                    .h_auto()
                    .whitespace_normal()
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.inspect_proposal(detail.clone(), window, cx)
                    })),
            );
        }
        if self.proposals.page["items"].is_array() {
            rows =
                rows.child(
                    super::super::brand::control("proposal-refresh", cx)
                        .label("Refresh suggestions")
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.refresh_proposals(false, window, cx)
                        })),
                );
        }
        if self.proposals.page["next_cursor"].is_string() {
            rows = rows.child(
                super::super::brand::control("proposal-more", cx)
                    .label("More suggestions")
                    .disabled(self.busy)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.refresh_proposals(true, window, cx)),
                    ),
            );
        }
        if !self.proposals.pending.is_empty() || !self.proposals.retry_pending.is_empty() {
            rows = rows.child(
                super::super::brand::control("proposal-recovery-open", cx)
                    .label("Proposal delivery needs recovery")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.proposals.active = true;
                        cx.notify();
                    })),
            );
        }
        if let Some(e) = &self.proposals.error {
            rows = rows.child(e.clone());
        }
        rows = rows.child(self.adoption_origin_banner(None, cx));
        rows.into_any_element()
    }
    pub fn proposal_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let detail = self.proposals.selected.clone();
        let record = &detail["record"];
        let mut panel = v_flex()
            .id("proposal-panel")
            .debug_selector(|| "proposal-panel".into())
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .whitespace_normal()
            .p_6()
            .gap_3()
            .child(
                div().text_2xl().child(
                    record["generated"]["title"]
                        .as_str()
                        .unwrap_or("Suggestion")
                        .to_owned(),
                ),
            )
            .child("Unverified AI proposal")
            .child(self.proposal_owner_title(&record["goal_id"]));
        if let Some(error) = &self.proposals.error {
            panel = panel.child(error.clone());
        }
        if let Some(notice) = &self.proposals.notice {
            panel = panel.child(notice.clone());
        }
        if let Some(p) = self.proposals.pending.first() {
            panel = panel
                .child(format!(
                    "Delivery unconfirmed · {} · proposal {} · {}",
                    p.request.operation_id,
                    p.request.proposal_id,
                    self.proposal_owner_title(&json!(p.request.goal_id))
                ))
                .child(
                    super::super::brand::control("proposal-recover", cx)
                        .debug_selector(|| "proposal-recover".into())
                        .label("Recover delivery")
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.disposition_proposal("recover", window, cx)
                        })),
                );
        }
        if let Some(p) = self.proposals.retry_pending.first() {
            panel =
                panel
                    .child(format!(
                        "Retry delivery unconfirmed · {} · {}",
                        p.request.operation_id,
                        self.proposal_owner_title(&json!(p.request.goal_id))
                    ))
                    .child(
                        super::super::brand::control("proposal-retry-recover", cx)
                            .debug_selector(|| "proposal-retry-recover".into())
                            .label("Recover Retry delivery")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.retry_proposal(true, window, cx)
                            })),
                    );
        }
        panel = panel.child(self.adoption_origin_banner(Some(&detail), cx));
        if self
            .expected_workspace
            .as_ref()
            .is_some_and(|w| super::proposal_form_state::eligible(&detail, w, &record["goal_id"]))
        {
            let selected = detail.clone();
            panel = panel.child(
                super::super::brand::control("proposal-use-draft", cx)
                    .label("Use draft")
                    .disabled(
                        self.busy
                            || self.capabilities["proposal_adopt"] != true
                            || self.adoption.active
                            || !self.adoption.pending.is_empty(),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.begin_proposal_adoption(selected.clone(), window, cx)
                    })),
            );
        }
        if !record.is_object() {
            return panel.into_any_element();
        }
        panel = panel
            .child(format!(
                "Attempt: {} · Disposition: {}",
                text(&record["attempt"]["state"]),
                text(&record["disposition"]["kind"])
            ))
            .child(format!(
                "Original trigger: {} · {}",
                text(&record["trigger"]["identity"]["kind"]),
                text(&record["trigger"]["received_at"])
            ))
            .child(format!(
                "Generation actor: {} · model: {} · attempt: {}",
                text(&record["generation_actor"]),
                text(&record["attempt"]["input"]["model"]),
                text(&record["attempt"]["id"])
            ))
            .child(format!(
                "Provider: {} · Generated: {}",
                text(&record["attempt"]["input"]["provider_identity"]),
                record["generated_at"].as_str().unwrap_or("not available")
            ));
        if let Ok(original) = decode_source(&record["captured"]["trigger_source"]) {
            let body = original
                .strip_prefix("---\n")
                .and_then(|v| v.split_once("\n---\n").map(|(_, body)| body))
                .unwrap_or(&original);
            let excerpt = body.chars().take(2000).collect::<String>();
            panel = panel
                .child(div().text_lg().child("Captured original input"))
                .child(excerpt);
            if body.chars().count() > 2000 {
                panel =
                    panel.child("Preview shortened. The full retained source is available below.");
            }
            panel = panel.child(
                super::super::brand::control("proposal-frozen-source", cx)
                    .debug_selector(|| "proposal-frozen-source".into())
                    .label(if self.proposals.show_frozen_source {
                        "Hide retained source"
                    } else {
                        "View full retained source"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.proposals.show_frozen_source = !this.proposals.show_frozen_source;
                        cx.notify();
                    })),
            );
            if self.proposals.show_frozen_source {
                panel = panel
                    .child(
                        "Retained original snapshot · read-only · may differ from the current file",
                    )
                    .child(div().text_sm().child(original.clone()))
                    .child(
                        super::super::brand::control("proposal-copy-frozen-source", cx)
                            .label("Copy retained source")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(original.clone()))
                            }),
                    );
            }
        }
        panel = panel.child(format!(
            "Trigger identity: {} · original revision {}",
            text(&record["trigger"]["identity"]["record_id"]),
            text(&record["trigger"]["identity"]["source_revision"])
        ));
        if detail["projection_pending"] == true {
            panel = panel.child(
                "Canonical projection pending. Actions are unavailable until recovery finishes.",
            );
        }
        if let Some(failure) = record["failure"].as_str() {
            panel = panel.child(format!("Generation unavailable: {failure}"));
        }
        if matches!(
            record["attempt"]["state"].as_str(),
            Some("failed" | "interrupted")
        ) {
            panel = panel.child("Retry requests one new attempt using the retained input and saved provider. Nothing is sent automatically.")
                .child(super::super::brand::control("proposal-retry",cx)
                    .debug_selector(||"proposal-retry".into()).label("Retry suggestion")
                    .disabled(self.busy || self.capabilities["proposal_retry"] != true || self.proposals.journal.is_none() || self.proposals.retry_journal.is_none() || !self.proposals.pending.is_empty() || !self.proposals.retry_pending.is_empty() || !proposal_retry_outbox::eligible(&detail))
                    .on_click(cx.listener(|this,_,window,cx|this.retry_proposal(false,window,cx))));
        }
        if !array(&record["attempt_history"]).is_empty() {
            panel = panel.child(div().text_lg().child("Previous attempts"));
            if array(&record["attempt_history"]).len() >= 32 {
                panel = panel.child(
                    "Attempt history is full. This suggestion cannot start another attempt.",
                );
            }
            for entry in array(&record["attempt_history"]) {
                panel = panel.child(format!(
                    "{} · {} · {} · archived {} · model {} · input {}",
                    text(&entry["attempt"]["id"]),
                    text(&entry["attempt"]["state"]),
                    text(&entry["failure"]),
                    text(&entry["archived_at"]),
                    text(&entry["attempt"]["input"]["model"]),
                    text(&entry["attempt"]["input"]["input_sha256"])
                ));
            }
        }
        if !record["generated"].is_object() {
            panel = panel.child("No validated generated content is available.");
        } else {
            panel = panel
                .child(text(&record["generated"]["rationale"]))
                .child(div().text_lg().child("Proposed criteria"));
            for criterion in array(&record["generated"]["criteria"]) {
                panel = panel.child(format!("• {}", text(&criterion)));
            }
            for question in array(&record["generated"]["open_questions"]) {
                panel = panel.child(format!("Open question: {}", text(&question)));
            }
        }
        for reason in array(&detail["stale_reasons"]) {
            panel = panel.child(format!("Source needs review: {}", text(&reason)));
        }
        for omission in array(&record["captured"]["omissions"]) {
            panel = panel.child(format!("Omitted input: {}", text(&omission)));
        }
        let mut sources = vec![
            (
                "Current canonical proposal file".to_string(),
                detail["source"].clone(),
            ),
            (
                "Current trigger file (may differ from captured input)".into(),
                record["captured"]["trigger_source"].clone(),
            ),
        ];
        for citation in array(&record["captured"]["citations"]) {
            panel = panel.child(format!(
                "Citation {} · {}\n{}",
                text(&citation["citation_id"]),
                text(&citation["locator"]),
                text(&citation["excerpt"])
            ));
            sources.push((
                "Current cited file (captured excerpt shown above)".into(),
                citation,
            ));
        }
        for (i, (label, source)) in sources.into_iter().enumerate() {
            let path = text(&source["path"]);
            if path.is_empty() {
                continue;
            }
            panel = panel
                .child(format!("{} · {}", label, text(&source["revision"])))
                .child(
                    super::super::brand::control(
                        SharedString::from(format!("proposal-source-{i}")),
                        cx,
                    )
                    .label(path.clone())
                    .h_auto()
                    .whitespace_normal()
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.proposals.leave();
                        this.collection = Collection::Sources;
                        this.open_source(path.clone(), window, cx);
                    })),
                );
        }
        if let Some(until) = record["disposition"]["until"].as_str() {
            panel = panel.child(format!(
                "Snooze deadline: {}{}",
                display_deadline(until),
                if snoozed(&detail) { "" } else { " · elapsed" }
            ));
        }
        let disabled = !matches!(
            record["attempt"]["state"].as_str(),
            Some("queued" | "running" | "interrupted" | "draft" | "failed" | "stale")
        ) || !matches!(
            record["disposition"]["kind"].as_str(),
            Some("unreviewed" | "rejected" | "snoozed")
        ) || self.busy
            || self.capabilities["proposal_disposition"] != true
            || !self.proposals.pending.is_empty()
            || !self.proposals.retry_pending.is_empty()
            || self.proposals.retry_journal.is_none()
            || detail["projection_pending"] != false
            || !array(&detail["stale_reasons"]).is_empty();
        panel = panel
            .child("Snooze until (absolute date/time with timezone offset)")
            .child(Input::new(&self.proposals.deadline).disabled(disabled))
            .child(
                parse_deadline(self.proposals.deadline.read(cx).value().as_ref())
                    .map(|v| display_deadline(&v))
                    .unwrap_or_else(|_| {
                        "Include the timezone offset; the saved deadline uses UTC.".into()
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        super::super::brand::control("proposal-snooze", cx)
                            .debug_selector(|| "proposal-snooze".into())
                            .label("Snooze")
                            .disabled(disabled)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.disposition_proposal("snooze", window, cx)
                            })),
                    )
                    .child(
                        super::super::brand::control("proposal-reject", cx)
                            .debug_selector(|| "proposal-reject".into())
                            .label("Reject")
                            .disabled(disabled || record["disposition"]["kind"] == "rejected")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.disposition_proposal("reject", window, cx)
                            })),
                    ),
            )
            .child("Disposition history");
        for h in array(&record["history"]) {
            panel = panel.child(format!(
                "{} · {} · {} · {}",
                text(&h["at"]),
                text(&h["actor"]),
                text(&h["disposition"]["kind"]),
                text(&h["operation_id"])
            ));
        }
        panel.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::super::proposal_outbox::tests::{detail, journal, receipt, workspace};
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn proposal_deadline_requires_offset_and_retains_absolute_instant() {
        assert_eq!(
            parse_deadline("2099-01-01T09:00:00+03:00").unwrap(),
            "2099-01-01T06:00:00Z"
        );
        assert!(parse_deadline("2099-01-01T09:00:00").is_err());
        assert!(parse_deadline("2000-01-01T09:00:00Z").is_err());
        let mut d = detail();
        d["record"]["disposition"] = json!({"kind":"snoozed","until":"2000-01-01T00:00:00Z"});
        assert!(!snoozed(&d));
        d["record"]["disposition"]["until"] = json!("2099-01-01T00:00:00Z");
        assert!(snoozed(&d));
    }
    fn generation_row(number: u128) -> Value {
        let record_id = Uuid::from_u128(number + 1).to_string();
        let identity = json!({"brain_id":workspace()["brain_id"],"kind":"inbox_saved","record_id":record_id,"source_revision":format!("sha256:{}", "d".repeat(64)),"policy_version":1});
        json!({"proposal_id":generation_identity(&identity).unwrap(),"trigger":{"identity":identity,"goal_id":null,"source_path":format!("records/inbox-{record_id}.md"),"received_at":"2026-09-07T13:00:00Z"},"attempt":{"id":Uuid::from_u128(number+1000).to_string(),"state":"queued","input":null},"issue":null})
    }
    fn mixed_page(rows: &[Value], more: bool) -> Value {
        let mut items = vec![];
        let mut generation = vec![];
        for (index, row) in rows.iter().enumerate() {
            if index % 3 == 0 {
                let mut canonical = detail();
                canonical["record"]["id"] = row["proposal_id"].clone();
                canonical["source"]["path"] =
                    json!(format!("records/proposal-{}.md", text(&row["proposal_id"])));
                items.push(canonical);
            } else {
                generation.push(row.clone());
            }
        }
        json!({"items":items,"generation":generation,"next_cursor":if more{rows.last().unwrap()["proposal_id"].clone()}else{Value::Null},"backlog":null})
    }
    #[gpui::test]
    fn proposal_mixed_pages_retain_queued_and_failed_rows_and_reject_foreign_owner(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            view.busy = true;
            view.expected_workspace = Some(workspace());
            view.collection = Collection::Attention;
            view.proposals.sequence = 1;
            let mut rows = (0..37).map(generation_row).collect::<Vec<_>>();
            rows.sort_by_key(|row| text(&row["proposal_id"]));
            rows[1]["attempt"]["state"] = json!("failed");
            rows[1]["issue"] = json!("input_unavailable");
            rows[2]["attempt"]["state"] = json!("stale");
            rows[2]["issue"] = json!("source_changed");
            let request = json!({"goal_id":null,"limit":20,"cursor":null,"_proposal_workspace":workspace(),"_proposal_sequence":1});
            let mut first = mixed_page(&rows[..20], true);
            first["_client_proposal_request"] = request.clone();
            view.proposal_reply("proposal_list", first, window, cx);
            assert!(view.proposals.error.is_none());
            assert_eq!(array(&view.proposals.page["items"]).len(), 7);
            assert_eq!(array(&view.proposals.page["generation"]).len(), 13);
            view.proposals.sequence = 2;
            let next_request = json!({"goal_id":null,"limit":20,"cursor":rows[19]["proposal_id"],"_proposal_workspace":workspace(),"_proposal_sequence":2});
            let mut second = mixed_page(&rows[20..], false);
            second["_client_proposal_request"] = next_request;
            view.proposal_reply("proposal_list", second, window, cx);
            assert!(view.proposals.error.is_none());
            assert_eq!(array(&view.proposals.page["items"]).len(), 13);
            assert_eq!(array(&view.proposals.page["generation"]).len(), 24);
            assert!(array(&view.proposals.page["generation"])
                .iter()
                .any(|row| row["issue"] == "input_unavailable"));
            assert!(array(&view.proposals.page["generation"])
                .iter()
                .any(|row| row["issue"] == "source_changed"));
            let retained = view.proposals.page.clone();
            let mut foreign = generation_row(100);
            foreign["trigger"]["goal_id"] = json!(Uuid::new_v4().to_string());
            let mut current_request = request;
            current_request["_proposal_sequence"] = json!(2);
            let bad = json!({"items":[],"generation":[foreign],"next_cursor":null,"_client_proposal_request":current_request});
            view.proposal_reply("proposal_list", bad, window, cx);
            assert!(view.proposals.error.is_some());
            assert_eq!(view.proposals.page, retained);
            view
        });
    }
    #[test]
    fn proposal_generation_rows_bind_identity_and_mixed_cursor_before_publication() {
        let row = generation_row(1);
        assert!(valid_generation(&row, &workspace(), &Value::Null));
        for field in ["brain_id", "record_id", "source_revision", "kind"] {
            let mut changed = row.clone();
            changed["trigger"]["identity"][field] = json!("changed");
            assert!(!valid_generation(&changed, &workspace(), &Value::Null));
        }
        let mut changed = row.clone();
        changed["proposal_id"] = json!("f".repeat(64));
        assert!(!valid_generation(&changed, &workspace(), &Value::Null));
        changed = row.clone();
        changed["trigger"]["source_path"] = json!("records/foreign.md");
        assert!(!valid_generation(&changed, &workspace(), &Value::Null));
        let request = json!({"cursor":null,"limit":20});
        let duplicate = json!({"items":[],"generation":[row.clone(),row],"next_cursor":null});
        assert!(merge_proposal_page(
            &Value::Null,
            &duplicate,
            &request,
            &workspace(),
            &Value::Null
        )
        .is_err());
        let mut page = mixed_page(&[generation_row(2)], false);
        page["next_cursor"] = json!("f".repeat(64));
        assert!(
            merge_proposal_page(&Value::Null, &page, &request, &workspace(), &Value::Null).is_err()
        );
    }
    #[gpui::test]
    fn proposal_late_receipt_reconciles_without_navigating_or_replacing_drafts(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = std::env::temp_dir().join(format!("proposal-ui-{}", uuid()));
        let cleanup = dir.clone();
        cx.add_window_view(move |window, cx| {
            let mut v = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            v.busy = true;
            v.expected_workspace = Some(workspace());
            v.collection = Collection::Goals;
            v.selected_goal_id = Some("other-goal".into());
            v.snapshot = json!({"goal":{"id":"other-goal"}});
            v.source.reset("source draft", window, cx);
            v.compose
                .update(cx, |i, cx| i.set_value("composer draft", window, cx));
            v.proposals.deadline.update(cx, |i, cx| {
                i.set_value("2099-02-01T09:00:00+03:00", window, cx)
            });
            let j = journal(&dir);
            let p = j
                .prepare(&detail(), Disposition::Rejected, "operator")
                .unwrap();
            j.retain(&p).unwrap();
            v.proposals.pending = vec![p.clone()];
            v.proposals.journal = Some(j);
            let mut response = receipt(&p);
            response["_client_proposal_request"] = p.wire();
            v.proposal_reply("proposal_disposition", response, window, cx);
            assert!(v.proposals.pending.is_empty());
            assert!(journal(&dir).pending().unwrap().is_empty());
            assert!(v.collection == Collection::Goals);
            assert_eq!(v.selected_goal_id.as_deref(), Some("other-goal"));
            assert_eq!(v.source.value(cx).as_ref(), "source draft");
            assert_eq!(v.compose.read(cx).value().as_ref(), "composer draft");
            assert_eq!(
                v.proposals.deadline.read(cx).value().as_ref(),
                "2099-02-01T09:00:00+03:00"
            );
            v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
    #[gpui::test]
    fn proposal_terminal_receipt_retires_without_claiming_saved_or_touching_other_draft(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = std::env::temp_dir().join(format!("proposal-ui-{}", uuid()));
        let cleanup = dir.clone();
        cx.add_window_view(move |window, cx| {
            let mut v = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            v.busy = true;
            v.expected_workspace = Some(workspace());
            v.collection = Collection::Goals;
            v.selected_goal_id = Some("other-goal".into());
            v.snapshot = json!({"goal":{"id":"other-goal"}});
            v.source.reset("source draft", window, cx);
            v.compose
                .update(cx, |i, cx| i.set_value("composer draft", window, cx));
            v.proposals.deadline.update(cx, |i, cx| {
                i.set_value("2099-02-01T09:00:00+03:00", window, cx)
            });
            let j = journal(&dir);
            let p = j
                .prepare(&detail(), Disposition::Snoozed { until: "2020-01-01T00:00:00Z".into() }, "operator")
                .unwrap();
            j.retain(&p).unwrap();
            v.proposals.pending = vec![p.clone()];
            v.proposals.journal = Some(j);
            v.capabilities["proposal_disposition_terminal"] = json!(true);
            let mut response = json!({"schema":"tessera-proposal-terminal/v1","outcome":"not_applied","workspace":p.workspace,"request":p.request,"path":p.path,"reason":"deadline_elapsed","at":"2026-09-07T12:00:00Z","replayed":true});
            response["_client_proposal_request"] = p.wire();
            v.proposal_reply("proposal_disposition", response, window, cx);
            assert!(v.proposals.notice.as_ref().unwrap().starts_with("Nothing applied:"));
            assert!(v.proposals.pending.is_empty());
            assert!(journal(&dir).pending().unwrap().is_empty());
            assert!(v.collection == Collection::Goals);
            assert_eq!(v.selected_goal_id.as_deref(), Some("other-goal"));
            assert_eq!(v.source.value(cx).as_ref(), "source draft");
            assert_eq!(v.compose.read(cx).value().as_ref(), "composer draft");
            assert_eq!(
                v.proposals.deadline.read(cx).value().as_ref(),
                "2099-02-01T09:00:00+03:00"
            );
            v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
    #[gpui::test]
    fn proposal_reads_are_owner_sequence_bound_and_stale_delivery_remains_retained(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = std::env::temp_dir().join(format!("proposal-ui-{}", uuid()));
        let cleanup = dir.clone();
        cx.add_window_view(move|window,cx|{let mut v=BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);v.busy=true;v.expected_workspace=Some(workspace());v.collection=Collection::Attention;v.proposals.sequence=2;v.proposals.select(detail(),window,cx);
            let original=v.proposals.selected.clone();let mut reply=detail();reply["record"]["generated"]=json!({"title":"late"});reply["_client_proposal_request"]=json!({"proposal_id":detail()["record"]["id"],"goal_id":null,"_proposal_workspace":workspace(),"_proposal_sequence":1});v.proposal_reply("proposal_get",reply.clone(),window,cx);assert_eq!(v.proposals.selected,original);
            reply["_client_proposal_request"]["_proposal_sequence"]=json!(2);v.proposal_reply("proposal_get",reply,window,cx);assert_eq!(v.proposals.selected["record"]["generated"]["title"],"late");
            let j=journal(&dir);let p=j.prepare(&detail(),Disposition::Rejected,"operator").unwrap();j.retain(&p).unwrap();v.proposals.pending=vec![p.clone()];v.proposals.journal=Some(j);v.proposals.deadline.update(cx,|i,cx|i.set_value("keep this deadline",window,cx));
            v.proposal_reply("proposal_disposition",json!({"_client_proposal_request":p.wire(),"_proposal_error":{"code":"runtime_error","message":"proposal or input changed; inspect current sources"}}),window,cx);assert_eq!(v.proposals.pending,vec![p.clone()]);assert_eq!(journal(&dir).pending().unwrap(),vec![p]);assert_eq!(v.proposals.deadline.read(cx).value().as_ref(),"keep this deadline");v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
    #[gpui::test]
    fn proposal_retry_late_ack_and_generic_errors_preserve_navigation_and_drafts(
        cx: &mut TestAppContext,
    ) {
        use super::proposal_retry_outbox::tests as retry;
        cx.update(gpui_component::init);
        let dir = std::env::temp_dir().join(format!("retry-ui-{}", uuid()));
        let cleanup = dir.clone();
        cx.add_window_view(move |window,cx| {
            let mut v=BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);
            v.busy=true;v.expected_workspace=Some(workspace());v.collection=Collection::Goals;v.selected_goal_id=Some("another goal".into());
            v.proposals.select(retry::detail(),window,cx);
            v.source.reset("source draft", window, cx);v.compose.update(cx,|i,cx|i.set_value("composer draft",window,cx));v.proposals.deadline.update(cx,|i,cx|i.set_value("keep deadline",window,cx));
            let j=retry::journal(&dir);let p=j.prepare(&retry::detail(),"operator").unwrap();j.retain(&p).unwrap();v.proposals.retry_pending=vec![p.clone()];v.proposals.retry_journal=Some(j);
            let selected=v.proposals.selected.clone();
            v.proposal_reply("proposal_retry",json!({"_client_proposal_request":p.wire(),"_proposal_error":{"code":"runtime_error","message":"input unavailable"}}),window,cx);
            assert_eq!(v.proposals.retry_pending,vec![p.clone()]);assert_eq!(retry::journal(&dir).pending().unwrap(),vec![p.clone()]);
            let mut response=retry::receipt(&p);response["_client_proposal_request"]=p.wire();
            v.expected_workspace=Some(json!({"brain_id":"other"}));v.proposal_reply("proposal_retry",response.clone(),window,cx);assert_eq!(v.proposals.retry_pending,vec![p.clone()]);
            v.expected_workspace=Some(workspace());v.proposal_reply("proposal_retry",response,window,cx);
            assert!(v.proposals.retry_pending.is_empty());assert!(retry::journal(&dir).pending().unwrap().is_empty());
            assert!(v.collection==Collection::Goals);assert_eq!(v.selected_goal_id.as_deref(),Some("another goal"));assert_eq!(v.proposals.selected,selected);
            assert_eq!(v.source.value(cx).as_ref(),"source draft");assert_eq!(v.compose.read(cx).value().as_ref(),"composer draft");assert_eq!(v.proposals.deadline.read(cx).value().as_ref(),"keep deadline");v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
}
