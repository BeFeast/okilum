//! Right-panel section labels and empty states (#646).
//!
//! Counts appear only when they are above zero. An empty section keeps its
//! header and shows one muted line, never «0 notes · 0 places».

use super::*;
#[path = "reader_file_outline.rs"]
pub(super) mod file_outline;

/// Attachment previews share the note outline's typography and independent
/// scroll areas, while unstructured files keep only their backlinks section.
pub(super) fn render_file(
    reader: &Reader,
    outline: Entity<file_outline::View>,
    cx: &mut Context<Reader>,
) -> AnyElement {
    file_outline::View::bind(&outline, reader, cx);
    let p = brand::palette(cx);
    let faint = brand::reader_palette(cx).text_faint;
    let section = |icon: &'static str, label: String| {
        h_flex()
            .flex_none()
            .gap_1p5()
            .px_4()
            .pt_3()
            .pb_1p5()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(faint)
            .child(Icon::default().path(icon).xsmall().text_color(faint))
            .child(label)
    };
    v_flex()
        .w_full()
        .h_full()
        .flex_none()
        .text_size(px(brand::READER_CHROME_FONT_SIZE))
        .when(outline.read(cx).has_entries(), |panel| {
            panel
                .child(
                    section("icons/list.svg", "Contents".into())
                        .debug_selector(|| "file-contents-heading".into()),
                )
                .child(outline)
                .child(
                    div()
                        .flex_none()
                        .h(px(1.))
                        .mx_4()
                        .my_1()
                        .bg(p.border_subtle),
                )
        })
        .child(
            section("icons/link.svg", reader.linked_from_title())
                .debug_selector(|| "file-linked-from-heading".into()),
        )
        .child(reader.render_backlinks(cx))
        .into_any_element()
}

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

/// The panel with no note open.
pub(super) const NO_NOTE: &str = "Open a note to see its headings and the notes that link to it.";

/// Empty sections remain quiet, including while inventory is pending.
pub(super) fn linked_from_empty(_scanned: bool, _complete: bool) -> &'static str {
    "—"
}

/// The same quiet empty state applies to notes and attachment previews.
pub(super) fn contents_empty(_file_preview: bool) -> &'static str {
    "—"
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

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
        assert_eq!(linked_from_empty(true, true), "—");
    }
}
