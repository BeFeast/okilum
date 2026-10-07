//! Filters over a `LogIndex` (#602, slice 3): level with an explicit
//! "without level" choice, case-insensitive text, time windows relative to the
//! newest record, and `field = value` chips. All active parts combine with AND.
//!
//! Evaluation is meant for a background thread. Every run carries a ticket
//! from `FilterGenerations`; a run whose ticket went stale stops early and
//! returns nothing, and a finished result is only current while its
//! generation is the latest, so an older filter never replaces a newer one.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::{Format, Level, LogIndex, Record};

/// Entries checked between two looks at the cancellation flag.
const BLOCK: usize = 2048;
/// Below this many entries per thread, another thread costs more than it saves.
const MIN_ENTRIES_PER_THREAD: usize = 16 * 1024;

/// Which levels pass. `min` ranks the six severities; everything that has no
/// rankable severity (no level field, an unmapped value, an unparsed line) is
/// governed by `without_level` alone, so a level filter never hides such
/// lines silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevelFilter {
    /// Lowest severity shown; `None` shows every severity.
    pub min: Option<Level>,
    pub without_level: bool,
}

impl Default for LevelFilter {
    fn default() -> Self {
        Self {
            min: None,
            without_level: true,
        }
    }
}

impl LevelFilter {
    pub fn admits(&self, level: Level) -> bool {
        if level.is_severity() {
            self.min.is_none_or(|min| level >= min)
        } else {
            self.without_level
        }
    }

    pub fn is_active(&self) -> bool {
        *self != Self::default()
    }
}

/// Time presets. The window ends at the newest timestamp in the file, not at
/// the wall clock: a log from last week is still worth reading by its last
/// five minutes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TimeWindow {
    #[default]
    All,
    Last5Minutes,
    LastHour,
    Last24Hours,
}

impl TimeWindow {
    pub const ALL: [TimeWindow; 4] = [
        TimeWindow::All,
        TimeWindow::Last5Minutes,
        TimeWindow::LastHour,
        TimeWindow::Last24Hours,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TimeWindow::All => "All time",
            TimeWindow::Last5Minutes => "Last 5 minutes",
            TimeWindow::LastHour => "Last hour",
            TimeWindow::Last24Hours => "Last 24 hours",
        }
    }

    /// Window length in nanoseconds; `None` for no limit.
    pub fn nanos(self) -> Option<i64> {
        const MINUTE: i64 = 60_000_000_000;
        match self {
            TimeWindow::All => None,
            TimeWindow::Last5Minutes => Some(5 * MINUTE),
            TimeWindow::LastHour => Some(60 * MINUTE),
            TimeWindow::Last24Hours => Some(24 * 60 * MINUTE),
        }
    }
}

/// `key = value` on the exact key spelling (dotted for nested objects) and
/// the field's display text. A record with duplicate keys matches when any
/// of them carries the value.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FieldChip {
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogFilter {
    pub level: LevelFilter,
    /// Case-insensitive; matched against each field as `key=value`, or the
    /// whole line when it is not a record. Surrounding whitespace is ignored.
    pub text: String,
    pub time: TimeWindow,
    pub chips: Vec<FieldChip>,
}

impl LogFilter {
    /// False when every entry passes without looking at it.
    pub fn is_active(&self) -> bool {
        self.level.is_active()
            || !self.text.trim().is_empty()
            || self.time != TimeWindow::All
            || !self.chips.is_empty()
    }

    /// Adds `chip` unless an equal one is there already.
    pub fn add_chip(&mut self, chip: FieldChip) -> bool {
        if self.chips.contains(&chip) {
            return false;
        }
        self.chips.push(chip);
        true
    }

    pub fn remove_chip(&mut self, index: usize) -> Option<FieldChip> {
        (index < self.chips.len()).then(|| self.chips.remove(index))
    }
}

/// Hands out increasing generations. Clones share the counter.
#[derive(Clone, Debug, Default)]
pub struct FilterGenerations {
    latest: Arc<AtomicU64>,
}

impl FilterGenerations {
    /// Starts a new generation; every earlier ticket is stale from now on.
    pub fn next(&self) -> FilterTicket {
        let generation = self.latest.fetch_add(1, Ordering::SeqCst) + 1;
        FilterTicket {
            generation,
            latest: self.latest.clone(),
        }
    }

    /// Whether a result of `generation` may still be shown.
    pub fn is_current(&self, generation: u64) -> bool {
        self.latest.load(Ordering::SeqCst) == generation
    }
}

