//! Revision-bound task edit plans and Unix safe-write/Undo integration.
//! Native controls and reader protocols are activated separately.
use comrak::{nodes::NodeValue, parse_document, Arena};
use sha2::{Digest, Sha256};
use std::ops::Range;
use time::Date;

#[derive(Clone, Debug)]
pub struct Target {
    path: String,
    revision: String,
    line: usize,
    checkbox: Range<usize>,
}
#[derive(Clone, Copy, Debug)]
pub enum Change {
    Checked(bool),
    /// View `check` action: toggle the checkbox and set `[done:: YYYY-MM-DD]`
    /// on completion, or remove the inline done date on reopening.
    CheckWithDone {
        checked: bool,
        done: Date,
    },
    Due(Date),
    /// A snooze changes scheduled metadata, preserving the task deadline.
    Scheduled(Date),
}
#[derive(Clone, Debug)]
pub struct Plan {
    pub path: String,
    pub before_revision: String,
    pub after_revision: String,
    pub before: String,
    pub after: String,
}
pub fn revision(source: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(source.as_bytes()))
}
impl Target {
    /// Capture from the exact source snapshot used to display this occurrence.
    pub fn capture(path: &str, source: &str, line: usize) -> Result<Self, String> {
        if path.is_empty()
            || path.contains('\\')
            || path
                .split('/')
                .any(|p| p.is_empty() || matches!(p, "." | ".."))
        {
            return Err("Invalid task source path".into());
        }
        if !crate::tasks::parse(path, source)
            .iter()
            .any(|t| t.line == line)
        {
            return Err("The source line is not a Markdown task".into());
        }
        let range = line_range(source, line).ok_or("Task line is unavailable")?;
        let raw = &source[range.clone()];
        let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
        let bom = source[range.clone()].len() - raw.len();
        let pattern = regex::Regex::new(
            r"^(?:[ \t]*>[ \t]?)*[ \t]*(?:[-+*]|[0-9]{1,9}[.)])[ \t]+\[([ xX])\]",
        )
        .unwrap();
        let status = pattern
            .captures(raw)
            .and_then(|c| c.get(1))
            .ok_or("Unsupported task checkbox syntax")?;
        Ok(Self {
            path: path.into(),
            revision: revision(source),
            line,
            checkbox: range.start + bom + status.start()..range.start + bom + status.end(),
        })
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn source_revision(&self) -> &str {
        &self.revision
    }
    pub fn line(&self) -> usize {
        self.line
    }

    pub fn plan(&self, source: &str, change: Change) -> Result<Plan, String> {
        if revision(source) != self.revision {
            return Err("The note changed, refresh.".into());
        }
        let verified = Self::capture(&self.path, source, self.line)?;
        if verified.checkbox != self.checkbox {
            return Err("The task occurrence changed".into());
        }
        let mut after = source.to_owned();
        match change {
            Change::Checked(checked) => {
                // Preserve uppercase X on an already-complete task.
                if checked != (source.get(self.checkbox.clone()) != Some(" ")) {
                    after.replace_range(self.checkbox.clone(), if checked { "x" } else { " " });
                }
            }
            Change::CheckWithDone { checked, done } => {
                if checked && !(1..=9999).contains(&done.year()) {
                    return Err("Task date is outside the supported calendar".into());
                }
                let range = line_range(source, self.line).ok_or("Task line is unavailable")?;
                let raw = &source[range.clone()];
                let rel_after_box = self.checkbox.end + 1 - range.start;
                let protected = protected_line_spans(source, self.line);
                let markers = done_markers(raw, rel_after_box, &protected);
                if markers.len() > 1 {
                    return Err("The task has more than one done date marker".into());
                }
                if let Some(marker) = markers.first() {
                    Date::parse(
                        marker.value,
                        &time::macros::format_description!("[year]-[month]-[day]"),
                    )
                    .map_err(|_| "The task has malformed done date metadata")?;
                }
                // Mutate inline done date before the checkbox so `self.checkbox` stays valid.
                if checked {
                    let value = format!(
                        "{:04}-{:02}-{:02}",
                        done.year(),
                        u8::from(done.month()),
                        done.day()
                    );
                    match markers.as_slice() {
                        [] => {
                            let trimmed = raw.trim_end_matches([' ', '\t']);
                            let block = regex::Regex::new(r"[ \t]+\^[A-Za-z0-9-]+$").unwrap();
                            let end = block.find(trimmed).map_or(trimmed.len(), |m| m.start());
                            if protected.iter().any(|span| span.contains(&end))
                                || is_escaped_at(raw, end)
                            {
                                return Err(
                                    "Cannot insert done date inside protected Markdown syntax"
                                        .into(),
                                );
                            }
                            after.insert_str(range.start + end, &format!(" [done:: {value}]"));
                        }
                        [marker] => {
                            if marker.value != value {
                                after.replace_range(
                                    range.start + marker.value_span.start
                                        ..range.start + marker.value_span.end,
                                    &value,
                                );
                            }
                        }
                        _ => unreachable!("checked above"),
                    }
                    if source.get(self.checkbox.clone()) == Some(" ") {
                        after.replace_range(self.checkbox.clone(), "x");
                    }
                    let after_range =
                        line_range(&after, self.line).ok_or("Task line is unavailable")?;
                    let after_protected = protected_line_spans(&after, self.line);
                    let after_markers = done_markers(
                        &after[after_range.clone()],
                        self.checkbox.end + 1 - after_range.start,
                        &after_protected,
                    );
                    if after_markers.len() != 1 || after_markers[0].value != value {
                        return Err(
                            "Cannot safely place done date outside protected Markdown syntax"
                                .into(),
                        );
                    }
                } else {
                    if let [marker] = markers.as_slice() {
                        let (rm_start, rm_end) = if marker.span.start > rel_after_box + 1
                            && matches!(raw.as_bytes()[marker.span.start - 1], b' ' | b'\t')
                        {
                            (marker.span.start - 1, marker.span.end)
                        } else if marker.span.start == rel_after_box + 1
                            && matches!(raw.as_bytes().get(marker.span.end), Some(b' ' | b'\t'))
                        {
                            (marker.span.start, marker.span.end + 1)
                        } else {
                            (marker.span.start, marker.span.end)
                        };
                        after.replace_range(range.start + rm_start..range.start + rm_end, "");
                    }
                    if source.get(self.checkbox.clone()) != Some(" ") {
                        after.replace_range(self.checkbox.clone(), " ");
                    }
                }
            }
            Change::Due(date) | Change::Scheduled(date) => {
                let marker = if matches!(change, Change::Scheduled(_)) {
                    '⏳'
                } else {
                    '📅'
                };
                if !(1..=9999).contains(&date.year()) {
                    return Err("Task date is outside the supported calendar".into());
                }
                let value = format!(
                    "{:04}-{:02}-{:02}",
                    date.year(),
                    u8::from(date.month()),
                    date.day()
                );
                let range = line_range(source, self.line).ok_or("Task line is unavailable")?;
                let raw = &source[range.clone()];
                let markers: Vec<_> = raw.match_indices(marker).collect();
                match markers.as_slice() {
                    [] => {
                        let trimmed = raw.trim_end_matches([' ', '\t']);
                        // Keep an Obsidian block ID at the end of its line.
                        let block = regex::Regex::new(r"[ \t]+\^[A-Za-z0-9-]+$").unwrap();
                        let end = block.find(trimmed).map_or(trimmed.len(), |m| m.start());
                        after.insert_str(range.start + end, &format!(" {marker} {value}"));
                    }
                    [(at, _)] => {
                        let pattern = regex::Regex::new(&format!(
                            r"^{marker}[ \t]+([0-9]{{4}}-[0-9]{{2}}-[0-9]{{2}})(?:[ \t]|$)"
                        ))
                        .unwrap();
                        let token = pattern
                            .captures(&raw[*at..])
                            .and_then(|c| c.get(1))
                            .ok_or("The task has malformed date metadata")?;
                        Date::parse(
                            token.as_str(),
                            &time::macros::format_description!("[year]-[month]-[day]"),
                        )
                        .map_err(|_| "The task has an invalid date")?;
                        let span = range.start + at..range.start + at + token.end();
                        if !plain_metadata(source, span) {
                            return Err("Date metadata must be plain task text".into());
                        }
                        after.replace_range(
                            range.start + at + token.start()..range.start + at + token.end(),
                            &value,
                        );
                    }
                    _ => {
                        return Err(
                            "The task has more than one occurrence of this date marker".into()
                        )
                    }
                }
            }
        }
        Ok(Plan {
            path: self.path.clone(),
            before_revision: self.revision.clone(),
            after_revision: revision(&after),
            before: source.into(),
            after,
        })
    }
}
impl Plan {
    /// Guard before passing `before` to the safe editor as an Undo replacement.
    /// The editor must still compare again during the atomic save.
    pub fn undo_source<'a>(&'a self, current: &str) -> Result<&'a str, String> {
        if revision(current) != self.after_revision {
            return Err("This note changed again. Undo cannot replace the newer content.".into());
        }
        Ok(&self.before)
    }
}
fn has_lone_cr(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes
        .iter()
        .enumerate()
        .any(|(i, &b)| b == b'\r' && bytes.get(i + 1) != Some(&b'\n'))
}

