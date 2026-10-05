//! Workspace-owned capture. Delivery identities live outside goal state and the
//! rebuildable index; an unknown reply never clears or replaces a request.
use super::*;
#[cfg(test)]
use std::path::PathBuf;

use super::native_outbox::InboxJournal;

pub(super) struct InboxUi {
    draft: Entity<TextareaState>,
    source: Entity<TextareaState>,
    body: Entity<TextareaState>,
    show_source: bool,
    journal: Option<InboxJournal>,
    pending: Vec<Value>,
    page: Value,
    selected: Option<String>,
    loaded: Value,
    sequence: u64,
    pub(super) error: Option<String>,
    composing: bool,
}
impl InboxUi {
    pub(super) fn new(window: &mut Window, cx: &mut Context<BrainView>) -> Self {
        Self {
            draft: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .rows(6)
                    .placeholder("A thought, a link, a reminder…")
            }),
            source: cx.new(|cx| TextareaState::new(window, cx).rows(18).soft_wrap(false)),
            body: cx.new(|cx| TextareaState::new(window, cx).rows(12)),
            show_source: false,
            journal: None,
            pending: vec![],
            page: Value::Null,
            selected: None,
            loaded: Value::Null,
            sequence: 0,
            error: None,
            composing: true,
        }
    }
    fn accepts(&self, request: &Value, workspace: Option<&Value>) -> bool {
        request["_inbox_workspace"].as_object().is_some()
            && Some(&request["_inbox_workspace"]) == workspace
            && request["_inbox_sequence"].as_u64() == Some(self.sequence)
    }
}

