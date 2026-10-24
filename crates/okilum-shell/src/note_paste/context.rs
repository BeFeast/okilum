//! Where a paste lands in the note's source (#1611): inside a fenced code
//! block or not, and how multi-line or block output fits the caret's line (a
//! list item continues indented, a blockquote keeps its `> ` markers).
//! Platform-neutral and pure.

use std::ops::Range;

/// Whether `offset` is inside a fenced code block (``` or ~~~), on one of its
/// content lines. The opening and closing fence lines are outside.
pub fn in_fenced_code(source: &str, offset: usize) -> bool {
    open_fence(source, offset).is_some()
}

/// The start of the opening fence line of the fenced code block `offset` is
/// inside, if any.
fn open_fence(source: &str, offset: usize) -> Option<usize> {
    let offset = offset.min(source.len());
    let mut open: Option<(u8, usize, usize)> = None;
    let mut line_start = 0;
    for line in source.split_inclusive('\n') {
        let line_end = line_start + line.trim_end_matches(['\n', '\r']).len();
        let fence = fence_marker(line);
        if offset <= line_end {
            return match (open, fence) {
                (Some((ch, len, at)), Some((fch, flen, rest_empty))) => {
                    (!(fch == ch && flen >= len && rest_empty)).then_some(at)
                }
                (Some((_, _, at)), None) => Some(at),
                (None, _) => None,
            };
        }
        match (open, fence) {
            (None, Some((ch, len, _))) => open = Some((ch, len, line_start)),
            (Some((ch, len, _)), Some((fch, flen, rest_empty)))
                if fch == ch && flen >= len && rest_empty =>
            {
                open = None
            }
            _ => {}
        }
        line_start += line.len();
    }
    // The caret after a final newline is on an empty last line.
    open.map(|(_, _, at)| at)
}

/// What every line of the fenced code block at `offset` starts with: its
/// opening fence's quote markers and indentation (a fence in a blockquote, a
/// callout or a list item). `None` outside a fence, for a fence with no
/// such prefix, or when the caret's line does not carry it.
pub fn code_block_prefix(source: &str, offset: usize) -> Option<String> {
    let offset = offset.min(source.len());
    let at = open_fence(source, offset)?;
    let line = source[at..].lines().next().unwrap_or_default();
    let prefix = &line[..line.find(['`', '~'])?];
    let caret_line = &source[source[..offset].rfind('\n').map_or(0, |i| i + 1)..offset];
    (!prefix.is_empty() && caret_line.starts_with(prefix)).then(|| prefix.to_string())
}

/// `text` with `prefix` in front of every line after the first (an empty
/// line gets the prefix without trailing spaces). Nothing else changes.
pub fn continue_lines(text: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
            if line.is_empty() || line == "\r" {
                out.push_str(prefix.trim_end());
            } else {
                out.push_str(prefix);
            }
        }
        out.push_str(line);
    }
    out
}

/// A fence on this line: its character, run length and whether nothing but
/// whitespace follows the run (a closing fence). Quote markers and up to
/// three spaces of indentation (more inside a list) are skipped.
fn fence_marker(line: &str) -> Option<(u8, usize, bool)> {
    let body = strip_quote_markers(line.trim_end_matches(['\n', '\r']));
    let body = body.trim_start_matches([' ', '\t']);
    let ch = *body.as_bytes().first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let len = body.bytes().take_while(|b| *b == ch).count();
    if len < 3 {
        return None;
    }
    let rest = &body[len..];
    // A backtick fence's info string never holds a backtick.
    if ch == b'`' && rest.contains('`') {
        return None;
    }
    Some((ch, len, rest.trim().is_empty()))
}

fn strip_quote_markers(line: &str) -> &str {
    let mut rest = line;
    loop {
        let trimmed = rest.trim_start_matches(' ');
        if let Some(after) = trimmed.strip_prefix('>') {
            rest = after.strip_prefix(' ').unwrap_or(after);
        } else {
            return rest;
        }
    }
}

/// The prefix a continuation line needs at the caret's line: the line's
/// quote markers, then spaces for its list marker (or its own indentation).
/// `None` when the caret sits before those markers.
pub fn continuation(line_before_caret: &str, whole_line: &str) -> Option<String> {
    let (pad, markers) = line_prefix(whole_line);
    (line_before_caret.len() >= markers).then_some(pad)
}

/// A line's continuation prefix and the byte length of the markers it
/// stands for (quote markers, indentation, list marker and task box).
fn line_prefix(whole_line: &str) -> (String, usize) {
    // Quote markers, kept verbatim ("> > ").
    let mut quote_len = 0;
    loop {
        let rest = &whole_line[quote_len..];
        let spaces = rest.len() - rest.trim_start_matches(' ').len();
        let after = &rest[spaces..];
        if spaces <= 3 && after.starts_with('>') {
            quote_len += spaces + 1;
            if whole_line[quote_len..].starts_with(' ') {
                quote_len += 1;
            }
        } else {
            break;
        }
    }
    let quote = &whole_line[..quote_len];
    let body = &whole_line[quote_len..];
    let indent = body.len() - body.trim_start_matches([' ', '\t']).len();
    let marker = list_marker_len(&body[indent..]).unwrap_or(0);
    let pad = format!("{quote}{}{}", &body[..indent], " ".repeat(marker));
    (pad, quote_len + indent + marker)
}

