//! Linux-only bounded rendering of the MCP projection. Data changes arrive by position.
use super::*;
use gpui_component::message_scroller::{MessageScroller, MessageScrollerState};
use okilum_t3::projection::{
    presentation::{self, Body},
    Changes as NativeChanges, RowKey,
};
use okilum_t3::TimelineItem;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, PartialEq)]
struct Message {
    user: bool,
    text: SharedString,
    preview: SharedString,
    kind: MessageKind,
}
#[derive(Clone, PartialEq)]
enum MessageKind {
    Chat,
    Reasoning,
    Compact {
        label: SharedString,
        collapsible: bool,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    Message(u64),
    Retained(usize),
}

/// A bounded plain-text hint; full retained detail is built only on disclosure.
fn activity_preview(text: &str) -> SharedString {
    let mut chars = text.lines().next().unwrap_or("").chars();
    let mut preview: String = chars.by_ref().take(120).collect();
    if chars.next().is_some() {
        preview.push('…');
    }
    preview.into()
}

pub(super) struct Feed {
    parent: WeakEntity<ProjectView>,
    messages: BTreeMap<u64, Message>,
    ids: BTreeMap<u64, String>,
    projected: HashMap<String, usize>,
    rows: Vec<Row>,
    indices: HashMap<u64, usize>,
    records: Vec<MessageRecord>,
    native_order: Option<Vec<u64>>,
    native_positions: HashMap<RowKey, u64>,
    expanded: HashSet<u64>,
    next_native_position: u64,
    scroller: Entity<MessageScrollerState>,
    enabled: bool,
    busy: bool,
    _scroll: Subscription,
    #[cfg(test)]
    pub(super) row_calls: std::cell::Cell<usize>,
    #[cfg(test)]
    pub(super) render_calls: std::cell::Cell<usize>,
}
impl Feed {
    pub(super) fn new(parent: WeakEntity<ProjectView>, cx: &mut Context<Self>) -> Self {
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
        let scroll = cx.observe(&scroller, |_, _, cx| cx.notify());
        Self {
            parent,
            messages: BTreeMap::new(),
            ids: BTreeMap::new(),
            projected: HashMap::new(),
            rows: Vec::new(),
            indices: HashMap::new(),
            records: Vec::new(),
            native_order: None,
            native_positions: HashMap::new(),
            expanded: HashSet::new(),
            next_native_position: 1,
            scroller,
            enabled: false,
            busy: false,
            _scroll: scroll,
            #[cfg(test)]
            row_calls: std::cell::Cell::new(0),
            #[cfg(test)]
            render_calls: std::cell::Cell::new(0),
        }
    }
    pub(super) fn reset(&mut self, cx: &mut Context<Self>) {
        self.messages.clear();
        self.ids.clear();
        self.projected.clear();
        self.rows.clear();
        self.indices.clear();
        self.records.clear();
        self.native_order = None;
        self.native_positions.clear();
        self.expanded.clear();
        self.next_native_position = 1;
        self.scroller.update(cx, |state, cx| state.reset(0, cx));
        cx.notify();
    }
    pub(super) fn set_controls(&mut self, enabled: bool, busy: bool, cx: &mut Context<Self>) {
        if (self.enabled, self.busy) != (enabled, busy) {
            self.enabled = enabled;
            self.busy = busy;
            cx.notify();
        }
    }
    pub(super) fn apply<'a>(
        &mut self,
        items: impl Iterator<Item = &'a TimelineItem>,
        records: Option<&[MessageRecord]>,
        cx: &mut Context<Self>,
    ) {
        // A late MCP result cannot replace an active native history.
        let native = self.native_order.is_some();
        self.apply_rows(items.filter(|_| !native), records, false, cx);
    }
    fn apply_rows<'a>(
        &mut self,
        items: impl Iterator<Item = &'a TimelineItem>,
        records: Option<&[MessageRecord]>,
        force_topology: bool,
        cx: &mut Context<Self>,
    ) {
        let mut topology = force_topology;
        let mut changed = Vec::new();
        for item in items {
            if self.ids.get(&item.position) != item.message_id.as_ref() {
                if let Some(old) = self.ids.remove(&item.position) {
                    if let Some(count) = self.projected.get_mut(&old) {
                        *count -= 1;
                        if *count == 0 {
                            self.projected.remove(&old);
                        }
                    }
                }
                if let Some(id) = &item.message_id {
                    self.ids.insert(item.position, id.clone());
                    *self.projected.entry(id.clone()).or_default() += 1;
                }
                topology = true;
            }
            let kind = match item.kind.as_str() {
                "user_message" | "assistant_message" => Some(MessageKind::Chat),
                "reasoning" => Some(MessageKind::Reasoning),
                "command_execution"
                | "file_change"
                | "file_search"
                | "web_search"
                | "approval_request"
                | "user_input_request"
                | "proposed_plan"
                | "todo_list"
                | "checkpoint"
                | "system_notice"
                | "error"
                | "notification"
                | "secret_request"
                | "run_interrupt_request"
                | "run_interrupt_result"
                | "compaction"
                | "handoff"
                | "fork"
                | "thread_created"
                | "subagent"
                | "dynamic_tool" => Some(MessageKind::Compact {
                    label: item
                        .title
                        .clone()
                        .unwrap_or_else(|| item.kind.replace('_', " "))
                        .into(),
                    // Questions, errors and notices stay visible without a click.
                    collapsible: matches!(
                        item.kind.as_str(),
                        "command_execution"
                            | "file_change"
                            | "file_search"
                            | "web_search"
                            | "dynamic_tool"
                    ),
                }),
                _ => None,
            };
            let next = kind.map(|kind| {
                let text: SharedString = item.text.clone().unwrap_or_default().into();
                let preview = if matches!(
                    kind,
                    MessageKind::Compact {
                        collapsible: true,
                        ..
                    }
                ) {
                    activity_preview(&text)
                } else {
                    SharedString::default()
                };
                Message {
                    user: item.kind == "user_message",
                    text,
                    preview,
                    kind,
                }
            });
            if self.messages.get(&item.position) != next.as_ref() {
                topology |= self.messages.contains_key(&item.position) != next.is_some();
                let hidden_detail_only = self
                    .messages
                    .get(&item.position)
                    .zip(next.as_ref())
                    .is_some_and(|(old, next)| {
                        old.kind == next.kind
                            && old.preview == next.preview
                            && old.user == next.user
                            && !self.expanded.contains(&item.position)
                            && matches!(
                                next.kind,
                                MessageKind::Reasoning
                                    | MessageKind::Compact {
                                        collapsible: true,
                                        ..
                                    }
                            )
                    });
                if let Some(next) = next {
                    self.messages.insert(item.position, next);
                } else {
                    self.messages.remove(&item.position);
                }
                if !hidden_detail_only {
                    changed.push(item.position);
                }
            }
        }
        let mut records_changed = false;
        if let Some(records) = records.filter(|records| *records != self.records.as_slice()) {
            self.records = records.to_vec();
            topology = true;
            records_changed = true;
        }
        let mut inserted = 0..0;
        if topology {
            let mut rows: Vec<_> = self
                .native_order
                .as_ref()
                .map(|order| order.iter().copied().map(Row::Message).collect())
                .unwrap_or_else(|| self.messages.keys().copied().map(Row::Message).collect());
            rows.extend(
                self.records
                    .iter()
                    .enumerate()
                    .filter(|(_, record)| {
                        !record
                            .receipt
                            .as_ref()
                            .is_some_and(|receipt| self.projected.contains_key(&receipt.message_id))
                    })
                    .map(|(index, _)| Row::Retained(index)),
            );
            if rows != self.rows {
                let prefix = self
                    .rows
                    .iter()
                    .zip(&rows)
                    .take_while(|(a, b)| a == b)
                    .count();
                let suffix = self
                    .rows
                    .iter()
                    .rev()
                    .zip(rows.iter().rev())
                    .take(self.rows.len().min(rows.len()) - prefix)
                    .take_while(|(a, b)| a == b)
                    .count();
                self.scroller.update(cx, |state, cx| {
                    state.splice(
                        prefix..self.rows.len() - suffix,
                        rows.len() - prefix - suffix,
                        cx,
                    );
                });
                inserted = prefix..rows.len() - suffix;
                self.rows = rows;
                self.indices = self
                    .rows
                    .iter()
                    .enumerate()
                    .filter_map(|(index, row)| match row {
                        Row::Message(position) => Some((*position, index)),
                        Row::Retained(_) => None,
                    })
                    .collect();
            }
        }
        let mut remeasure: Vec<usize> = changed
            .iter()
            .filter_map(|position| self.indices.get(position).copied())
            .collect();
        if records_changed {
            remeasure.extend(
                self.rows
                    .iter()
                    .enumerate()
                    .filter_map(|(index, row)| matches!(row, Row::Retained(_)).then_some(index)),
            );
        }
        remeasure.retain(|index| !inserted.contains(index));
        remeasure.sort_unstable();
        remeasure.dedup();
        if !remeasure.is_empty() {
            self.scroller.update(cx, |state, cx| {
                let mut iter = remeasure.into_iter().peekable();
                while let Some(start) = iter.next() {
                    let mut end = start + 1;
                    while iter.peek() == Some(&end) {
                        iter.next();
                        end += 1;
                    }
                    state.remeasure_items(start..end, cx);
                }
            });
        }
        if topology || !changed.is_empty() {
            cx.notify();
        }
    }
    fn row(&self, index: usize, cx: &mut App) -> AnyElement {
        #[cfg(test)]
        self.row_calls.set(self.row_calls.get() + 1);
        match self.rows.get(index) {
            Some(Row::Message(position)) => {
                let message = &self.messages[position];
                let position = *position;
                if message.kind != MessageKind::Chat {
                    return self.activity(position, message, cx);
                }
                v_flex()
                    .id(SharedString::from(format!("project-message-{position}")))
                    .debug_selector(move || format!("project-message-{position}"))
                    .gap_2()
                    .mb_3()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(brand::palette(cx).border_subtle)
                    .bg(if message.user {
                        brand::palette(cx).selected
                    } else {
                        brand::palette(cx).surface
                    })
                    .when(message.user, |row| row.ml_6())
                    .when(!message.user, |row| row.mr_6())
                    .child(
                        div()
                            .text_color(brand::palette(cx).text_muted)
                            .child(if message.user { "You" } else { "Manager" }),
                    )
                    .child(
                        TextView::markdown(
                            ("project-message", position as usize),
                            message.text.clone(),
                        )
                        .selectable(true)
                        .style(reader_text_style(cx.theme())),
                    )
                    .into_any_element()
            }
            Some(Row::Retained(index)) => {
                let record = &self.records[*index];
                let retry = record.clone();
                let parent = self.parent.clone();
                let index = *index;
                v_flex()
                    .id(SharedString::from(format!(
                        "project-retained-{}",
                        record.message.client_request_id
                    )))
                    .gap_1()
                    .child(div().text_color(brand::palette(cx).text_muted).child(
                        if record.receipt.is_some() {
                            "You · queued"
                        } else {
                            "Message delivery unconfirmed"
                        },
                    ))
                    .when(record.receipt.is_some(), |row| {
                        row.child(
                            TextView::markdown(
                                ("project-retained", index),
                                record.message.text.clone(),
                            )
                            .selectable(true)
                            .style(reader_text_style(cx.theme())),
                        )
                    })
                    .when(record.receipt.is_none(), |row| {
                        row.child(
                            Button::new(("project-retry", index))
                                .debug_selector(move || format!("project-retry-{index}"))
                                .ghost()
                                .icon(IconName::RotateCw)
                                .tooltip(format!("Retry retained message: {}", record.message.text))
                                .disabled(self.busy || !self.enabled)
                                .on_click(move |_, window, cx| {
                                    let _ = parent.update(cx, |this, cx| {
                                        this.send(Some(retry.clone()), window, cx)
                                    });
                                }),
                        )
                    })
                    .into_any_element()
            }
            None => div().into_any_element(),
        }
    }
}
impl Render for Feed {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        self.render_calls.set(self.render_calls.get() + 1);
        let entity = cx.entity().clone();
        let scroller = MessageScroller::new(
            "project-messages",
            self.scroller.clone(),
            move |index, _, cx| {
                let row = entity.update(cx, |feed, cx| feed.row(index, cx));
                if index != 0 {
                    return row;
                }
                let parent = entity.read(cx).parent.clone();
                div()
                    .w_full()
                    .child(row)
                    .on_children_prepainted(move |bounds, window, cx| {
                        // A virtual list also renders offscreen rows to measure
                        // them. Fetch only when the oldest row is actually visible.
                        let viewport = window.content_mask().bounds;
                        if bounds.iter().any(|row| row.intersects(&viewport)) {
                            let parent = parent.clone();
                            window.defer(cx, move |window, cx| {
                                let _ = parent.update(cx, |view, cx| {
                                    view.prefetch_native_history(window, cx)
                                });
                            });
                        }
                    })
                    .into_any_element()
            },
        )
        .with_jump_button_renderer(|button| button.debug_selector(|| "project-jump-latest".into()))
        .size_full()
        .min_h_0();
        div()
            .debug_selector(|| "project-history".into())
            .size_full()
            .min_h_0()
            .child(scroller)
    }
}

mod native;
#[cfg(test)]
mod tests;
