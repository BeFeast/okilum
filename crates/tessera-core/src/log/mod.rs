//! Read-only structured log indexing (#602).
//!
//! JSON lines and logfmt are detected from the head of a file and indexed in
//! parallel chunks into compact entries (byte range, line, level, timestamp).
//! Fields are decoded lazily per record and losslessly: keys keep their
//! spelling, values their text, duplicates their place. A line that is not a
//! record stays a row with `Level::Unparsed`; it is never dropped.
//!
//! Design reference: hl (github.com/pamburus/hl, MIT) for field aliases,
//! level normalisation and parallel chunked scanning. No hl code is used; see
//! docs/research/602-hl-log-viewer.md.

mod detect;
mod file;
mod index;
mod json;
mod level;
mod logfmt;
pub mod query;
mod record;
pub mod tail;
pub mod timestamp;

use std::path::Path;

pub use detect::{detect, Format};
pub use file::{LogFile, MAP_THRESHOLD_BYTES, MAX_LOG_BYTES};
pub use index::{LogEntry, LogIndex, LogStats};
pub use level::Level;
pub use query::{Query, QueryError};
pub use record::{Field, Record, Role, ValueKind};
pub use tail::LogTail;

/// File extensions the log viewer opens. Compressed and rotated names
/// (`.log.1`, `.gz`) are later slices.
pub const EXTENSIONS: [&str; 4] = ["log", "jsonl", "ndjson", "logfmt"];

pub fn is_log_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext)))
}

#[cfg(test)]
mod tests;
