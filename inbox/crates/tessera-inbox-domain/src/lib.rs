//! Detached Inbox values. No filesystem, provider, desktop or Brain dependency.
pub mod execution;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_CAPTURE_BYTES: usize = 64 * 1024;

/// Supplied by the authenticated transport, never taken from a capture body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnerId(pub Uuid);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub operation_id: Uuid,
    pub item_id: Uuid,
    pub text: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InvalidCapture {
    #[error("capture identifiers must not be nil")]
    Identity,
    #[error("capture text must contain non-whitespace content")]
    Empty,
    #[error("capture text exceeds the byte limit")]
    TooLarge,
}
impl Capture {
    pub fn validate(&self) -> Result<(), InvalidCapture> {
        if self.operation_id.is_nil() || self.item_id.is_nil() {
            return Err(InvalidCapture::Identity);
        }
        if self.text.trim().is_empty() {
            return Err(InvalidCapture::Empty);
        }
        if self.text.len() > MAX_CAPTURE_BYTES {
            return Err(InvalidCapture::TooLarge);
        }
        Ok(())
    }
}

/// Local processing state; it never means an external task was completed.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ItemState {
    New,
    Thinking,
    Ready,
    Processed,
    Dismissed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Item {
    pub id: Uuid,
    /// Exact capture bytes, including surrounding whitespace and newlines.
    pub original_text: String,
    pub state: ItemState,
    pub revision: u64,
    /// UTC Unix milliseconds assigned by the server, not the client.
    pub received_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaptureResult {
    pub operation_id: Uuid,
    pub item: Item,
}

/// Append-only capture feed in this first slice. Later item mutations must also
/// append events before using this feed for mutable snapshots.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaptureChange {
    pub sequence: u64,
    pub item: Item,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapturePage {
    pub changes: Vec<CaptureChange>,
    /// Fixed upper boundary for subsequent pages of this capture snapshot.
    pub through: u64,
    pub next_after: u64,
    pub has_more: bool,
}
