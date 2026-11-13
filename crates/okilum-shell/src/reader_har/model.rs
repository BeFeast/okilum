//! Pure HAR list state: the append-only request store, the filter/sort
//! projection that runs on workers, and the summary for the bottom line.
use okilum_core::har::Entry;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Requests in file order, in batches as the index delivered them. Clones are
/// cheap snapshots for workers; existing indices never change.
#[derive(Clone, Default)]
pub(crate) struct Entries {
    chunks: Vec<Arc<[Entry]>>,
    starts: Vec<usize>,
    len: usize,
}

impl Entries {
    pub(crate) fn push(&mut self, batch: Vec<Entry>) {
        if batch.is_empty() {
            return;
        }
        self.starts.push(self.len);
        self.len += batch.len();
        self.chunks.push(batch.into());
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn get(&self, index: usize) -> Option<&Entry> {
        let chunk = self
            .starts
            .partition_point(|&start| start <= index)
            .checked_sub(1)?;
        self.chunks[chunk].get(index - self.starts[chunk])
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.chunks.iter().flat_map(|chunk| chunk.iter())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Column {
    Method,
    Status,
    Name,
    Type,
    Size,
    Time,
    Started,
}

impl Column {
    pub(crate) const ALL: [Column; 7] = [
        Column::Method,
        Column::Status,
        Column::Name,
        Column::Type,
        Column::Size,
        Column::Time,
        Column::Started,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Column::Method => "Method",
            Column::Status => "Status",
            Column::Name => "Name",
            Column::Type => "Type",
            Column::Size => "Size",
            Column::Time => "Time",
            Column::Started => "Started",
        }
    }

    pub(crate) fn numeric(self) -> bool {
        matches!(
            self,
            Column::Status | Column::Size | Column::Time | Column::Started
        )
    }
}

/// What the list shows: a filter text and an optional sort.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Query {
    pub text: String,
    pub sort: Option<(Column, bool)>,
}

impl Query {
    /// Source order with no filter needs no projection.
    pub(crate) fn identity(&self) -> bool {
        self.text.trim().is_empty() && self.sort.is_none()
    }
}

/// The displayed order: every request in file order, or a projection.
#[derive(Clone)]
pub(crate) enum Rows {
    All(usize),
    Projected(Arc<Vec<u32>>),
}

impl Default for Rows {
    fn default() -> Self {
        Rows::All(0)
    }
}

impl Rows {
    pub(crate) fn len(&self) -> usize {
        match self {
            Rows::All(len) => *len,
            Rows::Projected(rows) => rows.len(),
        }
    }

    pub(crate) fn get(&self, position: usize) -> Option<usize> {
        match self {
            Rows::All(len) => (position < *len).then_some(position),
            Rows::Projected(rows) => rows.get(position).map(|&index| index as usize),
        }
    }

