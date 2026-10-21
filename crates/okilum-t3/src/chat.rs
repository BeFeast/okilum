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
        }
    }

    /// One page per poll. Previously growing items are refreshed by id even
    /// after the position cursor has advanced past them. A failed poll leaves
    /// the previous projection and its cursor intact.
    pub fn poll(&mut self, gateway: &dyn ManagerGateway) -> Result<()> {
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
        for old in self.items.values().filter(|item| is_growing(item)) {
            if refreshed.values().any(|item| item.item_id == old.item_id) {
                continue;
            }
            let page = gateway.read_thread(&self.query().item(&old.item_id, None))?;
            let item = page
                .items
                .into_iter()
                .find(|item| item.item_id == old.item_id)
                .ok_or_else(|| {
                    GatewayError::Protocol("a growing manager reply disappeared".into())
                })?;
            let item = self.complete(gateway, item)?;
            refreshed.insert(item.position, item);
        }
        let questions = gateway.pending_requests(&self.thread_id)?;
        self.items.extend(refreshed);
        self.project_id = Some(page.thread.project_id);
        if let Some(cursor) = page.next_position {
            self.cursor = Some(cursor);
        }
        self.has_more = page.has_more;
        self.pending_questions = questions.len();
        Ok(())
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
