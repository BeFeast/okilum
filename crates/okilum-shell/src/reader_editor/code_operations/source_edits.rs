//! Apply a Format/Repair result as the bytes that actually changed (#2804).
//!
//! A whole-target replacement makes the editor re-wrap, re-highlight and
//! store in Undo every byte of a multi-MB file, twice (apply and Undo), even
//! when Repair quoted three keys. The worker therefore reduces the complete
//! result to a short list of exact replacements of the original target.
//! Applying them reproduces the result byte for byte; Undo restores the
//! original byte for byte; bytes between edits are never rewritten.

use super::model;
use std::ops::Range;
use std::time::{Duration, Instant};

/// Each edit is one native replacement with its own wrap/highlight update, so
/// a long list costs more than one large replacement. Neighbouring edits are
/// merged until at most this many remain.
pub(super) const MAX_EDITS: usize = 32;
/// The line diff is a refinement; on a pathological input the worker stops
/// refining at this deadline and keeps the (still exact) coarser edits.
const DIFF_DEADLINE: Duration = Duration::from_millis(150);
/// Below this many changed bytes a single trimmed replacement is already cheap.
const REFINE_MIN_BYTES: usize = 64 * 1024;
/// Myers needs at least |N - M| steps over N + M lines. Past this estimate a
/// whole-file Format (every line changes) would only spend the deadline.
const MAX_DIFF_WORK: usize = 1 << 30;

/// Exact replacements of `old` (ranges of `old`, sorted, disjoint) whose
/// application yields `new`. `None` only on cancellation.
pub(super) fn source_edits(
    old: &str,
    new: &str,
    control: &model::Control,
) -> Option<Vec<(Range<usize>, String)>> {
    if old == new {
        return Some(Vec::new());
    }
    let (prefix, suffix) = common_ends(old.as_bytes(), new.as_bytes());
    let (old_mid, new_mid) = (prefix..old.len() - suffix, prefix..new.len() - suffix);
    let mut hunks = vec![(old_mid.clone(), new_mid.clone())];
    if old_mid.len().max(new_mid.len()) >= REFINE_MIN_BYTES {
        if let Some(refined) = line_hunks(old, new, old_mid, new_mid, control) {
            hunks = refined;
        }
    }
    if control.cancelled() {
        return None;
    }
    let hunks = coalesce(hunks, MAX_EDITS);
    Some(
        hunks
            .into_iter()
            .map(|(old, new_range)| (old, new[new_range].to_owned()))
            .collect(),
    )
}

/// Common prefix and suffix lengths on UTF-8 boundaries of both texts
/// (identical bytes, so a boundary in one is a boundary in the other). The
/// suffix never overlaps the prefix in either text.
fn common_ends(a: &[u8], b: &[u8]) -> (usize, usize) {
    let mut prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    while prefix > 0 && !is_boundary(a, prefix) {
        prefix -= 1;
    }
    let limit = a.len().min(b.len()) - prefix;
    let mut suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(limit)
        .take_while(|(x, y)| x == y)
        .count();
    while suffix > 0 && !is_boundary(a, a.len() - suffix) {
        suffix -= 1;
    }
    (prefix, suffix)
}

fn is_boundary(bytes: &[u8], at: usize) -> bool {
    at == bytes.len() || (bytes[at] as i8) >= -0x40
}

