//! Native bounded rendering of the MCP projection. Data changes arrive by position.
use super::*;
use crate::project::design;
use gpui_component::message_scroller::{MessageScroller, MessageScrollerState};
use okilum_t3::projection::{
    presentation::{self, Body},
    Changes as NativeChanges, RowKey,
};
use okilum_t3::TimelineItem;
use std::collections::{BTreeMap, HashMap, HashSet};
mod attachments;
pub(super) mod content;
mod details;
mod groups;
mod message_actions;
mod rich_cache;
mod viewport_paint;

#[derive(Clone, PartialEq)]
struct Message {
    source_id: String,
    user: bool,
    text: SharedString,
    status: SharedString,
    run_id: Option<String>,
    complete_text: bool,
    nonblank_text: bool,
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
    WorkSummary(okilum_t3::projection::timeline::GroupKey),
    WorkDetails(okilum_t3::projection::timeline::GroupKey),
    Fold(okilum_t3::projection::timeline::RunKey),
}

/// A bounded plain-text hint; full retained detail is built only on disclosure.
fn activity_preview(text: &str) -> SharedString {
    // Bound work as well as visible length, even for a multi-megabyte line.
    let mut chars = text.chars().take_while(|c| !matches!(c, '\n' | '\r'));
    let mut preview: String = chars.by_ref().take(120).collect();
    if chars.next().is_some() {
        preview.push('…');
    }
    preview.into()
}

pub(super) struct Feed {
    parent: WeakEntity<ProjectView>,
    pub(super) actions: Option<Entity<super::native::actions::Actions>>,
    request_cards: HashMap<u64, Entity<super::native::actions::RequestCard>>,
    messages: BTreeMap<u64, Message>,
    ids: BTreeMap<u64, String>,
    projected: HashMap<String, usize>,
    rows: Vec<Row>,
    canonical_rows: Vec<u64>,
    groups: groups::Timeline,
    indices: HashMap<u64, usize>,
    records: Vec<MessageRecord>,
    native_order: Option<Vec<u64>>,
    native_positions: HashMap<RowKey, u64>,
    expanded: HashSet<u64>,
    details: HashMap<u64, details::Detail>,
    attachments: attachments::History,
    copy_terminals: HashSet<u64>,
    copy_active_runs: HashSet<String>,
    copy_response_runs: HashSet<String>,
    copy_lease: Arc<()>,
    next_native_position: u64,
    scroller: Entity<MessageScrollerState>,
    rich: std::cell::RefCell<rich_cache::Cache>,
    enabled: bool,
    busy: bool,
    typed_revision: u64,
    typed_sources: Option<Arc<Vec<super::message_types::Source>>>,
    typed_source_owner: Option<(String, String, String)>,
    _scroll: Subscription,
    #[cfg(test)]
    pub(super) row_calls: std::cell::Cell<usize>,
    #[cfg(test)]
    pub(super) rendered_positions: std::cell::RefCell<Vec<u64>>,
    #[cfg(test)]
    pub(super) render_calls: std::cell::Cell<usize>,
}
impl Feed {
    pub(super) fn new(parent: WeakEntity<ProjectView>, cx: &mut Context<Self>) -> Self {
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
        let scroll = cx.observe(&scroller, |this, _, cx| {
            this.prewarm_markdown(cx);
            cx.notify();
        });
        Self {
            parent,
            actions: None,
            request_cards: HashMap::new(),
            messages: BTreeMap::new(),
            ids: BTreeMap::new(),
            projected: HashMap::new(),
            rows: Vec::new(),
            canonical_rows: Vec::new(),
            groups: Default::default(),
            indices: HashMap::new(),
            records: Vec::new(),
            native_order: None,
            native_positions: HashMap::new(),
            expanded: HashSet::new(),
            details: HashMap::new(),
            attachments: Default::default(),
            copy_terminals: HashSet::new(),
            copy_active_runs: HashSet::new(),
            copy_response_runs: HashSet::new(),
            copy_lease: Arc::new(()),
            next_native_position: 1,
            scroller,
            rich: Default::default(),
            enabled: false,
            busy: false,
            typed_revision: 0,
            typed_sources: None,
            typed_source_owner: None,
            _scroll: scroll,
            #[cfg(test)]
            row_calls: std::cell::Cell::new(0),
            #[cfg(test)]
            rendered_positions: std::cell::RefCell::new(Vec::new()),
            #[cfg(test)]
            render_calls: std::cell::Cell::new(0),
        }
    }
    #[cfg(test)]
    pub(in crate::project) fn performance_scroll_position(&self, cx: &App) -> usize {
        self.scroller.read(cx).logical_scroll_top().item_ix
    }
    #[cfg(test)]
    pub(in crate::project) fn test_scroll_anchor(
        &self,
        cx: &App,
    ) -> Option<(RowKey, usize, Pixels)> {
        let offset = self.scroller.read(cx).logical_scroll_top();
        let Row::Message(position) = self.rows.get(offset.item_ix)? else {
            return None;
        };
        self.native_positions.iter().find_map(|(key, member)| {
            (member == position).then(|| (key.clone(), offset.item_ix, offset.offset_in_item))
        })
    }
    /// Sent user prompts in chronological projection order. Native positions
    /// are allocation slots, not chronology; their RowKey element IDs survive
    /// pagination and updates independently of optional messageId receipts.
    pub(super) fn prompt_history(&self) -> Vec<super::composer::history::Entry> {
        let ordered: Vec<_> = self.native_order.as_ref().map_or_else(
            || self.messages.keys().copied().collect(),
            |order| order.clone(),
        );
        super::composer::history::build_entries(ordered.into_iter().filter_map(|position| {
            let message = self.messages.get(&position)?;
            (message.user && matches!(message.kind, MessageKind::Chat)).then_some(
                super::composer::history::Message {
                    id: &message.source_id,
                    role: "user",
                    text: message.text.as_str(),
                },
            )
        }))
    }

