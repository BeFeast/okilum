//! Derived G04/G05 rows. Source messages, request authority and lossless details
//! remain in Feed; the nested list resolves only viewport members on demand.
use super::*;
use gpui::{FollowMode, ListAlignment, ListOffset, ListState};
use okilum_t3::projection::timeline as plan;
use serde_json::Value;

fn duration_label(ms: u64) -> String {
    if ms < 1000 {
        return format!("{}ms", ms.max(1));
    }
    if ms < 10_000 {
        let tenths = ms.saturating_add(50) / 100;
        return if tenths >= 100 {
            "10s".into()
        } else {
            format!("{}.{:01}s", tenths / 10, tenths % 10)
        };
    }
    let seconds = ms.saturating_add(500) / 1000;
    if ms < 60_000 {
        return format!("{seconds}s");
    }
    let mut parts = Vec::new();
    for (count, unit) in [
        (seconds / 3600, "h"),
        ((seconds % 3600) / 60, "m"),
        (seconds % 60, "s"),
    ] {
        if count != 0 {
            parts.push(format!("{count}{unit}"));
        }
    }
    parts.join(" ")
}

#[derive(Default)]
pub(super) struct Timeline {
    pub(super) dirty: bool,
    facts: HashMap<u64, plan::SourceRow>,
    pub(super) bound_thread: Option<String>,
    runs: Vec<plan::RunFact>,
    expansion: plan::Expansion,
    session: plan::Session,
    activity: plan::Activity,
    summaries: HashMap<plan::GroupKey, plan::WorkGroup>,
    folds: HashMap<plan::RunKey, plan::Fold>,
    nested: HashMap<plan::GroupKey, Entity<WorkList>>,
    pub(super) error: bool,
    disclosure_generation: u64,
    #[cfg(test)]
    replans: usize,
}

fn timestamp(item: &Value, fields: &[&str]) -> Option<i64> {
    fields.iter().find_map(|field| {
        item[*field]
            .as_str()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.timestamp_millis())
    })
}
fn run_state(value: &str) -> plan::RunState {
    use plan::RunState::*;
    match value {
        "preparing" => Preparing,
        "queued" => Queued,
        "starting" => Starting,
        "running" => Running,
        "waiting" => Waiting,
        "completed" => Completed,
        "interrupted" => Interrupted,
        "failed" => Failed,
        "cancelled" => Cancelled,
        "rolled_back" => RolledBack,
        _ => Unknown,
    }
}

// Match T3's rendered-output failure check, never the authored tool input.
fn output_looks_failed(output: &Value) -> bool {
    match output {
        Value::String(text) => {
            let normalized = text.to_lowercase();
            [
                "file not found",
                "no files found",
                "enoent",
                "no such file or directory",
                "no such file",
                "commandnotfoundexception",
                "command not found",
                "is not recognized as the name of a cmdlet",
                "a parameter cannot be found that matches parameter name",
            ]
            .iter()
            .any(|needle| normalized.contains(needle))
                || (normalized.contains("cannot find path")
                    && normalized.contains("because it does not exist"))
                || (normalized.contains("is not recognized") && normalized.contains("the term '"))
                || {
                    static EXIT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(
                        || {
                            regex::Regex::new(
                                r"(?i)<exited with exit code\s+[1-9]\d*\s*>|exit(?:ed)? with exit code\s+[1-9]\d*|exit code\s*[:\s]\s*[1-9]\d*\b",
                            )
                            .expect("audited T3 output failure expression")
                        },
                    );
                    EXIT.is_match(text)
                }
        }
        Value::Array(values) => values.iter().any(output_looks_failed),
        Value::Object(values) => values.values().any(output_looks_failed),
        _ => false,
    }
}