    pub(crate) fn position(&self, entry: usize) -> Option<usize> {
        match self {
            Rows::All(len) => (entry < *len).then_some(entry),
            Rows::Projected(rows) => rows.iter().position(|&index| index as usize == entry),
        }
    }
}

/// Case-insensitive substring over method, URL, status, type, MIME type and
/// error. `None` when cancelled.
pub(crate) fn project(entries: &Entries, query: &Query, cancel: &AtomicBool) -> Option<Vec<u32>> {
    let needle = Needle::new(query.text.trim());
    let mut rows = Vec::new();
    let mut status = String::new();
    for (index, entry) in entries.iter().enumerate() {
        if index % 4096 == 0 && cancel.load(Ordering::Relaxed) {
            return None;
        }
        if needle.is_empty() || needle.matches_entry(entry, &mut status) {
            rows.push(index as u32);
        }
    }
    if let Some((column, ascending)) = query.sort {
        // Stable: equal keys keep file order in both directions.
        rows.sort_by(|&a, &b| {
            let (x, y) = (entries.get(a as usize), entries.get(b as usize));
            let (Some(x), Some(y)) = (x, y) else {
                return std::cmp::Ordering::Equal;
            };
            let order = compare(column, x, y);
            if ascending {
                order
            } else {
                order.reverse()
            }
        });
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
    }
    Some(rows)
}

fn compare(column: Column, a: &Entry, b: &Entry) -> std::cmp::Ordering {
    // Missing numbers sort after every known value in ascending order.
    fn numbers(a: Option<f64>, b: Option<f64>) -> std::cmp::Ordering {
        match (a, b) {
            (Some(a), Some(b)) => a.total_cmp(&b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    }
    match column {
        Column::Method => a.method.cmp(&b.method),
        Column::Status => numbers(a.status.map(|s| s as f64), b.status.map(|s| s as f64)),
        Column::Name => (a.host(), a.path()).cmp(&(b.host(), b.path())),
        Column::Type => a.type_label().cmp(b.type_label()),
        Column::Size => numbers(
            shown_size(a).map(|s| s as f64),
            shown_size(b).map(|s| s as f64),
        ),
        Column::Time => numbers(a.time, b.time),
        Column::Started => numbers(a.started_ms, b.started_ms),
    }
}

/// The list's Size: bytes on the wire when known, else the decoded size.
pub(crate) fn shown_size(entry: &Entry) -> Option<i64> {
    entry.transferred.or(entry.size)
}

struct Needle {
    ascii: Option<Vec<u8>>,
    lower: String,
}

impl Needle {
    fn new(text: &str) -> Self {
        Self {
            ascii: text
                .is_ascii()
                .then(|| text.bytes().map(|b| b.to_ascii_lowercase()).collect()),
            lower: text.to_lowercase(),
        }
    }

    fn is_empty(&self) -> bool {
        self.lower.is_empty()
    }

    fn matches(&self, hay: &str) -> bool {
        match &self.ascii {
            Some(needle) => hay
                .as_bytes()
                .windows(needle.len())
                .any(|window| window.eq_ignore_ascii_case(needle)),
            None => hay.to_lowercase().contains(&self.lower),
        }
    }

    fn matches_entry(&self, entry: &Entry, status: &mut String) -> bool {
        status.clear();
        if let Some(code) = entry.status {
            use std::fmt::Write as _;
            let _ = write!(status, "{code}");
        }
        self.matches(&entry.method)
            || self.matches(&entry.url)
            || self.matches(status)
            || self.matches(entry.type_label())
            || self.matches(&entry.mime)
            || self.matches(&entry.error)
    }
}

/// Totals for the bottom line, maintained as batches arrive.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Stats {
    pub requests: usize,
    pub transferred: i64,
    pub errors: usize,
    pub first_ms: Option<f64>,
    pub last_end_ms: Option<f64>,
}

impl Stats {
    pub(crate) fn add(&mut self, entry: &Entry) {
        self.requests += 1;
        self.transferred += shown_size(entry).unwrap_or(0).max(0);
        if entry.failed() {
            self.errors += 1;
        }
        if let Some(start) = entry.started_ms {
            self.first_ms = Some(self.first_ms.map_or(start, |first| first.min(start)));
            let end = start + entry.time.unwrap_or(0.).max(0.);
            self.last_end_ms = Some(self.last_end_ms.map_or(end, |last| last.max(end)));
        }
    }

    /// From the first request's start to the last request's end.
    pub(crate) fn span_ms(&self) -> Option<f64> {
        Some(self.last_end_ms? - self.first_ms?)
    }
}

pub(crate) fn bytes(bytes: i64) -> String {
    match bytes {
        b if b >= 1_000_000_000 => format!("{:.1} GB", b as f64 / 1e9),
        b if b >= 1_000_000 => format!("{:.1} MB", b as f64 / 1e6),
        b if b >= 1_000 => format!("{:.1} KB", b as f64 / 1e3),
        b => format!("{b} B"),
    }
}

pub(crate) fn millis(ms: f64) -> String {
    if ms >= 60_000. {
        format!("{:.1} min", ms / 60_000.)
    } else if ms >= 1000. {
        format!("{:.2} s", ms / 1000.)
    } else if ms >= 10. {
        format!("{ms:.0} ms")
    } else {
        format!("{ms:.1} ms")
    }
}

pub(crate) fn count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The time of day as written in the file (`HH:MM:SS.mmm`), without a time
/// zone conversion: the capture's own clock is what its author saw.
pub(crate) fn clock(started: &str) -> &str {
    let Some((_, time)) = started.split_once('T') else {
        return started;
    };
    let end = time
        .find(|c: char| !(c.is_ascii_digit() || c == ':' || c == '.'))
        .unwrap_or(time.len());
    let time = &time[..end];
    // Milliseconds are enough for a list.
    match time.find('.') {
        Some(dot) if time.len() > dot + 4 => &time[..dot + 4],
        _ => time,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn entry(method: &str, url: &str, status: Option<i64>, time: Option<f64>) -> Entry {
        Entry {
            method: method.into(),
            url: url.into(),
            status,
            time,
            ..Default::default()
        }
    }

    fn sample() -> Entries {
        let mut entries = Entries::default();
        entries.push(vec![
            entry("GET", "https://a.test/one", Some(200), Some(30.)),
            entry("POST", "https://b.test/Login", Some(401), Some(10.)),
        ]);
        entries.push(vec![
            entry("GET", "https://a.test/two", None, None),
            entry("GET", "https://ünï.test/Straße", Some(500), Some(20.)),
        ]);
        entries
    }

    #[test]
    fn entries_index_across_batches() {
        let entries = sample();
        assert_eq!(entries.len(), 4);
        assert_eq!(&*entries.get(2).unwrap().url, "https://a.test/two");
        assert!(entries.get(4).is_none());
        assert_eq!(entries.iter().count(), 4);
    }

    #[test]
    fn filter_is_case_insensitive_across_fields() {
        let entries = sample();
        let run = |text: &str| {
            project(
                &entries,
                &Query {
                    text: text.into(),
                    sort: None,
                },
                &AtomicBool::new(false),
            )
            .unwrap()
        };
        assert_eq!(run("login"), [1]);
        assert_eq!(run("post"), [1]);
        assert_eq!(run("40"), [1]);
        assert_eq!(run("STRASSE"), Vec::<u32>::new());
        assert_eq!(run("straße"), [3]);
        assert_eq!(run("ÜNÏ"), [3]);
        assert_eq!(run(""), [0, 1, 2, 3]);
    }

    #[test]
    fn sort_is_stable_and_missing_values_go_last() {
        let entries = sample();
        let run = |sort| {
            project(
                &entries,
                &Query {
                    text: String::new(),
                    sort: Some(sort),
                },
                &AtomicBool::new(false),
            )
            .unwrap()
        };
        assert_eq!(run((Column::Time, true)), [1, 3, 0, 2]);
        assert_eq!(run((Column::Time, false)), [2, 0, 3, 1]);
        assert_eq!(run((Column::Status, true)), [0, 1, 3, 2]);
        assert_eq!(run((Column::Method, true)), [0, 2, 3, 1]);
        assert_eq!(run((Column::Name, true)), [0, 2, 1, 3]);
    }

    #[test]
    fn cancelled_projection_returns_nothing() {
        let cancel = AtomicBool::new(true);
        assert!(project(&sample(), &Query::default(), &cancel).is_none());
    }

    #[test]
    fn rows_map_positions_both_ways() {
        let rows = Rows::Projected(Arc::new(vec![3, 1]));
        assert_eq!((rows.len(), rows.get(1), rows.get(2)), (2, Some(1), None));
        assert_eq!(rows.position(3), Some(0));
        assert_eq!(Rows::All(2).get(2), None);
    }

    #[test]
    fn stats_and_labels() {
        let mut stats = Stats::default();
        for (start, time, status) in [(1000., 50., 200), (1100., 300., 404)] {
            stats.add(&Entry {
                started_ms: Some(start),
                time: Some(time),
                status: Some(status),
                transferred: Some(1500),
                ..Default::default()
            });
        }
        assert_eq!(
            (stats.requests, stats.errors, stats.transferred),
            (2, 1, 3000)
        );
        assert_eq!(stats.span_ms(), Some(400.));
        assert_eq!(count(1_234_567), "1,234,567");
        assert_eq!(count(12), "12");
        assert_eq!(bytes(1500), "1.5 KB");
        assert_eq!(millis(1234.), "1.23 s");
        assert_eq!(millis(4.25), "4.2 ms");
        assert_eq!(clock("2026-11-13T08:00:01.123456Z"), "08:00:01.123");
        assert_eq!(clock("2026-11-13T10:00:00+02:00"), "10:00:00");
        assert_eq!(clock("garbage"), "garbage");
    }
}
