//! Explicit, transient Todoist Inbox browsing. Provider data never becomes a
//! local task catalog; only an accepted link updates the existing task view.
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Loading {
    Page,
    Link,
    Recover,
}

#[derive(Default)]
pub(super) struct PickerUi {
    open: bool,
    sequence: u64,
    loading: Option<Loading>,
    page: Option<Value>,
    selected: Option<String>,
    error: Option<String>,
    page_failed: bool,
    requires_refresh: bool,
}
impl PickerUi {
    pub(super) fn settling(&self) -> bool {
        matches!(self.loading, Some(Loading::Link | Loading::Recover))
    }
    pub(super) fn linking(&self) -> bool {
        self.loading == Some(Loading::Link)
    }
}

#[derive(Clone)]
struct Target {
    endpoint: SocketAddr,
    workspace: Option<Value>,
    goal: String,
    task: Value,
    sequence: u64,
}

pub(super) fn task_identity(snapshot: &Value) -> Value {
    let task = &snapshot["task"];
    json!([
        task["provider"],
        task["instance_id"],
        task["goal_id"],
        task["task_id"]
    ])
}

fn nonempty(value: &Value) -> bool {
    value.as_str().is_some_and(|value| !value.is_empty())
}

fn validate_page(page: Value, goal: &str, previous: Option<&Value>) -> Result<Value, String> {
    let malformed = || "Todoist returned an invalid Inbox page. Refresh the Inbox.".to_string();
    if page["schema"] != "okilum-todoist-inbox/v1"
        || page["goal_id"] != goal
        || !nonempty(&page["session_id"])
        || !nonempty(&page["account_id"])
        || !nonempty(&page["inbox_project_id"])
        || !page["complete"].is_boolean()
        || !page["can_load_more"].is_boolean()
        || !page["limit_reached"].is_boolean()
    {
        return Err(malformed());
    }
    let rows = page["items"].as_array().ok_or_else(malformed)?;
    if rows.len() > 500
        || page["can_load_more"].as_bool().unwrap()
            != (page["complete"] == false && page["limit_reached"] == false)
        || (page["complete"] == true && page["limit_reached"] == true)
    {
        return Err(malformed());
    }
    let mut ids = BTreeSet::new();
    for row in rows {
        if !nonempty(&row["task_id"])
            || !ids.insert(text(&row["task_id"]))
            || !row["content"].is_string()
            || !row["description"].is_string()
            || !row["labels"]
                .as_array()
                .is_some_and(|labels| labels.iter().all(Value::is_string))
            || !row["priority"]
                .as_u64()
                .is_some_and(|priority| (1..=4).contains(&priority))
            || !row["url"]
                .as_str()
                .is_some_and(|url| url.starts_with("https://app.todoist.com/app/task/"))
            || (!row["due"].is_null()
                && (!row["due"]["date"].is_string() || !row["due"]["is_recurring"].is_boolean()))
        {
            return Err(malformed());
        }
    }
    if let Some(previous) = previous {
        if ["session_id", "account_id", "inbox_project_id"]
            .iter()
            .any(|key| page[key] != previous[key])
        {
            return Err(
                "The Inbox session or account changed. Refresh before selecting a task.".into(),
            );
        }
        let old = previous["items"].as_array().ok_or_else(malformed)?;
        if rows.len() < old.len() || rows[..old.len()] != old[..] {
            return Err(
                "Previously displayed tasks changed. Refresh before selecting a task.".into(),
            );
        }
    }
    Ok(page)
}