fn classify(item: &Value) -> plan::RowKind {
    use plan::{Barrier, RowKind, WorkStatus};
    let kind = item["type"].as_str().unwrap_or("");
    if item["tone"] == "error" || kind == "error" {
        return RowKind::Barrier(Barrier::SevereFailure);
    }
    match kind {
        "user_message" => RowKind::User,
        "assistant_message" => RowKind::Assistant,
        "approval_request" | "user_input_request" | "secret_request" => {
            RowKind::Barrier(Barrier::Request)
        }
        "proposed_plan" => RowKind::Barrier(Barrier::Plan),
        "subagent" => RowKind::Barrier(Barrier::AgentSpawn),
        "compaction" => RowKind::Compaction,
        "reasoning" | "command_execution" | "file_change" | "file_search" | "web_search"
        | "dynamic_tool" | "todo_list" | "notification" | "system_notice" => {
            let status = match item["status"].as_str() {
                Some(
                    "pending" | "running" | "waiting" | "streaming" | "inProgress" | "in_progress"
                    | "preparing" | "started",
                ) => WorkStatus::InProgress,
                Some("completed") => WorkStatus::Completed,
                Some("failed") => WorkStatus::Failed,
                Some("declined") => WorkStatus::Declined,
                Some("interrupted" | "cancelled" | "stopped") => WorkStatus::Stopped,
                _ => WorkStatus::Unspecified,
            };
            let tool_like = matches!(
                kind,
                "reasoning"
                    | "command_execution"
                    | "file_change"
                    | "file_search"
                    | "web_search"
                    | "dynamic_tool"
            ) || matches!(item["tone"].as_str(), Some("tool" | "thinking"));
            let output_failed = item["outputIndicatesFailure"] == true
                || item["exitCode"].as_i64().is_some_and(|code| code != 0)
                || item["output"]["isError"] == true
                || (tool_like && output_looks_failed(&item["output"]));
            let failed =
                output_failed || matches!(status, WorkStatus::Failed | WorkStatus::Declined);
            let thinking = kind == "reasoning" || item["tone"] == "thinking";
            let tool_call_id = item["toolCallId"]
                .as_str()
                .filter(|id| !id.is_empty())
                .map(str::to_owned);
            let lifecycle = item["label"]
                .as_str()
                .filter(|label| !label.trim().is_empty())
                .and_then(|label| {
                    if tool_call_id.is_some() {
                        return None;
                    }
                    let key = format!("{kind}:{}", label.trim().to_lowercase());
                    match status {
                        WorkStatus::Unspecified if item.get("status").is_none() => {
                            Some(plan::Lifecycle::StatuslessIdlessMarker(key))
                        }
                        WorkStatus::Completed
                        | WorkStatus::Failed
                        | WorkStatus::Declined
                        | WorkStatus::Stopped => Some(plan::Lifecycle::Terminal(key)),
                        _ => None,
                    }
                });
            RowKind::Work(plan::WorkFact {
                tool_like,
                tool_call_id,
                status,
                output_failed,
                neutral: item["tone"] == "neutral"
                    || item["status"] == "neutral"
                    || (tool_like
                        && !failed
                        && (thinking
                            || matches!(status, WorkStatus::InProgress | WorkStatus::Stopped))),
                task_progress: item["kind"] == "task.progress",
                lifecycle,
            })
        }
        "checkpoint"
        | "run_interrupt_request"
        | "run_interrupt_result"
        | "handoff"
        | "fork"
        | "thread_created" => RowKind::Barrier(Barrier::Notice),
        _ => RowKind::Barrier(Barrier::Unknown),
    }
}

impl Timeline {
    pub(super) fn source_thread(&self, position: u64) -> Option<&str> {
        self.facts.get(&position).map(|row| row.key.thread.as_str())
    }
    pub(super) fn streaming_message(&self, position: u64) -> bool {
        self.facts
            .get(&position)
            .is_some_and(|row| row.kind == plan::RowKind::Assistant && row.streaming)
    }
    pub(super) fn retain(&mut self, position: u64, row: &okilum_t3::projection::TurnRow) {
        let next = plan::SourceRow {
            key: row.key.clone(),
            run: row.item["runId"]
                .as_str()
                .filter(|id| !id.is_empty())
                .map(|id| plan::RunKey {
                    thread: row.key.thread.clone(),
                    id: id.into(),
                }),
            kind: classify(&row.item),
            streaming: row.item["type"] == "assistant_message"
                && row.item["streaming"].as_bool().unwrap_or(matches!(
                    row.item["status"].as_str(),
                    Some("streaming" | "running" | "in_progress")
                )),
            started_at: timestamp(
                &row.item,
                if row.item["type"] == "user_message" {
                    &["createdAt", "startedAt"]
                } else {
                    &["startedAt"]
                },
            ),
            ended_at: timestamp(&row.item, &["completedAt", "updatedAt"]),
        };
        // Timing retained during a text flush is consumed by the next lifecycle
        // replan; it does not scan/rebuild all history for each streamed chunk.
        self.dirty |= self.facts.get(&position).is_none_or(|old| {
            old.key != next.key
                || old.run != next.run
                || old.kind != next.kind
                || old.streaming != next.streaming
        });
        self.facts.insert(position, next);
    }
    pub(super) fn remove(&mut self, position: u64) {
        self.dirty |= self.facts.remove(&position).is_some();
    }
}

impl Feed {
    pub(super) fn group_member_changed(&self, position: u64, cx: &mut Context<Self>) {
        let Some(source) = self.groups.facts.get(&position).map(|row| &row.key) else {
            return;
        };
        for key in &self.groups.expansion.groups {
            let Some(list) = self.groups.nested.get(key) else {
                continue;
            };
            list.update(cx, |list, cx| {
                if let Some(index) = list.member_indices.get(source).copied() {
                    list.list.set_follow_mode(FollowMode::Normal);
                    list.list.remeasure_items(index..index + 1);
                    #[cfg(test)]
                    {
                        list.remeasures += 1;
                    }
                    cx.notify();
                }
            });
        }
    }
    pub(in crate::project) fn set_timeline_runs(
        &mut self,
        thread: &str,
        values: &[Value],
        running: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.groups.bound_thread = Some(thread.into());
        let mut ordered: Vec<_> = values
            .iter()
            .filter_map(|value| {
                let id = value["id"].as_str()?;
                let key = plan::RunKey {
                    thread: value["threadId"].as_str().unwrap_or(thread).into(),
                    id: id.into(),
                };
                Some((
                    value["ordinal"].as_u64().unwrap_or(0),
                    plan::RunFact {
                        key: key.clone(),
                        state: run_state(value["status"].as_str().unwrap_or("")),
                        requested_at: timestamp(value, &["requestedAt"]),
                        started_at: timestamp(value, &["startedAt"]),
                        work_started_at: timestamp(value, &["workStartedAt"]),
                        completed_at: timestamp(value, &["completedAt"]),
                        restart_continuation_of: value["restartContinuationOfRunId"].as_str().map(
                            |id| plan::RunKey {
                                thread: key.thread.clone(),
                                id: id.into(),
                            },
                        ),
                    },
                ))
            })
            .collect();
        ordered.sort_by(|a, b| (a.0, &a.1.key).cmp(&(b.0, &b.1.key)));
        let runs: Vec<_> = ordered.into_iter().map(|(_, run)| run).collect();
        let activity = plan::Activity {
            is_working: running.is_some(),
            running: running.map(|id| plan::RunKey {
                thread: thread.into(),
                id,
            }),
            latest: runs.last().map(|run| run.key.clone()),
        };
        if self.groups.runs != runs
            || self.groups.activity.running != activity.running
            || self.groups.activity.latest != activity.latest
        {
            if self.groups.activity.latest != activity.latest {
                if let Some(previous) = &self.groups.activity.latest {
                    self.groups.expansion.runs.remove(previous);
                }
            }
            self.groups.session.observe_latest(runs.last());
            self.groups.runs = runs;
            self.groups.activity = activity;
            self.groups.dirty = true;
        }
        let _ = cx;
    }