fn markdown_line_ranges(text: &str) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' => {
                ranges.push(start..i);
                i += if bytes.get(i + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                start = i;
            }
            b'\n' => {
                ranges.push(start..i);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < bytes.len() {
        ranges.push(start..bytes.len());
    }
    ranges
}

fn line_range(source: &str, line: usize) -> Option<Range<usize>> {
    if line == 0 {
        return None;
    }
    let body = crate::render::without_frontmatter(source);
    let prefix_len = source.len() - body.len();
    let prefix = &source[..prefix_len];
    if has_lone_cr(prefix) {
        return None;
    }
    let prefix_lines = prefix.bytes().filter(|&b| b == b'\n').count();
    let body_line = line.checked_sub(prefix_lines).filter(|&n| n > 0)?;
    let range = markdown_line_ranges(body).into_iter().nth(body_line - 1)?;
    Some(prefix_len + range.start..prefix_len + range.end)
}

fn plain_metadata(source: &str, span: Range<usize>) -> bool {
    let body = crate::render::without_frontmatter(source);
    let prefix = source.len() - body.len();
    let starts: Vec<_> = markdown_line_ranges(body)
        .into_iter()
        .map(|range| range.start)
        .collect();
    let arena = Arena::new();
    let root = parse_document(&arena, body, &crate::render::comrak_options());
    root.descendants().any(|node| {
        let data = node.data.borrow();
        if !matches!(data.value, NodeValue::Text(_))
            || node.ancestors().skip(1).any(|n| {
                matches!(
                    n.data.borrow().value,
                    NodeValue::Link(_) | NodeValue::Image(_)
                )
            })
        {
            return false;
        }
        let p = data.sourcepos;
        let Some(start) = starts.get(p.start.line.saturating_sub(1)) else {
            return false;
        };
        let Some(end) = starts.get(p.end.line.saturating_sub(1)) else {
            return false;
        };
        prefix + start + p.start.column.saturating_sub(1) <= span.start
            && prefix + end + p.end.column >= span.end
    })
}