    pub(super) fn history_has_requests(&self, cx: &App) -> bool {
        self.actions
            .as_ref()
            .is_some_and(|actions| !actions.read(cx).state.requests.is_empty())
    }

    pub(super) fn typed_revision(&self) -> u64 {
        self.typed_revision
    }

    fn typed_data_changed(&mut self) {
        self.typed_revision = self
            .typed_revision
            .checked_add(1)
            .expect("typed source revision exhausted");
        self.typed_sources = None;
    }

    /// Full source and role, never prompt-recall normalization. Rebuilt only
    /// after data publication, not scrolling, paint or a disclosure change.
    pub(super) fn typed_snapshot(
        &mut self,
        scope: &super::composer::TypedScope,
    ) -> Arc<Vec<super::message_types::Source>> {
        let owner = (
            scope.environment.clone(),
            scope.project.clone(),
            scope.thread.clone(),
        );
        if self.typed_source_owner.as_ref() != Some(&owner) {
            self.typed_sources = None;
            self.typed_source_owner = Some(owner);
        }
        if let Some(sources) = &self.typed_sources {
            return sources.clone();
        }
        use okilum_t3::message_types::Author;
        let order = self
            .native_order
            .clone()
            .unwrap_or_else(|| self.messages.keys().copied().collect());
        let mut sources = Vec::new();
        let native_owners = self
            .native_positions
            .iter()
            .map(|(key, position)| (*position, key))
            .collect::<HashMap<_, _>>();
        for position in order {
            if self.native_order.is_some()
                && native_owners.get(&position).map(|key| key.thread.as_str())
                    != Some(scope.thread.as_str())
            {
                continue;
            }
            let Some(message) = self
                .messages
                .get(&position)
                .filter(|message| message.kind == MessageKind::Chat && message.complete_text)
            else {
                continue;
            };
            let mut aliases = self
                .ids
                .get(&position)
                .cloned()
                .into_iter()
                .collect::<Vec<_>>();
            if message.user {
                // Native send allocates the message ID before publication.
                // RowKey retains that factual ID; element_id is only a UI key.
                if let Some(key) = native_owners.get(&position) {
                    aliases.push(key.item.clone());
                }
            }
            for record in &self.records {
                if record.message.thread_id == scope.thread
                    && record.message.text == message.text.as_str()
                    && record.receipt.as_ref().is_some_and(|receipt| {
                        receipt.thread_id == scope.thread && aliases.contains(&receipt.message_id)
                    })
                {
                    aliases.push(record.message.client_request_id.clone());
                }
            }
            sources.push(super::message_types::Source {
                id: message.source_id.clone(),
                text: message.text.to_string(),
                author: if message.user {
                    Author::User
                } else {
                    Author::Manager
                },
                aliases,
            });
        }
        for record in &self.records {
            if record.message.thread_id != scope.thread {
                continue;
            }
            if record
                .receipt
                .as_ref()
                .is_some_and(|receipt| self.projected.contains_key(&receipt.message_id))
            {
                continue;
            }
            sources.push(super::message_types::Source {
                id: format!("project-retained-{}", record.message.client_request_id),
                text: record.message.text.clone(),
                author: Author::User,
                aliases: vec![record.message.client_request_id.clone()],
            });
        }
        let sources = Arc::new(sources);
        self.typed_sources = Some(sources.clone());
        sources
    }