    pub(super) fn grouped_rows(&mut self, fallback: Vec<Row>, cx: &mut Context<Self>) -> Vec<Row> {
        if self.native_order.is_none() || self.groups.facts.is_empty() {
            return fallback;
        }
        let facts: Vec<_> = self
            .canonical_rows
            .iter()
            .filter_map(|position| self.groups.facts.get(position).cloned())
            .collect();
        #[cfg(test)]
        {
            self.groups.replans += 1;
        }
        let plan = plan::derive(plan::Input {
            rows: &facts,
            runs: &self.groups.runs,
            expansion: &self.groups.expansion,
            session: &self.groups.session,
            activity: &self.groups.activity,
        });
        self.groups.dirty = false;
        let Ok(plan) = plan else {
            self.groups.error = true;
            return fallback;
        };
        self.groups.error = false;
        self.groups.summaries.clear();
        self.groups.folds.clear();
        let mut rows = Vec::new();
        for row in plan.rows {
            match row {
                plan::PlannedRow::Source(key) => {
                    if let Some(position) = self.native_positions.get(&key) {
                        rows.push(Row::Message(*position));
                    }
                }
                plan::PlannedRow::Fold(fold) => {
                    rows.push(Row::Fold(fold.run.clone()));
                    self.groups.folds.insert(fold.run.clone(), fold);
                }
                plan::PlannedRow::WorkSummary(group) => {
                    rows.push(Row::WorkSummary(group.key.clone()));
                    if let Some(list) = self.groups.nested.get(&group.key) {
                        list.update(cx, |list, cx| list.sync(group.members.clone(), cx));
                    }
                    self.groups.summaries.insert(group.key.clone(), group);
                }
                plan::PlannedRow::WorkDetails { group, members } => {
                    if !self.groups.nested.contains_key(&group) {
                        let feed = cx.weak_entity();
                        self.groups
                            .nested
                            .insert(group.clone(), cx.new(|cx| WorkList::new(feed, members, cx)));
                    }
                    rows.push(Row::WorkDetails(group));
                }
            }
        }
        rows
    }

    fn replan_groups(&mut self, cx: &mut Context<Self>) {
        self.groups.dirty = true;
        self.apply_rows(std::iter::empty(), None, true, None, cx);
    }
    fn toggle_work_group(
        &mut self,
        key: plan::GroupKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let offset = self.scroller.read(cx).logical_scroll_top();
        let anchor = self.rows.get(offset.item_ix).cloned();
        let rearm = self.scroller.read(cx).is_following_tail()
            || self.groups.expansion.groups.contains(&key);
        self.scroller.update(cx, |state, cx| state.pause_follow(cx));
        if !self.groups.expansion.groups.remove(&key) {
            self.groups.expansion.groups.insert(key);
        }
        self.replan_groups(cx);
        self.restore_disclosure_anchor(anchor, offset.offset_in_item, rearm, window, cx);
    }
    fn toggle_fold(&mut self, key: plan::RunKey, window: &mut Window, cx: &mut Context<Self>) {
        let offset = self.scroller.read(cx).logical_scroll_top();
        let rearm = self.scroller.read(cx).is_following_tail()
            || self
                .groups
                .folds
                .get(&key)
                .is_some_and(|fold| fold.expanded);
        // Fold summaries are deliberately excluded from anchor candidates.
        let anchor = self
            .rows
            .get(offset.item_ix)
            .filter(|row| !matches!(row, Row::Fold(_)))
            .cloned();
        self.scroller.update(cx, |state, cx| state.pause_follow(cx));
        self.groups
            .session
            .toggle_fold(&key, &mut self.groups.expansion);
        self.replan_groups(cx);
        self.restore_disclosure_anchor(anchor, offset.offset_in_item, rearm, window, cx);
    }
    fn restore_disclosure_anchor(
        &mut self,
        anchor: Option<Row>,
        offset: Pixels,
        rearm: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.groups.disclosure_generation = self.groups.disclosure_generation.wrapping_add(1);
        let generation = self.groups.disclosure_generation;
        let lease = self.copy_lease.clone();
        let feed = cx.weak_entity();
        let restore = move |cx: &mut App, finish: bool| {
            let _ = feed.update(cx, |feed, cx| {
                if generation != feed.groups.disclosure_generation
                    || !Arc::ptr_eq(&lease, &feed.copy_lease)
                {
                    return;
                }
                if let Some(index) = anchor
                    .as_ref()
                    .and_then(|anchor| feed.rows.iter().position(|row| row == anchor))
                {
                    feed.scroller.update(cx, |state, cx| {
                        state.scroll_to_offset(
                            ListOffset {
                                item_ix: index,
                                offset_in_item: offset,
                            },
                            cx,
                        )
                    });
                }
                if finish && rearm {
                    feed.scroller.update(cx, |state, cx| {
                        if !state.is_scrolled_up() {
                            state.scroll_to_end(cx);
                        }
                    });
                }
            });
        };
        let second = restore.clone();
        window.on_next_frame(move |window, cx| {
            restore(cx, false);
            window.on_next_frame(move |_, cx| second(cx, true));
        });
    }