struct DoneMarker<'a> {
    span: Range<usize>,
    value_span: Range<usize>,
    value: &'a str,
}

fn is_escaped_at(raw: &str, byte_index: usize) -> bool {
    raw[..byte_index]
        .bytes()
        .rev()
        .take_while(|&b| b == b'\\')
        .count()
        % 2
        == 1
}

fn protected_line_spans(source: &str, line: usize) -> Vec<Range<usize>> {
    let body = crate::render::without_frontmatter(source);
    let prefix_lines = source[..source.len() - body.len()]
        .bytes()
        .filter(|&b| b == b'\n')
        .count();
    let Some(body_line) = line.checked_sub(prefix_lines).filter(|&n| n > 0) else {
        return Vec::new();
    };
    let arena = Arena::new();
    let root = parse_document(&arena, body, &crate::render::comrak_options());
    root.descendants()
        .filter_map(|node| {
            let data = node.data.borrow();
            let pos = data.sourcepos;
            if matches!(
                data.value,
                NodeValue::Code(_)
                    | NodeValue::Link(_)
                    | NodeValue::WikiLink(_)
                    | NodeValue::Image(_)
                    | NodeValue::HtmlInline(_)
                    | NodeValue::Math(_)
                    | NodeValue::Raw(_)
            ) && pos.start.line <= body_line
                && pos.end.line >= body_line
            {
                let start = if pos.start.line < body_line {
                    0
                } else {
                    pos.start.column.saturating_sub(1)
                };
                let end = if pos.end.line > body_line {
                    usize::MAX
                } else {
                    pos.end.column
                };
                Some(start..end)
            } else {
                None
            }
        })
        .collect()
}