fn provider_error(data: &Value) -> Option<String> {
    let error = data.get("_todoist_picker_error")?;
    let message = error["message"]
        .as_str()
        .unwrap_or("Todoist could not complete this request.");
    let action = match error["code"].as_str().unwrap_or("") {
        "todoist_selection_stale" | "todoist_pagination_invalid" => {
            "Refresh the Inbox and select the task again."
        }
        "todoist_account_changed"
        | "todoist_account_unverified"
        | "todoist_unavailable"
        | "todoist_configuration_changed" => {
            "Open Connections to reconnect the saved Todoist account."
        }
        "todoist_picker_busy" => {
            "An Inbox request is still in progress. Try again when it finishes."
        }
        _ => match error["todoist"]["kind"].as_str().unwrap_or("") {
            "unauthorized" => "Todoist authorization expired. Reconnect in Connections.",
            "forbidden" => "Todoist denied access to this Inbox.",
            "rate_limited" => "Todoist is rate limited. Wait before trying again.",
            "not_found" => "The task is no longer available. Refresh the Inbox.",
            "transport" => "Todoist is unavailable. Try again when the connection is restored.",
            "malformed" => "Todoist returned an invalid response. Refresh to try again.",
            _ => "Refresh to recover the current state; the request was not retried.",
        },
    };
    let retry = error["todoist"]["retry_after_secs"]
        .as_u64()
        .map(|seconds| format!(" Retry after {seconds} seconds."))
        .unwrap_or_default();
    Some(format!("{message} {action}{retry}"))
}

fn row_details(row: &Value) -> String {
    let due = if row["due"].is_object() {
        format!(
            "Due {}{}{}",
            text(&row["due"]["date"]),
            if row["due"]["is_recurring"] == true {
                " · Recurring"
            } else {
                ""
            },
            row["due"]["string"]
                .as_str()
                .map(|s| format!(" · {s}"))
                .unwrap_or_default()
        )
    } else {
        "No due date".into()
    };
    let labels = array(&row["labels"])
        .iter()
        .map(text)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{due} · Priority {}{}",
        5 - row["priority"].as_u64().unwrap_or(1),
        if labels.is_empty() {
            String::new()
        } else {
            format!(" · Labels: {labels}")
        }
    )
}

impl BrainView {
    pub(super) fn reset_todoist_picker(&mut self) {
        self.todoist_picker = PickerUi {
            sequence: self.todoist_picker.sequence.wrapping_add(1),
            ..Default::default()
        };
    }

    fn picker_target(&self) -> Target {
        Target {
            endpoint: self.endpoint,
            workspace: self.expected_workspace.clone(),
            goal: self.goal_id(),
            task: task_identity(&self.snapshot),
            sequence: self.todoist_picker.sequence,
        }
    }

    fn picker_matches(&self, target: &Target) -> bool {
        self.todoist_picker.open
            && self.todoist_picker.sequence == target.sequence
            && self.endpoint == target.endpoint
            && self.expected_workspace == target.workspace
            && self.goal_id() == target.goal
            && self.selected_goal_id.as_deref() == Some(target.goal.as_str())
            && task_identity(&self.snapshot) == target.task
            && self.capabilities["todoist_inbox_picker"] == true
            && self.capabilities["todoist"] == true
    }

    fn finish_picker_page(
        &mut self,
        target: &Target,
        previous: Option<&Value>,
        response: Result<Value, String>,
    ) -> bool {
        if !self.picker_matches(target) {
            return false;
        }
        self.todoist_picker.loading = None;
        let requires_refresh = response.as_ref().is_ok_and(|data| {
            data.get("_todoist_picker_error").is_none()
                || matches!(
                    data["_todoist_picker_error"]["code"].as_str(),
                    Some(
                        "todoist_selection_stale"
                            | "todoist_pagination_invalid"
                            | "todoist_account_changed"
                            | "todoist_account_unverified"
                            | "todoist_configuration_changed"
                            | "todoist_unavailable"
                    )
                )
        });
        let response = response.and_then(|data| match provider_error(&data) {
            Some(error) => Err(error),
            None => validate_page(data, &target.goal, previous),
        });
        match response {
            Ok(page) => {
                if self.todoist_picker.selected.as_ref().is_some_and(|id| {
                    !array(&page["items"])
                        .iter()
                        .any(|row| row["task_id"] == *id)
                }) {
                    self.todoist_picker.selected = None;
                }
                self.todoist_picker.page = Some(page);
                self.todoist_picker.page_failed = false;
                self.todoist_picker.requires_refresh = false;
                self.todoist_picker.error = None;
            }
            Err(error) => {
                self.todoist_picker.error = Some(error);
                self.todoist_picker.page_failed = true;
                self.todoist_picker.requires_refresh = requires_refresh;
                self.todoist_picker.selected = None;
            }
        }
        true
    }