#[derive(Clone, Debug)]
pub struct FilterTicket {
    generation: u64,
    latest: Arc<AtomicU64>,
}

impl FilterTicket {
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn is_stale(&self) -> bool {
        self.latest.load(Ordering::SeqCst) != self.generation
    }
}

/// A finished run: entry indices that match, ascending.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterOutcome {
    pub generation: u64,
    pub rows: Vec<u32>,
    /// Entries the filter ran over.
    pub total: usize,
}

/// Runs `filter` over `index` (whose bytes are `data`). `None` when the
/// ticket went stale before the run finished.
pub fn evaluate(
    filter: &LogFilter,
    index: &LogIndex,
    data: &[u8],
    ticket: &FilterTicket,
) -> Option<FilterOutcome> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let rows = evaluate_with(filter, index, data, threads, &|| ticket.is_stale())?;
    Some(FilterOutcome {
        generation: ticket.generation,
        rows,
        total: index.len(),
    })
}

/// The run behind `evaluate`. `should_stop` is asked once per block of
/// entries, before the block, by every worker.
pub(crate) fn evaluate_with(
    filter: &LogFilter,
    index: &LogIndex,
    data: &[u8],
    threads: usize,
    should_stop: &(dyn Fn() -> bool + Sync),
) -> Option<Vec<u32>> {
    let plan = Plan::new(filter, index);
    let entries = index.len();
    let threads = threads.clamp(1, (entries / MIN_ENTRIES_PER_THREAD).max(1));
    let step = entries.div_ceil(threads).max(1);
    let ranges: Vec<_> = (0..entries)
        .step_by(step)
        .map(|start| start..(start + step).min(entries))
        .collect();
    let run = |range: std::ops::Range<usize>| plan.run(index, data, range, should_stop);
    let parts: Vec<Option<Vec<u32>>> = if ranges.len() <= 1 {
        ranges.into_iter().map(run).collect()
    } else {
        std::thread::scope(|scope| {
            let workers: Vec<_> = ranges
                .into_iter()
                .map(|range| scope.spawn(move || run(range)))
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().expect("log filter worker panicked"))
                .collect()
        })
    };
    let mut rows = Vec::new();
    for part in parts {
        rows.extend(part?);
    }
    // A worker that finished its blocks before a late stop still reports in;
    // the run as a whole is only good if nobody was told to stop.
    (!should_stop()).then_some(rows)
}

/// The filter, prepared once per run.
struct Plan<'f> {
    level: LevelFilter,
    needle: Option<Needle>,
    /// `[from, to]` in nanoseconds, when the time window restricts anything.
    window: Option<(i64, i64)>,
    chips: &'f [FieldChip],
    format: Format,
}

impl<'f> Plan<'f> {
    fn new(filter: &'f LogFilter, index: &LogIndex) -> Self {
        let window = filter.time.nanos().and_then(|length| {
            // No timestamp anywhere: there is nothing to measure a window
            // against, so the preset cannot restrict. The viewer does not
            // offer it for such a file.
            let newest = index.entries().iter().filter_map(|e| e.timestamp()).max()?;
            Some((newest.saturating_sub(length), newest))
        });
        Self {
            level: filter.level,
            needle: Needle::new(&filter.text),
            window,
            chips: &filter.chips,
            format: index.format(),
        }
    }

    fn run(
        &self,
        index: &LogIndex,
        data: &[u8],
        range: std::ops::Range<usize>,
        should_stop: &(dyn Fn() -> bool + Sync),
    ) -> Option<Vec<u32>> {
        let entries = index.entries();
        // A line without its own timestamp (a stack trace, a plain line)
        // belongs to the nearest timestamped entry above it.
        let mut carried = self.window.and_then(|_| {
            entries[..range.start]
                .iter()
                .rev()
                .find_map(|entry| entry.timestamp())
        });
        let mut rows = Vec::new();
        for (block_start, block) in (range.start..range.end)
            .step_by(BLOCK)
            .map(|start| (start, &entries[start..(start + BLOCK).min(range.end)]))
        {
            if should_stop() {
                return None;
            }
            for (offset, entry) in block.iter().enumerate() {
                if let Some(ts) = entry.timestamp() {
                    carried = Some(ts);
                }
                if !self.level.admits(entry.level()) {
                    continue;
                }
                if let Some((from, to)) = self.window {
                    if !carried.is_some_and(|ts| (from..=to).contains(&ts)) {
                        continue;
                    }
                }
                let raw = data.get(entry.range()).unwrap_or_default();
                if self.content_matches(raw, entry.level()) {
                    rows.push((block_start + offset) as u32);
                }
            }
        }
        Some(rows)
    }