    pub(super) fn typed_labels_changed(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }

    /// The caller revalidates its scoped source immediately before this jump;
    /// an absent or ambiguous ID never redirects to a neighboring row.
    pub(super) fn reveal_typed_source(
        &mut self,
        scope: &super::composer::TypedScope,
        expected: &super::message_types::Source,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(parent) = self.parent.upgrade() else {
            return false;
        };
        if parent.read(cx).typed_scope().as_ref() != Some(scope) {
            return false;
        }
        let sources = self.typed_snapshot(scope);
        let mut matches = sources.iter().filter(|source| source.id == expected.id);
        if matches.next() != Some(expected) || matches.next().is_some() {
            return false;
        }
        let mut found = self.rows.iter().enumerate().filter_map(|(index, row)| {
            let id = match row {
                Row::Message(position) => self.messages.get(position)?.source_id.clone(),
                Row::Retained(record) => format!(
                    "project-retained-{}",
                    self.records.get(*record)?.message.client_request_id
                ),
                _ => return None,
            };
            (id == expected.id).then_some(index)
        });
        let Some(index) = found.next() else {
            return false;
        };
        if found.next().is_some() {
            return false;
        }
        self.scroller
            .update(cx, |state, cx| state.scroll_to_item(index, cx))
    }

    fn typed_chip(&self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let Some(parent) = self.parent.upgrade() else {
            return div().into_any_element();
        };
        let (scope, composer) = {
            let parent = parent.read(cx);
            (parent.typed_scope(), parent.composer.clone())
        };
        let Some(scope) = scope else {
            return div().into_any_element();
        };
        let key = okilum_t3::message_types::MessageKey {
            environment: scope.environment,
            project: scope.project,
            thread: scope.thread,
            message: id.into(),
        };
        composer.update(cx, |composer, cx| composer.render_typed_chat_chip(&key, cx))
    }

