//! Exact observed-item actions. Reading or replying never selects a goal or
//! invokes workflow execution. Unknown delivery retains the immutable request.
use super::native_outbox::InboxJournal;
use super::*;

fn item_key(item: &Value) -> String {
    format!(
        "{}:{}:{}",
        text(&item["goal_id"]),
        text(&item["attention_id"]),
        text(&item["revision"])
    )
}
fn same_target(a: &Value, b: &Value) -> bool {
    a["goal_id"] == b["goal_id"] && a["attention_id"] == b["attention_id"]
}
fn permits(item: &Value, action: &str, actor: &str) -> bool {
    item["current"] == true
        && item["channel"] == "native"
        && item["actor_id"] == actor
        && item.get("stage_id").is_some()
        && item["revision"]
            .as_str()
            .is_some_and(|r| r.starts_with("sha256:"))
        && array(&item["allowed_actions"]).iter().any(|a| a == action)
}
fn action_request(
    journal: &InboxJournal,
    item: &Value,
    action: &str,
    body: &str,
    actor: &str,
) -> Result<Value, String> {
    if !matches!(action, "save_decision" | "ack_seen") || !permits(item, action, actor) {
        return Err(
            "This exact item does not allow that action. Refresh and review its current version."
                .into(),
        );
    }
    let mut request = journal.request(
        if action == "save_decision" {
            body
        } else {
            "Seen"
        },
        actor,
    )?;
    request["op"] = json!(if action == "save_decision" {
        "attention_reply"
    } else {
        "attention_ack"
    });
    request["goal_id"] = item["goal_id"].clone();
    request["attention_id"] = item["attention_id"].clone();
    request["expected_revision"] = item["revision"].clone();
    request["stage_id"] = item["stage_id"].clone();
    if action == "ack_seen" {
        request.as_object_mut().unwrap().remove("text");
    }
    Ok(request)
}

