//! Retained Tier 1 thread projection for the native Linux Manager (#1787).
//! Visibility/history semantics follow T3's orchestrationV2Projection,
//! orchestrationV2Timeline and threadHistoryMerge (upstream 71a283aed2).
//! The model owns history once. A text flush replaces one Arc-backed row;
//! the UI drains coalesced changes instead of cloning the conversation.
use crate::stream::{StreamError, StreamResult, ThreadUpdate};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub mod presentation;
#[cfg(test)]
mod tests;
mod visibility;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RowKey {
    pub thread: String,
    pub item: String,
}
impl RowKey {
    pub fn element_id(&self) -> String {
        format!("native-{}:{}:{}", self.thread.len(), self.thread, self.item)
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct TurnRow {
    pub key: RowKey,
    pub local: bool,
    pub item: Arc<Value>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    pub title: String,
    pub deleted: bool,
    pub history_cursor: Option<String>,
    pub has_more_history: bool,
    pub payload_budget_exceeded: bool,
    pub latest_local_turn_ordinal: Option<u64>,
}
#[derive(Default)]
pub struct Changes {
    pub rows: Vec<Arc<TurnRow>>,
    pub removed: Vec<RowKey>,
    pub order: Option<Vec<RowKey>>,
    pub reset: bool,
    pub metadata: Option<Metadata>,
    pub actions: Option<crate::actions::State>,
}
impl Changes {
    pub fn changed(&self) -> bool {
        self.reset
            || self.order.is_some()
            || self.metadata.is_some()
            || self.actions.is_some()
            || !self.rows.is_empty()
            || !self.removed.is_empty()
    }
}

pub struct Projection {
    thread: String,
    project: String,
    pub metadata: Metadata,
    rows: HashMap<RowKey, Arc<TurnRow>>,
    order: Vec<RowKey>,
    pub(crate) items: HashMap<String, Arc<Value>>,
    pub(crate) runs: HashMap<String, Value>,
    pub(crate) requests: HashMap<String, Value>,
    pub(crate) provider_threads: HashMap<String, Value>,
    pub(crate) provider_sessions: HashMap<String, Value>,
    pub(crate) actions_ready: bool,
    actions_dirty: bool,
    attempts: HashMap<String, Value>,
    dirty: HashSet<RowKey>,
    order_dirty: bool,
    metadata_dirty: bool,
    reset: bool,
    epoch: u64,
}

enum Operation {
    Snapshot(Box<Projection>),
    Event(String, Value),
}
fn invalid() -> StreamError {
    StreamError::Protocol("invalid native thread projection")
}
fn string<'a>(v: &'a Value, field: &str) -> StreamResult<&'a str> {
    v[field]
        .as_str()
        .filter(|x| !x.is_empty())
        .ok_or_else(invalid)
}
fn ordinal(v: &Value) -> StreamResult<u64> {
    v["ordinal"].as_u64().ok_or_else(invalid)
}
fn key(v: &Value) -> StreamResult<RowKey> {
    Ok(RowKey {
        thread: string(v, "threadId")?.into(),
        item: string(v, "id")?.into(),
    })
}
fn known(kind: &str) -> bool {
    matches!(
        kind,
        "user_message"
            | "assistant_message"
            | "reasoning"
            | "proposed_plan"
            | "todo_list"
            | "command_execution"
            | "file_change"
            | "file_search"
            | "web_search"
            | "approval_request"
            | "user_input_request"
            | "checkpoint"
            | "run_interrupt_request"
            | "run_interrupt_result"
            | "compaction"
            | "handoff"
            | "fork"
            | "thread_created"
            | "subagent"
            | "dynamic_tool"
            | "system_notice"
            | "error"
            | "notification"
            | "secret_request"
    )
}
fn item(v: &Value) -> StreamResult<Option<Arc<Value>>> {
    let kind = string(v, "type")?;
    if !known(kind) {
        return Ok(None);
    }
    key(v)?;
    ordinal(v)?;
    if matches!(kind, "user_message" | "assistant_message" | "reasoning") && !v["text"].is_string()
    {
        return Err(invalid());
    }
    Ok(Some(Arc::new(v.clone())))
}
fn entities(v: &Value) -> StreamResult<HashMap<String, Value>> {
    let mut map = HashMap::new();
    for entity in v.as_array().ok_or_else(invalid)? {
        let id = string(entity, "id")?;
        if map.insert(id.into(), entity.clone()).is_some() {
            return Err(invalid());
        }
    }
    Ok(map)
}

