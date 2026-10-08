//! Plain Tasks reminder formatting. Publication/Undo belong to the safe writer.
use time::Date;

/// Format one unchecked task from visible sentence text and a canonical
/// vault-relative Markdown path. The caller validates heading uniqueness against
/// the captured source; unrepresentable headings fall back to the note itself.
/// Return an error rather than silently change Tasks metadata or link identity.
pub fn format(
    sentence: &str,
    source: &str,
    heading: Option<&str>,
    due: Date,
) -> Result<String, &'static str> {
    if source.is_empty()
        || !source.to_lowercase().ends_with(".md")
        || source
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
        || source
            .chars()
            .any(|c| c.is_control() || "\\[]#|%^:".contains(c))
    {
        return Err("This note path cannot be represented as an unambiguous reminder link.");
    }
    let text = sentence.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() || text.chars().any(char::is_control) {
        return Err("Choose a sentence for the reminder.");
    }
    let mut escaped = String::new();
    for c in text.chars() {
        if "\\[]*_`<>!&".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    let fragment = heading
        .filter(|s| !s.trim().is_empty())
        .filter(|s| !s.chars().any(|c| c.is_control() || "\\[]#|%^".contains(c)))
        .map(|s| format!("#{s}"))
        .unwrap_or_default();
    let line = format!("- [ ] {escaped} [[{source}{fragment}]] 📅 {due}\n");
    let parsed = crate::tasks::parse("Reminders.md", &line);
    if parsed.len() != 1
        || parsed[0].checked
        || parsed[0].due != Some(due)
        || parsed[0].scheduled.is_some()
        || parsed[0].start.is_some()
        || parsed[0].done.is_some()
        || parsed[0].priority != 3
        || line.matches('📅').count() != 1
    {
        return Err("The reminder text contains Tasks metadata; edit the text before adding it.");
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn unicode_sentence_and_heading_round_trip_as_one_task() {
        let line = format(
            "  Перевести часы\n  ב־1 בנובמבר  ",
            "Calendar/DST.md",
            Some("Winter time"),
            date!(2026 - 11 - 01),
        )
        .unwrap();
        assert_eq!(
            line,
            "- [ ] Перевести часы ב־1 בנובמבר [[Calendar/DST.md#Winter time]] 📅 2026-11-01\n"
        );
        let task = crate::tasks::parse("Reminders.md", &line).remove(0);
        assert!(!task.checked);
        assert_eq!(task.due, Some(date!(2026 - 11 - 01)));
    }

    #[test]
    fn visible_text_cannot_inject_markdown_task_structure() {
        let line = format(
            "Review [x](evil)\n- [ ] *second*",
            "Start.md",
            None,
            date!(2026 - 11 - 01),
        )
        .unwrap();
        assert!(line.contains(r"Review \[x\](evil) - \[ \] \*second\*"));
        assert_eq!(crate::tasks::parse("Reminders.md", &line).len(), 1);
        let literal = format(
            "Keep &amp; literal",
            "Start.md",
            None,
            date!(2026 - 11 - 01),
        )
        .unwrap();
        assert!(literal.contains(r"Keep \&amp; literal"));
    }

    #[test]
    fn metadata_in_sentence_or_path_never_overrides_chosen_date() {
        for text in [
            "Old 📅 2025-01-01",
            "Schedule ⏳ 2026-10-30",
            "Done ✅ 2026-10-01",
            "High ⏫ priority",
            "Date 📅 unknown",
        ] {
            assert!(
                format(text, "Start.md", None, date!(2026 - 11 - 01)).is_err(),
                "{text}"
            );
        }
        assert!(format("Review", "📅 2025-01-01.md", None, date!(2026 - 11 - 01)).is_err());
    }

    #[test]
    fn unsafe_paths_refuse_and_unrepresentable_headings_use_note_link() {
        for path in [
            "../Start.md",
            "/Start.md",
            "a//b.md",
            "a/./b.md",
            "C:\\Start.md",
            "A#B.md",
            "A|B.md",
            "A%20B.md",
            "A\nB.md",
            "",
        ] {
            assert!(
                format("Review", path, None, date!(2026 - 11 - 01)).is_err(),
                "{path}"
            );
        }
        let line = format(
            "Review",
            "Folder/שלום.md",
            Some("Ambiguous # fragment"),
            date!(2026 - 11 - 01),
        )
        .unwrap();
        assert!(line.contains("[[Folder/שלום.md]]"));
        assert!(format(" \n ", "Start.md", None, date!(2026 - 11 - 01)).is_err());
    }
}
