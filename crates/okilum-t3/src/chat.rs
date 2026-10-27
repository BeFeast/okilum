//! Polling and durable message identity for the Project tab. History is a
//! projection of T3; send intents and acknowledgements are operational state.

use std::collections::BTreeMap;

use crate::{GatewayError, ManagerGateway, ThreadQuery, TimelineItem};

mod journal;
pub use journal::{MessageJournal, MessageRecord};

type Result<T> = std::result::Result<T, GatewayError>;
const CHUNK_LIMIT: u32 = 16_000;
const MAX_CHUNKS: usize = 128;

#[derive(Clone, Debug)]
pub struct Conversation {
    pub thread_id: String,
    pub project_id: Option<String>,
    pub items: BTreeMap<u64, TimelineItem>,
    pub cursor: Option<u64>,
    pub has_more: bool,
    pub pending_questions: usize,
    /// None only after a successful question read; an unavailable count is not zero.
    pub pending_question_error: Option<String>,
}

impl Conversation {
    pub fn new(thread_id: impl Into<String>) -> Self {
        Self {
            thread_id: thread_id.into(),
            project_id: None,
            items: BTreeMap::new(),
            cursor: None,
            has_more: true,
            pending_questions: 0,
            pending_question_error: None,
        }
    }

    /// Only the cursor and growing item identities cross the worker boundary.
    pub fn poll_request(&self) -> PollRequest {
        PollRequest {
            thread_id: self.thread_id.clone(),
            cursor: self.cursor,
            growing: self
                .items
                .values()
                .filter(|item| is_growing(item))
                .map(|item| item.item_id.clone())
                .collect(),
        }
    }

    /// Preserve the synchronous API for existing callers.
    pub fn poll(&mut self, gateway: &dyn ManagerGateway) -> Result<()> {
        let update = self.poll_request().fetch(gateway)?;
        self.apply_poll(update)?;
        Ok(())
    }

    /// Commit a complete successful read and report only changed positions.
    pub fn apply_poll(&mut self, update: PollUpdate) -> Result<PollChanges> {
        if update.thread_id != self.thread_id {
            return Err(GatewayError::Protocol(
                "T3 answered for another manager".into(),
            ));
        }
        let mut changes = PollChanges::default();
        for (position, item) in update.items {
            if self.items.get(&position) != Some(&item) {
                self.items.insert(position, item);
                changes.positions.push(position);
            }
        }
        let cursor = update.next_position.or(self.cursor);
        let (questions, error) = match update.questions {
            Ok(count) => (count, None),
            Err(error) => (self.pending_questions, Some(error)),
        };
        changes.metadata = self.project_id.as_deref() != Some(&update.project_id)
            || self.cursor != cursor
            || self.has_more != update.has_more
            || self.pending_questions != questions
            || self.pending_question_error != error;
        self.project_id = Some(update.project_id);
        self.cursor = cursor;
        self.has_more = update.has_more;
        self.pending_questions = questions;
        self.pending_question_error = error;
        Ok(changes)
    }
}

#[derive(Default, Debug)]
pub struct PollChanges {
    pub positions: Vec<u64>,
    pub metadata: bool,
}
impl PollChanges {
    pub fn changed(&self) -> bool {
        self.metadata || !self.positions.is_empty()
    }
}

pub struct PollRequest {
    thread_id: String,
    cursor: Option<u64>,
    growing: Vec<String>,
}

pub struct PollUpdate {
    thread_id: String,
    pub project_id: String,
    items: BTreeMap<u64, TimelineItem>,
    next_position: Option<u64>,
    pub has_more: bool,
    questions: std::result::Result<usize, String>,
}

impl PollRequest {
    /// A failed read never mutates the projection retained by the UI.
    pub fn fetch(self, gateway: &dyn ManagerGateway) -> Result<PollUpdate> {
        let mut query = self.query();
        query.after_position = self.cursor;
        let page = gateway.read_thread(&query)?;
        if page.thread.thread_id != self.thread_id {
            return Err(GatewayError::Protocol(
                "T3 answered for another manager".into(),
            ));
        }
        let mut refreshed = BTreeMap::new();
        for item in page.items {
            let item = self.complete(gateway, item)?;
            refreshed.insert(item.position, item);
        }
        for old in &self.growing {
            if refreshed.values().any(|item| &item.item_id == old) {
                continue;
            }
            let page = gateway.read_thread(&self.query().item(old, None))?;
            let item = page
                .items
                .into_iter()
                .find(|item| &item.item_id == old)
                .ok_or_else(|| {
                    GatewayError::Protocol("a growing manager reply disappeared".into())
                })?;
            let item = self.complete(gateway, item)?;
            refreshed.insert(item.position, item);
        }
        let questions = match gateway.pending_requests(&self.thread_id) {
            Ok(questions) => Ok(questions),
            Err(
                error @ (GatewayError::NotConnected
                | GatewayError::Expired
                | GatewayError::Revoked
                | GatewayError::InvalidCredential),
            ) => return Err(error),
            Err(error) => Err(error.to_string()),
        };
        Ok(PollUpdate {
            thread_id: self.thread_id.clone(),
            project_id: page.thread.project_id,
            items: refreshed,
            next_position: page.next_position,
            has_more: page.has_more,
            questions: questions.map(|questions| questions.len()),
        })
    }

    fn query(&self) -> ThreadQuery {
        ThreadQuery {
            thread_id: self.thread_id.clone(),
            limit: Some(100),
            max_chars_per_item: Some(CHUNK_LIMIT),
            ..Default::default()
        }
    }

    fn complete(
        &self,
        gateway: &dyn ManagerGateway,
        mut item: TimelineItem,
    ) -> Result<TimelineItem> {
        let mut previous_offset = 0;
        for _ in 0..MAX_CHUNKS {
            if !item.text_truncated {
                return Ok(item);
            }
            let offset = item
                .next_text_offset
                .filter(|offset| *offset > previous_offset)
                .ok_or_else(|| {
                    GatewayError::Protocol("manager text cursor did not advance".into())
                })?;
            let page = gateway.read_thread(&self.query().item(&item.item_id, Some(offset)))?;
            let chunk = page
                .items
                .into_iter()
                .find(|chunk| chunk.item_id == item.item_id)
                .ok_or_else(|| GatewayError::Protocol("manager text chunk is missing".into()))?;
            if let Some(text) = chunk.text {
                item.text.get_or_insert_default().push_str(&text);
            }
            item.text_truncated = chunk.text_truncated;
            item.next_text_offset = chunk.next_text_offset;
            item.status = chunk.status;
            item.updated_at = chunk.updated_at;
            previous_offset = offset;
        }
        Err(GatewayError::Protocol(
            "manager reply exceeds the readable chunk limit; open it in T3".into(),
        ))
    }
}

fn is_growing(item: &TimelineItem) -> bool {
    matches!(item.status.as_str(), "pending" | "running" | "waiting")
}

#[cfg(test)]
mod tests;
