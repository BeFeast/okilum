//! Level histogram over time (#1168): the counts per level per time bucket
//! that a Kibana Discover-style strip above the rows draws.
//!
//! The bucket width is chosen from [`WIDTHS`] so that the histogram spans at
//! most [`MAX_BUCKETS`] buckets, whatever the log's time range. Every width is
//! a whole multiple of the one before it and buckets are aligned to the epoch,
//! so a coarser histogram is the exact sum of finer buckets: the histogram can
//! start fine, coarsen as records with later (or earlier) times arrive, and
//! still end up identical to one built at the final width in one go. That is
//! what lets follow mode append to it instead of rebuilding it.
//!
//! A record without a time of its own takes the time of the nearest timestamped
//! record above it in the same file, the rule the time filter uses (filter.rs),
//! so the histogram, the time window and the rows agree on where a stack trace
//! belongs. Records above a file's first timestamp have no time at all; they
//! are counted apart ([`Histogram::untimed`]), never dropped.

use std::ops::Range;

use super::{Level, LogIndex};

/// The most buckets a histogram spans before it coarsens.
pub const MAX_BUCKETS: usize = 240;

const MS: i64 = 1_000_000;
const SECOND: i64 = 1_000 * MS;
const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;

/// Bucket widths in nanoseconds, each a whole multiple of the one before.
pub const WIDTHS: [i64; 19] = [
    MS,
    10 * MS,
    100 * MS,
    SECOND,
    5 * SECOND,
    10 * SECOND,
    30 * SECOND,
    MINUTE,
    5 * MINUTE,
    10 * MINUTE,
    30 * MINUTE,
    HOUR,
    3 * HOUR,
    6 * HOUR,
    12 * HOUR,
    DAY,
    7 * DAY,
    28 * DAY,
    364 * DAY,
];

type Counts = [u32; Level::COUNT];
const ZERO: Counts = [0; Level::COUNT];

/// One bucket of a histogram: `[from, to)` in Unix nanoseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bucket<'h> {
    pub from: i64,
    pub to: i64,
    pub counts: &'h [u32; Level::COUNT],
}