/// Line-level diff of the changed middle, each changed run trimmed to its
/// own changed bytes. `None` when a line diff cannot help (one side is a
/// single line, as in Format of minified JSON, or the line counts differ too
/// much for a short edit script) or on cancellation.
fn line_hunks(
    old: &str,
    new: &str,
    old_mid: Range<usize>,
    new_mid: Range<usize>,
    control: &model::Control,
) -> Option<Vec<(Range<usize>, Range<usize>)>> {
    let a: Vec<&str> = old[old_mid.clone()].split_inclusive('\n').collect();
    let b: Vec<&str> = new[new_mid.clone()].split_inclusive('\n').collect();
    if a.len() < 2
        || b.len() < 2
        || a.len().abs_diff(b.len()).saturating_mul(a.len() + b.len()) > MAX_DIFF_WORK
        || control.cancelled()
    {
        return None;
    }
    let ops = similar::capture_diff_slices_deadline(
        similar::Algorithm::Myers,
        &a,
        &b,
        Some(Instant::now() + DIFF_DEADLINE),
    );
    let (mut at_old, mut at_new) = (old_mid.start, new_mid.start);
    let mut hunks: Vec<(Range<usize>, Range<usize>)> = Vec::new();
    for op in ops {
        let old_len: usize = a[op.old_range()].iter().map(|line| line.len()).sum();
        let new_len: usize = b[op.new_range()].iter().map(|line| line.len()).sum();
        let (o, n) = (at_old..at_old + old_len, at_new..at_new + new_len);
        at_old += old_len;
        at_new += new_len;
        if op.tag() == similar::DiffTag::Equal {
            continue;
        }
        let (p, s) = common_ends(old[o.clone()].as_bytes(), new[n.clone()].as_bytes());
        let hunk = (o.start + p..o.end - s, n.start + p..n.end - s);
        match hunks.last_mut() {
            // Adjacent runs (delete then insert) are one replacement.
            Some(last) if last.0.end == hunk.0.start && last.1.end == hunk.1.start => {
                last.0.end = hunk.0.end;
                last.1.end = hunk.1.end;
            }
            _ => hunks.push(hunk),
        }
    }
    debug_assert_eq!((at_old, at_new), (old_mid.end, new_mid.end));
    Some(hunks)
}

/// Merge neighbouring hunks, smallest unchanged gaps first, until at most
/// `max` remain. A merged hunk rewrites its gap with the identical bytes.
fn coalesce(
    mut hunks: Vec<(Range<usize>, Range<usize>)>,
    max: usize,
) -> Vec<(Range<usize>, Range<usize>)> {
    if hunks.len() <= max {
        return hunks;
    }
    let mut gaps: Vec<usize> = hunks
        .windows(2)
        .map(|pair| pair[1].0.start - pair[0].0.end)
        .collect();
    gaps.sort_unstable();
    // Merge every gap up to the threshold that leaves `max` hunks; ties may
    // merge a few more, never fewer.
    let threshold = gaps[hunks.len() - max - 1];
    let mut merged: Vec<(Range<usize>, Range<usize>)> = Vec::with_capacity(max);
    for hunk in hunks.drain(..) {
        match merged.last_mut() {
            Some(last) if hunk.0.start - last.0.end <= threshold => {
                last.0.end = hunk.0.end;
                last.1.end = hunk.1.end;
            }
            _ => merged.push(hunk),
        }
    }
    merged
}

/// `edits` of `old` as one replacement of the span they cover.
pub(super) fn merged(old: &str, edits: Vec<(Range<usize>, String)>) -> Vec<(Range<usize>, String)> {
    if edits.len() < 2 {
        return edits;
    }
    let span = edits[0].0.start..edits[edits.len() - 1].0.end;
    let mut text = String::new();
    let mut at = span.start;
    for (range, inserted) in &edits {
        text.push_str(&old[at..range.start]);
        text.push_str(inserted);
        at = range.end;
    }
    vec![(span, text)]
}

/// A source offset of the original text carried through `edits`: before an
/// edit it stays, inside one it moves to that edit's start, after it shifts.
pub(super) fn map_offset(edits: &[(Range<usize>, String)], offset: usize) -> usize {
    let mut delta = 0isize;
    for (range, text) in edits {
        if offset < range.start || (offset == range.start && range.is_empty()) {
            break;
        }
        if offset < range.end {
            return (range.start as isize + delta) as usize;
        }
        delta += text.len() as isize - range.len() as isize;
    }
    (offset as isize + delta) as usize
}