    fn finish_picker_recovery(&mut self, target: &Target, response: Result<Value, String>) -> bool {
        if !self.picker_matches(target) {
            return false;
        }
        self.todoist_picker.loading = None;
        match response {
            Ok(snapshot)
                if snapshot["goal"]["id"] == target.goal
                    && (snapshot["task"].is_null()
                        || (snapshot["task"].is_object()
                            && nonempty(&snapshot["task"]["task_id"])
                            && snapshot["task"]["goal_id"] == target.goal)) =>
            {
                // A link may have committed before its acknowledgement was lost.
                // Recover the established binding before enabling a new selection.
                self.snapshot["task"] = snapshot["task"].clone();
                self.pending_task = snapshot["pending_task_operation_id"]
                    .as_str()
                    .map(str::to_owned);
                self.request_generation = self.request_generation.wrapping_add(1);
                true
            }
            result => {
                let error = result.err().unwrap_or_else(|| {
                    "The saved task response belongs to another goal or is invalid.".into()
                });
                self.todoist_picker.error = Some(format!(
                    "{error} The current task could not be recovered. Refresh Inbox to try again."
                ));
                self.todoist_picker.requires_refresh = true;
                false
            }
        }
    }

    fn load_todoist_inbox(&mut self, more: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy
            || self.todoist_picker.loading.is_some()
            || self.goal_id().is_empty()
            || self.pending_capture_id.is_some()
            || self.capabilities["todoist"] != true
            || self.capabilities["todoist_inbox_picker"] != true
        {
            return;
        }
        if more {
            self.begin_todoist_page(true, window, cx);
            return;
        }
        self.todoist_picker.sequence = self.todoist_picker.sequence.wrapping_add(1);
        self.todoist_picker.open = true;
        self.todoist_picker.loading = Some(Loading::Recover);
        self.todoist_picker.requires_refresh = true;
        self.todoist_picker.selected = None;
        self.todoist_picker.error = None;
        let target = self.picker_target();
        let request = json!({"op":"snapshot", "goal_id":target.goal});
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let captured = target.clone();
            let response = cx
                .background_executor()
                .spawn(async move {
                    rpc_guarded(captured.endpoint, request, captured.workspace.as_ref())
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.finish_picker_recovery(&target, response) {
                    this.begin_todoist_page(false, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn begin_todoist_page(&mut self, more: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy
            || self.todoist_picker.loading.is_some()
            || self.goal_id().is_empty()
            || self.pending_capture_id.is_some()
            || self.capabilities["todoist"] != true
            || self.capabilities["todoist_inbox_picker"] != true
        {
            return;
        }
        if more && self.todoist_picker.requires_refresh {
            return;
        }
        let previous = if more {
            let Some(page) = self
                .todoist_picker
                .page
                .as_ref()
                .filter(|page| page["can_load_more"] == true)
            else {
                return;
            };
            Some(page.clone())
        } else {
            None
        };
        self.todoist_picker.sequence = self.todoist_picker.sequence.wrapping_add(1);
        self.todoist_picker.open = true;
        self.todoist_picker.loading = Some(Loading::Page);
        self.todoist_picker.error = None;
        if !more {
            self.todoist_picker.page = None;
            self.todoist_picker.selected = None;
        }
        let target = self.picker_target();
        let request = json!({"op":"todoist_inbox_list","goal_id":target.goal,"session_id":previous.as_ref().map(|page| &page["session_id"])});
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let captured = target.clone();
            let response = cx
                .background_executor()
                .spawn(async move {
                    rpc_guarded(captured.endpoint, request, captured.workspace.as_ref())
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                if this.finish_picker_page(&target, previous.as_ref(), response) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn finish_picker_link(
        &mut self,
        target: &Target,
        session: &str,
        task: &str,
        response: Result<Value, String>,
    ) -> bool {
        if !self.picker_matches(target)
            || self
                .todoist_picker
                .page
                .as_ref()
                .is_none_or(|page| page["session_id"] != session)
            || self.todoist_picker.selected.as_deref() != Some(task)
        {
            return false;
        }
        self.todoist_picker.loading = None;
        match response.and_then(|data| match provider_error(&data) {
            Some(error) => Err(error),
            None => Ok(data),
        }) {
            Ok(data)
                if data["status"] == "accepted"
                    && data["task"]["task_id"] == task
                    && data["task"]["goal_id"] == target.goal =>
            {
                self.request_generation = self.request_generation.wrapping_add(1);
                self.snapshot["task"] = data["task"].clone();
                self.reset_todoist_picker();
                self.notice = "Todoist task linked".into();
            }
            Ok(_) => {
                self.todoist_picker.error = Some("The link acknowledgement could not be verified. Refresh the goal to recover the saved task; nothing was retried.".into());
                self.todoist_picker.selected = None;
                self.todoist_picker.requires_refresh = true;
            }
            Err(error) => {
                self.todoist_picker.error = Some(error);
                self.todoist_picker.selected = None;
                self.todoist_picker.requires_refresh = true;
            }
        }
        true
    }

    fn link_todoist_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy
            || self.todoist_picker.loading.is_some()
            || self.todoist_picker.requires_refresh
            || self.pending_task.is_some()
            || self.pending_capture_id.is_some()
        {
            return;
        }
        let Some(page) = &self.todoist_picker.page else {
            return;
        };
        let Some(task) = self.todoist_picker.selected.clone() else {
            return;
        };
        if !array(&page["items"])
            .iter()
            .any(|row| row["task_id"] == task)
        {
            return;
        }
        let session = text(&page["session_id"]);
        self.todoist_picker.sequence = self.todoist_picker.sequence.wrapping_add(1);
        self.todoist_picker.loading = Some(Loading::Link);
        self.todoist_picker.error = None;
        let target = self.picker_target();
        // Drop any older generic snapshot read before accepting a fresh link.
        self.request_generation = self.request_generation.wrapping_add(1);
        let request = json!({"op":"task_link","goal_id":target.goal,"task_id":task,"picker_session_id":session});
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let captured = target.clone();
            let response = cx
                .background_executor()
                .spawn(async move {
                    rpc_guarded(captured.endpoint, request, captured.workspace.as_ref())
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                if this.finish_picker_link(&target, &session, &task, response) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn todoist_picker_panel(&mut self, cx: &Context<Self>) -> AnyElement {
        let colors = super::super::brand::palette(cx);
        let busy = self.busy || self.todoist_picker.loading.is_some();
        let mut panel = v_flex().id("todoist-inbox-picker").gap_3().min_w_0();
        if self.capabilities["todoist_inbox_picker"] != true {
            return panel
                .child(
                    div()
                        .text_sm()
                        .child("Inbox browsing requires an updated backend."),
                )
                .into_any_element();
        }
        panel = panel.child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(
                    super::super::brand::control("browse-todoist-inbox", cx)
                        .label(if self.todoist_picker.open {
                            "Refresh Inbox"
                        } else {
                            "Browse Todoist Inbox"
                        })
                        .disabled(
                            busy || self.goal_id().is_empty()
                                || self.capabilities["todoist"] != true
                                || self.pending_capture_id.is_some(),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.load_todoist_inbox(false, window, cx)
                        })),
                )
                .when(self.todoist_picker.open, |row| {
                    row.child(
                        super::super::brand::control("close-todoist-inbox", cx)
                            .label("Close Inbox")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.reset_todoist_picker();
                                cx.notify();
                            })),
                    )
                }),
        );
        if !self.todoist_picker.open {
            return panel.into_any_element();
        }
        if let Some(page) = &self.todoist_picker.page {
            panel = panel.child(div().text_sm().child(format!(
                "Todoist account {} · Inbox {}",
                text(&page["account_id"]),
                text(&page["inbox_project_id"])
            )));
            let items = array(&page["items"]);
            let status = if self.todoist_picker.page_failed {
                "Partial list — the last page could not be loaded."
            } else if page["limit_reached"] == true {
                "Partial list — browsing limit reached. Open Todoist for the full Inbox."
            } else if page["complete"] == true && items.is_empty() {
                "Your Todoist Inbox is empty."
            } else if page["complete"] == true {
                "All available Inbox tasks loaded."
            } else {
                "Partial list — more tasks may be available."
            };
            panel = panel.child(div().text_sm().child(status));
            let mut rows = v_flex()
                .id("todoist-inbox-rows")
                .max_h(px(360.))
                .overflow_y_scroll()
                .gap_2();
            for item in items {
                let id = text(&item["task_id"]);
                let selected = self.todoist_picker.selected.as_ref() == Some(&id);
                let selected_id = id.clone();
                rows = rows.child(
                    v_flex()
                        .id(SharedString::from(format!("todoist-row-{id}")))
                        .debug_selector({
                            let id = id.clone();
                            move || format!("todoist-row-{id}")
                        })
                        .gap_1()
                        .p_3()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(if selected { colors.link } else { colors.border })
                        .when(selected, |row| row.bg(colors.selected))
                        .child(
                            div()
                                .font_weight(FontWeight::BOLD)
                                .child(text(&item["content"])),
                        )
                        .child(div().text_sm().child(text(&item["description"])))
                        .child(div().text_sm().child(row_details(&item)))
                        .child(
                            h_flex()
                                .gap_2()
                                .flex_wrap()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.text_muted)
                                        .child(format!("Task {id}")),
                                )
                                .child(
                                    super::super::brand::control(
                                        SharedString::from(format!("select-todoist-{id}")),
                                        cx,
                                    )
                                    .small()
                                    .label(if selected { "Selected" } else { "Select task" })
                                    .disabled(busy || self.todoist_picker.requires_refresh)
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.todoist_picker.selected =
                                                Some(selected_id.clone());
                                            cx.notify();
                                        },
                                    )),
                                )
                                .child(
                                    super::super::brand::control(
                                        SharedString::from(format!("open-todoist-{id}")),
                                        cx,
                                    )
                                    .small()
                                    .label("Open in Todoist")
                                    .on_click({
                                        let url = text(&item["url"]);
                                        move |_, _, cx| cx.open_url(&url)
                                    }),
                                ),
                        ),
                );
            }
            panel = panel.child(rows);
            panel = panel.child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .when(page["can_load_more"] == true, |row| {
                        row.child(
                            super::super::brand::control("more-todoist-inbox", cx)
                                .label("Load more")
                                .disabled(busy || self.todoist_picker.requires_refresh)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.load_todoist_inbox(true, window, cx)
                                })),
                        )
                    })
                    .child(
                        super::super::brand::control("link-selected-todoist", cx)
                            .primary()
                            .label("Link selected task")
                            .disabled(
                                busy || self.todoist_picker.selected.is_none()
                                    || self.todoist_picker.requires_refresh
                                    || self.pending_task.is_some(),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.link_todoist_selection(window, cx)
                            })),
                    ),
            );
        }
        if let Some(loading) = self.todoist_picker.loading {
            panel = panel.child(div().text_sm().child(if loading == Loading::Link {
                "Checking and linking the selected task…"
            } else {
                "Loading Todoist Inbox…"
            }));
        }
        if self.todoist_picker.requires_refresh {
            panel = panel.child(div().text_sm().child("Refresh Inbox before choosing another task. Your current link and drafts are retained."));
        }
        if let Some(error) = &self.todoist_picker.error {
            panel = panel.child(div().text_sm().child(error.clone()));
        }
        panel.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn row(id: &str, description: &str) -> Value {
        json!({"task_id":id,"content":"Follow up","description":description,"due":{"date":"2026-09-09","is_recurring":true,"string":"every Wednesday"},"labels":["personal"],"priority":3,"url":format!("https://app.todoist.com/app/task/{id}")})
    }
    fn page() -> Value {
        json!({"schema":"okilum-todoist-inbox/v1","session_id":"session-a","goal_id":"goal-a","account_id":"account-a","inbox_project_id":"inbox-a","items":[row("task-one","Call the clinic"),row("task-two","Check the school dates")],"complete":false,"can_load_more":true,"limit_reached":false})
    }
    fn setup(window: &mut Window, cx: &mut Context<BrainView>) -> BrainView {
        let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
        view.expected_workspace = Some(json!({"brain_id":"brain-a","root":"/fixture"}));
        view.selected_goal_id = Some("goal-a".into());
        view.snapshot = json!({"goal":{"id":"goal-a","title":"Goal A"},"task":{"task_id":"current","instance_id":"connector-a","goal_id":"goal-a"}});
        view.capabilities = json!({"todoist":true,"todoist_inbox_picker":true});
        view.todoist_picker.open = true;
        view.todoist_picker.page = Some(page());
        view.todoist_picker.selected = Some("task-two".into());
        view.compose.update(cx, |input, cx| {
            input.set_value("Unsent personal question", window, cx)
        });
        view.next_step.update(cx, |input, cx| {
            input.set_value("Retained task draft", window, cx)
        });
        view
    }

    #[test]
    fn cumulative_pages_preserve_duplicate_titles_and_exact_frozen_identity() {
        let first = validate_page(page(), "goal-a", None).unwrap();
        assert_eq!(first["items"][0]["content"], first["items"][1]["content"]);
        assert_ne!(first["items"][0]["task_id"], first["items"][1]["task_id"]);
        assert_ne!(
            first["items"][0]["description"],
            first["items"][1]["description"]
        );
        let mut next = first.clone();
        next["items"]
            .as_array_mut()
            .unwrap()
            .push(row("task-three", "Different follow-up"));
        next["complete"] = json!(true);
        next["can_load_more"] = json!(false);
        assert!(validate_page(next.clone(), "goal-a", Some(&first)).is_ok());
        next["items"][1]["description"] = json!("Changed after selection");
        assert!(validate_page(next, "goal-a", Some(&first)).is_err());
        for key in ["goal_id", "session_id", "account_id", "inbox_project_id"] {
            let mut changed = first.clone();
            changed[key] = json!("other");
            assert!(validate_page(changed, "goal-a", Some(&first)).is_err());
        }
        let mut duplicate = first.clone();
        duplicate["items"][1] = duplicate["items"][0].clone();
        assert!(validate_page(duplicate, "goal-a", None).is_err());
        let details = row_details(&first["items"][1]);
        assert!(
            details.contains("Recurring")
                && details.contains("every Wednesday")
                && details.contains("personal")
                && details.contains("Priority 2")
        );
    }

    #[test]
    fn empty_partial_limit_and_provider_errors_are_distinct() {
        let mut empty = page();
        empty["items"] = json!([]);
        assert!(validate_page(empty.clone(), "goal-a", None).is_ok());
        empty["complete"] = json!(true);
        assert!(validate_page(empty.clone(), "goal-a", None).is_err());
        empty["can_load_more"] = json!(false);
        assert!(validate_page(empty.clone(), "goal-a", None).is_ok());
        empty["limit_reached"] = json!(true);
        assert!(validate_page(empty, "goal-a", None).is_err());
        let errors=["unauthorized","forbidden","rate_limited","transport","malformed"].map(|kind| provider_error(&json!({"_todoist_picker_error":{"code":"todoist_provider_error","message":"Provider refused","todoist":{"kind":kind,"retry_after_secs":12}}})).unwrap());
        assert_eq!(errors.iter().collect::<BTreeSet<_>>().len(), errors.len());
        assert!(errors[2].contains("12 seconds"));
    }

    #[gpui::test]
    fn late_pages_cannot_cross_goal_workspace_session_or_task_selection(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = setup(window, cx);
            let target = view.picker_target();
            assert!(view.finish_picker_page(&target, None, Ok(page())));
            let accepted = view.todoist_picker.page.clone();
            view.todoist_picker.sequence += 1;
            assert!(!view.finish_picker_page(&target, None, Ok(page())));
            view.todoist_picker.sequence = target.sequence;
            view.expected_workspace = Some(json!({"brain_id":"brain-a","root":"/different"}));
            assert!(!view.finish_picker_page(&target, None, Ok(page())));
            view.expected_workspace = target.workspace.clone();
            view.snapshot["goal"]["id"] = json!("goal-b");
            assert!(!view.finish_picker_page(&target, None, Ok(page())));
            view.snapshot["goal"]["id"] = json!("goal-a");
            view.snapshot["task"]["task_id"] = json!("other-task");
            assert!(!view.finish_picker_page(&target, None, Ok(page())));
            assert_eq!(view.todoist_picker.page, accepted);
            view
        });
    }

    #[gpui::test]
    fn page_and_link_failures_retain_rows_current_task_and_drafts(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window,cx| {
            let mut view=setup(window,cx); let target=view.picker_target(); let task=view.snapshot["task"].clone(); let first=page();
            assert!(view.finish_picker_page(&target,Some(&first),Err("Provider unavailable".into())));
            assert_eq!(view.todoist_picker.page,Some(first));
            assert!(view.todoist_picker.page_failed);
            view.todoist_picker.selected=Some("task-two".into());
            let failure=json!({"_todoist_picker_error":{"code":"todoist_selection_stale","message":"Task moved from Inbox"}});
            assert!(view.finish_picker_link(&target,"session-a","task-two",Ok(failure)));
            assert!(view.todoist_picker.requires_refresh);
            assert!(view.todoist_picker.selected.is_none());
            assert_eq!(view.snapshot["task"],task);
            assert_eq!(view.compose.read(cx).value().as_ref(),"Unsent personal question");
            assert_eq!(view.next_step.read(cx).value().as_ref(),"Retained task draft");
            view
        });
    }

    #[gpui::test]
    fn exact_selected_task_is_installed_only_for_accepted_current_reply(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window,cx| {
            let mut view=setup(window,cx); let target=view.picker_target();
            let response=json!({"status":"accepted","task":{"task_id":"task-two","goal_id":"goal-a","instance_id":"connector-a","content":"Follow up","status":"open"}});
            assert!(!view.finish_picker_link(&target,"old-session","task-two",Ok(response.clone())));
            view.todoist_picker.selected=Some("task-one".into());
            assert!(!view.finish_picker_link(&target,"session-a","task-two",Ok(response.clone())));
            view.todoist_picker.selected=Some("task-two".into());
            assert!(view.finish_picker_link(&target,"session-a","task-two",Ok(response)));
            assert_eq!(view.snapshot["task"]["task_id"],"task-two");
            assert!(!view.todoist_picker.open);
            assert_eq!(view.next_step.read(cx).value().as_ref(),"Retained task draft");
            view
        });
    }
    #[gpui::test]
    fn lost_link_acknowledgement_recovers_binding_before_new_selection(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = setup(window, cx);
            let target = view.picker_target();
            assert!(view.finish_picker_link(&target, "session-a", "task-two", Err("Acknowledgement lost".into())));
            assert_eq!(view.snapshot["task"]["task_id"], "current");
            assert!(view.todoist_picker.requires_refresh && view.todoist_picker.selected.is_none());
            let recovered = json!({"goal":{"id":"goal-a"},"task":{"task_id":"task-two","goal_id":"goal-a","instance_id":"connector-a"},"pending_task_operation_id":null});
            assert!(view.finish_picker_recovery(&target, Ok(recovered)));
            assert_eq!(view.snapshot["task"]["task_id"], "task-two");
            assert!(view.todoist_picker.requires_refresh && view.todoist_picker.selected.is_none());
            let next_target = view.picker_target();
            let mut refreshed = page(); refreshed["session_id"] = json!("refreshed-session");
            assert!(view.finish_picker_page(&next_target, None, Ok(refreshed)));
            assert!(!view.todoist_picker.requires_refresh);
            assert_eq!(view.snapshot["task"]["task_id"], "task-two");
            assert_eq!(view.next_step.read(cx).value().as_ref(), "Retained task draft");
            assert!(!view.finish_picker_recovery(&target, Ok(json!({"goal":{"id":"goal-b"},"task":null}))));
            assert_eq!(view.snapshot["task"]["task_id"], "task-two");
            view
        });
    }
}