fn done_markers<'a>(
    raw: &'a str,
    rel_after_box: usize,
    protected: &[Range<usize>],
) -> Vec<DoneMarker<'a>> {
    // Only Dataview inline fields are mutable; compatibility emoji (`✅`) stays read-only.
    static DONE_MARKER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\[([\w-]+)::([^\]\r\n]*)\]|\(([\w-]+)::([^()\r\n]*)\)").unwrap()
    });
    let tail = raw.get(rel_after_box..).unwrap_or("");
    let mut found = Vec::new();
    for cap in DONE_MARKER.captures_iter(tail) {
        let full = cap.get(0).unwrap();
        let abs_start = rel_after_box + full.start();
        let abs_end = rel_after_box + full.end();
        if protected.iter().any(|span| span.contains(&abs_start)) || is_escaped_at(raw, abs_start) {
            continue;
        }
        let Some(key) = cap.get(1).or_else(|| cap.get(3)) else {
            continue;
        };
        let key = key.as_str();
        if !key.eq_ignore_ascii_case("done") && !key.eq_ignore_ascii_case("completion") {
            continue;
        }
        let val_match = cap.get(2).or_else(|| cap.get(4)).unwrap();
        let raw_val = val_match.as_str();
        let trimmed_start = raw_val.len() - raw_val.trim_start_matches([' ', '\t']).len();
        let trimmed = raw_val.trim_matches([' ', '\t']);
        let val_start = rel_after_box + val_match.start() + trimmed_start;
        let val_end = val_start + trimmed.len();
        found.push(DoneMarker {
            span: abs_start..abs_end,
            value_span: val_start..val_end,
            value: trimmed,
        });
    }
    found
}

pub mod write;

#[cfg(test)]
mod tests {
    use super::*;
    const TOMORROW: Date = time::macros::date!(2026 - 10 - 08);
    #[test]
    fn exact_occurrence_preserves_bom_crlf_nested_lists_and_other_metadata() {
        let source = "\u{feff}---\r\ntype: Note\r\n---\r\n- Parent\r\n  - [ ] Привет 📅 2026-10-07 ⏫ ^task\r\n  - [ ] Привет 📅 2026-10-07 ⏫ ^copy\r\n";
        let target = Target::capture("daily/Today.md", source, 5).unwrap();
        let plan = target.plan(source, Change::Due(TOMORROW)).unwrap();
        assert_eq!(plan.after, source.replacen("2026-10-07", "2026-10-08", 1));
        assert_eq!(plan.undo_source(&plan.after).unwrap(), source);
        assert!(plan
            .undo_source(&(plan.after.clone() + "\r\nExternal"))
            .is_err());
        assert!(target
            .plan(&(source.to_owned() + "\r\nExternal"), Change::Checked(true))
            .is_err());
        let checked = target.plan(source, Change::Checked(true)).unwrap();
        assert_eq!(checked.after, source.replacen("[ ]", "[x]", 1));
    }
    #[test]
    fn plain_metadata_only_and_no_guessing() {
        for text in [
            "bad 📅 nope",
            "bad 📅 2026-02-30",
            "two 📅 2026-10-07 📅 2026-10-08",
            "code `📅 2026-10-07`",
            "[📅 2026-10-07](note.md)",
        ] {
            let s = format!("- [ ] {text}\n");
            let t = Target::capture("a.md", &s, 1).unwrap();
            assert!(t.plan(&s, Change::Due(TOMORROW)).is_err(), "{text}");
            assert!(t.plan(&s, Change::Checked(true)).is_ok());
        }
        let s = "```md\n- [ ] example\n```\n";
        assert!(Target::capture("a.md", s, 2).is_err());
        assert!(Target::capture("../a.md", "- [ ] real", 1).is_err());
    }
    #[test]
    fn scheduled_dates_preserve_due_and_refuse_ambiguous_metadata() {
        let s = "- [ ] Task 📅 2026-10-10 ^id\r\n";
        let t = Target::capture("a.md", s, 1).unwrap();
        assert_eq!(
            t.plan(s, Change::Scheduled(TOMORROW)).unwrap().after,
            "- [ ] Task 📅 2026-10-10 ⏳ 2026-10-08 ^id\r\n"
        );
        for metadata in [
            "⏳ nope",
            "⏳ 2026-02-30",
            "⏳ 2026-10-07 ⏳ 2026-10-08",
            "`⏳ 2026-10-07`",
            "[⏳ 2026-10-07](note.md)",
        ] {
            let s = format!("- [ ] Task {metadata}\n");
            let t = Target::capture("a.md", &s, 1).unwrap();
            assert!(
                t.plan(&s, Change::Scheduled(TOMORROW)).is_err(),
                "{metadata}"
            );
        }
    }
    #[test]
    fn insert_before_block_id_and_preserve_uppercase_status() {
        let s = "> 1. [X] Word ^id  \r\n";
        let t = Target::capture("a.md", s, 1).unwrap();
        assert_eq!(
            t.plan(s, Change::Due(TOMORROW)).unwrap().after,
            "> 1. [X] Word 📅 2026-10-08 ^id  \r\n"
        );
        assert_eq!(t.plan(s, Change::Checked(true)).unwrap().after, s);
        assert_eq!(
            t.plan(s, Change::Checked(false)).unwrap().after,
            "> 1. [ ] Word ^id  \r\n"
        );
    }