#[cfg(test)]
pub(super) fn apply(old: &str, edits: &[(Range<usize>, String)]) -> String {
    let mut out = String::with_capacity(old.len());
    let mut at = 0;
    for (range, text) in edits {
        out.push_str(&old[at..range.start]);
        out.push_str(text);
        at = range.end;
    }
    out.push_str(&old[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn check(old: &str, new: &str) -> Vec<(Range<usize>, String)> {
        let edits = source_edits(old, new, &model::Control::default()).unwrap();
        assert_eq!(apply(old, &edits), new);
        assert!(edits.len() <= MAX_EDITS);
        for pair in edits.windows(2) {
            assert!(pair[0].0.end <= pair[1].0.start, "sorted and disjoint");
        }
        for (range, _) in &edits {
            assert!(old.is_char_boundary(range.start) && old.is_char_boundary(range.end));
        }
        edits
    }

    #[test]
    fn large2804_edits_reproduce_result_exactly() {
        assert!(check("same", "same").is_empty());
        check("", "{}");
        check("{}", "");
        check("{a:1}", "{\"a\":1}");
        check("שלום{a:1}", "שלום{\"a\":1}");
        // Shared bytes inside a multibyte character never split it.
        check("aש", "aל");
        check("xשy", "xלy");
        check("{\"k\":\"ש\"}", "{\n  \"k\": \"ש\"\n}\n");
    }

    #[test]
    fn large2804_repair_of_few_keys_touches_only_those_bytes() {
        let row = |i: usize, bare: bool| {
            if bare {
                format!("  {{\n    id: {i},\n    \"name\": \"item {i} שלום\"\n  }},\n")
            } else {
                format!("  {{\n    \"id\": {i},\n    \"name\": \"item {i} שלום\"\n  }},\n")
            }
        };
        let old: String = (0..40_000).map(|i| row(i, i % 13_000 == 7)).collect();
        let new: String = (0..40_000).map(|i| row(i, false)).collect();
        assert!(old.len() > 2 * 1024 * 1024);
        let edits = check(&old, &new);
        assert_eq!(edits.len(), 4, "{edits:?}");
        assert!(edits
            .iter()
            .all(|(range, text)| range.len() <= 2 && text.len() <= 4));
    }

    #[test]
    fn large2804_minified_format_stays_one_replacement() {
        let old: String = (0..30_000).map(|i| format!("{{\"k\":{i}}},")).collect();
        let new: String = (0..30_000)
            .map(|i| format!("{{\n  \"k\": {i}\n}},\n"))
            .collect();
        let edits = check(&old, &new);
        assert_eq!(edits.len(), 1);
    }

    #[test]
    fn large2804_record_per_line_format_skips_the_line_diff() {
        let old: String = (0..30_000)
            .map(|i| format!("{{\"k\":{i},\"v\":[1,2]}},\n"))
            .collect();
        let new: String = (0..30_000)
            .map(|i| {
                format!("  {{\n    \"k\": {i},\n    \"v\": [\n      1,\n      2\n    ]\n  }},\n")
            })
            .collect();
        let edits = check(&old, &new);
        assert_eq!(edits.len(), 1);
    }

    #[test]
    fn large2804_many_changed_lines_coalesce_to_the_cap() {
        let old: String = (0..20_000)
            .map(|i| format!("    \"k{i}\": {i},\n"))
            .collect();
        let new: String = (0..20_000).map(|i| format!("  \"k{i}\": {i},\n")).collect();
        let edits = check(&old, &new);
        assert!(!edits.is_empty() && edits.len() <= MAX_EDITS);
    }

    #[test]
    fn large2804_merged_edits_keep_the_same_result() {
        let old = "a{x:1}b{y:2}c";
        let edits = check(old, "a{\"x\":1}b{\"y\":2}c");
        assert_eq!(edits.len(), 1, "small inputs are one trimmed replacement");
        let split = vec![(2..3, "\"x\"".to_owned()), (8..9, "\"y\"".to_owned())];
        assert_eq!(apply(old, &split), "a{\"x\":1}b{\"y\":2}c");
        let one = merged(old, split.clone());
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].0, 2..9);
        assert_eq!(apply(old, &one), apply(old, &split));
    }

    #[test]
    fn large2804_cancelled_diff_returns_nothing() {
        let control = model::Control::default();
        control.cancel();
        let old = "a\n".repeat(100_000);
        let new = "b\n".repeat(100_000);
        assert!(source_edits(&old, &new, &control).is_none());
    }

    #[test]
    fn large2804_offsets_follow_edits() {
        let edits = vec![(2..3, "xyz".to_owned()), (5..5, "++".to_owned())];
        assert_eq!(map_offset(&edits, 0), 0);
        assert_eq!(map_offset(&edits, 2), 2);
        assert_eq!(map_offset(&edits, 3), 5);
        assert_eq!(
            map_offset(&edits, 5),
            7,
            "the caret stays before an insertion at its offset"
        );
        assert_eq!(map_offset(&edits, 6), 10);
    }
}