    fn content_matches(&self, raw: &[u8], level: Level) -> bool {
        if self.needle.is_none() && self.chips.is_empty() {
            return true;
        }
        let record = if level == Level::Unparsed {
            None
        } else {
            if let Some(needle) = &self.needle {
                if needle.absent_from_unescaped(raw) {
                    return false;
                }
            }
            Record::parse(raw, self.format)
        };
        let Some(record) = record else {
            // Not a record: no field can match a chip, and text is matched
            // against the line as written.
            return self.chips.is_empty()
                && self
                    .needle
                    .as_ref()
                    .is_some_and(|needle| needle.found_in(&String::from_utf8_lossy(raw)));
        };
        let chips_match = self.chips.iter().all(|chip| {
            record
                .fields
                .iter()
                .any(|field| field.key == chip.key && field.value == chip.value)
        });
        chips_match
            && self.needle.as_ref().is_none_or(|needle| {
                let mut pair = String::new();
                record.fields.iter().any(|field| {
                    pair.clear();
                    pair.push_str(&field.key);
                    pair.push('=');
                    pair.push_str(&field.value);
                    needle.found_in(&pair)
                })
            })
    }
}

/// A case-insensitive search term.
struct Needle {
    lower: String,
    ascii: bool,
}

impl Needle {
    fn new(text: &str) -> Option<Self> {
        let text = text.trim();
        (!text.is_empty()).then(|| Self {
            lower: text.to_lowercase(),
            ascii: text.is_ascii(),
        })
    }

    fn found_in(&self, haystack: &str) -> bool {
        if self.ascii {
            contains_ascii_ci(haystack.as_bytes(), self.lower.as_bytes())
        } else {
            haystack.to_lowercase().contains(&self.lower)
        }
    }

    /// A cheap proof of absence on the undecoded line. Without escapes,
    /// every key segment and value is spelled verbatim in the line, so a term
    /// that cannot span a `.` joint or the `=` between key and value and is
    /// missing from the line is missing from every field too.
    fn absent_from_unescaped(&self, raw: &[u8]) -> bool {
        self.ascii
            && !self.lower.contains(['.', '='])
            && memchr::memchr(b'\\', raw).is_none()
            && !contains_ascii_ci(raw, self.lower.as_bytes())
    }
}

