//! Lossless reminder insertion plans, checked again by the writer under its lock.
//! The plan performs no IO; `write` applies it through the guarded editor.

#[cfg(any(unix, windows))]
pub mod write;

#[derive(Clone, Debug)]
pub struct Plan {
    before: String,
    after: String,
}

impl Plan {
    /// Append one already-formatted reminder, preserving every existing byte.
    /// Verify the resulting Markdown actually exposes that task, rather than
    /// swallowing it in an unclosed fence, frontmatter or HTML block.
    pub fn new(source: &str, reminder: &str) -> Result<Self, &'static str> {
        let line = reminder.strip_suffix('\n').unwrap_or(reminder);
        if !line.starts_with("- [ ] ") || line.contains(['\r', '\n']) {
            return Err("Expected one formatted reminder task.");
        }
        let expected = crate::tasks::parse("Reminders.md", reminder);
        if expected.len() != 1 || expected[0].checked || expected[0].due.is_none() {
            return Err("Expected one unchecked reminder with a due date.");
        }
        let newline = if source.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let mut after = source.to_owned();
        if !after.is_empty() {
            if !after.ends_with('\n') {
                after.push_str(newline);
            }
            // A blank separator ends ordinary paragraphs/lists without changing
            // the source preimage; non-ordinary open blocks are refused below.
            after.push_str(newline);
        }
        let inserted_line = after.bytes().filter(|&b| b == b'\n').count() + 1;
        after.push_str(line);
        after.push_str(newline);
        let before_tasks = crate::tasks::parse("Reminders.md", source);
        let after_tasks = crate::tasks::parse("Reminders.md", &after);
        if after_tasks.len() != before_tasks.len() + 1
            || after_tasks[..before_tasks.len()] != before_tasks
        {
            return Err(
                "The reminder would change existing Markdown tasks. Close the open block first.",
            );
        }
        let inserted = after_tasks.last().unwrap();
        if inserted.line != inserted_line
            || inserted.text != expected[0].text
            || inserted.due != expected[0].due
            || inserted.checked
        {
            return Err("The reminder is not a standalone task at this location.");
        }
        Ok(Self {
            before: source.into(),
            after,
        })
    }

    /// Call only after opening the existing destination with FileEditor. The
    /// writer must retain its normal compare-before-save and recovery semantics.
    pub fn apply_source<'a>(&'a self, current: &str) -> Result<&'a str, &'static str> {
        if current != self.before {
            return Err("The reminders note changed. Refresh before adding the reminder.");
        }
        Ok(&self.after)
    }

    /// Only a successful-save receipt may expose this as user-facing Undo.
    pub fn undo_source<'a>(&'a self, current: &str) -> Result<&'a str, &'static str> {
        if current != self.after {
            return Err("The reminders note changed again. Undo cannot replace newer content.");
        }
        Ok(&self.before)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const TASK: &str = "- [ ] Review [[Start.md]] 📅 2026-11-01\n";

    #[test]
    fn append_and_undo_preserve_bom_crlf_and_missing_final_newline() {
        for source in [
            "",
            "# Reminders",
            "# Reminders\n",
            "\u{feff}---\r\ntype: Note\r\n---\r\n# תזכורות\r\n",
            "- [ ] Older 📅 2026-10-08",
        ] {
            let plan = Plan::new(source, TASK).unwrap();
            let after = plan.apply_source(source).unwrap();
            assert!(after.starts_with(source));
            assert_eq!(plan.undo_source(after).unwrap(), source);
            if source.contains("\r\n") {
                assert!(after.ends_with("📅 2026-11-01\r\n"));
            }
        }
    }

    #[test]
    fn refuses_hidden_task_in_open_markdown_blocks() {
        for source in [
            "```rust\nexample",
            "~~~\nexample\n",
            "<!-- open comment\n",
            "<script>\n",
        ] {
            assert!(Plan::new(source, TASK).is_err(), "{source}");
        }
        assert!(
            Plan::new("```rust\nexample\n```\n", TASK).is_ok(),
            "closed fence positive control"
        );
    }

    #[test]
    fn stale_apply_and_undo_do_not_search_for_matching_task_text() {
        let source = TASK;
        let plan = Plan::new(source, TASK).unwrap();
        assert!(plan.apply_source(&format!("{source}external\n")).is_err());
        let after = plan.apply_source(source).unwrap();
        assert_eq!(crate::tasks::parse("Reminders.md", after).len(), 2);
        assert!(plan.undo_source(&format!("{after}external\n")).is_err());
        assert_eq!(plan.undo_source(after).unwrap(), source);
    }

    #[test]
    fn refuses_multiple_lines_and_non_reminder_input() {
        for line in [
            "plain text",
            "- [ ] Missing date\n",
            "- [x] Done 📅 2026-11-01\n",
            "- [ ] First 📅 2026-11-01\n- [ ] Extra\n",
        ] {
            assert!(Plan::new("", line).is_err(), "{line}");
        }
    }
}