    #[test]
    fn check_with_done_preserves_bom_crlf_trailing_spaces_block_id_and_other_bytes() {
        let done = time::macros::date!(2026 - 11 - 24);
        let later = time::macros::date!(2026 - 11 - 25);
        let source = "\u{feff}---\r\ntype: questions\r\n---\r\n- Parent\r\n  - [ ] Write view schema [owner:: oleg] 📅 2026-11-25 ⏫ ^blk  \r\n  - [ ] Write view schema [owner:: oleg] 📅 2026-11-25 ⏫ ^copy  \r\n";
        let target = Target::capture("state/dev/okilum/questions.md", source, 5).unwrap();
        let completed = target
            .plan(
                source,
                Change::CheckWithDone {
                    checked: true,
                    done,
                },
            )
            .unwrap();
        let expected = "\u{feff}---\r\ntype: questions\r\n---\r\n- Parent\r\n  - [x] Write view schema [owner:: oleg] 📅 2026-11-25 ⏫ [done:: 2026-11-24] ^blk  \r\n  - [ ] Write view schema [owner:: oleg] 📅 2026-11-25 ⏫ ^copy  \r\n";
        assert_eq!(completed.after.as_bytes(), expected.as_bytes());
        assert_eq!(
            completed.undo_source(&completed.after).unwrap().as_bytes(),
            source.as_bytes()
        );

        // Updating an existing inline done date only replaces the date bytes.
        let target_checked =
            Target::capture("state/dev/okilum/questions.md", &completed.after, 5).unwrap();
        let updated = target_checked
            .plan(
                &completed.after,
                Change::CheckWithDone {
                    checked: true,
                    done: later,
                },
            )
            .unwrap();
        assert_eq!(
            updated.after.as_bytes(),
            expected.replace("2026-11-24", "2026-11-25").as_bytes()
        );

        // Reopening removes the inline done date and restores every byte identically.
        let reopened = target_checked
            .plan(
                &completed.after,
                Change::CheckWithDone {
                    checked: false,
                    done,
                },
            )
            .unwrap();
        assert_eq!(reopened.after.as_bytes(), source.as_bytes());
    }