/// `needle` must already be lowercase ASCII.
fn contains_ascii_ci(haystack: &[u8], needle: &[u8]) -> bool {
    let Some((&first, rest)) = needle.split_first() else {
        return true;
    };
    let mut from = 0;
    while let Some(at) = memchr::memchr2(first, first.to_ascii_uppercase(), &haystack[from..]) {
        let start = from + at + 1;
        if haystack
            .get(start..start + rest.len())
            .is_some_and(|tail| tail.eq_ignore_ascii_case(rest))
        {
            return true;
        }
        from = start;
    }
    false
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    const DATA: &str = concat!(
        r#"{"ts":"2025-09-30T23:50:00Z","level":"debug","msg":"boot","logger":"app"}"#,
        "\n",
        r#"{"ts":"2025-10-01T00:30:00Z","level":"info","msg":"Request served","status":200,"user":{"role":"owner"}}"#,
        "\n",
        r#"{"ts":"2025-10-01T00:50:00Z","level":"warn","msg":"slow","status":503}"#,
        "\n",
        r#"{"ts":"2025-10-01T00:59:00Z","level":"error","msg":"cache miss","status":500,"path":"/v1/notes"}"#,
        "\n",
        "panicked at main.rs:12\n",
        r#"{"ts":"2025-10-01T01:00:00Z","level":"loud","msg":"unmapped level"}"#,
        "\n",
        r#"{"ts":"2025-10-01T01:00:00Z","msg":"no level here","error":"disk full"}"#,
        "\n",
        r#"{"level":"info","msg":"no time","status":500}"#,
        "\n",
    );

    fn index() -> LogIndex {
        LogIndex::build_with_threads(DATA.as_bytes(), 1)
    }

    fn run(filter: &LogFilter) -> Vec<u32> {
        let index = index();
        let rows = evaluate_with(filter, &index, DATA.as_bytes(), 1, &|| false).unwrap();
        for threads in [2, 3, 8] {
            assert_eq!(
                evaluate_with(filter, &index, DATA.as_bytes(), threads, &|| false).unwrap(),
                rows
            );
        }
        rows
    }

    fn text(text: &str) -> LogFilter {
        LogFilter {
            text: text.into(),
            ..Default::default()
        }
    }

    fn chip(key: &str, value: &str) -> FieldChip {
        FieldChip {
            key: key.into(),
            value: value.into(),
        }
    }

    #[test]
    fn fixture_has_the_expected_shape() {
        let levels: Vec<_> = index().entries().iter().map(|e| e.level()).collect();
        assert_eq!(
            levels,
            [
                Level::Debug,
                Level::Info,
                Level::Warn,
                Level::Error,
                Level::Unparsed,
                Level::Unknown,
                Level::Missing,
                Level::Info,
            ]
        );
    }

    #[test]
    fn an_inactive_filter_passes_everything() {
        let filter = LogFilter::default();
        assert!(!filter.is_active());
        assert!(!text("   ").is_active());
        assert_eq!(run(&filter), [0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn levels_rank_severities_and_keep_levelless_lines_by_choice() {
        let mut filter = LogFilter::default();
        filter.level.min = Some(Level::Warn);
        assert!(filter.is_active());
        // Unparsed, unknown and missing levels stay visible by default.
        assert_eq!(run(&filter), [2, 3, 4, 5, 6]);
        filter.level.without_level = false;
        assert_eq!(run(&filter), [2, 3]);
        filter.level.min = Some(Level::Fatal);
        assert!(run(&filter).is_empty());
        // "Without level" alone: only the lines no severity can rank.
        filter.level = LevelFilter {
            min: Some(Level::Fatal),
            without_level: true,
        };
        assert_eq!(run(&filter), [4, 5, 6]);
        filter.level = LevelFilter {
            min: None,
            without_level: false,
        };
        assert_eq!(run(&filter), [0, 1, 2, 3, 7]);
    }

    #[test]
    fn unknown_level_is_not_ranked() {
        let filter = LogFilter {
            level: LevelFilter {
                min: Some(Level::Trace),
                without_level: false,
            },
            ..Default::default()
        };
        assert!(!run(&filter).contains(&5), "`loud` is not a severity");
    }

    #[test]
    fn text_is_case_insensitive_across_message_fields_and_raw_lines() {
        assert_eq!(run(&text("CACHE")), [3], "message");
        assert_eq!(run(&text("owner")), [1], "nested field value");
        assert_eq!(run(&text("user.role=own")), [1], "dotted key and value");
        assert_eq!(run(&text("status=50")), [2, 3, 7]);
        assert_eq!(
            run(&text("Disk Full")),
            [6],
            "a field of a levelless record"
        );
        assert_eq!(run(&text("main.rs")), [4], "an unparsed line as written");
        assert_eq!(run(&text("  served ")), [1], "trimmed");
        assert!(run(&text("absent")).is_empty());
        // Text is matched in fields, not in JSON syntax.
        assert!(run(&text("\":\"")).is_empty());
    }

    #[test]
    fn text_sees_decoded_values_and_non_ascii() {
        let data = concat!(
            r#"{"msg":"café \"quoted\""}"#,
            "\n",
            r#"{"msg":"STRASSE Ärger"}"#,
            "\n",
            "msg=\"tab\\there\" level=info\n",
        );
        let index = LogIndex::build_with_threads(data.as_bytes(), 1);
        let rows = |needle: &str| {
            evaluate_with(&text(needle), &index, data.as_bytes(), 1, &|| false).unwrap()
        };
        assert_eq!(rows("Café"), [0], "escaped in JSON, matched decoded");
        assert_eq!(rows("\"quoted\""), [0]);
        assert_eq!(rows("ärger"), [1]);
        assert_eq!(rows("tab\there"), [2], "logfmt escape decoded");
    }

    #[test]
    fn the_unescaped_shortcut_agrees_with_the_full_match() {
        let lines = [
            r#"{"a":{"b":"Value"},"n":12.50,"arr":[1,"x"],"ok":true}"#,
            "key=value other=\"two words\"",
        ];
        for line in lines {
            let format = crate::log::detect(line.as_bytes());
            let record = Record::parse(line.as_bytes(), format).unwrap();
            for field in &record.fields {
                for term in [field.key.as_str(), field.value.as_str()] {
                    for part in term.split(['.', '=']).filter(|p| !p.is_empty()) {
                        let needle = Needle::new(part).unwrap();
                        assert!(!needle.absent_from_unescaped(line.as_bytes()), "{part}");
                    }
                }
            }
        }
        assert!(Needle::new("zzz")
            .unwrap()
            .absent_from_unescaped(lines[0].as_bytes()));
    }

    #[test]
    fn time_windows_end_at_the_newest_record_and_carry_to_untimed_lines() {
        let filter = |time| LogFilter {
            time,
            ..Default::default()
        };
        // Newest is 01:00; the panic line inherits 00:59 from the record
        // above it, and the final untimed record inherits 01:00.
        assert_eq!(run(&filter(TimeWindow::Last5Minutes)), [3, 4, 5, 6, 7]);
        assert_eq!(run(&filter(TimeWindow::LastHour)), [1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(
            run(&filter(TimeWindow::Last24Hours)),
            [0, 1, 2, 3, 4, 5, 6, 7]
        );
        assert_eq!(TimeWindow::All.nanos(), None);

        // Lines before the first timestamp cannot be placed and are left out.
        let data = "{\"msg\":\"untimed head\"}\n{\"ts\":1759276800,\"msg\":\"a\"}\nstack line\n";
        let index = LogIndex::build_with_threads(data.as_bytes(), 1);
        let rows = evaluate_with(
            &filter(TimeWindow::Last5Minutes),
            &index,
            data.as_bytes(),
            1,
            &|| false,
        )
        .unwrap();
        assert_eq!(rows, [1, 2]);

        // No timestamp anywhere: the window has no anchor and restricts
        // nothing (the viewer does not offer it).
        let data = "{\"msg\":\"a\"}\n{\"msg\":\"b\"}\n";
        let index = LogIndex::build_with_threads(data.as_bytes(), 1);
        let rows = evaluate_with(
            &filter(TimeWindow::Last5Minutes),
            &index,
            data.as_bytes(),
            1,
            &|| false,
        )
        .unwrap();
        assert_eq!(rows, [0, 1]);
    }

    #[test]
    fn chips_match_exact_fields_and_combine_with_everything_else() {
        let mut filter = LogFilter::default();
        assert!(filter.add_chip(chip("status", "500")));
        assert!(!filter.add_chip(chip("status", "500")), "no duplicates");
        assert_eq!(run(&filter), [3, 7]);
        filter.add_chip(chip("path", "/v1/notes"));
        assert_eq!(run(&filter), [3]);
        // Keys are exact: neither case nor a prefix of the value matches.
        assert!(run(&LogFilter {
            chips: vec![chip("Status", "500")],
            ..Default::default()
        })
        .is_empty());
        assert!(run(&LogFilter {
            chips: vec![chip("status", "50")],
            ..Default::default()
        })
        .is_empty());
        assert_eq!(
            run(&LogFilter {
                chips: vec![chip("user.role", "owner")],
                ..Default::default()
            }),
            [1]
        );
        // Chip + level + time + text, all ANDed.
        let mut combined = LogFilter {
            chips: vec![chip("status", "500")],
            time: TimeWindow::Last5Minutes,
            ..Default::default()
        };
        assert_eq!(run(&combined), [3, 7]);
        combined.level.min = Some(Level::Error);
        assert_eq!(run(&combined), [3]);
        combined.text = "nothing like this".into();
        assert!(run(&combined).is_empty());
        // Removing chips restores the wider result; out of range is a no-op.
        assert_eq!(filter.remove_chip(1), Some(chip("path", "/v1/notes")));
        assert_eq!(filter.remove_chip(5), None);
        assert_eq!(run(&filter), [3, 7]);
        // An unparsed line has no fields, so a chip never matches it.
        let raw = LogFilter {
            text: "panicked".into(),
            chips: vec![chip("status", "500")],
            ..Default::default()
        };
        assert!(run(&raw).is_empty());
    }

    #[test]
    fn duplicate_keys_match_any_occurrence() {
        let data = "{\"tag\":\"a\",\"tag\":\"b\"}\n";
        let index = LogIndex::build_with_threads(data.as_bytes(), 1);
        let filter = LogFilter {
            chips: vec![chip("tag", "b")],
            ..Default::default()
        };
        assert_eq!(
            evaluate_with(&filter, &index, data.as_bytes(), 1, &|| false).unwrap(),
            [0]
        );
    }

    fn large() -> (Vec<u8>, LogIndex) {
        let mut data = Vec::new();
        for i in 0..120_000u64 {
            if i % 11 == 0 {
                data.extend_from_slice(b"  at frame\n");
                continue;
            }
            data.extend_from_slice(
                format!(
                    "{{\"ts\":{},\"level\":\"{}\",\"msg\":\"m{i}\",\"status\":{}}}\n",
                    1_759_276_800 + i,
                    ["debug", "info", "warn", "error"][(i % 4) as usize],
                    if i % 10 == 0 { 500 } else { 200 },
                )
                .as_bytes(),
            );
        }
        let index = LogIndex::build(&data);
        (data, index)
    }

    #[test]
    fn large_index_counts_match_the_generator_on_every_thread_count() {
        let (data, index) = large();
        assert_eq!(index.len(), 120_000);
        // Warn and error records with status 500: i % 4 == 2 (warn; error is
        // odd) and i % 10 == 0, so i % 20 == 10. Frames have no level and are
        // excluded.
        let filter = LogFilter {
            level: LevelFilter {
                min: Some(Level::Warn),
                without_level: false,
            },
            chips: vec![chip("status", "500")],
            ..Default::default()
        };
        // Positive control: the generator says which lines must match.
        let expected: Vec<u32> = (0..120_000u32)
            .filter(|i| i % 11 != 0 && i % 20 == 10)
            .collect();
        assert!(expected.len() > 5_000);
        for threads in [1, 2, 4, 7] {
            let rows = evaluate_with(&filter, &index, &data, threads, &|| false).unwrap();
            assert_eq!(rows, expected, "{threads}");
        }
        let generations = FilterGenerations::default();
        let ticket = generations.next();
        let outcome = evaluate(&filter, &index, &data, &ticket).unwrap();
        assert_eq!(outcome.rows, expected);
        assert_eq!(outcome.total, 120_000);
        assert!(generations.is_current(outcome.generation));
    }

    #[test]
    fn a_stop_request_ends_a_large_run_early() {
        let (data, index) = large();
        let filter = text("m1");
        let calls = AtomicUsize::new(0);
        let stop_after_three = || calls.fetch_add(1, Ordering::SeqCst) >= 3;
        assert!(evaluate_with(&filter, &index, &data, 1, &stop_after_three).is_none());
        // Positive control: it stopped at the fourth block, not after all
        // of them, and the same run without a stop request completes.
        let blocks = index.len().div_ceil(BLOCK);
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        assert!(blocks > 4);
        assert!(evaluate_with(&filter, &index, &data, 1, &|| false).is_some());
        // Every worker of a parallel run stops as well.
        let calls = AtomicUsize::new(0);
        let stop_now = || {
            calls.fetch_add(1, Ordering::SeqCst);
            true
        };
        assert!(evaluate_with(&filter, &index, &data, 4, &stop_now).is_none());
        assert!(calls.load(Ordering::SeqCst) < blocks);
    }

    #[test]
    fn a_stale_result_never_replaces_a_newer_one() {
        let (data, index) = large();
        let generations = FilterGenerations::default();
        let older = generations.next();
        let newer = generations.next();
        assert!(older.is_stale() && !newer.is_stale());
        // The older run is cancelled outright...
        assert!(evaluate(&text("m1"), &index, &data, &older).is_none());
        // ...and even a result it finished before the newer ticket existed is
        // not current any more.
        let generations = FilterGenerations::default();
        let first = generations.next();
        let finished = evaluate(&text("m1"), &index, &data, &first).unwrap();
        assert!(generations.is_current(finished.generation));
        let second = generations.next();
        assert!(!generations.is_current(finished.generation));
        let latest = evaluate(&text("m2"), &index, &data, &second).unwrap();
        assert!(generations.is_current(latest.generation));
        assert_ne!(finished.rows, latest.rows);

        // A race: the newer generation starts while the older run is busy.
        // Whatever the timing, the older run either stops or is refused.
        let generations = FilterGenerations::default();
        let racing = generations.next();
        let outcome = std::thread::scope(|scope| {
            let worker = scope.spawn(|| evaluate(&text("m"), &index, &data, &racing));
            generations.next();
            worker.join().unwrap()
        });
        assert!(outcome.is_none_or(|outcome| !generations.is_current(outcome.generation)));
    }

    #[test]
    fn ascii_search_finds_every_case_and_overlap() {
        assert!(contains_ascii_ci(b"xxERRor", b"error"));
        assert!(
            contains_ascii_ci(b"aaab", b"aab"),
            "retries after a partial match"
        );
        assert!(!contains_ascii_ci(b"err", b"error"));
        assert!(contains_ascii_ci(b"anything", b""));
        assert!(contains_ascii_ci("Ärger ok".as_bytes(), b"ok"));
    }
}