    pub(super) fn work_summary(&self, key: &plan::GroupKey, cx: &mut Context<Self>) -> AnyElement {
        let Some(group) = self.groups.summaries.get(key) else {
            return div().into_any_element();
        };
        let expanded = group.expanded;
        let label = if group.working {
            format!("Working · {} activities", group.members.len())
        } else if group.latest_tool_failed {
            format!("Work failed · {} activities", group.members.len())
        } else {
            format!("Work · {} activities", group.members.len())
        };
        let key = key.clone();
        div()
            .id(SharedString::from(format!("work-summary-{key:?}")))
            .aria_expanded(expanded)
            .tab_group()
            .child(
                Button::new("work-group-toggle")
                    .ghost()
                    .icon(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .label(label)
                    .accessibility_label(if expanded {
                        "Collapse work"
                    } else {
                        "Expand work"
                    })
                    .debug_selector(|| "manager-work-toggle".into())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.toggle_work_group(key.clone(), window, cx)
                    })),
            )
            .into_any_element()
    }
    pub(super) fn work_details(&self, key: &plan::GroupKey, _cx: &mut Context<Self>) -> AnyElement {
        self.groups
            .nested
            .get(key)
            .map(|list| {
                div()
                    .w_full()
                    .min_w_0()
                    .child(list.clone())
                    .into_any_element()
            })
            .unwrap_or_else(|| div().into_any_element())
    }
    pub(super) fn fold_summary(&self, key: &plan::RunKey, cx: &mut Context<Self>) -> AnyElement {
        let Some(fold) = self.groups.folds.get(key) else {
            return div().into_any_element();
        };
        let elapsed = match &fold.label {
            plan::FoldLabel::Worked { elapsed_ms } | plan::FoldLabel::Stopped { elapsed_ms } => {
                elapsed_ms.map(duration_label)
            }
        };
        let label = match (&fold.label, elapsed) {
            (plan::FoldLabel::Worked { .. }, Some(duration)) => format!("Worked for {duration}"),
            (plan::FoldLabel::Stopped { .. }, Some(duration)) => {
                format!("You stopped after {duration}")
            }
            (plan::FoldLabel::Worked { .. }, None) => "Worked".into(),
            _ => "You stopped this response".into(),
        };
        let expanded = fold.expanded;
        let key = key.clone();
        div()
            .id(SharedString::from(format!("fold-{key:?}")))
            .aria_expanded(expanded)
            .tab_group()
            .child(
                Button::new("completed-fold-toggle")
                    .ghost()
                    .icon(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .label(label)
                    .debug_selector(|| "manager-fold-toggle".into())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.toggle_fold(key.clone(), window, cx)
                    })),
            )
            .into_any_element()
    }
}