    pub(super) fn reset(&mut self, cx: &mut Context<Self>) {
        self.rich.borrow_mut().clear();
        self.typed_data_changed();
        self.request_cards.clear();
        self.messages.clear();
        self.ids.clear();
        self.projected.clear();
        self.rows.clear();
        self.canonical_rows.clear();
        self.groups = Default::default();
        self.indices.clear();
        self.records.clear();
        self.native_order = None;
        self.native_positions.clear();
        self.expanded.clear();
        self.details.clear();
        self.attachments = Default::default();
        self.copy_terminals.clear();
        self.copy_active_runs.clear();
        self.copy_response_runs.clear();
        self.copy_lease = Arc::new(());
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
        self.apply_rows(items.filter(|_| !native), records, false, None, cx);
    }
    fn apply_rows<'a>(
        &mut self,
        items: impl Iterator<Item = &'a TimelineItem>,
        records: Option<&[MessageRecord]>,
        force_topology: bool,
        previews: Option<&HashMap<u64, SharedString>>,
        cx: &mut Context<Self>,
    ) {
        let mut typed_changed = force_topology;
        let mut topology = force_topology || self.groups.dirty;
        let mut changed = Vec::new();
        let mut copy_groups_changed = topology;
        for item in items {
            if self.ids.get(&item.position) != item.message_id.as_ref() {
                typed_changed = true;
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
                    previews
                        .and_then(|previews| previews.get(&item.position))
                        .cloned()
                        .unwrap_or_else(|| activity_preview(&text))
                } else {
                    SharedString::default()
                };
                Message {
                    source_id: item.item_id.clone(),
                    user: item.kind == "user_message",
                    nonblank_text: !text.trim().is_empty(),
                    text,
                    status: item.status.clone().into(),
                    run_id: item.run_id.clone(),
                    complete_text: !item.text_truncated && item.next_text_offset.is_none(),
                    preview,
                    kind,
                }
            });
            if self.messages.get(&item.position) != next.as_ref() {
                let exact = |message: &Message| {
                    (
                        message.source_id.clone(),
                        message.text.clone(),
                        message.user,
                        message.complete_text,
                        message.kind == MessageKind::Chat,
                    )
                };
                typed_changed |=
                    self.messages.get(&item.position).map(exact) != next.as_ref().map(exact);
                copy_groups_changed |= self
                    .messages
                    .get(&item.position)
                    .map(|message| (&message.kind, message.user, &message.run_id))
                    != next
                        .as_ref()
                        .map(|message| (&message.kind, message.user, &message.run_id));
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
            typed_changed = true;
        }
        if typed_changed {
            self.typed_data_changed();
        }
        let mut inserted = 0..0;
        if topology {
            let mut rows: Vec<_> = self
                .native_order
                .as_ref()
                .map(|order| order.iter().copied().map(Row::Message).collect())
                .unwrap_or_else(|| self.messages.keys().copied().map(Row::Message).collect());
            self.canonical_rows = rows
                .iter()
                .filter_map(|row| match row {
                    Row::Message(position) => Some(*position),
                    _ => None,
                })
                .collect();
            rows = self.grouped_rows(rows, cx);
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
                        _ => None,
                    })
                    .collect();
            }
        }
        if copy_groups_changed {
            changed.extend(self.refresh_copy_terminals());
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
        for position in &changed {
            self.group_member_changed(*position, cx);
        }
        if topology || !changed.is_empty() {
            cx.notify();
        }
    }
    fn row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(all(target_os = "linux", feature = "project-ui-harness"))]
        let _work = super::performance::Work::start(2);
        #[cfg(test)]
        self.row_calls.set(self.row_calls.get() + 1);
        match self.rows.get(index) {
            Some(Row::Message(position)) => {
                #[cfg(test)]
                self.rendered_positions.borrow_mut().push(*position);
                let message = &self.messages[position];
                let position = *position;
                if message.kind != MessageKind::Chat {
                    return self.activity(position, message, cx);
                }
                let colors = brand::palette(cx);
                let heading = h_flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .min_h(px(28.))
                    .gap(px(design::GAP))
                    .debug_selector(move || format!("project-message-header-{position}"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(design::LABEL))
                            .line_height(px(design::LINE))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors.text_muted)
                            .child(if message.user { "You" } else { "Manager" }),
                    )
                    .child(self.typed_chip(&message.source_id, cx))
                    .when(self.copy_text(position).is_some(), |row| {
                        row.child(message_actions::MessageActions {
                            feed: cx.weak_entity(),
                            position,
                            lease: self.copy_lease.clone(),
                            user: message.user,
                            source_id: message.source_id.clone(),
                        })
                    });
                // One reading column and quiet role chrome carry the hierarchy.
                // The user prompt has a distinct accent surface; assistant prose
                // stays directly on the shared conversation surface.
                // The scroller has already resolved the reading width. Block
                // flow avoids asking the complete rich message for flex
                // intrinsic sizes again as the splitter changes that width.
                let body = div()
                    .w_full()
                    .min_w_0()
                    .max_w(px(if message.user { 640. } else { 720. }))
                    .debug_selector(move || format!("project-message-surface-{position}"))
                    .text_color(colors.text)
                    .when(message.user, |row| {
                        row.p(px(design::INSET))
                            .rounded(px(design::RADIUS))
                            .bg(colors.accent.opacity(0.08))
                    })
                    .when(!message.user, |row| row.py(px(design::GAP)))
                    .child(heading)
                    .child(
                        self.markdown(
                            ("project-message", position as usize),
                            message.text.clone(),
                            cx,
                        )
                        .mt(px(design::GAP * 0.5)),
                    )
                    .child(
                        self.render_attachments(position, cx)
                            .mt(px(design::GAP * 0.5)),
                    );
                div()
                    .id(SharedString::from(format!("project-message-{position}")))
                    .debug_selector(move || format!("project-message-{position}"))
                    .w_full()
                    .min_w_0()
                    .px(px(design::INSET))
                    .mb(px(design::GAP * 2.5))
                    .when(message.user, |row| row.pl(px(design::INSET * 2.)))
                    .child(body.when(message.user, |body| body.ml_auto()))
                    .into_any_element()
            }
            Some(Row::WorkSummary(key)) => self.work_summary(key, cx),
            Some(Row::WorkDetails(key)) => self.work_details(key, cx),
            Some(Row::Fold(key)) => self.fold_summary(key, cx),
            Some(Row::Retained(index)) => {
                let record = &self.records[*index];
                let retry = record.clone();
                let parent = self.parent.clone();
                let index = *index;
                let chip = self.typed_chip(
                    &format!("project-retained-{}", record.message.client_request_id),
                    cx,
                );
                v_flex()
                    .id(SharedString::from(format!(
                        "project-retained-{}",
                        record.message.client_request_id
                    )))
                    .child(chip)
                    .px(px(design::INSET))
                    .py(px(design::GAP))
                    .mb(px(design::GAP * 2.))
                    .gap(px(design::GAP))
                    .child(
                        div()
                            .text_size(px(design::LABEL))
                            .line_height(px(design::LINE))
                            .text_color(brand::palette(cx).text_muted)
                            .child(if record.receipt.is_some() {
                                "You · queued"
                            } else {
                                "Message delivery unconfirmed"
                            }),
                    )
                    .when(record.receipt.is_some(), |row| {
                        row.child(self.markdown(
                            ("project-retained", index),
                            record.message.text.clone(),
                            cx,
                        ))
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
                let row = viewport_paint::RowPaint::new(row).into_any_element();
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
        let root = div()
            .debug_selector(|| "project-history".into())
            .bg(brand::palette(cx).surface)
            .size_full()
            .min_h_0()
            .when(self.groups.error, |view| {
                view.child(
                    div()
                        .text_color(cx.theme().danger)
                        .child("Activity grouping is unavailable. Showing individual entries."),
                )
            })
            .child(scroller)
            .into_any_element();
        #[cfg(all(target_os = "linux", feature = "project-ui-harness"))]
        let root = super::performance::Trace::new(root, 1).into_any_element();
        root
    }
}

mod native;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod typed_1478_tests;

#[cfg(test)]
mod design_layout_tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn concept_message_surfaces_keep_copy_reachable_inside_narrow_reading_columns(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::test_init);
        let temp = tempfile::tempdir().unwrap();
        let owner = std::rc::Rc::new(std::cell::RefCell::new(None));
        let retained_owner = owner.clone();
        let authored = "Original **prompt** with retained Markdown.";
        let (root, visual) = cx.add_window_view(|window, cx| {
            let parent = cx.new(|cx| ProjectView::empty(Some(temp.path().to_owned()), window, cx));
            let feed = parent.read(cx).feed.clone();
            let items = [
                serde_json::from_value::<TimelineItem>(serde_json::json!({
                    "position":1,"itemId":"design-response","type":"assistant_message",
                    "status":"completed","text":"## Clear next step\n\nA readable reply with **emphasis** and a [link](https://example.org).","updatedAt":"now"
                })).unwrap(),
                serde_json::from_value::<TimelineItem>(serde_json::json!({
                    "position":2,"itemId":"design-prompt","type":"user_message",
                    "status":"completed","text":authored,"updatedAt":"now"
                })).unwrap(),
            ];
            feed.update(cx, |feed, cx| feed.apply(items.iter(), None, cx));
            *retained_owner.borrow_mut() = Some(parent);
            Root::new(feed, window, cx)
        });
        for width in [760., 320.] {
            visual.simulate_resize(size(px(width), px(600.)));
            visual.run_until_parked();
            let history = visual
                .debug_bounds("project-history")
                .expect("actual history is mounted");
            for (surface_selector, heading_selector, copy_selector) in [
                (
                    "project-message-surface-1",
                    "project-message-header-1",
                    "project-message-copy-1",
                ),
                (
                    "project-message-surface-2",
                    "project-message-header-2",
                    "project-message-copy-2",
                ),
            ] {
                let surface = visual
                    .debug_bounds(surface_selector)
                    .expect("actual message surface is painted");
                let heading = visual
                    .debug_bounds(heading_selector)
                    .expect("role and copy share a stable header");
                let copy = visual
                    .debug_bounds(copy_selector)
                    .expect("whole-message copy remains reachable");
                assert!(surface.left() >= history.left() && surface.right() <= history.right(),
                    "message surface cannot escape its reading column: surface={surface:?}, history={history:?}");
                assert!(copy.left() >= surface.left() && copy.right() <= surface.right());
                assert!(copy.top() >= heading.top() && copy.bottom() <= heading.bottom());
                assert!(
                    heading.bottom() < surface.bottom(),
                    "header must leave space for the actual prose"
                );
            }
            let user = visual.debug_bounds("project-message-surface-2").unwrap();
            let assistant = visual.debug_bounds("project-message-surface-1").unwrap();
            assert!(
                user.left() > assistant.left(),
                "the quiet user surface has distinct alignment"
            );
        }
        let copy = visual.debug_bounds("project-message-copy-2").unwrap();
        visual.simulate_click(copy.center(), Modifiers::default());
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some(authored)
            )
        });
        drop(root);
        drop(owner);
    }
}