pub(super) struct AttentionUi {
    journal: Option<InboxJournal>,
    pending: Vec<Value>,
    archived: Vec<Value>,
    replacement: Option<(Value, Value)>,
    page: Value,
    selected: Value,
    stale_current: Option<Value>,
    draft: Entity<TextareaState>,
    drafts: BTreeMap<String, String>,
    sequence: u64,
    pub(super) error: Option<String>,
    notice: Option<String>,
}
impl AttentionUi {
    pub(super) fn new(window: &mut Window, cx: &mut Context<BrainView>) -> Self {
        Self {
            journal: None,
            pending: vec![],
            archived: vec![],
            replacement: None,
            page: Value::Null,
            selected: Value::Null,
            stale_current: None,
            draft: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .rows(6)
                    .placeholder("Your decision or reply…")
            }),
            drafts: BTreeMap::new(),
            sequence: 0,
            error: None,
            notice: None,
        }
    }
    fn accepts(&self, request: &Value, workspace: Option<&Value>) -> bool {
        request["_attention_workspace"].is_object()
            && Some(&request["_attention_workspace"]) == workspace
            && request["_attention_sequence"].as_u64() == Some(self.sequence)
    }
    fn select(&mut self, item: Value, window: &mut Window, cx: &mut Context<BrainView>) {
        if self.selected.is_object() {
            self.drafts.insert(
                item_key(&self.selected),
                self.draft.read(cx).value().to_string(),
            );
        }
        let body = self
            .drafts
            .get(&item_key(&item))
            .cloned()
            .unwrap_or_default();
        self.draft
            .update(cx, |input, cx| input.set_value(body, window, cx));
        self.selected = item;
        self.notice = None;
    }
}
impl BrainView {
    pub(super) fn use_bound_attention(&self) -> bool {
        self.capabilities["attention_read"] == true
            || self.bound_attention.page.is_object()
            || !self.bound_attention.pending.is_empty()
            || !self.bound_attention.archived.is_empty()
    }
    pub(super) fn ensure_bound_attention(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bound_attention.journal.is_some() {
            return;
        }
        let Some(workspace) = self.expected_workspace.as_ref() else {
            return;
        };
        match InboxJournal::open_attention(workspace)
            .and_then(|journal| Ok((journal.pending()?, journal.rejected()?, journal)))
        {
            Ok((pending, archived, journal)) => {
                self.bound_attention.archived = archived;
                if let Some(first) = pending.first() {
                    let item = json!({"goal_id":first["goal_id"], "attention_id":first["attention_id"],
                        "revision":first["expected_revision"], "stage_id":first["stage_id"],
                        "message":"An exact request is waiting for delivery recovery.","current":null});
                    self.bound_attention
                        .drafts
                        .insert(item_key(&item), text(&first["text"]));
                    self.bound_attention.select(item, window, cx);
                }
                self.bound_attention.pending = pending;
                self.bound_attention.journal = Some(journal);
            }
            Err(error) => self.bound_attention.error = Some(error),
        }
    }
    pub(super) fn refresh_bound_attention(
        &mut self,
        more: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.capabilities["attention_read"] != true {
            return;
        }
        self.ensure_bound_attention(window, cx);
        self.bound_attention.sequence += 1;
        let cursor = if more {
            self.bound_attention.page["next_cursor"].clone()
        } else {
            Value::Null
        };
        let mut requests = vec![
            json!({"op":"attention_list","channel":"native","limit":50,"cursor":cursor,
            "_attention_sequence":self.bound_attention.sequence,"_attention_workspace":self.expected_workspace}),
        ];
        if !more {
            if let Some(request) = self.proposal_list_request(false) {
                requests.push(request);
            }
        }
        self.batch(requests, window, cx);
    }
    fn open_bound_attention(&mut self, item: Value, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.proposals.leave();
        self.bound_attention.sequence += 1;
        self.bound_attention.select(item.clone(), window, cx);
        self.batch(vec![json!({"op":"attention_get","channel":"native","goal_id":item["goal_id"],
            "attention_id":item["attention_id"],"revision":item["revision"],
            "_attention_sequence":self.bound_attention.sequence,"_attention_workspace":self.expected_workspace})], window, cx);
    }
    fn send_bound_attention(
        &mut self,
        action: &str,
        replace_stale: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.ensure_bound_attention(window, cx);
        let old = self
            .bound_attention
            .replacement
            .as_ref()
            .map(|r| r.0.clone())
            .or_else(|| self.bound_attention.pending.first().cloned());
        let request = if let Some((_, next)) = &self.bound_attention.replacement {
            Ok(next.clone())
        } else if action == "recover" && !replace_stale {
            old.clone()
                .ok_or("No retained delivery to recover.".to_string())
        } else if old.is_some() && !replace_stale {
            Err("Recover the retained delivery before sending another action.".into())
        } else {
            let item = if replace_stale {
                self.bound_attention
                    .stale_current
                    .clone()
                    .unwrap_or(Value::Null)
            } else {
                self.bound_attention.selected.clone()
            };
            let body = if replace_stale {
                old.as_ref().map(|r| text(&r["text"])).unwrap_or_default()
            } else {
                self.bound_attention.draft.read(cx).value().to_string()
            };
            self.bound_attention
                .journal
                .as_ref()
                .ok_or("Attention recovery is unavailable.".to_string())
                .and_then(|j| {
                    action_request(j, &item, action, &body, &text(&self.capabilities["actor"]))
                })
        };
        let result = request.and_then(|request| {
            let capability = if request["op"] == "attention_reply" { "attention_reply" } else { "attention_ack" };
            if self.capabilities[capability] != true && !(action == "recover" && old.is_some() && !replace_stale) { return Err("This connection does not support this action. Your retained request stays here.".into()); }
            if replace_stale && self.bound_attention.replacement.is_none() {
                if let Some(old) = &old { self.bound_attention.replacement = Some((old.clone(), request.clone())); }
            }
            if !self.bound_attention.pending.iter().any(|p| p == &request) {
                // Keep the chosen new identity even if fsync fails after publication.
                self.bound_attention.pending.insert(0, request.clone());
            }
            let journal = self.bound_attention.journal.as_ref().ok_or("Attention recovery is unavailable.")?;
            journal.retain(&request)?;
            if self.bound_attention.replacement.is_some() {
                if let Some(old) = &old {
                    journal.supersede(old, &request)?;
                    self.bound_attention.pending.retain(|p| p["operation_id"] != old["operation_id"]);
                }
                self.bound_attention.stale_current = None;
                self.bound_attention.replacement = None;
            }
            self.bound_attention.error = None;
            self.bound_attention.notice = None;
            // A past rejection is not proof about this new delivery attempt.
            // An exact retry can commit if the original revision becomes current
            // again; an unknown reply must never be archived using stale proof.
            self.bound_attention.stale_current = None;
            self.batch(vec![request], window, cx);
            Ok(())
        });
        if let Err(error) = result {
            self.bound_attention.error = Some(error);
        }
        cx.notify();
    }
    fn archive_rejected_attention(&mut self, cx: &mut Context<Self>) {
        if self.busy
            || self.bound_attention.stale_current.is_none()
            || self.bound_attention.replacement.is_some()
        {
            return;
        }
        let Some(request) = self.bound_attention.pending.first().cloned() else {
            return;
        };
        let result = self
            .bound_attention
            .journal
            .as_ref()
            .ok_or("Attention recovery is unavailable.".into())
            .and_then(|j| j.archive_rejected(&request));
        match result {
            Ok(()) => {
                self.bound_attention.pending.remove(0);
                self.bound_attention.archived.push(request);
                self.bound_attention.stale_current = None;
                self.bound_attention.error = None;
                self.bound_attention.notice = Some(
                    "Rejected delivery archived. Its original text and target are saved below."
                        .into(),
                );
            }
            Err(error) => self.bound_attention.error = Some(error),
        }
        cx.notify();
    }
    pub(super) fn bound_attention_reply(
        &mut self,
        operation: &str,
        data: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let request = data["_client_attention_request"].clone();
        let mutation = matches!(operation, "attention_reply" | "attention_ack");
        let pending_index = self
            .bound_attention
            .pending
            .iter()
            .position(|p| p == &request);
        if mutation {
            if pending_index.is_none()
                || self.bound_attention.journal.as_ref().map(|j| &j.workspace)
                    != self.expected_workspace.as_ref()
            {
                return;
            }
        } else if !self
            .bound_attention
            .accepts(&request, self.expected_workspace.as_ref())
        {
            return;
        }
        if data["_attention_error"].is_object() {
            let error = &data["_attention_error"];
            self.bound_attention.error = Some(format!(
                "{} Your decision draft is retained.",
                text(&error["message"])
            ));
            if mutation && error["code"] == "attention_stale" {
                self.bound_attention.stale_current = Some(error["current"].clone());
                if same_target(&self.bound_attention.selected, &request)
                    && self.bound_attention.selected["revision"] == request["expected_revision"]
                {
                    self.bound_attention.selected["current"] = json!(false);
                    self.bound_attention.selected["allowed_actions"] = json!([]);
                }
            }
            return;
        }
        if mutation {
            let result = self
                .bound_attention
                .journal
                .as_ref()
                .unwrap()
                .acknowledge(&request, &data);
            if let Err(error) = result {
                self.bound_attention.error = Some(error);
                return;
            }
            self.bound_attention.pending.remove(pending_index.unwrap());
            self.bound_attention.stale_current = None;
            self.bound_attention.error = None;
            self.bound_attention.notice = Some(if operation == "attention_reply" {
                format!(
                    "Decision saved as unverified knowledge: {}. Workflow status is unchanged.",
                    text(&data["path"])
                )
            } else {
                "Marked seen for this account and this version. The item remains in Attention."
                    .into()
            });
            if operation == "attention_reply" {
                let key = item_key(
                    &json!({"goal_id":request["goal_id"],"attention_id":request["attention_id"],"revision":request["expected_revision"]}),
                );
                if self
                    .bound_attention
                    .drafts
                    .get(&key)
                    .is_some_and(|s| s == &text(&request["text"]))
                {
                    self.bound_attention.drafts.remove(&key);
                }
                if item_key(&self.bound_attention.selected) == key
                    && self.bound_attention.draft.read(cx).value().as_ref()
                        == text(&request["text"])
                {
                    self.bound_attention
                        .draft
                        .update(cx, |i, cx| i.set_value("", window, cx));
                }
            }
            if same_target(&self.bound_attention.selected, &request) {
                if data["item"].is_object()
                    && data["item"]["revision"] == self.bound_attention.selected["revision"]
                {
                    self.bound_attention.selected = data["item"].clone();
                } else {
                    self.bound_attention.selected["current"] = json!(false);
                    self.bound_attention.selected["allowed_actions"] = json!([]);
                }
            }
            self.refresh_bound_attention(false, window, cx);
        } else if operation == "attention_list" {
            if request["cursor"].is_null() {
                self.bound_attention.page = data;
            } else if self.bound_attention.page["generation"] == data["generation"] {
                let mut items = array(&self.bound_attention.page["items"]);
                items.extend(array(&data["items"]));
                self.bound_attention.page = data;
                self.bound_attention.page["items"] = json!(items);
            } else {
                self.bound_attention.error =
                    Some("Attention changed while loading. Refresh the list.".into());
                return;
            }
            // Selection stays pinned to its observed revision until explicitly opened.
            self.bound_attention.error = None;
        } else if operation == "attention_get"
            && same_target(&request, &data["item"])
            && same_target(&request, &self.bound_attention.selected)
            && request["revision"] == data["item"]["revision"]
        {
            self.bound_attention.selected = data["item"].clone();
            self.bound_attention.error = None;
        }
    }
    pub(super) fn bound_attention_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut rows = v_flex().gap_2();
        let query = self.filter.read(cx).value().to_lowercase();
        for item in array(&self.bound_attention.page["items"]) {
            if !format!("{} {}", text(&item["goal_title"]), text(&item["message"]))
                .to_lowercase()
                .contains(&query)
            {
                continue;
            }
            let id = format!(
                "bound-attention-{}-{}",
                text(&item["goal_id"]),
                text(&item["attention_id"])
            );
            rows = rows.child(
                super::super::brand::control(SharedString::from(id.clone()), cx)
                    .debug_selector(move || id.clone())
                    .w_full()
                    .h_auto()
                    .py_3()
                    .justify_start()
                    .child(
                        v_flex()
                            .w_full()
                            .whitespace_normal()
                            .text_left()
                            .items_start()
                            .gap_1()
                            .child(text(&item["goal_title"]))
                            .child(div().text_sm().child(text(&item["message"])))
                            .child(div().text_xs().child(if item["seen"] == true {
                                "Seen"
                            } else {
                                "Unseen"
                            })),
                    )
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_bound_attention(item.clone(), window, cx)
                    })),
            );
        }
        if self.bound_attention.error.is_none()
            && self.capabilities["attention_read"] == true
            && self.bound_attention.page["complete"] == true
            && array(&self.bound_attention.page["items"]).is_empty()
        {
            rows = rows.child("No current attention items.");
        }
        if self.bound_attention.page["next_cursor"].is_string() {
            rows = rows.child(
                super::super::brand::control("attention-more", cx)
                    .label("Load more")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.refresh_bound_attention(true, window, cx)
                    })),
            );
        }
        rows.into_any_element()
    }
    pub(super) fn bound_attention_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let item = self.bound_attention.selected.clone();
        let pending = self.bound_attention.pending.first().cloned();
        let actor = text(&self.capabilities["actor"]);
        let mut panel = v_flex()
            .id("bound-attention-panel")
            .debug_selector(|| "bound-attention-panel".into())
            .flex_1()
            .min_w_0()
            .min_h_0()
            .whitespace_normal()
            .overflow_y_scroll()
            .p_6()
            .gap_4()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().text_2xl().child("Attention"))
                    .child(
                        super::super::brand::control("bound-attention-refresh", cx)
                            .label("Refresh attention")
                            .disabled(self.busy || self.capabilities["attention_read"] != true)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.refresh_bound_attention(false, window, cx)
                            })),
                    ),
            );
        if self.capabilities["attention_read"] != true {
            panel = panel.child("Bound attention actions are unavailable on this connection. Saved context and retained deliveries remain here.");
        }
        if let Some(error) = &self.bound_attention.error {
            panel = panel.child(
                div()
                    .w_full()
                    .min_w_0()
                    .whitespace_normal()
                    .text_color(super::super::brand::palette(cx).danger)
                    .child(error.clone()),
            );
        }
        if let Some(notice) = &self.bound_attention.notice {
            panel = panel.child(notice.clone());
        }
        if let Some(request) = &pending {
            let op = if request["op"] == "attention_reply" {
                "decision"
            } else {
                "seen acknowledgement"
            };
            panel = panel
                .child(format!(
                    "Unconfirmed {op} · goal {} · item {} · version {}",
                    text(&request["goal_id"]),
                    text(&request["attention_id"]),
                    text(&request["expected_revision"])
                ))
                .when(request["text"].is_string(), |p| {
                    p.child(text(&request["text"]))
                })
                .child(
                    super::super::brand::control("attention-recover", cx)
                        .label("Recover delivery")
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.send_bound_attention("recover", false, window, cx)
                        })),
                );
            if self.bound_attention.stale_current.is_some() {
                panel = panel.child(
                    super::super::brand::control("attention-archive-rejected", cx)
                        .label("Archive rejected delivery and keep draft")
                        .disabled(self.busy || self.bound_attention.replacement.is_some())
                        .on_click(
                            cx.listener(|this, _, _, cx| this.archive_rejected_attention(cx)),
                        ),
                );
            }
            if let Some(current) = &self.bound_attention.stale_current {
                if current.is_object() {
                    let action = if request["op"] == "attention_reply" {
                        "save_decision"
                    } else {
                        "ack_seen"
                    };
                    panel = panel
                        .child(
                            div()
                                .text_lg()
                                .child("The item changed. Review its current version:"),
                        )
                        .child(text(&current["message"]))
                        .child(format!(
                            "{} · stage {} · {}",
                            text(&current["goal_title"]),
                            current["stage_id"],
                            text(&current["revision"])
                        ))
                        .child(
                            super::super::brand::control("attention-rebind", cx)
                                .label(if action == "save_decision" {
                                    "Save decision for displayed version"
                                } else {
                                    "Mark displayed version seen"
                                })
                                .disabled(self.busy || !permits(current, action, &actor))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.send_bound_attention(action, true, window, cx)
                                })),
                        );
                } else {
                    panel = panel.child("The original item is no longer current. Its retained request and reply remain available; no action was applied.");
                }
            }
        }
        if item.is_object() {
            panel = panel.child(div().text_lg().child(text(&item["goal_title"]))).child(text(&item["message"]))
                .child(div().text_xs().child(format!("{} · {}",match item["kind"].as_str(){Some("blocker")=>"Blocker",Some("decision")=>"Decision",Some("final")=>"Result",_=>"Attention"},if item["seen"]==true{"Seen"}else if item["seen"]==false{"Unseen"}else{"Seen state not loaded"})))
                .when(item["current"] != true,|p|p.child(if item["current"].is_null() {"The current version has not been checked. Recover delivery or open the item to review it."} else {"This saved version is no longer current. Open the latest item from the list to review it."}))
                .child(Textarea::new(&self.bound_attention.draft).h(px(160.)).disabled(self.busy || pending.is_some()))
                .child("Saving a decision records unverified knowledge. Mark seen only records that you saw this version.")
                .child(h_flex().gap_2().flex_wrap()
                    .child(super::super::brand::control("attention-save-decision",cx).primary().label("Save decision")
                        .disabled(self.busy || pending.is_some() || self.capabilities["attention_reply"]!=true || !permits(&item,"save_decision",&actor))
                        .on_click(cx.listener(|this,_,window,cx|this.send_bound_attention("save_decision",false,window,cx))))
                    .child(super::super::brand::control("attention-mark-seen",cx).label("Mark seen")
                        .disabled(self.busy || pending.is_some() || self.capabilities["attention_ack"]!=true || !permits(&item,"ack_seen",&actor))
                        .on_click(cx.listener(|this,_,window,cx|this.send_bound_attention("ack_seen",false,window,cx)))));
        } else {
            panel = panel
                .child("Select an item to read its context, save a decision, or mark it seen.");
        }
        for archived in &self.bound_attention.archived {
            panel = panel
                .child(
                    div()
                        .text_lg()
                        .child("Saved draft from a rejected delivery"),
                )
                .child(format!(
                    "Goal {} · item {} · version {}",
                    text(&archived["goal_id"]),
                    text(&archived["attention_id"]),
                    text(&archived["expected_revision"])
                ))
                .child(text(&archived["text"]));
        }
        panel.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    fn workspace() -> Value {
        json!({"brain_id":"cc000000-0000-4000-8000-000000000125","root":"/synthetic/attention","records_dir":"records","managed":true})
    }
    fn item() -> Value {
        json!({"goal_id":"ee000000-0000-4000-8000-000000000125","attention_id":"decision-one","revision":"sha256:one","stage_id":null,"result_id":null,"kind":"decision","message":"Choose a direction","goal_title":"Other goal","current":true,"seen":false,"allowed_actions":["save_decision","ack_seen"],"actor_id":"local:oleg","channel":"native"})
    }
    fn temp() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tessera-attention-ui-{}", uuid()))
    }
    fn journal(dir: &std::path::Path) -> InboxJournal {
        InboxJournal::at(dir.to_path_buf(), &workspace())
            .unwrap()
            .for_attention()
    }
    fn committed(request: &Value) -> Value {
        json!({"receipt":{"status":"committed","operation_id":request["operation_id"]},"decision_id":"dd000000-0000-4000-8000-000000000125","revision":"sha256:decision","path":"records/decision.md","acknowledged_at":"2026-09-06T23:00:00Z","item":item(),"_client_attention_request":request})
    }
    #[test]
    fn exact_authority_excludes_final_reply_wrong_actor_channel_and_missing_stage() {
        let dir = temp();
        let j = journal(&dir);
        let item = item();
        let request = action_request(
            &j,
            &item,
            "save_decision",
            "  reply\r\nno final newline  ",
            "local:oleg",
        )
        .unwrap();
        assert_eq!(request["stage_id"], Value::Null);
        assert!(request.get("stage_id").is_some());
        assert_eq!(request["text"], "  reply\r\nno final newline  ");
        assert_eq!(request["source"]["message_id"], request["operation_id"]);
        assert_eq!(request["expected_revision"], item["revision"]);
        let mut final_item = item.clone();
        final_item["kind"] = json!("final");
        final_item["allowed_actions"] = json!(["ack_seen"]);
        assert!(action_request(&j, &final_item, "save_decision", "reply", "local:oleg").is_err());
        let ack = action_request(&j, &final_item, "ack_seen", "ignored", "local:oleg").unwrap();
        assert!(ack.get("text").is_none());
        assert_eq!(ack["op"], "attention_ack");
        assert!(action_request(&j, &item, "ack_seen", "", "other").is_err());
        let mut bad = item.clone();
        bad["channel"] = json!("telegram");
        assert!(!permits(&bad, "ack_seen", "local:oleg"));
        bad = item.clone();
        bad.as_object_mut().unwrap().remove("stage_id");
        assert!(!permits(&bad, "ack_seen", "local:oleg"));
        bad = item.clone();
        bad["current"] = json!(false);
        assert!(!permits(&bad, "ack_seen", "local:oleg"));
        assert!(action_request(&j, &item, "start", "reply", "local:oleg").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn inbox_and_attention_share_client_without_mixing_recovery_and_replay_exact_requests() {
        let dir = temp();
        let inbox = InboxJournal::at(dir.clone(), &workspace()).unwrap();
        let j = journal(&dir);
        let capture = inbox.request("thought", "local:oleg").unwrap();
        inbox.retain(&capture).unwrap();
        let request =
            action_request(&j, &item(), "save_decision", "decision", "local:oleg").unwrap();
        j.retain(&request).unwrap();
        assert_eq!(
            capture["source"]["instance_id"],
            request["source"]["instance_id"]
        );
        assert_eq!(inbox.pending().unwrap(), vec![capture]);
        assert_eq!(journal(&dir).pending().unwrap(), vec![request.clone()]);
        let mut wrong = committed(&request);
        wrong["receipt"]["operation_id"] = json!(uuid());
        assert!(j.acknowledge(&request, &wrong).is_err());
        j.acknowledge(&request, &committed(&request)).unwrap();
        assert!(journal(&dir).pending().unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn stale_replacement_and_archived_draft_survive_restart_without_erasing_originals() {
        let dir = temp();
        let j = journal(&dir);
        let original =
            action_request(&j, &item(), "save_decision", "keep me", "local:oleg").unwrap();
        j.retain(&original).unwrap();
        let mut current = item();
        current["revision"] = json!("sha256:two");
        let next = action_request(&j, &current, "save_decision", "keep me", "local:oleg").unwrap();
        j.supersede(&original, &next).unwrap();
        assert_eq!(journal(&dir).pending().unwrap(), vec![next.clone()]);
        assert_ne!(next["operation_id"], original["operation_id"]);
        assert_eq!(original["expected_revision"], "sha256:one");
        j.archive_rejected(&next).unwrap();
        assert!(journal(&dir).pending().unwrap().is_empty());
        assert_eq!(journal(&dir).rejected().unwrap(), vec![next]);
        assert!(j.supersede(&original, &original).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[gpui::test]
    fn reads_and_saved_decision_preserve_active_goal_source_and_later_drafts(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = temp();
        let cleanup = dir.clone();
        cx.add_window_view(move |window,cx|{
            let mut v=BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);v.busy=true;v.expected_workspace=Some(workspace());
            v.snapshot=json!({"goal":{"id":"active","status":"running"},"stage":{"id":"active-stage"}});let snapshot=v.snapshot.clone();v.selected_goal_id=Some("active".into());
            v.source.reset("editor draft", window, cx);v.compose.update(cx,|i,cx|i.set_value("chat draft",window,cx));
            v.bound_attention.sequence=2;v.bound_attention.selected=item();
            let read=json!({"op":"attention_get","goal_id":item()["goal_id"],"attention_id":item()["attention_id"],"revision":"sha256:one","_attention_sequence":1,"_attention_workspace":workspace()});
            let mut changed=item();changed["message"]=json!("late response");
            v.bound_attention_reply("attention_get",json!({"item":changed,"_client_attention_request":read}),window,cx);assert_eq!(v.bound_attention.selected,item());
            let mut read=read;read["_attention_sequence"]=json!(2);
            v.bound_attention_reply("attention_get",json!({"item":changed,"_client_attention_request":read}),window,cx);assert_eq!(v.bound_attention.selected["message"],"late response");
            let j=journal(&dir);let request=action_request(&j,&item(),"save_decision","original reply","local:oleg").unwrap();j.retain(&request).unwrap();
            v.bound_attention.pending=vec![request.clone()];v.bound_attention.journal=Some(j);v.bound_attention.draft.update(cx,|i,cx|i.set_value("later reply",window,cx));
            let mut reply=committed(&request);reply["item"]["revision"]=json!("sha256:newer");v.bound_attention_reply("attention_reply",reply,window,cx);
            assert!(v.bound_attention.pending.is_empty());assert_eq!(v.bound_attention.draft.read(cx).value().as_ref(),"later reply");
            assert_eq!(v.bound_attention.selected["revision"],"sha256:one","a fresh receipt must not rebind a later draft");assert_eq!(v.bound_attention.selected["current"],false);
            assert_eq!(v.snapshot,snapshot);assert_eq!(v.selected_goal_id.as_deref(),Some("active"));assert_eq!(v.source.value(cx).as_ref(),"editor draft");assert_eq!(v.compose.read(cx).value().as_ref(),"chat draft");v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
    #[gpui::test]
    fn stale_reply_retains_draft_until_explicit_rebind_and_local_failure_reuses_identity(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = temp();
        let cleanup = dir.clone();
        cx.add_window_view(move |window,cx|{
            let mut v=BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);v.expected_workspace=Some(workspace());v.capabilities=json!({"attention_reply":true,"actor":"local:oleg"});
            v.bound_attention.selected=item();v.bound_attention.journal=Some(journal(&dir));v.bound_attention.draft.update(cx,|i,cx|i.set_value("original reply",window,cx));
            v.bound_attention.journal.as_ref().unwrap().fail_retain_after_publish.set(true);
            v.send_bound_attention("save_decision",false,window,cx);assert!(!v.busy);let request=v.bound_attention.pending[0].clone();
            v.bound_attention.journal.as_ref().unwrap().fail_retain_after_publish.set(true);
            v.send_bound_attention("recover",false,window,cx);assert!(!v.busy);assert_eq!(v.bound_attention.pending,vec![request.clone()]);
            let mut current=item();current["revision"]=json!("sha256:two");current["message"]=json!("Changed question");
            v.bound_attention_reply("attention_reply",json!({"_client_attention_request":request,"_attention_error":{"code":"attention_stale","message":"Item changed","current":current}}),window,cx);
            assert!(!v.busy);assert_eq!(v.bound_attention.pending,vec![request.clone()]);assert_eq!(v.bound_attention.selected["current"],false);assert!(array(&v.bound_attention.selected["allowed_actions"]).is_empty());assert_eq!(v.bound_attention.selected["revision"],item()["revision"]);assert_eq!(v.bound_attention.draft.read(cx).value().as_ref(),"original reply");
            v.bound_attention.journal.as_ref().unwrap().fail_retain_after_publish.set(true);
            v.send_bound_attention("save_decision",true,window,cx);assert!(!v.busy);let next=v.bound_attention.pending[0].clone();assert_ne!(next["operation_id"],request["operation_id"]);assert_eq!(next["expected_revision"],"sha256:two");
            v.send_bound_attention("recover",false,window,cx);assert!(v.busy);assert_eq!(v.bound_attention.pending,vec![next.clone()]);assert_eq!(journal(&dir).pending().unwrap(),vec![next]);v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
    #[gpui::test]
    fn stale_null_can_archive_exact_rejected_request_but_unknown_delivery_cannot(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = temp();
        let cleanup = dir.clone();
        cx.add_window_view(move |window,cx|{
            let mut v=BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);v.busy=false;v.expected_workspace=Some(workspace());
            let j=journal(&dir);let request=action_request(&j,&item(),"save_decision","retired decision","local:oleg").unwrap();j.retain(&request).unwrap();v.bound_attention.journal=Some(j);v.bound_attention.pending=vec![request.clone()];
            v.archive_rejected_attention(cx);assert_eq!(v.bound_attention.pending.len(),1);
            v.bound_attention_reply("attention_reply",json!({"_client_attention_request":request,"_attention_error":{"code":"attention_stale","message":"Retired","current":null}}),window,cx);
            v.archive_rejected_attention(cx);assert!(v.bound_attention.pending.is_empty());assert_eq!(journal(&dir).rejected().unwrap(),vec![request]);v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
    #[gpui::test]
    fn disabled_new_writes_still_allow_exact_recovery_and_clear_old_stale_proof(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = temp();
        let cleanup = dir.clone();
        cx.add_window_view(move |window,cx| {
            let mut v=BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);
            v.expected_workspace=Some(workspace());
            v.capabilities=json!({"attention_read":false,"attention_reply":false,"attention_ack":false,"actor":"local:oleg"});
            v.bound_attention.selected=item();v.bound_attention.journal=Some(journal(&dir));
            v.bound_attention.draft.update(cx,|i,cx|i.set_value("original reply",window,cx));
            v.send_bound_attention("save_decision",false,window,cx);
            assert!(!v.busy);assert!(v.bound_attention.pending.is_empty(),"new action cannot use recovery exemption");
            let request=action_request(v.bound_attention.journal.as_ref().unwrap(),&item(),"save_decision","original reply","local:oleg").unwrap();
            v.bound_attention.journal.as_ref().unwrap().retain(&request).unwrap();v.bound_attention.pending=vec![request.clone()];
            v.bound_attention.stale_current=Some(Value::Null);
            v.send_bound_attention("recover",false,window,cx);assert!(v.busy,"exact retained replay remains available during recovery");
            assert_eq!(v.bound_attention.pending,vec![request.clone()]);assert!(v.bound_attention.stale_current.is_none());
            v.busy=false;v.archive_rejected_attention(cx);assert_eq!(v.bound_attention.pending,vec![request.clone()],"a later unknown attempt cannot use previous rejection proof");
            v.bound_attention_reply("attention_reply",json!({"_client_attention_request":request,"_attention_error":{"code":"attention_projection_pending","message":"Projection pending"}}),window,cx);
            assert_eq!(v.bound_attention.pending,vec![request]);assert!(v.bound_attention.stale_current.is_none());assert_eq!(v.bound_attention.draft.read(cx).value().as_ref(),"original reply");v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
    #[test]
    fn real_rpc_preserves_structured_stale_error_and_required_null_stage() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(socket.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["op"], "attention_reply");
            assert!(request.get("stage_id").is_some());
            assert_eq!(request["stage_id"], Value::Null);
            assert_eq!(request["expected_workspace"], workspace());
            let reply = json!({"schema":request["schema"],"id":request["id"],"ok":false,"error":{"code":"attention_stale","message":"Changed","current":item()}});
            socket.write_all(format!("{reply}\n").as_bytes()).unwrap();
        });
        let result = rpc_guarded(
            endpoint,
            json!({"op":"attention_reply","stage_id":null}),
            Some(&workspace()),
        )
        .unwrap();
        assert_eq!(result["_attention_error"]["current"], item());
        worker.join().unwrap();
    }
}