impl Projection {
    pub fn new(thread: impl Into<String>, project: impl Into<String>) -> Self {
        Self {
            thread: thread.into(),
            project: project.into(),
            metadata: Metadata::default(),
            rows: HashMap::new(),
            order: Vec::new(),
            items: HashMap::new(),
            runs: HashMap::new(),
            requests: HashMap::new(),
            provider_threads: HashMap::new(),
            provider_sessions: HashMap::new(),
            actions_ready: false,
            actions_dirty: false,
            attempts: HashMap::new(),
            dirty: HashSet::new(),
            order_dirty: false,
            metadata_dirty: false,
            reset: false,
            epoch: 0,
        }
    }
    pub fn row(&self, key: &RowKey) -> Option<&Arc<TurnRow>> {
        self.rows.get(key)
    }
    pub fn keys(&self) -> &[RowKey] {
        &self.order
    }
    pub fn take_changes(&mut self) -> Changes {
        let mut changes = Changes {
            reset: self.reset,
            actions: self.actions_dirty.then(|| self.action_state()),
            order: self.order_dirty.then(|| self.order.clone()),
            metadata: self.metadata_dirty.then(|| self.metadata.clone()),
            ..Default::default()
        };
        if self.reset {
            changes.rows = self.rows.values().cloned().collect();
            self.dirty.clear();
        } else {
            for key in self.dirty.drain() {
                match self.rows.get(&key) {
                    Some(row) => changes.rows.push(row.clone()),
                    None => changes.removed.push(key),
                }
            }
        }
        self.reset = false;
        self.order_dirty = false;
        self.metadata_dirty = false;
        self.actions_dirty = false;
        changes
    }
    /// Prepare the entire chunk before mutating the retained model. A malformed
    /// known item or changed binding cannot partly commit an otherwise valid batch.
    pub fn apply(&mut self, updates: &[ThreadUpdate]) -> StreamResult<()> {
        let mut operations = Vec::new();
        for update in updates {
            match update {
                ThreadUpdate::Snapshot {
                    projection,
                    history_cursor,
                    has_more_history,
                    payload_budget_exceeded,
                    latest_local_turn_ordinal,
                    ..
                } => {
                    let mut next = Self::new(&self.thread, &self.project);
                    next.check_thread(&projection["thread"])?;
                    next.metadata = Metadata {
                        title: projection["thread"]["title"].as_str().unwrap_or("").into(),
                        history_cursor: history_cursor.clone(),
                        has_more_history: *has_more_history,
                        payload_budget_exceeded: *payload_budget_exceeded,
                        latest_local_turn_ordinal: *latest_local_turn_ordinal,
                        ..Default::default()
                    };
                    next.runs = entities(&projection["runs"])?;
                    // Optional only for older synthetic/read-only snapshots. Missing
                    // command context disables controls instead of inventing it.
                    for (name, map) in [
                        ("runtimeRequests", &mut next.requests),
                        ("providerThreads", &mut next.provider_threads),
                        ("providerSessions", &mut next.provider_sessions),
                    ] {
                        if let Some(value) = projection.get(name) {
                            *map = entities(value)?;
                        }
                    }
                    next.actions_ready = projection["runtimeRequests"].is_array()
                        && projection["providerThreads"].is_array()
                        && projection["providerSessions"].is_array();
                    next.actions_dirty = true;
                    next.attempts = entities(&projection["attempts"])?;
                    for value in projection["turnItems"].as_array().ok_or_else(invalid)? {
                        if let Some(value) = item(value)? {
                            if value["threadId"] != self.thread
                                || next
                                    .items
                                    .insert(string(&value, "id")?.into(), value.clone())
                                    .is_some()
                            {
                                return Err(invalid());
                            }
                        }
                    }
                    for row in projection["visibleTurnItems"]
                        .as_array()
                        .ok_or_else(invalid)?
                    {
                        if let Some(value) = item(&row["item"])? {
                            let row_key = key(&value)?;
                            if row["sourceThreadId"] != row_key.thread
                                || row["sourceItemId"] != row_key.item
                            {
                                return Err(invalid());
                            }
                            let local = string(row, "visibility")? == "local";
                            if local && row_key.thread != self.thread {
                                return Err(invalid());
                            }
                            if local && !next.visible(&value) {
                                continue;
                            }
                            if next
                                .rows
                                .insert(
                                    row_key.clone(),
                                    Arc::new(TurnRow {
                                        key: row_key.clone(),
                                        local,
                                        item: value,
                                    }),
                                )
                                .is_some()
                            {
                                return Err(invalid());
                            }
                            next.order.push(row_key);
                        }
                    }
                    next.reset = true;
                    next.order_dirty = true;
                    next.metadata_dirty = true;
                    operations.push(Operation::Snapshot(Box::new(next)));
                }
                ThreadUpdate::Event { event, .. } => {
                    let kind = string(event, "type")?;
                    // Unknown domain events intentionally advance only the wire cursor.
                    if !matches!(
                        kind,
                        "turn-item.updated"
                            | "run.created"
                            | "run.updated"
                            | "run-attempt.created"
                            | "run-attempt.updated"
                            | "runtime-request.updated"
                            | "provider-thread.updated"
                            | "provider-session.attached"
                            | "provider-session.updated"
                            | "thread.deleted"
                            | "thread.metadata-updated"
                            | "thread.archived"
                            | "thread.unarchived"
                            | "thread.settled"
                            | "thread.unsettled"
                    ) {
                        continue;
                    }
                    if event["threadId"] != self.thread {
                        return Err(invalid());
                    }
                    let payload = &event["payload"];
                    if kind == "turn-item.updated" {
                        if item(payload)?.is_none() {
                            continue;
                        }
                        if payload["threadId"] != self.thread {
                            return Err(invalid());
                        }
                    } else if kind.starts_with("thread.") {
                        self.check_thread(payload)?;
                    } else {
                        string(payload, "id")?;
                    }
                    operations.push(Operation::Event(kind.into(), payload.clone()));
                }
                ThreadUpdate::Synchronized => {}
            }
        }
        for operation in operations {
            match operation {
                Operation::Snapshot(mut next) => {
                    next.epoch = self.epoch.wrapping_add(1);
                    *self = *next;
                }
                Operation::Event(kind, payload) => self.event(&kind, payload),
            }
        }
        Ok(())
    }
    fn check_thread(&self, thread: &Value) -> StreamResult<()> {
        if thread["id"] != self.thread || thread["projectId"] != self.project {
            Err(StreamError::Protocol(
                "the native manager belongs to another thread or project",
            ))
        } else {
            Ok(())
        }
    }
    fn remove(&mut self, key: &RowKey) {
        if self.rows.remove(key).is_some() {
            self.order.retain(|old| old != key);
            self.dirty.insert(key.clone());
            self.order_dirty = true;
        }
    }
    fn event(&mut self, kind: &str, payload: Value) {
        if self.metadata.deleted {
            return;
        }
        match kind {
            "runtime-request.updated"
            | "provider-thread.updated"
            | "provider-session.attached"
            | "provider-session.updated" => {
                let map = match kind {
                    "runtime-request.updated" => &mut self.requests,
                    "provider-thread.updated" => &mut self.provider_threads,
                    _ => &mut self.provider_sessions,
                };
                let id = payload["id"].as_str().expect("prepared entity").to_owned();
                if map.get(&id) != Some(&payload) {
                    map.insert(id, payload);
                    self.actions_dirty = true;
                }
            }
            "thread.deleted" => {
                self.metadata.deleted = true;
                self.actions_dirty = true;
                self.metadata_dirty = true;
                for key in self.order.clone() {
                    self.remove(&key);
                }
            }
            kind if kind.starts_with("thread.") => {
                let title = payload["title"].as_str().unwrap_or("");
                if self.metadata.title != title {
                    self.metadata.title = title.into();
                    self.metadata_dirty = true;
                }
            }
            "turn-item.updated" => {
                let row_key = key(&payload).expect("prepared item");
                if self
                    .items
                    .get(&row_key.item)
                    .is_some_and(|old| old.as_ref() == &payload)
                    && (self.rows.contains_key(&row_key) || !self.visible(&payload))
                {
                    return;
                }
                if !self.items.contains_key(&row_key.item) && self.below_partial_window(&payload) {
                    return;
                }
                if matches!(
                    payload["type"].as_str(),
                    Some("approval_request" | "user_input_request")
                ) {
                    self.actions_dirty = true;
                }
                let interrupt = payload["type"] == "run_interrupt_request"
                    || self
                        .items
                        .get(&row_key.item)
                        .is_some_and(|old| old["type"] == "run_interrupt_request");
                let value = Arc::new(payload);
                self.items.insert(row_key.item.clone(), value.clone());
                if interrupt {
                    self.prune();
                }
                if !self.visible(&value) {
                    self.remove(&row_key);
                    return;
                }
                let ordinal = value["ordinal"].as_u64().expect("prepared ordinal");
                if self.metadata.has_more_history || self.metadata.history_cursor.is_some() {
                    self.metadata.latest_local_turn_ordinal = Some(
                        self.metadata
                            .latest_local_turn_ordinal
                            .unwrap_or(0)
                            .max(ordinal),
                    );
                }
                let previous = self.rows.insert(
                    row_key.clone(),
                    Arc::new(TurnRow {
                        key: row_key.clone(),
                        local: true,
                        item: value,
                    }),
                );
                let reordered = previous
                    .as_ref()
                    .is_some_and(|row| row.item["ordinal"].as_u64() != Some(ordinal));
                if reordered {
                    self.order.retain(|old| old != &row_key);
                }
                if previous.is_none() || reordered {
                    let index = self
                        .order
                        .iter()
                        .position(|key| {
                            let row = &self.rows[key];
                            row.local
                                && (row.item["ordinal"].as_u64().unwrap_or(0), key.item.as_str())
                                    > (ordinal, row_key.item.as_str())
                        })
                        .unwrap_or(self.order.len());
                    self.order.insert(index, row_key.clone());
                    self.order_dirty = true;
                }
                self.dirty.insert(row_key);
            }
            kind => {
                let map = if kind.starts_with("run-attempt.") {
                    &mut self.attempts
                } else {
                    &mut self.runs
                };
                let id = payload["id"].as_str().expect("prepared id").to_owned();
                if map.get(&id) == Some(&payload) {
                    return;
                }
                map.insert(id, payload);
                self.actions_dirty = true;
                self.prune();
            }
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    pub items: Vec<Value>,
    pub next_cursor: Option<String>,
    pub has_more_history: bool,
}
#[derive(Clone)]
pub struct HistoryRequest {
    cursor: String,
    epoch: u64,
}
impl HistoryRequest {
    pub fn cursor(&self) -> &str {
        &self.cursor
    }
}
impl Projection {
    pub fn history_request(&self) -> Option<HistoryRequest> {
        self.metadata
            .has_more_history
            .then(|| {
                self.metadata
                    .history_cursor
                    .as_ref()
                    .map(|cursor| HistoryRequest {
                        cursor: cursor.clone(),
                        epoch: self.epoch,
                    })
            })
            .flatten()
    }
    /// Merge against the current model: a slow history response must not
    /// replace a row already updated by the live stream or resurrect a hidden one.
    pub fn apply_history(
        &mut self,
        request: &HistoryRequest,
        page: HistoryPage,
    ) -> StreamResult<()> {
        if self.epoch != request.epoch
            || self.metadata.history_cursor.as_deref() != Some(&request.cursor)
        {
            return Ok(());
        }
        let mut rows = Vec::new();
        for row in &page.items {
            if let Some(value) = item(&row["item"])? {
                let row_key = key(&value)?;
                if row["sourceThreadId"] != row_key.thread || row["sourceItemId"] != row_key.item {
                    return Err(invalid());
                }
                let local = row["visibility"] == "local" || row_key.thread == self.thread;
                if local && row_key.thread != self.thread {
                    return Err(invalid());
                }
                rows.push(TurnRow {
                    key: row_key,
                    local,
                    item: value,
                });
            }
        }
        let mut page_keys = HashSet::new();
        let mut page_order = Vec::new();
        for mut row in rows {
            if page_keys.contains(&row.key) {
                continue;
            }
            if let Some(current) = self.rows.get(&row.key) {
                row = current.as_ref().clone();
            } else if row.local {
                if let Some(current) = self.items.get(&row.key.item) {
                    if current["type"] != "run_interrupt_request" || !self.visible(current) {
                        continue;
                    }
                    row.item = current.clone();
                }
            }
            page_keys.insert(row.key.clone());
            page_order.push(row.key.clone());
            if row.local {
                self.items
                    .entry(row.key.item.clone())
                    .or_insert_with(|| row.item.clone());
            }
            if !self.rows.contains_key(&row.key) {
                self.dirty.insert(row.key.clone());
                self.rows.insert(row.key.clone(), Arc::new(row));
            }
        }
        if !page_order.is_empty() {
            page_order.extend(
                self.order
                    .iter()
                    .filter(|key| !page_keys.contains(*key))
                    .cloned(),
            );
            if page_order != self.order {
                self.order = page_order;
                self.order_dirty = true;
            }
        }
        self.metadata.history_cursor = page.next_cursor;
        self.metadata.has_more_history = page.has_more_history;
        self.metadata_dirty = true;
        Ok(())
    }
}
