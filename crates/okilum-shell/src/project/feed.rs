//! Linux-only bounded rendering of the MCP projection. Data changes arrive by position.
use super::*;
use gpui_component::message_scroller::{MessageScroller, MessageScrollerState};
use okilum_t3::TimelineItem;
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, PartialEq)]
struct Message {
    user: bool,
    text: SharedString,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    Message(u64),
    Retained(usize),
}

pub(super) struct Feed {
    parent: WeakEntity<ProjectView>,
    messages: BTreeMap<u64, Message>,
    ids: BTreeMap<u64, String>,
    projected: HashMap<String, usize>,
    rows: Vec<Row>,
    indices: HashMap<u64, usize>,
    records: Vec<MessageRecord>,
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
        let mut topology = false;
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
            let next = item
                .text
                .as_ref()
                .filter(|_| matches!(item.kind.as_str(), "user_message" | "assistant_message"));
            let same = match (self.messages.get(&item.position), next) {
                (Some(old), Some(text)) => {
                    old.user == (item.kind == "user_message") && old.text.as_ref() == text.as_str()
                }
                (None, None) => true,
                _ => false,
            };
            if !same {
                topology |= self.messages.contains_key(&item.position) != next.is_some();
                if let Some(text) = next {
                    self.messages.insert(
                        item.position,
                        Message {
                            user: item.kind == "user_message",
                            text: text.clone().into(),
                        },
                    );
                } else {
                    self.messages.remove(&item.position);
                }
                changed.push(item.position);
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
            let mut rows: Vec<_> = self.messages.keys().copied().map(Row::Message).collect();
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
                v_flex()
                    .id(SharedString::from(format!("project-message-{position}")))
                    .debug_selector(move || format!("project-message-{position}"))
                    .gap_1()
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
            move |index, _, cx| entity.update(cx, |feed, cx| feed.row(index, cx)),
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

#[cfg(test)]
mod tests;