impl BrainView {
    pub(super) fn ensure_inbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inbox_ui.journal.is_some() {
            return;
        }
        let Some(workspace) = self.expected_workspace.as_ref() else {
            return;
        };
        match InboxJournal::open(workspace).and_then(|j| {
            let p = j.pending()?;
            Ok((j, p))
        }) {
            Ok((journal, pending)) => {
                if let Some(request) = pending.first() {
                    self.inbox_ui.draft.update(cx, |input, cx| {
                        input.set_value(text(&request["text"]), window, cx)
                    });
                }
                self.inbox_ui.journal = Some(journal);
                self.inbox_ui.pending = pending;
            }
            Err(error) => self.inbox_ui.error = Some(error),
        }
    }
    pub(crate) fn begin_thought(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.note_guard_navigation(PendingNavigation::Thought, cx) {
            return;
        }
        if self.busy {
            return;
        }
        self.ensure_inbox(window, cx);
        self.ensure_inbox_plan(window, cx);
        self.inbox_plan.remember(cx);
        self.inbox_plan.form = false;
        self.show_capture = false;
        self.collection = Collection::Inbox;
        self.filter.update(cx, |input, cx| {
            input.set_placeholder("Search inbox…", window, cx)
        });
        self.inbox_ui.composing = true;
        self.inbox_ui
            .draft
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        self.refresh_inbox(false, window, cx);
        cx.notify();
    }
    pub(super) fn refresh_inbox(
        &mut self,
        more: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.capabilities["inbox_read"] != true {
            return;
        }
        self.ensure_inbox(window, cx);
        self.inbox_ui.sequence += 1;
        let cursor = if more {
            self.inbox_ui.page["next_cursor"].clone()
        } else {
            Value::Null
        };
        let request = json!({"op":"inbox_list","limit":50,"cursor":cursor,
            "_inbox_sequence":self.inbox_ui.sequence,"_inbox_workspace":self.expected_workspace});
        self.batch(vec![request], window, cx);
    }
    fn capture_inbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.capabilities["inbox_capture"] != true {
            return;
        }
        self.ensure_inbox(window, cx);
        let request = if let Some(pending) = self.inbox_ui.pending.first() {
            Ok(pending.clone())
        } else if let Some(journal) = &self.inbox_ui.journal {
            journal.request(
                self.inbox_ui.draft.read(cx).value().as_ref(),
                &text(&self.capabilities["actor"]),
            )
        } else {
            Err("Reconnect a saved workspace before capturing a thought.".into())
        };
        match request {
            Ok(request) => {
                if self.inbox_ui.pending.is_empty() {
                    self.inbox_ui.pending.push(request.clone());
                }
                // Retain the chosen identity even if publishing/fsync fails
                // after the file became visible. Every retry re-establishes
                // durability for these exact bytes before any TCP request.
                let retained = self
                    .inbox_ui
                    .journal
                    .as_ref()
                    .ok_or("Inbox recovery is unavailable.".to_string())
                    .and_then(|journal| journal.retain(&request));
                if let Err(error) = retained {
                    self.inbox_ui.error = Some(error);
                    cx.notify();
                    return;
                }
                self.inbox_ui.error = None;
                self.batch(vec![request], window, cx);
            }
            Err(error) => {
                self.inbox_ui.error = Some(error);
                cx.notify();
            }
        }
    }
    pub(super) fn open_inbox(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.inbox_plan.remember(cx);
        self.inbox_plan.form = false;
        self.inbox_ui.sequence += 1;
        self.inbox_ui.selected = Some(id.clone());
        self.inbox_ui.composing = false;
        self.inbox_ui.loaded = Value::Null;
        self.inbox_ui.show_source = false;
        self.batch(
            vec![json!({"op":"inbox_get","capture_id":id,
            "_inbox_sequence":self.inbox_ui.sequence,"_inbox_workspace":self.expected_workspace})],
            window,
            cx,
        );
    }
    pub(super) fn inbox_reply(
        &mut self,
        operation: &str,
        data: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let request = &data["_client_inbox_request"];
        if operation == "inbox_capture" {
            let Some(index) = self
                .inbox_ui
                .pending
                .iter()
                .position(|p| p["operation_id"] == request["operation_id"])
            else {
                return;
            };
            let pending = self.inbox_ui.pending[index].clone();
            if &pending != request
                || self.inbox_ui.journal.as_ref().map(|j| &j.workspace)
                    != self.expected_workspace.as_ref()
            {
                self.inbox_ui.error = Some("Inbox reply belongs to a different request or workspace; recovery is retained.".into());
                return;
            }
            let result = self
                .inbox_ui
                .journal
                .as_ref()
                .ok_or("Inbox recovery is unavailable.".to_string())
                .and_then(|j| j.acknowledge(&pending, &data));
            if let Err(error) = result {
                self.inbox_ui.error = Some(error);
                return;
            }
            self.inbox_ui.pending.remove(index);
            if self.inbox_ui.draft.read(cx).value().as_ref() == text(&pending["text"]) {
                let next = self
                    .inbox_ui
                    .pending
                    .first()
                    .map(|p| text(&p["text"]))
                    .unwrap_or_default();
                self.inbox_ui
                    .draft
                    .update(cx, |input, cx| input.set_value(next, window, cx));
            }
            self.inbox_ui.error = None;
            self.notice = "Thought saved in your inbox.".into();
            self.refresh_inbox(false, window, cx);
        } else if self
            .inbox_ui
            .accepts(request, self.expected_workspace.as_ref())
        {
            if operation == "inbox_list" {
                if request["cursor"].is_null() {
                    self.inbox_ui.page = data;
                } else if self.inbox_ui.page["generation"] == data["generation"] {
                    let mut items = array(&self.inbox_ui.page["items"]);
                    items.extend(array(&data["items"]));
                    self.inbox_ui.page = data;
                    self.inbox_ui.page["items"] = json!(items);
                } else {
                    self.inbox_ui.error =
                        Some("Inbox changed while loading. Refresh the list.".into());
                    return;
                }
                self.inbox_ui.error = None;
            } else if operation == "inbox_get"
                && self.inbox_ui.selected.as_deref() == data["item"]["capture_id"].as_str()
                && data["item"]["capture_id"] == request["capture_id"]
            {
                if data["source"]["brain_id"] != request["_inbox_workspace"]["brain_id"]
                    || data["source"]["path"] != data["item"]["path"]
                    || data["source"]["revision"] != data["item"]["revision"]
                {
                    self.inbox_ui.error =
                        Some("Inbox source identity does not match the requested capture.".into());
                    return;
                }
                let Some(body) = data["text"].as_str() else {
                    self.inbox_ui.error = Some("The backend did not return this thought's original text. Refresh after updating the connection.".into());
                    return;
                };
                self.inbox_ui
                    .body
                    .update(cx, |input, cx| input.set_value(body, window, cx));
                match decode_source(&data["source"]) {
                    Ok(source) => {
                        self.inbox_ui
                            .source
                            .update(cx, |input, cx| input.set_value(source, window, cx));
                        self.inbox_ui.loaded = data;
                        self.inbox_ui.error = None;
                    }
                    Err(error) => self.inbox_ui.error = Some(error.into()),
                }
            }
        }
    }
    pub(super) fn inbox_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let colors = super::super::brand::palette(cx);
        let mut rows = v_flex()
            .id("inbox-items")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_2()
            .gap_2();
        let message = if self.capabilities.is_null() {
            "Connecting to your inbox…"
        } else if self.capabilities["inbox_read"] != true {
            "Inbox is unavailable on this backend. Existing goals remain available."
        } else if self.inbox_ui.page.is_null() {
            "Open or refresh the inbox to load saved thoughts."
        } else if array(&self.inbox_ui.page["items"]).is_empty() {
            "Your inbox is empty. Capture a thought without planning it yet."
        } else {
            "Saved thoughts"
        };
        rows = rows.child(div().text_sm().text_color(colors.text_muted).child(message));
        let query = self.filter.read(cx).value().to_lowercase();
        for item in array(&self.inbox_ui.page["items"]) {
            if !text(&item["title"]).to_lowercase().contains(&query) {
                continue;
            }
            let id = text(&item["capture_id"]);
            let key = format!("inbox-item-{id}");
            rows = rows.child(
                super::super::brand::control(SharedString::from(key.clone()), cx)
                    .debug_selector(move || key.clone())
                    .w_full()
                    .h_auto()
                    .py_3()
                    .justify_start()
                    .when(self.inbox_ui.selected.as_ref() == Some(&id), |b| {
                        b.bg(colors.selected)
                    })
                    .child(
                        v_flex()
                            .w_full()
                            .items_start()
                            .text_left()
                            .whitespace_normal()
                            .gap_1()
                            .child(text(&item["title"]))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(colors.text_muted)
                                    .child(text(&item["received_at"])),
                            ),
                    )
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_inbox(id.clone(), window, cx)
                    })),
            );
        }
        if self.inbox_ui.page["next_cursor"].is_string() {
            rows = rows.child(
                super::super::brand::control("inbox-more", cx)
                    .label("Load more")
                    .disabled(self.busy)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.refresh_inbox(true, window, cx)),
                    ),
            );
        }
        rows.into_any_element()
    }
    pub(super) fn inbox_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let pending = !self.inbox_ui.pending.is_empty();
        let mut panel = v_flex()
            .id("inbox-panel")
            .debug_selector(|| "inbox-panel".into())
            .h_full()
            .flex_1()
            .min_w_0()
            .whitespace_normal()
            .min_h_0()
            .overflow_y_scroll()
            .p_6()
            .gap_4()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().text_2xl().child("Inbox"))
                    .child(
                        super::super::brand::control("inbox-refresh", cx)
                            .label("Refresh inbox")
                            .disabled(self.busy || self.capabilities["inbox_read"] != true)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.refresh_inbox(false, window, cx)
                            })),
                    ),
            )
            .child("Keep a thought now. Decide what to do with it later.")
            .child(self.inbox_plan_summary(cx));
        if let Some(error) = &self.inbox_ui.error {
            panel = panel.child(
                div()
                    .w_full()
                    .min_w_0()
                    .whitespace_normal()
                    .text_color(super::super::brand::palette(cx).danger)
                    .child(error.clone()),
            );
        }
        if self.inbox_ui.composing {
            panel=panel.child(Textarea::new(&self.inbox_ui.draft).h(px(180.)).disabled(self.busy||pending))
                .when(pending,|p|p.child("Delivery is unconfirmed. Recover the same saved request; no second thought will be created."))
                .when(self.capabilities["inbox_capture"]!=true,|p|p.child("Capture is unavailable on this backend. Your draft stays here."))
                .child(h_flex().gap_2().flex_wrap()
                    .child(super::super::brand::control("inbox-capture",cx).primary().label(if pending{"Recover capture"}else{"Save thought"})
                        .disabled(self.busy||self.capabilities["inbox_capture"]!=true||self.inbox_ui.journal.is_none())
                        .on_click(cx.listener(|this,_,window,cx|this.capture_inbox(window,cx))))
                    .child(super::super::brand::control("inbox-create-goal",cx).label("Create a goal instead")
                        .disabled(self.busy).on_click(cx.listener(|this,_,window,cx|{this.collection=Collection::Goals;this.begin_capture(window,cx);})))) ;
        } else if self.inbox_ui.loaded.is_object() {
            let origin = self.inbox_ui.loaded.clone();
            let known_links = origin["planned_goals"].is_array();
            let linked = array(&origin["planned_goals"]);
            panel = panel.child(
                super::super::brand::control("inbox-plan-open", cx)
                    .debug_selector(|| "inbox-plan-open".into())
                    .label(if linked.is_empty() {
                        "Plan this thought"
                    } else {
                        "Plan another goal"
                    })
                    .disabled(self.busy || self.capabilities["inbox_plan"] != true || !known_links)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.begin_inbox_plan(origin.clone(), window, cx)
                    })),
            );
            if !known_links {
                panel = panel.child("Planning links are unavailable on this backend. Existing goals remain available.");
            }
            for item in linked {
                let goal = text(&item["goal_id"]);
                panel = panel.child(
                    super::super::brand::control(
                        SharedString::from(format!("inbox-linked-{goal}")),
                        cx,
                    )
                    .label(format!("Open goal: {}", text(&item["title"])))
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.inbox_plan.form = false;
                        this.select_collection(Collection::Goals, window, cx);
                        this.select_goal(goal.clone(), window, cx);
                    })),
                );
            }
            panel = panel
                .child(
                    div()
                        .text_lg()
                        .child(text(&self.inbox_ui.loaded["item"]["title"])),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(
                            super::super::brand::control("inbox-view-thought", cx)
                                .label("Thought")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.inbox_ui.show_source = false;
                                    cx.notify();
                                })),
                        )
                        .child(
                            super::super::brand::control("inbox-view-source", cx)
                                .label("Markdown source")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.inbox_ui.show_source = true;
                                    cx.notify();
                                })),
                        ),
                )
                .when(self.inbox_ui.show_source, |p| {
                    p.child(div().text_xs().child(format!(
                        "{} · {}",
                        text(&self.inbox_ui.loaded["item"]["path"]),
                        text(&self.inbox_ui.loaded["item"]["revision"])
                    )))
                })
                .child(
                    Textarea::new(if self.inbox_ui.show_source {
                        &self.inbox_ui.source
                    } else {
                        &self.inbox_ui.body
                    })
                    .readonly(true)
                    .h(px(340.)),
                )
                .child(
                    super::super::brand::control("inbox-new", cx)
                        .label("New thought")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.begin_thought(window, cx)),
                        ),
                );
        } else {
            panel = panel.child("Loading the exact saved Markdown…");
        }
        panel.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    fn workspace() -> Value {
        json!({"brain_id":"cc000000-0000-4000-8000-000000000124","root":"/synthetic/brain","records_dir":"records","managed":true})
    }
    fn temp() -> PathBuf {
        std::env::temp_dir().join(format!("tessera-inbox-native-{}", uuid()))
    }
    fn committed(request: &Value) -> Value {
        json!({"capture_id":"dd000000-0000-4000-8000-000000000124","path":"records/inbox-dd000000-0000-4000-8000-000000000124.md","revision":"sha256:source","received_at":"2026-09-06T22:00:00Z","receipt":{"status":"committed","operation_id":request["operation_id"]}})
    }
    #[gpui::test]
    fn saved_thought_keeps_plan_action_visible_and_panel_within_viewport(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            v.busy = true;
            v.collection = Collection::Inbox;
            v.inbox_ui.composing = false;
            v.inbox_ui.loaded =
                json!({"item":{"title":"Saved thought"},"planned_goals":[],"text":"Original"});
            v
        });
        cx.run_until_parked();
        let height = cx.update(|window, _| window.viewport_size().height);
        let panel = cx
            .debug_bounds("inbox-panel")
            .expect("saved thought panel is rendered");
        let action = cx
            .debug_bounds("inbox-plan-open")
            .expect("positive control: explicit planning action exists");
        assert!(
            panel.bottom() <= height,
            "fixed source height must not push the panel outside the window"
        );
        assert!(
            action.top() >= panel.top() && action.bottom() <= panel.bottom(),
            "Plan action must be visible before the long source field"
        );
    }
    #[test]
    fn outbox_survives_unknown_reply_and_restart_with_exact_bytes_and_identity() {
        let dir = temp();
        let j = InboxJournal::at(dir.clone(), &workspace()).unwrap();
        let text = "  A thought\r\n\nWith no final newline  ";
        let request = j.request(text, "local:oleg").unwrap();
        j.retain(&request).unwrap();
        drop(j);
        let reopened = InboxJournal::at(dir.clone(), &workspace()).unwrap();
        assert_eq!(reopened.pending().unwrap(), vec![request.clone()]);
        assert_eq!(request["text"], text);
        assert_eq!(request["source"]["message_id"], request["operation_id"]);
        assert_eq!(request["source"]["instance_id"], reopened.instance);
        reopened.retain(&request).unwrap();
        let mut forged = request.clone();
        forged["text"] = json!("different thought");
        assert!(reopened.retain(&forged).is_err());
        let mut wrong_receipt = committed(&request);
        wrong_receipt["receipt"]["operation_id"] = json!(uuid());
        assert!(reopened.acknowledge(&request, &wrong_receipt).is_err());
        assert_eq!(reopened.pending().unwrap(), vec![request.clone()]);
        reopened
            .acknowledge(&request, &committed(&request))
            .unwrap();
        assert!(InboxJournal::at(dir.clone(), &workspace())
            .unwrap()
            .pending()
            .unwrap()
            .is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn concurrent_windows_share_client_but_cannot_clear_another_capture() {
        let dir = temp();
        let a = InboxJournal::at(dir.clone(), &workspace()).unwrap();
        let b = InboxJournal::at(dir.clone(), &workspace()).unwrap();
        assert_eq!(a.instance, b.instance);
        let first = a.request("one", "local").unwrap();
        let second = b.request("two", "local").unwrap();
        a.retain(&first).unwrap();
        b.retain(&second).unwrap();
        a.acknowledge(&first, &committed(&first)).unwrap();
        assert_eq!(b.pending().unwrap(), vec![second]);
        let mut moved = workspace();
        moved["root"] = json!("/other");
        assert!(InboxJournal::at(dir.clone(), &moved)
            .unwrap()
            .pending()
            .is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn input_validation_precedes_outbox_and_preserves_utf8_byte_boundary() {
        let dir = temp();
        let j = InboxJournal::at(dir.clone(), &workspace()).unwrap();
        assert!(j.request(" \r\n", "local").is_err());
        assert!(j.request(&"é".repeat(32769), "local").is_err());
        assert!(j.request("valid", "").is_err());
        assert!(j.pending().unwrap().is_empty());
        assert!(j.request(&"é".repeat(32768), "local").is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[gpui::test]
    fn inbox_reads_reject_late_identity_and_do_not_touch_goal_or_editor_drafts(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window,cx|{
            let mut v=BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);v.busy=true;
            v.expected_workspace=Some(workspace());v.snapshot=json!({"goal":{"id":"active","title":"Keep me"}});v.selected_goal_id=Some("active".into());
            v.compose.update(cx,|i,cx|i.set_value("conversation draft",window,cx));
            v.source.reset("editor draft", window, cx);v.source_editable=true;
            v.inbox_ui.sequence=2;v.inbox_ui.selected=Some("selected".into());
            let request=json!({"op":"inbox_get","capture_id":"old","_inbox_sequence":1,"_inbox_workspace":workspace()});
            v.inbox_reply("inbox_get",json!({"item":{"capture_id":"old"},"_client_inbox_request":request}),window,cx);
            assert!(v.inbox_ui.loaded.is_null());
            let body="  exact thought\r\nwithout final newline";
            let canonical=format!("---\nrecord_type: inbox\n---\n{body}");
            let request=json!({"op":"inbox_get","capture_id":"selected","_inbox_sequence":2,"_inbox_workspace":workspace()});
            let reply=json!({"item":{"capture_id":"selected","path":"records/inbox-selected.md","revision":"sha256:current"},"text":body,
                "source":{"schema":SCHEMA,"brain_id":workspace()["brain_id"],"path":"records/inbox-selected.md","revision":"sha256:current","content_base64":STANDARD.encode(canonical.as_bytes())},"_client_inbox_request":request});
            v.inbox_reply("inbox_get",reply,window,cx);
            assert_eq!(v.inbox_ui.body.read(cx).value().as_ref(),body,"positive control: the selected capture loads its original body");
            assert_eq!(v.inbox_ui.source.read(cx).value().as_ref(),canonical,"canonical Markdown remains available separately");
            assert!(!v.inbox_ui.show_source);

            let request=json!({"op":"inbox_list","cursor":null,"_inbox_sequence":2,"_inbox_workspace":workspace()});
            v.inbox_reply("inbox_list",json!({"items":[{"capture_id":"selected","title":"Thought"}],"generation":"one","next_cursor":"next","_client_inbox_request":request}),window,cx);
            assert_eq!(array(&v.inbox_ui.page["items"]).len(),1,"positive control: current page is accepted");
            let request=json!({"op":"inbox_list","cursor":"next","_inbox_sequence":2,"_inbox_workspace":workspace()});
            v.inbox_reply("inbox_list",json!({"items":[{"capture_id":"wrong"}],"generation":"changed","_client_inbox_request":request}),window,cx);
            assert_eq!(array(&v.inbox_ui.page["items"]).len(),1);assert!(v.inbox_ui.error.is_some());
            assert_eq!(v.goal_id(),"active");assert_eq!(v.selected_goal_id.as_deref(),Some("active"));
            assert_eq!(v.compose.read(cx).value().as_ref(),"conversation draft");assert_eq!(v.source.value(cx).as_ref(),"editor draft");assert!(v.dirty(cx));
            v
        });
    }
    #[gpui::test]
    fn committed_capture_keeps_later_draft_and_goal_while_recovering_exact_request(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = temp();
        let cleanup = dir.clone();
        cx.add_window_view(move |window, cx| {
            let mut v = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            v.busy = true;
            v.expected_workspace = Some(workspace());
            v.snapshot = json!({"goal":{"id":"active"}});
            v.selected_goal_id = Some("active".into());
            let j = InboxJournal::at(dir, &workspace()).unwrap();
            let request = j.request("original thought", "local").unwrap();
            j.retain(&request).unwrap();
            v.inbox_ui.pending = vec![request.clone()];
            v.inbox_ui.journal = Some(j);
            v.inbox_ui
                .draft
                .update(cx, |i, cx| i.set_value("later draft", window, cx));
            let mut reply = committed(&request);
            reply["_client_inbox_request"] = request;
            v.inbox_reply("inbox_capture", reply, window, cx);
            assert!(v.inbox_ui.pending.is_empty());
            assert!(v
                .inbox_ui
                .journal
                .as_ref()
                .unwrap()
                .pending()
                .unwrap()
                .is_empty());
            assert_eq!(v.inbox_ui.draft.read(cx).value().as_ref(), "later draft");
            assert_eq!(v.goal_id(), "active");
            assert_eq!(v.selected_goal_id.as_deref(), Some("active"));
            v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
    #[gpui::test]
    fn failed_local_publication_retries_same_identity_and_requires_durability_before_tcp(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = temp();
        let cleanup = dir.clone();
        cx.add_window_view(move |window, cx| {
            let mut v = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
            v.expected_workspace = Some(workspace());
            v.capabilities = json!({"inbox_capture":true,"actor":"local"});
            let j = InboxJournal::at(dir, &workspace()).unwrap();
            j.fail_retain_after_publish.set(true);
            v.inbox_ui.journal = Some(j);
            v.inbox_ui
                .draft
                .update(cx, |i, cx| i.set_value("one thought", window, cx));
            v.capture_inbox(window, cx);
            assert!(!v.busy, "local failure must not start TCP");
            assert_eq!(v.inbox_ui.pending.len(), 1);
            let original = v.inbox_ui.pending[0].clone();
            assert_eq!(
                v.inbox_ui.journal.as_ref().unwrap().pending().unwrap(),
                vec![original.clone()]
            );
            v.inbox_ui
                .journal
                .as_ref()
                .unwrap()
                .fail_retain_after_publish
                .set(true);
            v.capture_inbox(window, cx);
            assert!(
                !v.busy,
                "retry must re-establish local durability before TCP"
            );
            assert_eq!(v.inbox_ui.pending, vec![original.clone()]);
            v.capture_inbox(window, cx);
            assert!(
                v.busy,
                "positive control: successful durable retry dispatches"
            );
            assert_eq!(v.inbox_ui.pending, vec![original]);
            v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
    #[gpui::test]
    fn native_capture_without_providers_retains_one_request_before_network(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = temp();
        let cleanup = dir.clone();
        cx.add_window_view(move |window,cx| {
            let mut v=BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);
            v.expected_workspace=Some(workspace());v.snapshot=json!({"goal":{"id":"active"}});v.selected_goal_id=Some("active".into());
            v.inbox_ui.journal=Some(InboxJournal::at(dir,&workspace()).unwrap());
            v.inbox_ui.draft.update(cx,|i,cx|i.set_value("thought without criteria",window,cx));
            v.capabilities=json!({"inbox_capture":true,"inbox_read":true,"actor":"local","chat":false,"t3":false,"todoist":false});
            v.capture_inbox(window,cx);
            assert_eq!(v.inbox_ui.pending.len(),1);
            assert_eq!(v.inbox_ui.journal.as_ref().unwrap().pending().unwrap(),v.inbox_ui.pending);
            assert_eq!(v.goal_id(),"active");assert_eq!(v.selected_goal_id.as_deref(),Some("active"));
            v.busy=false;v.capabilities=json!({"chat":true});let before=v.inbox_ui.pending.clone();
            v.capture_inbox(window,cx);assert_eq!(v.inbox_ui.pending,before);
            v
        });
        std::fs::remove_dir_all(cleanup).unwrap();
    }
    #[gpui::test]
    fn provider_absence_does_not_disable_capture_but_older_backend_does(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view,cx)=cx.add_window_view(|window,cx|{
            let mut v=BrainView::new("127.0.0.1:1".parse().unwrap(),window,cx);v.busy=true;
            v.collection=Collection::Inbox;v.capabilities=json!({"inbox_read":true,"inbox_capture":true,"chat":false,"t3":false,"todoist":false});
            v.inbox_ui.draft.update(cx,|i,cx|i.set_value("retained thought",window,cx));v
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("inbox-panel").is_some());
        view.update(cx, |v, cx| {
            v.capabilities = json!({"chat":true});
            assert_eq!(
                v.inbox_ui.draft.read(cx).value().as_ref(),
                "retained thought"
            );
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("inbox-panel").is_some());
    }
}
