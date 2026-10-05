//! Pure, bounded line-level merge preview. No file I/O, RPC, or implicit writes.
//! A clean result proves only this conservative text policy, not semantic agreement.
use similar::{capture_diff_slices_deadline, Algorithm, DiffTag};
use std::{
    collections::BTreeMap,
    ops::Range,
    time::{Duration, Instant},
};

pub(super) const MAX_INPUT_BYTES: usize = 256 * 1024;
pub(super) const MAX_LINES: usize = 4096;
const MAX_ELAPSED: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ManualReason {
    MissingBase,
    MissingCurrent,
    NotUtf8,
    InputLimit,
    WorkLimit,
    AmbiguousAlignment,
    Overlap,
}
impl ManualReason {
    pub fn message(&self) -> &'static str {
        match self {
            Self::MissingBase => "The original common base is unavailable. Resolve the preserved versions manually.",
            Self::MissingCurrent => "The current note is missing. It will not be recreated automatically.",
            Self::NotUtf8 => "A preserved version is not UTF-8. Its bytes cannot be merged by this text preview.",
            Self::InputLimit => "This note exceeds the merge preview size limit. The existing conflict remains available.",
            Self::WorkLimit => "Merge preview reached its work limit. Resolve the preserved versions manually.",
            Self::AmbiguousAlignment => "Repeated or moved lines make the edit locations ambiguous. Both versions are retained.",
            Self::Overlap => "The edits touch the same lines or insertion point. Both versions are retained for resolution.",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MergePreview {
    Ready {
        text: String,
        current_edits: usize,
        draft_edits: usize,
    },
    Manual {
        reason: ManualReason,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Edit<'a> {
    range: Range<usize>,
    replacement: Vec<&'a str>,
}

/// Call from a background task after validating snapshot/workspace identity.
/// Adoption must still compare the caller's frozen draft generation and current
/// revision, persist through editor recovery, then require an explicit Save.
pub(super) fn preview_merge(
    base: Option<&[u8]>,
    current: Option<&[u8]>,
    draft: &[u8],
) -> MergePreview {
    preview_until(base, current, draft, Instant::now() + MAX_ELAPSED)
}

fn preview_until(
    base: Option<&[u8]>,
    current: Option<&[u8]>,
    draft: &[u8],
    deadline: Instant,
) -> MergePreview {
    match merge(base, current, draft, deadline) {
        Ok((text, current_edits, draft_edits)) => MergePreview::Ready {
            text,
            current_edits,
            draft_edits,
        },
        Err(reason) => MergePreview::Manual { reason },
    }
}
fn check_time(deadline: Instant) -> Result<(), ManualReason> {
    if Instant::now() >= deadline {
        Err(ManualReason::WorkLimit)
    } else {
        Ok(())
    }
}
fn merge(
    base: Option<&[u8]>,
    current: Option<&[u8]>,
    draft: &[u8],
    deadline: Instant,
) -> Result<(String, usize, usize), ManualReason> {
    let base = base.ok_or(ManualReason::MissingBase)?;
    let current = current.ok_or(ManualReason::MissingCurrent)?;
    if [base.len(), current.len(), draft.len()]
        .into_iter()
        .any(|n| n > MAX_INPUT_BYTES)
    {
        return Err(ManualReason::InputLimit);
    }
    let parse = |bytes| std::str::from_utf8(bytes).map_err(|_| ManualReason::NotUtf8);
    let (base, current, draft) = (parse(base)?, parse(current)?, parse(draft)?);
    let lines = |text: &str| {
        text.bytes().filter(|b| *b == b'\n').count()
            + usize::from(!text.is_empty() && !text.ends_with('\n'))
    };
    if [base, current, draft]
        .into_iter()
        .any(|text| lines(text) > MAX_LINES)
    {
        return Err(ManualReason::InputLimit);
    }
    check_time(deadline)?;
    // These exact identities are unambiguous even if the note contains repeated
    // lines. Missing/invalid/budgeted inputs were refused before these shortcuts.
    if current == draft {
        return Ok((
            current.into(),
            usize::from(current != base),
            usize::from(draft != base),
        ));
    }
    if current == base {
        return Ok((draft.into(), 0, 1));
    }
    if draft == base {
        return Ok((current.into(), 1, 0));
    }
    // Keep LF/CRLF/BOM and missing final newline in the tokens themselves.
    let b: Vec<&str> = base.split_inclusive('\n').collect();
    let c: Vec<&str> = current.split_inclusive('\n').collect();
    let d: Vec<&str> = draft.split_inclusive('\n').collect();
    let mut counts = BTreeMap::new();
    for line in &b {
        *counts.entry(*line).or_insert(0usize) += 1;
    }
    let current_edits = edits(&b, &c, &counts, deadline)?;
    let draft_edits = edits(&b, &d, &counts, deadline)?;
    let counts = (current_edits.len(), draft_edits.len());
    let mut all = current_edits;
    all.extend(draft_edits);
    all.sort_by_key(|e| (e.range.start, e.range.end));
    // Identical changes at the same base location appear only once.
    all.dedup();
    for pair in all.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let overlap = if a.range.is_empty() {
            // Same-anchor inserts and inserts touching a replacement/deletion
            // boundary require manual intent, even if ordering seems possible.
            a.range.start >= b.range.start && a.range.start <= b.range.end
        } else if b.range.is_empty() {
            b.range.start >= a.range.start && b.range.start <= a.range.end
        } else {
            b.range.start < a.range.end
        };
        if overlap {
            return Err(ManualReason::Overlap);
        }
    }
    let mut out = String::new();
    let mut cursor = 0;
    for edit in all {
        check_time(deadline)?;
        for line in &b[cursor..edit.range.start] {
            out.push_str(line);
        }
        for line in edit.replacement {
            out.push_str(line);
        }
        cursor = edit.range.end;
    }
    for line in &b[cursor..] {
        out.push_str(line);
    }
    check_time(deadline)?;
    if out.len() > MAX_INPUT_BYTES {
        return Err(ManualReason::InputLimit);
    }
    Ok((out, counts.0, counts.1))
}

fn edits<'a>(
    base: &[&'a str],
    next: &[&'a str],
    counts: &BTreeMap<&str, usize>,
    deadline: Instant,
) -> Result<Vec<Edit<'a>>, ManualReason> {
    check_time(deadline)?;
    let ops = capture_diff_slices_deadline(Algorithm::Myers, base, next, Some(deadline));
    // similar may produce a coarse valid diff on deadline; never treat that
    // fallback as proof that a merge location is unambiguous.
    check_time(deadline)?;
    let mut out = vec![];
    for op in ops {
        if op.tag() == DiffTag::Equal {
            continue;
        }
        let range = op.old_range();
        let replacement = next[op.new_range()].to_vec();
        // A repeated removed/replaced line admits multiple alignments. Likewise,
        // repeated adjacent anchors cannot uniquely locate an insertion.
        if base[range.clone()]
            .iter()
            .any(|s| counts.get(s).copied().unwrap_or(0) > 1)
        {
            return Err(ManualReason::AmbiguousAlignment);
        }
        if range.is_empty() {
            let anchors = [
                range.start.checked_sub(1),
                (range.start < base.len()).then_some(range.start),
            ];
            if anchors
                .into_iter()
                .flatten()
                .any(|i| counts.get(base[i]).copied().unwrap_or(0) > 1)
            {
                return Err(ManualReason::AmbiguousAlignment);
            }
        }
        // Moving/copying existing lines can turn a clean diff into a misleading
        // merge. Leave it manual instead of guessing which occurrence was meant.
        if replacement.iter().any(|s| counts.contains_key(s)) {
            return Err(ManualReason::AmbiguousAlignment);
        }
        out.push(Edit { range, replacement });
    }
    // Reconstruct the side from its edits as an independent consistency check.
    let mut reconstructed = Vec::new();
    let mut cursor = 0;
    for edit in &out {
        reconstructed.extend_from_slice(&base[cursor..edit.range.start]);
        reconstructed.extend_from_slice(&edit.replacement);
        cursor = edit.range.end;
    }
    reconstructed.extend_from_slice(&base[cursor..]);
    if reconstructed != next {
        return Err(ManualReason::AmbiguousAlignment);
    }
    check_time(deadline)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn result(base: &str, current: &str, draft: &str) -> MergePreview {
        preview_merge(
            Some(base.as_bytes()),
            Some(current.as_bytes()),
            draft.as_bytes(),
        )
    }
    fn ready(base: &str, current: &str, draft: &str) -> String {
        match result(base, current, draft) {
            MergePreview::Ready { text, .. } => text,
            other => panic!("Expected independent merge: {other:?}"),
        }
    }
    fn manual(base: &str, current: &str, draft: &str) {
        assert!(matches!(
            result(base, current, draft),
            MergePreview::Manual { .. }
        ));
    }
    #[test]
    fn independent_replacements_preserve_markdown_and_mixed_endings_exactly() {
        let base = "\u{feff}---\r\ntype: Note\r\n---\n# Title\nAlpha 🧠\r\n[[target|имя]]\nOmega";
        let current = base.replace("Alpha 🧠", "Alpha שלום");
        let draft = base.replace("Omega", "Last мысль");
        assert_eq!(
            ready(base, &current, &draft),
            current.replace("Omega", "Last мысль")
        );
    }
    #[test]
    fn independent_insert_delete_and_replace_have_symmetric_exact_output() {
        let base = "Header\nOne\nTwo\nThree\nTail\n";
        let current = "Header\nOne\nThree\nTail\n";
        let draft = "Header\nNew\nOne\nTwo\nThree\nTail\n";
        let expected = "Header\nNew\nOne\nThree\nTail\n";
        assert_eq!(ready(base, current, draft), expected);
        assert_eq!(ready(base, draft, current), expected);
        assert_eq!(
            ready(
                base,
                &base.replace("One", "First"),
                &base.replace("Three", "Third")
            ),
            "Header\nFirst\nTwo\nThird\nTail\n"
        );
    }
    #[test]
    fn equal_changes_deduplicate_and_unchanged_sides_are_exact() {
        let b = "A\nB\nC\nD\n";
        let c = "X\nB\nC\nD\n";
        let d = "X\nB\nC\nZ\n";
        assert_eq!(ready(b, c, d), d);
        assert_eq!(ready(b, b, d), d);
        assert_eq!(ready(b, c, b), c);
        assert_eq!(
            ready("repeat\nrepeat\n", "repeat\n", "repeat\n"),
            "repeat\n"
        );
        assert_eq!(
            ready("", "", "text without newline"),
            "text without newline"
        );
    }
    #[test]
    fn overlapping_lines_delete_edit_and_same_anchor_insert_stay_manual() {
        manual("A\nB\nC\n", "A\nX\nC\n", "A\nY\nC\n");
        manual("A\nB\nC\n", "A\nC\n", "A\nY\nC\n");
        manual("A\nB\n", "A\nX\nB\n", "A\nY\nB\n");
        manual("A\nB\nC\n", "A\nX\nB\nC\n", "A\nY\nC\n");
        manual("alpha beta\n", "ALPHA beta\n", "alpha BETA\n");
    }
    #[test]
    fn repeated_blocks_and_moves_never_guess_an_edit_location() {
        manual(
            "Head\nrepeat\nrepeat\nTail\n",
            "Head\nrepeat\nTail\n",
            "Changed\nrepeat\nrepeat\nTail\n",
        );
        manual(
            "Head\nrepeat\nrepeat\nTail\n",
            "Head\nNew\nrepeat\nrepeat\nTail\n",
            "Changed\nrepeat\nrepeat\nTail\n",
        );
        manual("A\nB\nC\nD\n", "B\nA\nC\nD\n", "A\nB\nC\nLast\n");
    }
    #[test]
    fn final_newline_and_bom_are_real_edits_not_normalized() {
        assert_eq!(ready("A\r\nB", "X\r\nB", "A\r\nB\n"), "X\r\nB\n");
        assert_eq!(
            ready("\u{feff}A\nB\n", "A\nB\n", "\u{feff}A\nC\n"),
            "A\nC\n"
        );
        manual("A\nB\n", "A\nB", "A\nZ\n");
    }
    #[test]
    fn invalid_missing_and_oversized_inputs_are_explicit_manual_results() {
        assert_eq!(
            preview_merge(None, Some(b"x"), b"y"),
            MergePreview::Manual {
                reason: ManualReason::MissingBase
            }
        );
        assert_eq!(
            preview_merge(Some(b"x"), None, b"y"),
            MergePreview::Manual {
                reason: ManualReason::MissingCurrent
            }
        );
        assert_eq!(
            preview_merge(Some(&[255]), Some(b"x"), b"y"),
            MergePreview::Manual {
                reason: ManualReason::NotUtf8
            }
        );
        let large = vec![b'x'; MAX_INPUT_BYTES + 1];
        assert_eq!(
            preview_merge(Some(&large), Some(&large), &large),
            MergePreview::Manual {
                reason: ManualReason::InputLimit
            }
        );
        let lines = "x\n".repeat(MAX_LINES + 1);
        assert_eq!(
            result(&lines, &lines, &lines),
            MergePreview::Manual {
                reason: ManualReason::InputLimit
            }
        );
        assert_eq!(
            preview_until(Some(b"A\nB\n"), Some(b"X\nB\n"), b"A\nY\n", Instant::now()),
            MergePreview::Manual {
                reason: ManualReason::WorkLimit
            }
        );
    }
    #[test]
    fn every_distinct_pair_of_unique_line_edits_preserves_both_changes() {
        let base: Vec<String> = (0..24).map(|i| format!("Original line {i}\r\n")).collect();
        for a in 0..base.len() {
            for b in 0..base.len() {
                if a == b {
                    continue;
                }
                let mut current = base.clone();
                current[a] = format!("Current edit {a}\r\n");
                let mut draft = base.clone();
                draft[b] = format!("Draft edit {b}\r\n");
                let mut expected = current.clone();
                expected[b] = draft[b].clone();
                assert_eq!(
                    ready(&base.concat(), &current.concat(), &draft.concat()),
                    expected.concat()
                );
            }
        }
    }
    #[test]
    fn maximum_line_count_sparse_changes_are_exact_and_expiry_stays_manual() {
        let base: Vec<String> = (0..MAX_LINES)
            .map(|i| format!("Distinct line {i}\n"))
            .collect();
        let mut current = base.clone();
        current[20] = "Current changed\n".into();
        let mut draft = base.clone();
        draft[MAX_LINES - 20] = "Draft changed\n".into();
        let mut expected = current.clone();
        expected[MAX_LINES - 20] = draft[MAX_LINES - 20].clone();
        let (base, current, draft) = (base.concat(), current.concat(), draft.concat());
        // Test byte correctness separately from the production responsiveness
        // deadline: a loaded CI scheduler may legitimately exhaust 100 ms.
        assert_eq!(
            preview_until(
                Some(base.as_bytes()),
                Some(current.as_bytes()),
                draft.as_bytes(),
                Instant::now() + Duration::from_secs(10),
            ),
            MergePreview::Ready {
                text: expected.concat(),
                current_edits: 1,
                draft_edits: 1,
            }
        );
        assert_eq!(
            preview_until(
                Some(base.as_bytes()),
                Some(current.as_bytes()),
                draft.as_bytes(),
                Instant::now(),
            ),
            MergePreview::Manual {
                reason: ManualReason::WorkLimit,
            }
        );
    }
}
