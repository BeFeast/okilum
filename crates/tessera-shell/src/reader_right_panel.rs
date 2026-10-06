//! Right-panel section labels and empty states (#646).
//!
//! Counts appear only when they are above zero. An empty section keeps its
//! header and shows one muted line, never «0 notes · 0 places».

/// Distinct linking notes and link places. `paths` arrive grouped by source
/// note, as `Vault::backlinks` returns them.
pub(super) fn link_counts<'a>(paths: impl IntoIterator<Item = &'a str>) -> (usize, usize) {
    let mut notes = 0;
    let mut places = 0;
    let mut previous: Option<&str> = None;
    for path in paths {
        places += 1;
        if previous != Some(path) {
            notes += 1;
            previous = Some(path);
        }
    }
    (notes, places)
}

/// «Linked from», with «N notes · M places» only when something links here.
/// `partial` (unreadable items) is only worth a word next to real counts; the
/// empty line already says the search covered readable items only.
pub(super) fn linked_from_title(
    scanned: bool,
    notes: usize,
    places: usize,
    partial: bool,
) -> String {
    if !scanned || places == 0 {
        return "Linked from".into();
    }
    let label = format!(
        "Linked from · {notes} {} · {places} {}",
        if notes == 1 { "note" } else { "notes" },
        if places == 1 { "place" } else { "places" }
    );
    if partial {
        format!("{label} · partial")
    } else {
        label
    }
}

/// The one muted line under «Linked from» when it lists nothing.
pub(super) fn linked_from_empty(scanned: bool, complete: bool) -> &'static str {
    if !scanned {
        "Looking for links…"
    } else if complete {
        "No links yet"
    } else {
        "No links yet in readable notes"
    }
}

/// The one muted line under «Contents» when the document has no headings.
pub(super) fn contents_empty(file_preview: bool) -> &'static str {
    if file_preview {
        "No headings in this file"
    } else {
        "No headings yet"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_group_runs_of_one_source() {
        assert_eq!(link_counts([]), (0, 0));
        assert_eq!(link_counts(["a.md", "a.md", "b.md"]), (2, 3));
    }

    #[test]
    fn empty_linked_from_has_no_zero_counts() {
        for partial in [false, true] {
            let title = linked_from_title(true, 0, 0, partial);
            assert_eq!(title, "Linked from");
            assert!(!title.contains('0'));
        }
        assert_eq!(linked_from_title(false, 0, 0, false), "Linked from");
    }

    #[test]
    fn linked_from_counts_when_present() {
        assert_eq!(
            linked_from_title(true, 1, 1, false),
            "Linked from · 1 note · 1 place"
        );
        assert_eq!(
            linked_from_title(true, 2, 5, true),
            "Linked from · 2 notes · 5 places · partial"
        );
        // Counts left over from a scan that has not finished are not shown.
        assert_eq!(linked_from_title(false, 2, 5, false), "Linked from");
    }

    #[test]
    fn empty_lines_are_single_and_count_free() {
        for line in [
            linked_from_empty(false, false),
            linked_from_empty(true, true),
            linked_from_empty(true, false),
            contents_empty(false),
            contents_empty(true),
        ] {
            assert!(!line.is_empty());
            assert!(!line.chars().any(|c| c.is_ascii_digit()));
            assert!(!line.contains('\n'));
        }
        assert_eq!(linked_from_empty(true, true), "No links yet");
    }
}
