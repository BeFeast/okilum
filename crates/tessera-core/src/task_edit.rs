//! Revision-bound task edits. Produces bytes only; callers must commit through
//! the platform's safe editor and retain the preimage for revision-checked Undo.
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
    Due(Date),
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
            return Err("This note changed. Refresh the dashboard before editing the task.".into());
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
            Change::Due(date) => {
                if !(1..=9999).contains(&date.year()) {
                    return Err("Due date is outside the supported calendar".into());
                }
                let value = format!(
                    "{:04}-{:02}-{:02}",
                    date.year(),
                    u8::from(date.month()),
                    date.day()
                );
                let range = line_range(source, self.line).ok_or("Task line is unavailable")?;
                let raw = &source[range.clone()];
                let markers: Vec<_> = raw.match_indices('📅').collect();
                match markers.as_slice() {
                    [] => {
                        let trimmed = raw.trim_end_matches([' ', '\t']);
                        // Keep an Obsidian block ID at the end of its line.
                        let block = regex::Regex::new(r"[ \t]+\^[A-Za-z0-9-]+$").unwrap();
                        let end = block.find(trimmed).map_or(trimmed.len(), |m| m.start());
                        after.insert_str(range.start + end, &format!(" 📅 {value}"));
                    }
                    [(at, _)] => {
                        let pattern =
                            regex::Regex::new(r"^📅[ \t]+([0-9]{4}-[0-9]{2}-[0-9]{2})(?:[ \t]|$)")
                                .unwrap();
                        let token = pattern
                            .captures(&raw[*at..])
                            .and_then(|c| c.get(1))
                            .ok_or("The task has malformed due metadata")?;
                        Date::parse(
                            token.as_str(),
                            &time::macros::format_description!("[year]-[month]-[day]"),
                        )
                        .map_err(|_| "The task has an invalid due date")?;
                        let span = range.start + at..range.start + at + token.end();
                        if !plain_metadata(source, span) {
                            return Err("Due metadata must be plain task text".into());
                        }
                        after.replace_range(
                            range.start + at + token.start()..range.start + at + token.end(),
                            &value,
                        );
                    }
                    _ => return Err("The task has more than one due date".into()),
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
fn line_range(source: &str, line: usize) -> Option<Range<usize>> {
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
}