/// The byte length of a list marker and its spaces (`- `, `1. `, `- [ ] `).
fn list_marker_len(body: &str) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut at = match bytes.first()? {
        b'-' | b'*' | b'+' => 1,
        b'0'..=b'9' => {
            let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
            if digits > 9 || !matches!(bytes.get(digits), Some(b'.' | b')')) {
                return None;
            }
            digits + 1
        }
        _ => return None,
    };
    let spaces = bytes[at..].iter().take_while(|b| **b == b' ').count();
    if spaces == 0 && at < bytes.len() {
        return None;
    }
    at += spaces.max(usize::from(at == bytes.len()));
    let at = at.min(bytes.len());
    // A task box belongs to the marker.
    let rest = &body[at..];
    if rest.len() >= 3
        && rest.as_bytes()[0] == b'['
        && rest.as_bytes()[2] == b']'
        && matches!(rest.as_bytes()[1], b' ' | b'x' | b'X')
    {
        let after = &rest[3..];
        let spaces = after.len() - after.trim_start_matches(' ').len();
        if spaces > 0 {
            return Some(at + 3 + spaces);
        }
    }
    Some(at)
}

/// `text` fitted to replace `selection` in `source`.
///
/// Multi-line text continues each later line with the caret line's prefix
/// (list indentation, `> `). Block output (a table, a fence, a heading, a
/// list, several embeds) also starts on its own line when the caret is not
/// at the start of its line's content, and ends with a newline when the rest
/// of the line is not empty.
pub fn fit(text: &str, block: bool, source: &str, selection: Range<usize>) -> String {
    let start = floor_boundary(source, selection.start.min(source.len()));
    let end = floor_boundary(source, selection.end.clamp(start, source.len()));
    let line_start = source[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = source[end..].find('\n').map_or(source.len(), |i| end + i);
    let before = &source[line_start..start];
    let whole = format!("{}{}", before, &source[end..line_end]);
    let rest = source[end..line_end].trim_end_matches('\r');
    let (line_pad, markers) = line_prefix(&whole);
    let after_markers = before.len() >= markers;
    let pad = if after_markers {
        line_pad
    } else {
        String::new()
    };
    let content_before = if after_markers {
        &before[markers..]
    } else {
        before
    };
    let multi = text.contains('\n');
    let mut out = String::with_capacity(text.len() + 16);
    if block && !content_before.trim().is_empty() {
        out.push('\n');
        out.push_str(&pad);
    }
    if multi && !pad.is_empty() {
        for (index, line) in text.split('\n').enumerate() {
            if index > 0 {
                out.push('\n');
                if line.is_empty() {
                    out.push_str(pad.trim_end());
                } else {
                    out.push_str(&pad);
                }
            }
            out.push_str(line);
        }
    } else {
        out.push_str(text);
    }
    if block && !rest.trim().is_empty() && !out.ends_with('\n') {
        out.push('\n');
        out.push_str(&pad);
    }
    out
}

/// Converted Markdown (a table, a fence, rich text) fitted to replace
/// `selection` in `source`, by the converter's [`okilum_paste::adapt_to_context`]:
/// block output gets a blank line before it (caret mid-line) and after it
/// (text after the caret), so a table or heading is not absorbed into the
/// paragraph; continuation lines follow list indentation and `> ` markers.
pub fn fit_markdown(text: &str, block: bool, source: &str, selection: Range<usize>) -> String {
    let start = floor_boundary(source, selection.start.min(source.len()));
    let end = floor_boundary(source, selection.end.clamp(start, source.len()));
    let line_start = source[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = source[end..].find('\n').map_or(source.len(), |i| end + i);
    let context = okilum_paste::InsertContext {
        line_prefix_before_caret: &source[line_start..start],
        rest_of_line: source[end..line_end].trim_end_matches('\r'),
        in_code_block: false,
    };
    let fitted = okilum_paste::adapt_to_context(text, block, &context);
    // At the start of the line right under a paragraph line: a blank line
    // first, or a table (a heading, a list) joins that paragraph.
    if block && line_start == start && line_start > 0 && !fitted.starts_with('\n') {
        let previous = source[..line_start - 1]
            .rsplit('\n')
            .next()
            .unwrap_or_default()
            .trim_end_matches('\r');
        if is_paragraph_line(previous) {
            return format!("\n{fitted}");
        }
    }
    fitted
}

/// A line of paragraph text: not blank, not a list item, quote, heading,
/// table row, fence or indented code.
fn is_paragraph_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || line.len() - trimmed.len() >= 4 {
        return false;
    }
    let (_, markers) = line_prefix(line);
    markers == line.len() - trimmed.len()
        && !trimmed.starts_with(['#', '>', '|', '`', '~'])
        && !trimmed.starts_with("---")
        && !trimmed.starts_with("***")
}

fn floor_boundary(text: &str, mut at: usize) -> usize {
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caret(source_with_bar: &str) -> (String, usize) {
        let at = source_with_bar.find('|').unwrap();
        (source_with_bar.replacen('|', "", 1), at)
    }

    #[test]
    fn fenced_code_is_detected_by_content_lines_only() {
        for (text, inside) in [
            ("```rust\nfn |main() {}\n```\n", true),
            ("```rust|\nfn main() {}\n```\n", false),
            ("```\ncode\n```|\n", false),
            ("```\ncode\n```\nafter|", false),
            ("~~~\n|\n~~~", true),
            ("````\n```\n|\n````", true),
            ("```\nunclosed\n|", true),
            ("plain |text", false),
            ("> ```\n> qu|oted\n> ```", true),
            ("- item\n  ```\n  co|de\n  ```", true),
            ("inline ``` not| a fence", false),
            ("``\n|\n``", false),
        ] {
            let (source, at) = caret(text);
            assert_eq!(in_fenced_code(&source, at), inside, "{text:?}");
        }
    }

    #[test]
    fn continuation_follows_lists_and_quotes() {
        assert_eq!(continuation("- item", "- item"), Some("  ".into()));
        assert_eq!(continuation("  1. one", "  1. one"), Some("     ".into()));
        assert_eq!(
            continuation("- [ ] task", "- [ ] task"),
            Some("      ".into())
        );
        assert_eq!(continuation("> quote", "> quote"), Some("> ".into()));
        assert_eq!(continuation("> - in", "> - in"), Some(">   ".into()));
        assert_eq!(continuation("plain", "plain"), Some(String::new()));
        // Before the marker: nothing to continue.
        assert_eq!(continuation("", "- item"), None);
    }

    #[test]
    fn multi_line_text_continues_list_items_and_quotes() {
        let (source, at) = caret("- first |\n- next");
        assert_eq!(fit("a\nb", false, &source, at..at), "a\n  b");
        let (source, at) = caret("> said |");
        assert_eq!(fit("one\n\ntwo", false, &source, at..at), "one\n>\n> two");
        // Single-line text is inserted as it is.
        assert_eq!(fit("x", true, &source, at..at), "\n> x");
        let (source, at) = caret("plain |");
        assert_eq!(fit("a\nb", false, &source, at..at), "a\nb");
    }

    #[test]
    fn block_output_gets_its_own_lines() {
        let table = "| a | b |\n| --- | --- |\n| 1 | 2 |";
        let (source, at) = caret("text |more");
        assert_eq!(fit(table, true, &source, at..at), format!("\n{table}\n"));
        let (source, at) = caret("|");
        assert_eq!(fit(table, true, &source, at..at), table);
        let (source, at) = caret("line\n|");
        assert_eq!(fit(table, true, &source, at..at), table);
        // Inside a list item, right after its marker: no blank start line.
        let (source, at) = caret("- |");
        assert_eq!(
            fit("![[a.png]]\n![[b.png]]", true, &source, at..at),
            "![[a.png]]\n  ![[b.png]]"
        );
        // A selection is replaced; the rest of the line moves down.
        let source = "keep REPLACE tail";
        assert_eq!(fit("```\nx\n```", true, source, 5..12), "\n```\nx\n```\n");
    }

    #[test]
    fn code_block_prefix_comes_from_the_fence_container() {
        let (source, at) = caret("> [!note]\n> ```\n> |\n> ```\n");
        assert_eq!(code_block_prefix(&source, at).as_deref(), Some("> "));
        assert_eq!(continue_lines("a\n\nc", "> "), "a\n>\n> c");
        let (source, at) = caret("- item\n  ```\n  co|de\n  ```\n");
        assert_eq!(code_block_prefix(&source, at).as_deref(), Some("  "));
        let (source, at) = caret("```\n    co|de\n```\n");
        assert_eq!(code_block_prefix(&source, at), None);
        let (source, at) = caret("text |here");
        assert_eq!(code_block_prefix(&source, at), None);
    }

    #[test]
    fn a_block_under_a_paragraph_line_gets_a_blank_line() {
        let (source, at) = caret("Results:\n|");
        assert_eq!(
            fit_markdown("| a | b |\n| --- | --- |", true, &source, at..at),
            "\n| a | b |\n| --- | --- |"
        );
        let (source, at) = caret("Results:\n\n|");
        assert!(!fit_markdown("| a |", true, &source, at..at).starts_with('\n'));
        let (source, at) = caret("- item\n|");
        assert!(!fit_markdown("| a |", true, &source, at..at).starts_with('\n'));
    }
}