impl Bucket<'_> {
    pub fn total(&self) -> u64 {
        self.counts.iter().map(|&n| u64::from(n)).sum()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Histogram {
    width_ix: usize,
    /// Bucket number (`time.div_euclid(width)`) of `buckets[0]`.
    first: i64,
    buckets: Vec<Counts>,
    untimed: Counts,
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

impl Histogram {
    pub fn new() -> Self {
        Self {
            width_ix: 0,
            first: 0,
            buckets: Vec::new(),
            untimed: ZERO,
        }
    }

    /// Counts every entry of `index`, or only `selected` (ascending entry
    /// indices, e.g. the rows of a filter run).
    pub fn add_index(&mut self, index: &LogIndex, selected: Option<&[u32]>) {
        self.add_range(index, 0..index.len(), selected);
    }

    /// Counts the entries of `range`, or those of `selected` that fall in it.
    /// Entries before `range` still give the time of an untimed entry at its
    /// start, so adding a file piece by piece equals adding it whole.
    pub fn add_range(&mut self, index: &LogIndex, range: Range<usize>, selected: Option<&[u32]>) {
        let entries = index.entries();
        let range = range.start.min(entries.len())..range.end.min(entries.len());
        let mut carried = entries[..range.start]
            .iter()
            .rev()
            .find_map(|entry| entry.timestamp());
        let mut next =
            selected.map(|rows| rows.partition_point(|&row| (row as usize) < range.start));
        for at in range {
            let entry = &entries[at];
            if let Some(time) = entry.timestamp() {
                carried = Some(time);
            }
            let counted = match (selected, next.as_mut()) {
                (Some(rows), Some(next)) => {
                    let hit = rows.get(*next).is_some_and(|&row| row as usize == at);
                    *next += usize::from(hit);
                    hit
                }
                _ => true,
            };
            if counted {
                match carried {
                    Some(time) => self.push(time, entry.level()),
                    None => {
                        let slot = &mut self.untimed[entry.level() as usize];
                        *slot = slot.saturating_add(1);
                    }
                }
            }
        }
    }

    /// Counts one record of `level` at `time` (Unix nanoseconds).
    pub fn push(&mut self, time: i64, level: Level) {
        // Coarsen before growing: one stray timestamp must not allocate the
        // millions of milliseconds between it and the rest.
        let (index, first, last) = loop {
            let width = WIDTHS[self.width_ix];
            let index = time.div_euclid(width);
            let (first, last) = match self.buckets.len() {
                0 => (index, index),
                len => (
                    self.first.min(index),
                    (self.first + len as i64 - 1).max(index),
                ),
            };
            if (last - first) as usize >= MAX_BUCKETS && self.width_ix + 1 < WIDTHS.len() {
                self.coarsen();
                continue;
            }
            break (index, first, last);
        };
        if self.buckets.is_empty() {
            self.first = first;
        } else if first < self.first {
            let missing = (self.first - first) as usize;
            self.buckets
                .splice(0..0, std::iter::repeat_n(ZERO, missing));
            self.first = first;
        }
        let wanted = (last - self.first) as usize + 1;
        if wanted > self.buckets.len() {
            self.buckets.resize(wanted, ZERO);
        }
        let slot = &mut self.buckets[(index - self.first) as usize][level as usize];
        *slot = slot.saturating_add(1);
    }

    /// One step coarser: the next width, every bucket added to the one that
    /// contains it.
    fn coarsen(&mut self) {
        let ratio = WIDTHS[self.width_ix + 1] / WIDTHS[self.width_ix];
        self.width_ix += 1;
        if self.buckets.is_empty() {
            return;
        }
        let first = self.first.div_euclid(ratio);
        let last = (self.first + self.buckets.len() as i64 - 1).div_euclid(ratio);
        let mut merged = vec![ZERO; (last - first) as usize + 1];
        for (at, counts) in self.buckets.iter().enumerate() {
            let target = &mut merged[((self.first + at as i64).div_euclid(ratio) - first) as usize];
            for (slot, &n) in target.iter_mut().zip(counts) {
                *slot = slot.saturating_add(n);
            }
        }
        self.first = first;
        self.buckets = merged;
    }

    /// Bucket width in nanoseconds.
    pub fn width(&self) -> i64 {
        WIDTHS[self.width_ix]
    }

    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }

    /// Start of the first bucket.
    pub fn start(&self) -> Option<i64> {
        (!self.is_empty()).then(|| self.edge_of(0))
    }

    /// End (exclusive) of the last bucket.
    pub fn end(&self) -> Option<i64> {
        (!self.is_empty()).then(|| self.edge_of(self.buckets.len()))
    }

    fn edge_of(&self, at: usize) -> i64 {
        (self.first + at as i64).saturating_mul(self.width())
    }

    pub fn bucket(&self, at: usize) -> Option<Bucket<'_>> {
        let counts = self.buckets.get(at)?;
        Some(Bucket {
            from: self.edge_of(at),
            to: self.edge_of(at + 1),
            counts,
        })
    }

    pub fn buckets(&self) -> impl Iterator<Item = Bucket<'_>> {
        (0..self.buckets.len()).filter_map(|at| self.bucket(at))
    }

    /// The bucket that holds `time`, if it is inside the histogram.
    pub fn bucket_at(&self, time: i64) -> Option<usize> {
        let at = time.div_euclid(self.width()) - self.first;
        usize::try_from(at)
            .ok()
            .filter(|&at| at < self.buckets.len())
    }

    /// The largest bucket total, the height the strip scales to.
    pub fn max_total(&self) -> u64 {
        self.buckets()
            .map(|bucket| bucket.total())
            .max()
            .unwrap_or(0)
    }

    /// Records counted in buckets.
    pub fn timed(&self) -> u64 {
        self.buckets().map(|bucket| bucket.total()).sum()
    }

    /// Records with no time at all (nothing timestamped above them).
    pub fn untimed(&self) -> u64 {
        self.untimed.iter().map(|&n| u64::from(n)).sum()
    }

    /// The time window `[from, to]` (inclusive, nanoseconds) covered by buckets
    /// `a` through `b`, in either order; `None` when one is outside.
    pub fn window(&self, a: usize, b: usize) -> Option<(i64, i64)> {
        let (low, high) = (a.min(b), a.max(b));
        let from = self.bucket(low)?.from;
        let to = self.bucket(high)?.to;
        Some((from, to.saturating_sub(1)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{filter, FilterGenerations, LogFilter, TimeWindow};

    const S: i64 = SECOND;

    fn naive(times: &[(i64, Level)], width: i64) -> std::collections::BTreeMap<i64, Counts> {
        let mut map = std::collections::BTreeMap::new();
        for &(time, level) in times {
            map.entry(time.div_euclid(width)).or_insert(ZERO)[level as usize] += 1;
        }
        map
    }

    fn as_map(histogram: &Histogram) -> std::collections::BTreeMap<i64, Counts> {
        histogram
            .buckets()
            .enumerate()
            .filter(|(_, bucket)| bucket.total() > 0)
            .map(|(_, bucket)| (bucket.from / histogram.width(), *bucket.counts))
            .collect()
    }

    #[test]
    fn every_width_is_a_whole_multiple_of_the_one_before() {
        for pair in WIDTHS.windows(2) {
            assert_eq!(pair[1] % pair[0], 0, "{pair:?}");
            assert!(pair[1] > pair[0]);
        }
    }

    #[test]
    fn the_width_follows_the_span_and_stays_within_the_bucket_limit() {
        for (span, width) in [
            (0, MS),
            (200 * MS, MS),
            (90 * S, S),
            (30 * MINUTE, 10 * S),
            (3 * DAY, 30 * MINUTE),
            (400 * DAY, 7 * DAY),
        ] {
            let mut histogram = Histogram::new();
            histogram.push(1_700_000_000 * S, Level::Info);
            histogram.push(1_700_000_000 * S + span, Level::Info);
            assert!(
                histogram.len() <= MAX_BUCKETS,
                "{span}: {}",
                histogram.len()
            );
            assert_eq!(histogram.width(), width, "span {span}");
            assert_eq!(histogram.timed(), 2);
        }
    }

    #[test]
    fn a_stray_timestamp_coarsens_instead_of_allocating() {
        let mut histogram = Histogram::new();
        histogram.push(1_700_000_000 * S, Level::Error);
        // 1970 and the far future: millions of milliseconds away.
        histogram.push(0, Level::Info);
        histogram.push(4_000_000_000 * S, Level::Info);
        histogram.push(-5 * DAY, Level::Info);
        assert!(histogram.len() <= 400, "{}", histogram.len());
        assert_eq!(histogram.timed(), 4);
        assert_eq!(histogram.width(), WIDTHS[WIDTHS.len() - 1]);
    }

    #[test]
    fn coarsening_keeps_the_exact_counts_whatever_the_order() {
        // A deterministic spread: clusters, a gap, one early and one late record.
        let mut times = Vec::new();
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..4000 {
            let base = [0, 40 * S, 3_600 * S][(next() % 3) as usize];
            let level = Level::ALL[(next() % Level::COUNT as u64) as usize];
            times.push((
                1_700_000_000 * S + base + (next() % (5 * S as u64)) as i64,
                level,
            ));
        }
        times.push((1_700_000_000 * S - 9 * DAY, Level::Fatal));
        times.push((1_700_000_000 * S + 2 * DAY, Level::Warn));
        let mut forward = Histogram::new();
        let mut backward = Histogram::new();
        let mut shuffled = Histogram::new();
        for &(time, level) in &times {
            forward.push(time, level);
        }
        for &(time, level) in times.iter().rev() {
            backward.push(time, level);
        }
        let mut order: Vec<_> = times.clone();
        for ix in (1..order.len()).rev() {
            order.swap(ix, (next() % (ix as u64 + 1)) as usize);
        }
        for &(time, level) in &order {
            shuffled.push(time, level);
        }
        assert_eq!(forward, backward);
        assert_eq!(forward, shuffled);
        assert!(forward.len() <= MAX_BUCKETS);
        assert!(forward.width() > MS, "positive control: it did coarsen");
        // Exactly what a direct count at the final width gives.
        assert_eq!(as_map(&forward), naive(&times, forward.width()));
        // And that width is the finest one that fits.
        let finer = WIDTHS[WIDTHS.iter().position(|&w| w == forward.width()).unwrap() - 1];
        let direct = naive(&times, finer);
        let span = direct.keys().next_back().unwrap() - direct.keys().next().unwrap();
        assert!(
            span as usize >= MAX_BUCKETS,
            "the next finer width would not fit"
        );
    }

    fn index(text: &str) -> LogIndex {
        LogIndex::build_with_threads(text.as_bytes(), 1)
    }

    const LOG: &str = concat!(
        r#"{"level":"info","msg":"no time yet"}"#,
        "\n",
        r#"{"ts":"2025-10-01T00:00:00Z","level":"info","msg":"a"}"#,
        "\n",
        r#"{"ts":"2025-10-01T00:00:05Z","level":"error","msg":"b"}"#,
        "\n",
        r#"{"level":"warn","msg":"no time of its own"}"#,
        "\n",
        r#"{"ts":"2025-10-01T00:01:00Z","level":"debug","msg":"c"}"#,
        "\n",
    );

    #[test]
    fn untimed_entries_take_the_time_above_and_leading_ones_are_counted_apart() {
        let index = index(LOG);
        let mut histogram = Histogram::new();
        histogram.add_index(&index, None);
        assert_eq!(histogram.timed() + histogram.untimed(), index.len() as u64);
        assert_eq!(
            histogram.untimed(),
            1,
            "only the record above the first time"
        );
        let at = |text: &str| {
            let time = crate::log::timestamp::parse_text(text.as_bytes()).unwrap();
            histogram
                .bucket(histogram.bucket_at(time).unwrap())
                .unwrap()
        };
        // "no time of its own" follows the error at :05 and counts with it.
        let bucket = at("2025-10-01T00:00:05Z");
        assert_eq!(bucket.counts[Level::Error as usize], 1);
        assert_eq!(bucket.counts[Level::Warn as usize], 1);
    }

    #[test]
    fn adding_piece_by_piece_or_a_subset_equals_the_filter_run() {
        let index = index(LOG);
        let whole = {
            let mut histogram = Histogram::new();
            histogram.add_index(&index, None);
            histogram
        };
        for split in 0..=index.len() {
            let mut parts = Histogram::new();
            parts.add_range(&index, 0..split, None);
            parts.add_range(&index, split..index.len(), None);
            assert_eq!(parts, whole, "split at {split}");
        }
        // The rows of a filter run: the same entries, the same counts.
        let filter = LogFilter {
            text: "no time of its own".into(),
            ..Default::default()
        };
        let rows = filter::evaluate(
            &filter,
            &index,
            LOG.as_bytes(),
            &FilterGenerations::default().next(),
        )
        .unwrap()
        .rows;
        assert_eq!(rows.len(), 1, "positive control");
        let mut selected = Histogram::new();
        selected.add_index(&index, Some(&rows));
        assert_eq!(selected.timed(), 1);
        assert_eq!(selected.untimed(), 0);
        let mut pieces = Histogram::new();
        pieces.add_range(&index, 0..3, Some(&rows));
        pieces.add_range(&index, 3..index.len(), Some(&rows));
        assert_eq!(pieces, selected);
    }

    #[test]
    fn a_window_over_buckets_is_inclusive_and_the_filter_agrees() {
        let index = index(LOG);
        let mut histogram = Histogram::new();
        histogram.add_index(&index, None);
        // From the first to the error's bucket, given in the opposite order.
        let first = histogram.bucket_at(parse("2025-10-01T00:00:00Z")).unwrap();
        let error = histogram.bucket_at(parse("2025-10-01T00:00:05Z")).unwrap();
        let (from, to) = histogram.window(error, first).unwrap();
        assert_eq!(histogram.bucket(first).unwrap().from, from);
        assert_eq!(histogram.bucket(error).unwrap().to - 1, to);
        let filter = LogFilter {
            time: TimeWindow::Range { from, to },
            ..Default::default()
        };
        let rows = filter::evaluate(
            &filter,
            &index,
            LOG.as_bytes(),
            &FilterGenerations::default().next(),
        )
        .unwrap()
        .rows;
        // The leading untimed record and the minute-later one are out; the record
        // without a time of its own rides with the error above it.
        let lines: Vec<_> = rows
            .iter()
            .map(|&row| {
                String::from_utf8_lossy(index.raw(LOG.as_bytes(), row as usize).unwrap())
                    .into_owned()
            })
            .collect();
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].contains("\"msg\":\"a\""), "{lines:?}");
        assert!(lines[1].contains("\"msg\":\"b\""), "{lines:?}");
        assert!(lines[2].contains("no time of its own"), "{lines:?}");
        assert_eq!(histogram.window(0, 10_000), None);
    }

    fn parse(text: &str) -> i64 {
        crate::log::timestamp::parse_text(text.as_bytes()).unwrap()
    }
}
