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
        if has_lone_cr(source) {
            return Err("Unsupported lone carriage return in note".into());
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
        let checkbox = range.start + bom + status.start()..range.start + bom + status.end();
        let checkbox_brackets =
            (checkbox.start - range.start).saturating_sub(1)..(checkbox.end - range.start) + 1;
        let protected = writer_protected_line_spans(source, line);
        if protected
            .iter()
            .any(|span| range_overlaps(span, &checkbox_brackets))
        {
            return Err("The task checkbox is inside protected Markdown syntax".into());
        }
        Ok(Self {
            path: path.into(),
            revision: revision(source),
            line,
            checkbox,
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
        if has_lone_cr(source) {
            return Err("Unsupported lone carriage return in note".into());
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
                let before_tasks = crate::tasks::parse(&self.path, source);
                let before_pos = before_tasks
                    .iter()
                    .position(|t| t.line == self.line)
                    .ok_or("The source line is not a Markdown task")?;
                let range = line_range(source, self.line).ok_or("Task line is unavailable")?;
                let raw = &source[range.clone()];
                let rel_after_box = self.checkbox.end + 1 - range.start;
                let protected = writer_protected_line_spans(source, self.line);
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
                let mut inline_edit: Option<(Range<usize>, usize)> = None;
                let value = format!(
                    "{:04}-{:02}-{:02}",
                    done.year(),
                    u8::from(done.month()),
                    done.day()
                );
                if checked {
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
                            let inserted = format!(" [done:: {value}]");
                            let at = range.start + end;
                            after.insert_str(at, &inserted);
                            inline_edit = Some((at..at, inserted.len()));
                        }
                        [marker] => {
                            if marker.value != value {
                                let span = range.start + marker.value_span.start
                                    ..range.start + marker.value_span.end;
                                after.replace_range(span.clone(), &value);
                                inline_edit = Some((span, value.len()));
                            }
                        }
                        _ => unreachable!("checked above"),
                    }
                    if source.get(self.checkbox.clone()) == Some(" ") {
                        after.replace_range(self.checkbox.clone(), "x");
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
                        let span = range.start + rm_start..range.start + rm_end;
                        after.replace_range(span.clone(), "");
                        inline_edit = Some((span, 0));
                    }
                    if source.get(self.checkbox.clone()) != Some(" ") {
                        after.replace_range(self.checkbox.clone(), " ");
                    }
                }
                verify_check_with_done_postcondition(
                    &self.path,
                    self.line,
                    &self.checkbox,
                    source,
                    &after,
                    inline_edit,
                    &before_tasks,
                    before_pos,
                    checked,
                    done,
                    &value,
                )?;
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

fn line_range(source: &str, line: usize) -> Option<Range<usize>> {
    if line == 0 || has_lone_cr(source) {
        return None;
    }
    let mut offset = 0;
    for (i, raw) in source.split_inclusive('\n').enumerate() {
        if i + 1 == line {
            let text = raw.strip_suffix('\n').unwrap_or(raw);
            let text = text.strip_suffix('\r').unwrap_or(text);
            return Some(offset..offset + text.len());
        }
        offset += raw.len();
    }
    None
}

fn plain_metadata(source: &str, span: Range<usize>) -> bool {
    let body = crate::render::without_frontmatter(source);
    let prefix = source.len() - body.len();
    let starts: Vec<_> = std::iter::once(0)
        .chain(body.match_indices('\n').map(|(i, _)| i + 1))
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

/// Stricter writer-side protected-span set: parses with dollar math enabled and
/// combines AST spans (code, links, wikilinks, images, inline HTML, math, raw)
/// with Obsidian `%%…%%` comment spans so edits never modify protected content.
fn writer_protected_line_spans(source: &str, line: usize) -> Vec<Range<usize>> {
    let body = crate::render::without_frontmatter(source);
    let prefix_len = source.len() - body.len();
    let prefix_lines = source[..prefix_len].bytes().filter(|&b| b == b'\n').count();
    let Some(body_line) = line.checked_sub(prefix_lines).filter(|&n| n > 0) else {
        return Vec::new();
    };
    let Some(abs_range) = line_range(source, line) else {
        return Vec::new();
    };
    let body_line_range =
        abs_range.start.saturating_sub(prefix_len)..abs_range.end.saturating_sub(prefix_len);
    let mut options = crate::render::comrak_options();
    options.extension.math_dollars = true;
    let arena = Arena::new();
    let root = parse_document(&arena, body, &options);
    let mut spans: Vec<Range<usize>> = root
        .descendants()
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
        .collect();
    for comment in crate::obsidian::comment_spans(body) {
        if comment.end <= body_line_range.start || comment.start >= body_line_range.end {
            continue;
        }
        let start = comment.start.saturating_sub(body_line_range.start);
        let end = if comment.end > body_line_range.end {
            usize::MAX
        } else {
            comment.end - body_line_range.start
        };
        spans.push(start..end);
    }
    spans
}

struct DoneMarker<'a> {
    span: Range<usize>,
    value_span: Range<usize>,
    value: &'a str,
}

fn range_overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
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

fn has_plain_legacy_done_emoji(
    raw: &str,
    rel_after_box: usize,
    protected: &[Range<usize>],
) -> bool {
    let tail = raw.get(rel_after_box..).unwrap_or("");
    tail.match_indices('✅').any(|(at, _)| {
        let abs = rel_after_box + at;
        !protected.iter().any(|span| span.contains(&abs)) && !is_escaped_at(raw, abs)
    })
}

#[allow(clippy::too_many_arguments)]
fn verify_check_with_done_postcondition(
    path: &str,
    line: usize,
    checkbox: &Range<usize>,
    source: &str,
    after: &str,
    inline_edit: Option<(Range<usize>, usize)>,
    before_tasks: &[crate::tasks::Task],
    before_pos: usize,
    checked: bool,
    done: Date,
    done_value: &str,
) -> Result<(), String> {
    // 1. Byte-level invariant: every byte outside the checkbox char and the single
    //    inline done-field edit must remain identical to `source`.
    let bytes_preserved = match inline_edit {
        None => {
            after.len() == source.len()
                && after.as_bytes()[..checkbox.start] == source.as_bytes()[..checkbox.start]
                && after.as_bytes()[checkbox.end..] == source.as_bytes()[checkbox.end..]
        }
        Some((span, replacement_len)) => {
            span.start >= checkbox.end
                && span.end <= source.len()
                && after.len() == source.len() - (span.end - span.start) + replacement_len
                && after.as_bytes()[..checkbox.start] == source.as_bytes()[..checkbox.start]
                && after.as_bytes()[checkbox.end..span.start]
                    == source.as_bytes()[checkbox.end..span.start]
                && after.as_bytes()[span.start + replacement_len..] == source.as_bytes()[span.end..]
        }
    };
    if !bytes_preserved {
        return Err("Task edit violated byte-preservation invariant".into());
    }

    // 2. Inline marker and checkbox invariants on the edited line (under writer protection).
    let after_range = line_range(after, line).ok_or("Task line is unavailable")?;
    let after_raw = &after[after_range.clone()];
    let after_rel = checkbox.end + 1 - after_range.start;
    let after_protected = writer_protected_line_spans(after, line);
    let after_checkbox_brackets = (checkbox.start - after_range.start).saturating_sub(1)
        ..(checkbox.end - after_range.start) + 1;
    if after_protected
        .iter()
        .any(|span| range_overlaps(span, &after_checkbox_brackets))
    {
        return Err("The task checkbox is inside protected Markdown syntax".into());
    }
    let after_markers = done_markers(after_raw, after_rel, &after_protected);
    if checked {
        if after_markers.len() != 1 || after_markers[0].value != done_value {
            return Err("Cannot safely place done date outside protected Markdown syntax".into());
        }
    } else if !after_markers.is_empty() {
        return Err("Could not cleanly remove inline done date metadata".into());
    }

    // 3. Re-projection invariant with the same Tasks projection used by views.
    let after_tasks = crate::tasks::parse(path, after);
    if after_tasks.len() != before_tasks.len() {
        return Err("Task edit would change the note's task structure".into());
    }
    for (idx, (before_t, after_t)) in before_tasks.iter().zip(after_tasks.iter()).enumerate() {
        if idx != before_pos && before_t != after_t {
            return Err("Task edit would alter another task in the note".into());
        }
    }
    let before_task = &before_tasks[before_pos];
    let after_task = &after_tasks[before_pos];
    if after_task.path != before_task.path
        || after_task.line != before_task.line
        || after_task.body_line != before_task.body_line
        || after_task.checked != checked
        || after_task.metadata.text != before_task.metadata.text
        || after_task.due != before_task.due
        || after_task.scheduled != before_task.scheduled
        || after_task.start != before_task.start
        || after_task.priority != before_task.priority
        || after_task.metadata.owner != before_task.metadata.owner
        || after_task.metadata.thread != before_task.metadata.thread
        || after_task.metadata.created != before_task.metadata.created
        || after_task.metadata.other != before_task.metadata.other
    {
        return Err(
            "Task edit would change projected fields other than checkbox and done date".into(),
        );
    }
    let has_done_issue = after_task
        .metadata
        .issues
        .iter()
        .any(|issue| issue.key == "done");
    let has_duplicate_done = after_task.metadata.issues.iter().any(|issue| {
        issue.key == "done" && issue.kind == crate::tasks::MetadataIssueKind::Duplicate
    });
    if checked {
        if after_task.done != Some(done) || has_done_issue {
            return Err(
                "Cannot set done date without conflicting with existing read-only metadata".into(),
            );
        }
    } else if has_plain_legacy_done_emoji(after_raw, after_rel, &after_protected) {
        if after_task.done != before_task.done || has_duplicate_done {
            return Err("Cannot reopen task cleanly alongside read-only done metadata".into());
        }
    } else if after_task.done.is_some() || has_done_issue {
        return Err("Cannot clear done date cleanly without modifying read-only metadata".into());
    }
    Ok(())
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
    fn check_with_done_refuses_lone_cr_notes_without_writing() {
        for (source, line) in [
            ("- [ ] First\r- [x] Second [done:: 2026-01-01]\n", 1),
            ("- [ ] First\r- [ ] Second\n- [ ] Third\n", 1),
            ("- [ ] First\r- [ ] Second\n- [ ] Third\n", 2),
            ("- [ ] First\r- [ ] Second\n- [ ] Third\n", 3),
            ("- [ ] Task\nOther\rLine\n", 1),
        ] {
            assert!(
                Target::capture("test.md", source, line).is_err(),
                "must refuse note containing lone CR: {source:?} line {line}"
            );
        }
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

    #[test]
    fn read_projection_matches_main_and_writer_refuses_protected_or_readonly_conflicts() {
        let done = time::macros::date!(2026 - 11 - 24);
        let jan1 = time::macros::date!(2026 - 01 - 01);

        // 1. READ projection equals main byte-for-byte outside commented checkboxes:
        let solve = crate::tasks::parse("test.md", "- [ ] Solve $x$ [owner:: oleg]\n");
        assert_eq!(solve.len(), 1);
        assert_eq!(
            solve[0].display.as_deref(),
            Some("Solve $x$ [owner:: oleg]")
        );
        assert_eq!(solve[0].metadata.owner.as_deref(), Some("oleg"));
        assert_eq!(solve[0].metadata.text, "Solve $x$");

        let eq = crate::tasks::parse("test.md", "- [ ] Equation $[owner:: oleg]$\n");
        assert_eq!(eq.len(), 1);
        assert_eq!(eq[0].metadata.owner.as_deref(), Some("oleg"));

        let html_owner = crate::tasks::parse(
            "test.md",
            "- [ ] <span title=\"[owner:: oleg]\">Task</span>\n",
        );
        assert_eq!(html_owner.len(), 1);
        assert_eq!(html_owner[0].metadata.owner.as_deref(), Some("oleg"));

        let comment_done =
            crate::tasks::parse("test.md", "- [ ] Actual %% [done:: 2026-01-01] %%\n");
        assert_eq!(comment_done.len(), 1);
        assert_eq!(comment_done[0].done, Some(jan1));
        assert_eq!(comment_done[0].metadata.text, "Actual %% %%");

        // Heading titles and anchors match main identically.
        let schema = std::sync::Arc::new(
            crate::records::FieldSchema::load(std::path::Path::new(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/views"
            )))
            .unwrap(),
        );
        let heading_cases = [
            (
                "knowledge/link-in-dollars.md",
                "# $[label](a.md)$\n",
                "$label$",
            ),
            (
                "knowledge/entity-in-dollars.md",
                "# $a &amp; b$\n",
                "$a & b$",
            ),
            ("knowledge/code-in-dollars.md", "# $`code`$\n", "$code$"),
            ("knowledge/escape-in-dollars.md", "# $\\$x$\n", "$$x$"),
            ("knowledge/solve.md", "# Solve $x^2$\n", "Solve $x^2$"),
            ("knowledge/energy.md", "# $E=mc^2$\n", "$E=mc^2$"),
            ("knowledge/money.md", "# $2$ a month\n", "$2$ a month"),
            ("knowledge/display.md", "# $$a+b$$\n", "$$a+b$$"),
        ];
        let sources: Vec<_> = heading_cases
            .iter()
            .map(|(path, body, _)| crate::records::Source {
                path,
                bytes: body.as_bytes(),
                mtime: None,
            })
            .collect();
        let projection = crate::records::Projection::build(schema, sources);
        for (path, _, expected) in heading_cases {
            let note = projection.note(path).unwrap();
            assert_eq!(
                note.fields.get("title"),
                Some(&crate::records::Value::String(expected.to_owned())),
                "{path}"
            );
            assert_eq!(note.sections[0].anchor, expected, "{path}");
        }

        // 2. Ordinary math without protected done metadata still completes cleanly.
        let ordinary_math = "- [ ] Solve $x^2$\n";
        let plan = Target::capture("test.md", ordinary_math, 1)
            .unwrap()
            .plan(
                ordinary_math,
                Change::CheckWithDone {
                    checked: true,
                    done,
                },
            )
            .unwrap();
        assert_eq!(
            plan.after.as_bytes(),
            b"- [x] Solve $x^2$ [done:: 2026-11-24]\n"
        );

        // 3. Writer refuses any edit that would touch protected math/HTML/comment content
        //    or leave the main projection with a conflicting/duplicate done date.
        for source in [
            "- [ ] Review $[done:: 2026-01-01]$\n",
            "- [ ] Review $$[done:: 2026-01-01]$$\n",
            "- [ ] Review $(completion:: 2026-01-01)$\n",
            "- Parent\n\t- [ ] $[done:: 2026-01-01]$\n",
            "> - [ ] $$[done:: 2026-01-01]$$\n",
            "\u{feff}- [ ] $[done:: 2026-01-01]$\n",
            "- [ ] Example $a +\n  b$\n",
            "- [ ] <!-- [done:: 2026-01-01] --> Task\n",
            "- [ ] <span title=\"[done:: 2026-01-01]\">Task</span>\n",
            "- [ ] Actual %% [done:: 2026-01-01] %%\n",
            "- [ ] Actual %% hidden\n  still hidden %%\n",
            "- [ ] Actual %% [done:: 2026-01-01]\n  still hidden %%\n",
            "- [ ] Task ✅ 2026-01-01\n",
            "- [ ] Task ✅ nope\n",
            "- [ ] Task ✅ 2026-01-01 [done:: 2026-01-02]\n",
            "- [ ] Task ✅ 2026-11-24\n",
        ] {
            let line = crate::tasks::parse("test.md", source)[0].line;
            let target = Target::capture("test.md", source, line).unwrap();
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
                "must refuse protected/conflicting completion: {source:?}"
            );
        }

        // 4. Checkboxes inside `%%…%%` comments are hidden from Tasks projection and
        //    refused by Target::capture (cannot be reopened or completed).
        let hidden_checked = "%%\n- [x] Hidden\n%%\n";
        assert!(crate::tasks::parse("test.md", hidden_checked).is_empty());
        assert!(Target::capture("test.md", hidden_checked, 2).is_err());

        let split_comment = "- [ ] A %%\n- [ ] C %%\n";
        let split_tasks = crate::tasks::parse("test.md", split_comment);
        assert_eq!(split_tasks.len(), 1);
        assert_eq!(split_tasks[0].line, 1);
        assert!(Target::capture("test.md", split_comment, 2).is_err());
        let target_a = Target::capture("test.md", split_comment, 1).unwrap();
        assert!(target_a
            .plan(
                split_comment,
                Change::CheckWithDone {
                    checked: true,
                    done,
                },
            )
            .is_err());
    }
}