struct WorkList {
    feed: WeakEntity<Feed>,
    members: Arc<[RowKey]>,
    member_indices: HashMap<RowKey, usize>,
    list: ListState,
    #[cfg(test)]
    rendered: std::cell::RefCell<HashSet<RowKey>>,
    #[cfg(test)]
    remeasures: usize,
}
impl WorkList {
    fn new(feed: WeakEntity<Feed>, members: Arc<[RowKey]>, cx: &mut Context<Self>) -> Self {
        let list = ListState::new(members.len(), ListAlignment::Top, px(240.));
        let weak = cx.weak_entity();
        list.set_scroll_handler(move |_, _, cx| {
            let weak = weak.clone();
            cx.defer(move |cx| {
                let _ = weak.update(cx, |_, cx| cx.notify());
            });
        });
        Self {
            feed,
            member_indices: members
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, key)| (key, index))
                .collect(),
            members,
            list,
            #[cfg(test)]
            rendered: Default::default(),
            #[cfg(test)]
            remeasures: 0,
        }
    }
    fn sync(&mut self, members: Arc<[RowKey]>, cx: &mut Context<Self>) {
        if self.members == members {
            return;
        }
        let before = self.list.logical_scroll_top();
        let anchor = self.members.get(before.item_ix).cloned();
        let near_end = self.list.max_offset_for_scrollbar().y
            + self.list.scroll_px_offset_for_scrollbar().y
            <= px(1.);
        let append = members.len() > self.members.len() && members.starts_with(&self.members);
        self.list.set_follow_mode(FollowMode::Normal);
        if append {
            self.list.splice(
                self.members.len()..self.members.len(),
                members.len() - self.members.len(),
            );
        } else {
            self.list.splice(0..self.members.len(), members.len());
        }
        self.members = members;
        self.member_indices = self
            .members
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, key)| (key, index))
            .collect();
        if append && near_end {
            self.list.set_follow_mode(FollowMode::Tail);
            self.list.scroll_to_end();
        } else if let Some(index) =
            anchor.and_then(|key| self.members.iter().position(|member| member == &key))
        {
            self.list.scroll_to(ListOffset {
                item_ix: index,
                offset_in_item: before.offset_in_item,
            });
        }
        cx.notify();
    }
}
impl Render for WorkList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let height = px(288.).min(window.viewport_size().height * 0.5);
        let entity = cx.entity();
        div()
            .debug_selector(|| "manager-work-details".into())
            .w_full()
            .h(height)
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .relative()
            .child(
                gpui::list(self.list.clone(), move |index, _, cx| {
                    entity.update(cx, |this, cx| {
                        let Some(key) = this.members.get(index) else {
                            return div().into_any_element();
                        };
                        #[cfg(test)]
                        this.rendered.borrow_mut().insert(key.clone());
                        this.feed
                            .upgrade()
                            .map(|feed| {
                                feed.update(cx, |feed, cx| {
                                    feed.native_positions
                                        .get(key)
                                        .and_then(|position| {
                                            feed.messages.get(position).map(|message| {
                                                feed.activity(*position, message, cx)
                                            })
                                        })
                                        .unwrap_or_else(|| div().into_any_element())
                                })
                            })
                            .unwrap_or_else(|| div().into_any_element())
                    })
                })
                .size_full(),
            )
            .child(
                gpui_component::scroll::ScrollableMask::new(gpui::Axis::Vertical, &self.list)
                    .id("work-scroll-mask"),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use okilum_t3::projection::{Changes, TurnRow};
    use serde_json::json;

    struct Host {
        feed: Entity<Feed>,
        _parent: Entity<ProjectView>,
    }
    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.feed.clone())
        }
    }
    fn row(id: &str, ordinal: u64, kind: &str) -> Arc<TurnRow> {
        Arc::new(TurnRow {
            key: RowKey {
                thread: "thread".into(),
                item: id.into(),
            },
            local: true,
            item: Arc::new(
                json!({"id":id,"threadId":"thread","ordinal":ordinal,"type":kind,
                "runId":"run","status":"completed","text":format!("Authored {id}"),
                "input":format!("printf '{id}'"),"output":format!("Full output {id}"),
                "startedAt":"2026-10-31T12:00:01Z","updatedAt":"2026-10-31T12:00:47Z"}),
            ),
        })
    }
    fn run(status: &str, ordinal: u64) -> Value {
        json!({"id":"run","threadId":"thread","ordinal":ordinal,"status":status,
            "requestedAt":"2026-10-31T12:00:00Z","startedAt":"2026-10-31T12:00:00Z",
            "completedAt":if status=="running" {Value::Null} else {json!("2026-10-31T12:00:47Z")}})
    }
    fn fixture<'a>(
        cx: &'a mut TestAppContext,
        rows: Vec<Arc<TurnRow>>,
        state: &str,
    ) -> (Entity<Feed>, &'a mut VisualTestContext) {
        cx.update(crate::test_init);
        let (root, visual) = cx.add_window_view(|window, cx| {
            let parent = cx.new(|cx| ProjectView::empty(None, window, cx));
            parent.update(cx, |view, _| view.active = false);
            let feed = parent.read(cx).feed.clone();
            feed.update(cx, |feed, cx| {
                feed.set_timeline_runs(
                    "thread",
                    &[run(state, 1)],
                    (state == "running").then(|| "run".into()),
                    cx,
                );
                feed.apply_native(
                    Changes {
                        order: Some(rows.iter().map(|row| row.key.clone()).collect()),
                        rows,
                        ..Default::default()
                    },
                    &[],
                    cx,
                );
            });
            let host = cx.new(|_| Host {
                feed,
                _parent: parent,
            });
            Root::new(host, window, cx)
        });
        visual.simulate_resize(size(px(760.), px(900.)));
        visual.run_until_parked();
        let feed = root.read_with(visual, |root, cx| {
            root.view()
                .clone()
                .downcast::<Host>()
                .unwrap()
                .read(cx)
                .feed
                .clone()
        });
        (feed, visual)
    }
    fn draw(visual: &mut VisualTestContext) {
        visual.update(|window, cx| {
            // Headless GPUI has no native frame source: drive both disclosure
            // reconciliation frames explicitly, retaining real layout/paint.
            for _ in 0..2 {
                window.simulate_next_frame(cx);
                window.refresh();
                window.draw(cx).clear(cx);
            }
        });
    }
    fn key(visual: &mut VisualTestContext, name: &str) {
        if name == "enter" {
            visual.update(|window, cx| {
                window.focus_next(cx);
                window.refresh();
                window.draw(cx).clear(cx);
            });
            visual.simulate_keystrokes("tab");
            visual.run_until_parked();
        }
        let keystroke = Keystroke::parse(name).unwrap();
        visual.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        visual.simulate_event(KeyUpEvent { keystroke });
        visual.run_until_parked();
        draw(visual);
    }

    #[gpui::test]
    fn thousand_members_use_one_bounded_details_row_and_restore_member_offset(
        cx: &mut TestAppContext,
    ) {
        let (feed, visual) = fixture(
            cx,
            (0..1000)
                .map(|i| row(&format!("tool-{i}"), i, "command_execution"))
                .collect(),
            "running",
        );
        feed.read_with(visual, |feed, _| {
            assert_eq!(feed.rows.len(), 1);
            assert!(matches!(feed.rows[0], Row::WorkSummary(_)));
            assert!(
                feed.groups.nested.is_empty(),
                "collapsed work must not eagerly create detail views"
            );
        });
        let toggle = visual
            .debug_bounds("manager-work-toggle")
            .expect("positive control: rendered group toggle");
        visual.simulate_click(toggle.center(), Modifiers::default());
        visual.run_until_parked();
        draw(visual);
        let details = visual
            .debug_bounds("manager-work-details")
            .expect("actual click opens the bounded list");
        assert!(details.size.height <= px(288.));
        let nested = feed.read_with(visual, |feed, _| {
            assert_eq!(
                feed.rows.len(),
                2,
                "exactly one outer details row for every member"
            );
            feed.groups.nested.values().next().unwrap().clone()
        });
        nested.read_with(visual, |list, _| {
            assert_eq!(list.members.len(), 1000);
            assert!(
                !list.rendered.borrow().is_empty(),
                "positive control: nested rows were rendered"
            );
            assert!(
                list.rendered.borrow().len() <= 64,
                "offscreen members must remain unrendered"
            );
        });
        visual.simulate_event(gpui::ScrollWheelEvent {
            position: details.center(),
            delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-1600.))),
            ..Default::default()
        });
        visual.run_until_parked();
        let before = nested.read_with(visual, |list, _| {
            let offset = list.list.logical_scroll_top();
            assert!(
                offset.item_ix > 0,
                "positive control: actual wheel reached an interior member"
            );
            (list.members[offset.item_ix].clone(), offset.offset_in_item)
        });
        draw(visual);
        let toggle = visual.debug_bounds("manager-work-toggle").unwrap();
        visual.simulate_click(toggle.center(), Modifiers::default());
        visual.run_until_parked();
        draw(visual);
        assert!(visual.debug_bounds("manager-work-details").is_none());
        key(visual, "enter");
        assert!(
            visual.debug_bounds("manager-work-details").is_some(),
            "Tab/Enter reopens the work group"
        );
        nested.read_with(visual, |list, _| {
            let offset = list.list.logical_scroll_top();
            assert_eq!(
                (list.members[offset.item_ix].clone(), offset.offset_in_item),
                before
            );
        });
        // Streaming output alone retains facts/order and never reruns the O(N)
        // planner or rearms follow after an interior scroll.
        let replans = feed.read_with(visual, |feed, _| feed.groups.replans);
        let mut update = row("tool-20", 20, "command_execution").as_ref().clone();
        Arc::make_mut(&mut update.item)["output"] = json!("New retained full output");
        feed.update(visual, |feed, cx| {
            feed.apply_native(
                Changes {
                    rows: vec![Arc::new(update)],
                    ..Default::default()
                },
                &[],
                cx,
            )
        });
        visual.run_until_parked();
        assert_eq!(
            feed.read_with(visual, |feed, _| feed.groups.replans),
            replans
        );
        assert!(!nested.read_with(visual, |list, _| list.list.is_following_tail()));
        let append = row("tool-1000", 1000, "command_execution");
        feed.update(visual, |feed, cx| {
            let mut order: Vec<_> = feed
                .canonical_rows
                .iter()
                .filter_map(|position| feed.groups.facts.get(position).map(|fact| fact.key.clone()))
                .collect();
            order.push(append.key.clone());
            feed.apply_native(
                Changes {
                    rows: vec![append],
                    order: Some(order),
                    ..Default::default()
                },
                &[],
                cx,
            );
        });
        visual.run_until_parked();
        nested.read_with(visual, |list, _| {
            let offset = list.list.logical_scroll_top();
            assert_eq!(
                (list.members[offset.item_ix].clone(), offset.offset_in_item),
                before,
                "interior append preserves member and pixel offset"
            );
        });
        key(visual, "space");
        assert!(
            visual.debug_bounds("manager-work-details").is_none(),
            "Space collapses the focused toggle"
        );
    }

    #[gpui::test]
    fn interrupted_reload_folds_stale_in_progress_tool_rows(cx: &mut TestAppContext) {
        let mut tool = row("stale-tool", 0, "command_execution").as_ref().clone();
        Arc::make_mut(&mut tool.item)["status"] = json!("running");
        let (feed, visual) = fixture(
            cx,
            vec![Arc::new(tool), row("answer", 1, "assistant_message")],
            "interrupted",
        );
        assert!(
            visual.debug_bounds("manager-fold-toggle").is_some(),
            "a settled run, not a stale tool status, owns fold authority"
        );
        feed.read_with(visual, |feed, _| {
            assert!(!feed.groups.folds.values().next().unwrap().expanded)
        });
    }

    #[gpui::test]
    fn remote_detail_completion_remeasures_the_nested_member(cx: &mut TestAppContext) {
        use okilum_t3::stream::{NativeClient, NativeToken};
        use std::{
            io::{BufRead, BufReader, Write},
            net::TcpListener,
            thread,
            time::{Duration, Instant},
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let environment = Environment::new(
            "nested-detail",
            &format!("http://{}", listener.local_addr().unwrap()),
        )
        .unwrap();
        let server = thread::spawn(move || {
            let accept = || {
                let deadline = Instant::now() + Duration::from_secs(8);
                loop {
                    if let Ok((socket, _)) = listener.accept() {
                        socket
                            .set_read_timeout(Some(Duration::from_secs(4)))
                            .unwrap();
                        return socket;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "positive control: nested detail RPC must reach the server"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
            };
            let mut ticket = accept();
            let mut reader = BufReader::new(&mut ticket);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            drop(reader);
            let body = r#"{"ticket":"fixture"}"#;
            write!(
                ticket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            drop(ticket);
            let mut socket = tungstenite::accept(accept()).unwrap();
            let request: Value =
                serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(request["tag"], "orchestration.getTurnItem");
            assert_eq!(
                request["payload"],
                json!({"threadId":"thread","itemId":"tool-0","revision":"terminal-1"})
            );
            let item = json!({"id":"tool-0","threadId":"thread","type":"command_execution","status":"completed","input":"cargo test","output":format!("{}\nNESTED_FULL_END","full line\n".repeat(200)),"exitCode":0});
            socket.send(tungstenite::Message::Text(json!({"_tag":"Exit","requestId":"1","exit":{"_tag":"Success","value":{"item":item}}}).to_string().into())).unwrap();
        });
        let token=NativeToken::from_secret(&environment,&serde_json::to_vec(&json!({"origin":environment.origin(),"access_token":"fixture","expires_at":chrono::Utc::now().timestamp() as u64+3600,"scope":"orchestration:read"})).unwrap()).unwrap();
        let client = Arc::new(
            NativeClient::new(environment.clone(), token, Duration::from_secs(3)).unwrap(),
        );
        let mut omitted = row("tool-0", 0, "command_execution").as_ref().clone();
        {
            let item = Arc::make_mut(&mut omitted.item);
            item["outputOmitted"] = json!(true);
            item["updatedAt"] = json!("terminal-1");
        }
        let (feed, visual) = fixture(
            cx,
            vec![Arc::new(omitted), row("tool-1", 1, "command_execution")],
            "running",
        );
        feed.read_with(visual, |feed, _| feed.parent.upgrade().unwrap())
            .update(visual, |parent, _| {
                parent.environment = Some(environment);
                parent.native.set_detail_test_client(client);
            });
        let toggle = visual.debug_bounds("manager-work-toggle").unwrap();
        visual.simulate_click(toggle.center(), Modifiers::default());
        visual.run_until_parked();
        draw(visual);
        let nested = feed.read_with(visual, |feed, _| {
            feed.groups.nested.values().next().unwrap().clone()
        });
        let tool = visual
            .debug_bounds("project-tool-toggle-1")
            .expect("positive control: nested member's real disclosure is painted");
        visual.simulate_click(tool.center(), Modifiers::default());
        visual.run_until_parked();
        server.join().unwrap();
        draw(visual);
        feed.read_with(visual, |feed, _| {
            assert!(feed
                .detail_text(1, &feed.messages[&1].text)
                .contains("NESTED_FULL_END"))
        });
        nested.read_with(visual,|list,_| {
            assert!(list.remeasures>=2,"member disclosure and actual RPC completion both invalidate nested height");
            assert!(list.list.max_offset_for_scrollbar().y>px(0.),"full remote output is measured as scrollable content rather than clipped to the old preview height");
        });
    }

    #[gpui::test]
    fn completed_fold_keeps_final_copy_and_errors_and_stop_is_manually_collapsible(
        cx: &mut TestAppContext,
    ) {
        let mut rows: Vec<_> = (0..30)
            .map(|i| row(&format!("tool-{i}"), i, "command_execution"))
            .collect();
        rows.extend([
            row("answer", 30, "assistant_message"),
            row("failure", 31, "error"),
            row("pending", 32, "approval_request"),
        ]);
        let (feed, visual) = fixture(cx, rows, "completed");
        let fold = visual
            .debug_bounds("manager-fold-toggle")
            .expect("positive control: completed-response fold");
        feed.read_with(visual, |feed, _| {
            assert_eq!(
                feed.groups.folds.values().next().unwrap().label,
                plan::FoldLabel::Worked {
                    elapsed_ms: Some(47_000)
                }
            );
            assert_eq!(
                feed.rows.len(),
                4,
                "fold plus final answer plus protected error and request"
            );
            let final_position = feed.native_positions[&RowKey {
                thread: "thread".into(),
                item: "answer".into(),
            }];
            assert_eq!(
                feed.copy_text(final_position).as_deref(),
                Some("Authored answer"),
                "Copy uses canonical source order, not collapsed rows"
            );
        });
        visual.simulate_click(fold.center(), Modifiers::default());
        visual.run_until_parked();
        draw(visual);
        assert!(visual.debug_bounds("manager-work-toggle").is_some());
        feed.update(visual, |feed, cx| {
            feed.set_timeline_runs("thread", &[run("running", 1)], Some("run".into()), cx);
            feed.apply_native(Changes::default(), &[], cx);
            feed.set_timeline_runs("thread", &[run("interrupted", 1)], None, cx);
            feed.apply_native(Changes::default(), &[], cx);
        });
        visual.run_until_parked();
        draw(visual);
        assert!(
            feed.read_with(visual, |feed, _| feed
                .groups
                .folds
                .values()
                .next()
                .unwrap()
                .expanded),
            "in-session Stop stays open"
        );
        let fold = visual.debug_bounds("manager-fold-toggle").unwrap();
        visual.simulate_click(fold.center(), Modifiers::default());
        visual.run_until_parked();
        draw(visual);
        assert!(
            !feed.read_with(visual, |feed, _| feed
                .groups
                .folds
                .values()
                .next()
                .unwrap()
                .expanded),
            "a stopped fold can still be manually collapsed"
        );
    }

    #[test]
    fn classification_and_durations_keep_exact_boundaries_without_input_failure_guessing() {
        assert_eq!(
            classify(&json!({"type":"approval_request"})),
            plan::RowKind::Barrier(plan::Barrier::Request)
        );
        assert_eq!(
            classify(&json!({"type":"proposed_plan"})),
            plan::RowKind::Barrier(plan::Barrier::Plan)
        );
        assert_eq!(
            classify(&json!({"type":"future_tool"})),
            plan::RowKind::Barrier(plan::Barrier::Unknown)
        );
        let plan::RowKind::Work(work) = classify(
            &json!({"type":"command_execution","input":"echo failed","status":"completed"}),
        ) else {
            panic!("known work");
        };
        assert!(!work.failed());
        assert_eq!(duration_label(0), "1ms");
        assert_eq!(duration_label(1250), "1.3s");
        assert_eq!(duration_label(47_000), "47s");
        assert_eq!(duration_label(3_661_000), "1h 1m 1s");
        assert!(timestamp(&json!({"startedAt":"not a date"}), &["startedAt"]).is_none());
    }

    #[test]
    fn native_work_status_and_rendered_output_match_t3_presentation() {
        let work = |value: Value| {
            let plan::RowKind::Work(work) = classify(&value) else {
                panic!("known native work");
            };
            work
        };
        for status in ["pending", "running", "waiting", "inProgress"] {
            let fact = work(json!({"type":"command_execution", "status":status}));
            assert_eq!(fact.status, plan::WorkStatus::InProgress, "{status}");
            assert!(fact.neutral, "inactive work hides a neutral {status} row");
        }
        for status in ["cancelled", "interrupted", "stopped"] {
            let fact = work(json!({"type":"command_execution", "status":status}));
            assert_eq!(fact.status, plan::WorkStatus::Stopped, "{status}");
            assert!(fact.neutral);
        }
        let thinking = work(json!({"type":"reasoning", "status":"completed"}));
        assert!(thinking.tool_like && thinking.neutral);
        let failed_thinking = work(json!({"type":"reasoning", "status":"failed"}));
        assert!(failed_thinking.failed() && !failed_thinking.neutral);
        let success = work(json!({"type":"command_execution", "status":"completed"}));
        assert!(!success.neutral && !success.failed());
        for output in [
            "File not found",
            "ENOENT: open failed",
            "No such file or directory",
            "The term 'bad' is not recognized",
            "Cannot find path X because it does not exist",
            "<exited with exit code 2>",
            "Exited with exit code 19",
            "Exit code: 3",
        ] {
            let fact =
                work(json!({"type":"command_execution", "status":"completed", "output":output}));
            assert!(fact.failed() && !fact.neutral, "{output}");
            let command_only = work(
                json!({"type":"command_execution", "status":"completed", "input":output, "output":"Exit code: 0"}),
            );
            assert!(!command_only.failed(), "authored input: {output}");
        }
        assert!(work(json!({"type":"dynamic_tool", "status":"completed", "output":{"content":[{"type":"text", "text":"command not found"}]}})).failed());
    }

    #[test]
    fn native_user_started_at_and_explicit_message_streaming_are_retained() {
        // Audited v2 TurnItemBaseFields: startedAt/completedAt/updatedAt;
        // createdAt is not required on a user_message. Prefer it if supplied.
        let user = row("user", 0, "user_message");
        assert!(user.item.get("createdAt").is_none());
        let mut timeline = Timeline::default();
        timeline.retain(1, &user);
        assert_eq!(
            timeline.facts[&1].started_at,
            timestamp(&user.item, &["startedAt"])
        );
        assert!(timeline.facts[&1].started_at.is_some());
        let mut preferred = user.as_ref().clone();
        Arc::make_mut(&mut preferred.item)["createdAt"] = json!("2026-10-31T11:59:59Z");
        timeline.retain(1, &preferred);
        assert_eq!(
            timeline.facts[&1].started_at,
            timestamp(&preferred.item, &["createdAt"])
        );
        let mut assistant = row("assistant", 1, "assistant_message").as_ref().clone();
        Arc::make_mut(&mut assistant.item)["streaming"] = json!(true);
        timeline.retain(2, &assistant);
        assert!(
            timeline.streaming_message(2),
            "canonical streaming bool wins over a stale completed status"
        );
        let mut tool = row("tool", 2, "command_execution").as_ref().clone();
        Arc::make_mut(&mut tool.item)["status"] = json!("running");
        timeline.retain(3, &tool);
        assert!(
            !timeline.facts[&3].streaming,
            "tool lifecycle cannot permanently suppress a settled-response fold"
        );
    }
}
