//! Prepared block-embed presentation, carried in the derived renderer source.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Ready,
    MissingBlock,
    DuplicateBlock,
    MissingNote,
    UnreadableNote,
    SelfReference,
    Pending,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Info {
    /// Resolved vault identity; absent until resolution succeeds.
    pub path: Option<String>,
    pub title: String,
    /// The block fragment stays internal to navigation, never the label.
    pub id: String,
    pub status: Status,
}

impl Info {
    pub fn fence_meta(&self) -> String {
        format!(
            "block {}",
            serde_json::to_string(self).expect("string fields serialize")
        )
    }

    pub fn parse(meta: &str) -> Option<Self> {
        serde_json::from_str(meta.strip_prefix("block ")?).ok()
    }
}
