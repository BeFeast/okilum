//! Read-only structured log indexing (#602).
//!
//! JSON lines, logfmt and the text formats of text.rs (spdlog, glog, access
//! logs, syslog) are detected from the head of a file and indexed in
//! parallel chunks into compact entries (byte range, line, level, timestamp).
//! Fields are decoded lazily per record and losslessly: keys keep their
//! spelling, values their text, duplicates their place. A line that is not a
//! record stays a row with `Level::Unparsed`; it is never dropped.
//!
//! Design reference: hl (github.com/pamburus/hl, MIT) for field aliases,
//! level normalisation and parallel chunked scanning. No hl code is used; see
//! docs/research/602-hl-log-viewer.md.

pub mod ansi;
pub mod block;
pub mod correlation;
mod detect;
pub mod elapsed;
pub mod export;
mod file;
pub mod filter;
pub mod histogram;
mod index;
mod json;
mod level;
mod logfmt;
pub mod merge;
pub mod query;
mod record;
pub mod saved_query;
pub mod source_link;
pub mod tail;
mod text;
pub mod timestamp;

use std::path::Path;

pub use block::BlockKind;
pub use detect::{detect, Format};
pub use file::{LogFile, MAP_THRESHOLD_BYTES, MAX_LOG_BYTES};
pub use filter::{
    FieldChip, FilterGenerations, FilterOutcome, FilterTicket, LevelFilter, LogFilter, TimeWindow,
};
pub use histogram::Histogram;
pub use index::{LogEntry, LogIndex, LogStats};
pub use level::Level;
pub use merge::{FileId, MergeUpdate, MergedLog, MergedRow};
pub use query::{Query, QueryError};
pub use record::{Field, Record, Role, ValueKind};
pub use tail::LogTail;

/// File extensions the log viewer opens.
pub const EXTENSIONS: [&str; 4] = ["log", "jsonl", "ndjson", "logfmt"];

/// Compression suffixes the viewer decodes, read-only (#1164).
pub const COMPRESSED: [&str; 3] = ["gz", "zst", "bz2"];

/// glog's per-severity file names: `prog.host.user.log.INFO.<date>-<time>.<pid>`
/// and the `prog.INFO` link to the newest one.
const GLOG_LEVELS: [&str; 4] = ["INFO", "WARNING", "ERROR", "FATAL"];

/// Whether the viewer opens `path` as a log: a log extension, a rotated log
/// (`app.log.1`, `app.log.2026-10-10`, logrotate's `app.log-20261010`), a
/// glog file, and any of these compressed (`app.log.1.gz`). A compressed
/// file is a log only by the name inside: `site.tar.gz` is not.
pub fn is_log_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let name = compression(path).map_or(name, |ext| &name[..name.len() - ext.len() - 1]);
    is_log_name(name)
}

/// The compression suffix of `path`, if the viewer decodes it.
pub fn compression(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?;
    COMPRESSED
        .into_iter()
        .find(|known| known.eq_ignore_ascii_case(ext))
}

fn is_log_name(name: &str) -> bool {
    let parts: Vec<&str> = name.split('.').collect();
    let is_log_ext = |part: &str| EXTENSIONS.iter().any(|ext| ext.eq_ignore_ascii_case(part));
    let Some((last, before)) = parts.split_last() else {
        return false;
    };
    // A name needs a stem: `.log` alone is a hidden file, not a log.
    if before.iter().all(|part| part.is_empty()) {
        return false;
    }
    if is_log_ext(last) || GLOG_LEVELS.contains(last) {
        return true;
    }
    // logrotate's dateext: `app.log-20261010`.
    if let Some((ext, date)) = last.split_once('-') {
        if is_log_ext(ext) && date.len() == 8 && date.bytes().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }
    // Rotation suffix after a log extension: `.1`, `.2026-10-10`, `.20261010`.
    let rotated = !last.is_empty() && last.bytes().all(|c| c.is_ascii_digit() || c == b'-');
    if rotated && before.len() >= 2 && is_log_ext(before[before.len() - 1]) {
        return true;
    }
    // glog: `.log.INFO.20261010-060400.4411`.
    parts.windows(3).any(|w| {
        w[0].eq_ignore_ascii_case("log")
            && GLOG_LEVELS.contains(&w[1])
            && w[2].len() == 15
            && w[2].bytes().enumerate().all(|(i, c)| {
                if i == 8 {
                    c == b'-'
                } else {
                    c.is_ascii_digit()
                }
            })
    })
}

#[cfg(test)]
mod tests;
