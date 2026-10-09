//! Byte ranges belong to the exact string sent to GPUI, including truncation.
use std::ops::Range;

fn floor_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Last defensive boundary before handing a highlight to the platform shaper.
/// Debug builds expose producer bugs; release builds discard/clip bad ranges.
pub(super) fn safe_highlight(text: &str, range: Range<usize>) -> Option<Range<usize>> {
    debug_assert!(range.start <= range.end);
    debug_assert!(text.is_char_boundary(range.start) && text.is_char_boundary(range.end));
    let range = floor_boundary(text, range.start)..floor_boundary(text, range.end);
    (range.start < range.end).then_some(range)
}

pub(super) fn snippet(
    mut text: String,
    highlight: Option<Range<usize>>,
    limit: usize,
) -> (String, Option<Range<usize>>) {
    let end = floor_boundary(&text, limit);
    // Clip to the retained ORIGINAL prefix, before appending an ellipsis.
    // Original offsets must never be interpreted as offsets into the ellipsis.
    let highlight = highlight.and_then(|range| {
        let range = safe_highlight(&text, range)?;
        safe_highlight(&text[..end], range.start.min(end)..range.end.min(end))
    });
    if end < text.len() {
        text.truncate(end);
        text.push('…');
    }
    (text, highlight)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncated_link_never_highlights_inside_ellipsis() {
        // Old producer clipped to 163 AFTER appending a three-byte ellipsis:
        // the old 158..162 link range then split that ellipsis at byte 162.
        let source = format!("{}[[target|сс]] хвост", "я".repeat(79));
        let (original, range) =
            okilum_core::render::strip_inline_markdown_tracking(&source, Some("target"));
        assert_eq!(range, Some(158..162));
        let (text, range) = snippet(original, range, 160);
        assert_eq!(range, Some(158..160));
        assert_eq!(&text[160..], "…");
        assert!(!text.is_char_boundary(162)); // positive control for #398
    }

    #[test]
    fn unicode_links_and_every_truncation_boundary() {
        for original in ["я🧠e\u{301} ссылка хвост", "🧠ссылкаe\u{301}🦀"] {
            let start = original.find("ссылка").unwrap();
            let end = start + "ссылка".len();
            for limit in 0..=original.len() + 3 {
                let (text, range) = snippet(original.into(), Some(start..end), limit);
                if let Some(range) = range {
                    assert!(text.is_char_boundary(range.start));
                    assert!(text.is_char_boundary(range.end));
                    assert!(!text[range].contains('…'));
                }
            }
        }
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn release_clamps_malformed_ranges_without_panicking() {
        let text = "я🧠e\u{301}";
        for start in 0..text.len() + 4 {
            for end in 0..text.len() + 4 {
                if let Some(range) = safe_highlight(text, start..end) {
                    assert!(text.get(range).is_some());
                }
            }
        }
    }
}