    #[test]
    fn check_with_done_preserves_readonly_legacy_emoji_code_spans_and_refuses_ambiguous() {
        let done = time::macros::date!(2026 - 11 - 24);
        // Legacy compatibility emoji (`✅`) is read-only: reopening flips only the checkbox.
        let legacy = "- [x] Task ✅ 2026-01-01  \r\n";
        let t = Target::capture("a.md", legacy, 1).unwrap();
        let reopened = t
            .plan(
                legacy,
                Change::CheckWithDone {
                    checked: false,
                    done,
                },
            )
            .unwrap();
        assert_eq!(
            reopened.after.as_bytes(),
            "- [ ] Task ✅ 2026-01-01  \r\n".as_bytes()
        );

        // Reopening removes an inline [done:: ...] while still preserving legacy emoji.
        let legacy_plus_inline = "- [x] Task ✅ 2026-01-01 [done:: 2026-11-24]  \r\n";
        let t = Target::capture("a.md", legacy_plus_inline, 1).unwrap();
        let reopened = t
            .plan(
                legacy_plus_inline,
                Change::CheckWithDone {
                    checked: false,
                    done,
                },
            )
            .unwrap();
        assert_eq!(
            reopened.after.as_bytes(),
            "- [ ] Task ✅ 2026-01-01  \r\n".as_bytes()
        );

        // Code spans, links, and escaped markers are untouched when adding [done:: ...].
        let protected =
            "- [ ] Document `[done:: 2026-01-01]` and [ref [done:: 2026-01-02]](a.md) and \\[done:: 2026-01-03]  \r\n";
        let t = Target::capture("a.md", protected, 1).unwrap();
        let checked = t
            .plan(
                protected,
                Change::CheckWithDone {
                    checked: true,
                    done,
                },
            )
            .unwrap();
        assert_eq!(
            checked.after.as_bytes(),
            b"- [x] Document `[done:: 2026-01-01]` and [ref [done:: 2026-01-02]](a.md) and \\[done:: 2026-01-03] [done:: 2026-11-24]  \r\n"
        );

        // Malformed or duplicate plain inline done markers fail closed.
        for bad in [
            "- [ ] Bad [done:: nope]\n",
            "- [ ] Bad (completion:: 2026-02-30)\n",
            "- [ ] Two [done:: 2026-11-20] [done:: 2026-11-21]\n",
            "- [ ] Two [done:: 2026-11-20] (completion:: 2026-11-21)\n",
        ] {
            let t = Target::capture("a.md", bad, 1).unwrap();
            assert!(
                t.plan(
                    bad,
                    Change::CheckWithDone {
                        checked: true,
                        done,
                    },
                )
                .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn check_with_done_respects_lone_cr_line_boundaries() {
        let done = time::macros::date!(2026 - 11 - 24);
        let source = "- [ ] First\r- [x] Second [done:: 2026-01-01]\n";
        let target = Target::capture("test.md", source, 1).unwrap();
        let plan = target
            .plan(
                source,
                Change::CheckWithDone {
                    checked: true,
                    done,
                },
            )
            .unwrap();
        assert_eq!(
            plan.after.as_bytes(),
            b"- [x] First [done:: 2026-11-24]\r- [x] Second [done:: 2026-01-01]\n"
        );
        assert_eq!(
            plan.undo_source(&plan.after).unwrap().as_bytes(),
            source.as_bytes()
        );
    }

    #[test]
    fn check_with_done_refuses_multiline_code_span_and_link_insertion() {
        let done = time::macros::date!(2026 - 11 - 24);
        for source in [
            "- [ ] Example `code\n  continued`\n",
            "- [ ] [Example\n  label](https://example.org)\n",
            "- [ ] Example trailing escape\\\n  continued\n",
        ] {
            let target = Target::capture("test.md", source, 1).unwrap();
            assert!(
                target
                    .plan(
                        source,
                        Change::CheckWithDone {
                            checked: true,
                            done,
                        },
                    )
                    .is_err(),
                "must refuse inserting done marker inside protected span: {source:?}"
            );
        }
        // Positive control: closed single-line code span still succeeds outside code.
        let control = "- [ ] Example `code [done:: 2026-01-01]`  \r\n";
        let target = Target::capture("test.md", control, 1).unwrap();
        let plan = target
            .plan(
                control,
                Change::CheckWithDone {
                    checked: true,
                    done,
                },
            )
            .unwrap();
        assert_eq!(
            plan.after.as_bytes(),
            b"- [x] Example `code [done:: 2026-01-01]` [done:: 2026-11-24]  \r\n"
        );
        assert_eq!(
            crate::tasks::parse("test.md", &plan.after)[0].done,
            Some(done)
        );
    }
}
