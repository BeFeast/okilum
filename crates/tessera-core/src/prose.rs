//! Which bytes of a Markdown note are prose, as opposed to code.
//!
//! Wikilinks are rewritten and indexed with regexes over the note text. A
//! `[[x]]` inside an inline code span, a fenced block or an indented block is
//! text *about* a link, not a link: it must not be rewritten (#20 — the vault's
//! own convention docs rendered struck through) and it must not become a
//! backlink. This module draws that boundary once, so the source rewriters and
//! the link graph cannot disagree about it.
//!
//! It is a small state machine, not a Markdown parser. It recognises:
//!
//! - fenced code blocks: a run of three or more `` ` `` or `~` at line start
//!   (up to three spaces of indent allowed), closed by a run of the same
//!   character at least as long; an unclosed fence runs to end of text; a
//!   backtick fence whose info string contains a backtick is not a fence;
//! - indented code: a non-blank line starting with four spaces or a tab, when it
//!   follows a blank line (or the start of the text) and the last non-blank
//!   prose line was not a list item, or when it directly continues an indented
//!   code line;
//! - inline code spans: a backtick run closed by the next run of exactly the
//!   same length, within one paragraph (a run of non-blank prose lines); an
//!   unmatched run is literal text.
//!
//! Known limitation: an indented paragraph that continues a list item across a
//! blank line is only recognised when the last non-blank line *is* the list
//! item. Text nested deeper (list item, blank, indented paragraph, blank,
//! another indented paragraph) is treated as indented code from the second
//! paragraph on, so a wikilink there is left as written. Real list-continuation
//! text that deep is rare in this vault, and the miss is on the safe side: a
//! link is left literal, never invented.

use std::ops::Range;

/// Byte ranges of `text` that are prose: everything outside fenced, indented
/// and inline code. Sorted, non-overlapping, non-empty, and covering every
/// byte that is not code.
pub fn prose_spans(text: &str) -> Vec<Range<usize>> {
    let mut code: Vec<Range<usize>> = Vec::new();

    // Line pass: fences and indented code, plus the paragraphs (runs of
    // non-blank prose lines) that the inline pass may scan.
    let mut fence: Option<Fence> = None;
    let mut prev_blank = true;
    let mut prev_indented_code = false;
    let mut last_nonblank_is_list = false;
    let mut paragraph: Option<Range<usize>> = None;
    let mut paragraphs: Vec<Range<usize>> = Vec::new();
    let mut off = 0;

    for line in text.split_inclusive('\n') {
        let range = off..off + line.len();
        off = range.end;
        let blank = line.trim().is_empty();

        let kind = if let Some(open) = &fence {
            if fence_close(line, open) {
                fence = None;
            }
            Kind::Fence
        } else if let Some(open) = fence_open(line) {
            fence = Some(open);
            Kind::Fence
        } else if blank {
            Kind::Blank
        } else if is_indented(line)
            && ((prev_blank && !last_nonblank_is_list) || prev_indented_code)
        {
            Kind::Indented
        } else {
            Kind::Prose
        };

        match kind {
            Kind::Fence | Kind::Indented => {
                code.push(range);
                paragraphs.extend(paragraph.take());
                // A code line ends whatever list was open; the text inside it
                // is not consulted for list markers.
                last_nonblank_is_list = false;
            }
            Kind::Blank => paragraphs.extend(paragraph.take()),
            Kind::Prose => {
                match &mut paragraph {
                    Some(p) => p.end = range.end,
                    None => paragraph = Some(range.clone()),
                }
                last_nonblank_is_list = is_list_item(line);
            }
        }

        prev_indented_code = matches!(kind, Kind::Indented);
        prev_blank = blank;
    }
    paragraphs.extend(paragraph.take());

    for para in paragraphs {
        inline_code_ranges(text, para, &mut code);
    }
    code.sort_by_key(|r| r.start);

    // Complement.
    let mut prose = Vec::new();
    let mut pos = 0;
    for r in code {
        if r.start > pos {
            prose.push(pos..r.start);
        }
        pos = pos.max(r.end);
    }
    if pos < text.len() {
        prose.push(pos..text.len());
    }
    prose
}

/// Apply `f` to every prose span of `text` and splice the results back between
/// the code spans, which are copied verbatim.
pub fn replace_in_prose(text: &str, mut f: impl FnMut(&str) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for span in prose_spans(text) {
        out.push_str(&text[pos..span.start]);
        out.push_str(&f(&text[span.clone()]));
        pos = span.end;
    }
    out.push_str(&text[pos..]);
    out
}

#[derive(Clone, Copy)]
enum Kind {
    Fence,
    Indented,
    Blank,
    Prose,
}

struct Fence {
    ch: u8,
    len: usize,
}

fn strip_up_to_3_spaces(line: &str) -> &str {
    let n = line.bytes().take(3).take_while(|&c| c == b' ').count();
    &line[n..]
}

fn fence_open(line: &str) -> Option<Fence> {
    let stripped = strip_up_to_3_spaces(line);
    let ch = *stripped.as_bytes().first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let len = stripped.bytes().take_while(|&c| c == ch).count();
    if len < 3 {
        return None;
    }
    // CommonMark: a backtick fence's info string may not contain a backtick;
    // such a line is an inline code span instead.
    if ch == b'`' && stripped[len..].contains('`') {
        return None;
    }
    Some(Fence { ch, len })
}

fn fence_close(line: &str, open: &Fence) -> bool {
    let stripped = strip_up_to_3_spaces(line);
    let len = stripped.bytes().take_while(|&c| c == open.ch).count();
    len >= open.len && stripped[len..].trim().is_empty()
}

fn is_indented(line: &str) -> bool {
    line.starts_with('\t') || line.starts_with("    ")
}

fn is_list_item(line: &str) -> bool {
    let t = line.trim_start();
    if let Some(rest) = t.strip_prefix(['-', '*', '+']) {
        return rest.is_empty() || rest.starts_with([' ', '\t']);
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 9 {
        return false;
    }
    let rest = &t[digits..];
    if !rest.starts_with(['.', ')']) {
        return false;
    }
    let after = &rest[1..];
    after.is_empty() || after.starts_with([' ', '\t'])
}

fn run_len(b: &[u8], at: usize, end: usize) -> usize {
    b[at..end].iter().take_while(|&&c| c == b'`').count()
}

/// Inline code spans within one paragraph, appended to `out`.
fn inline_code_ranges(text: &str, para: Range<usize>, out: &mut Vec<Range<usize>>) {
    let b = text.as_bytes();
    let mut i = para.start;
    while i < para.end {
        if b[i] != b'`' {
            i += 1;
            continue;
        }
        let n = run_len(b, i, para.end);
        let mut j = i + n;
        let mut close = None;
        while j < para.end {
            if b[j] == b'`' {
                let m = run_len(b, j, para.end);
                if m == n {
                    close = Some(j + m);
                    break;
                }
                j += m;
            } else {
                j += 1;
            }
        }
        match close {
            Some(end) => {
                out.push(i..end);
                i = end;
            }
            None => i += n,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The prose of `text`, joined with `|` at every code boundary, so a test
    /// reads as "what survived".
    fn prose(text: &str) -> String {
        prose_spans(text)
            .into_iter()
            .map(|r| &text[r])
            .collect::<Vec<_>>()
            .join("|")
    }

    #[test]
    fn plain_text_is_one_span() {
        assert_eq!(prose_spans("a [[b]] c\n"), vec![0..10]);
        assert_eq!(prose_spans(""), Vec::<Range<usize>>::new());
    }

    #[test]
    fn inline_code_is_cut_out() {
        assert_eq!(prose("x `[[a]]` y `z`"), "x | y ");
        assert_eq!(prose("`a` `b`"), " ");
    }

    #[test]
    fn a_double_backtick_span_may_contain_a_single_backtick() {
        assert_eq!(prose("x `` a ` b `` y"), "x | y");
        // And a run of a different length does not close it.
        assert_eq!(prose("x ``` a `` b ``` y"), "x | y");
    }

    #[test]
    fn an_unmatched_backtick_is_literal() {
        assert_eq!(prose("x ` y [[a]]"), "x ` y [[a]]");
        assert_eq!(prose("x `` y ` z"), "x `` y ` z");
    }

    #[test]
    fn an_inline_span_may_cross_a_line_but_not_a_blank_line() {
        assert_eq!(prose("x `a\nb` y"), "x | y");
        assert_eq!(prose("x `a\n\nb` y"), "x `a\n\nb` y");
    }

    #[test]
    fn text_after_a_closed_span_is_prose() {
        assert_eq!(prose("`a` [[b]]"), " [[b]]");
    }

    #[test]
    fn a_backtick_fence_is_code_to_its_closing_line() {
        let t = "p\n```yaml\nk: [[x]]\n```\nq\n";
        assert_eq!(prose(t), "p\n|q\n");
    }

    #[test]
    fn a_tilde_fence_is_code_too() {
        let t = "p\n~~~\n[[x]]\n~~~\nq\n";
        assert_eq!(prose(t), "p\n|q\n");
    }

    #[test]
    fn a_fence_closes_only_on_the_same_char_at_least_as_long() {
        let t = "````\n```\n[[x]]\n~~~~\n`````\nq\n";
        assert_eq!(prose(t), "q\n");
    }

    #[test]
    fn a_fence_may_be_indented_up_to_three_spaces() {
        assert_eq!(prose("   ```\n[[x]]\n   ```\nq"), "q");
    }

    #[test]
    fn a_backtick_fence_line_with_a_backtick_in_its_info_string_is_inline() {
        // CommonMark: not a fence, so the next line is prose.
        assert_eq!(prose("``` `a` ```\n[[b]]\n"), "\n[[b]]\n");
    }

    #[test]
    fn an_unclosed_fence_runs_to_the_end() {
        assert_eq!(prose("p\n```\n[[x]]\n\n[[y]]"), "p\n");
    }

    #[test]
    fn indented_code_after_a_blank_line_is_code() {
        assert_eq!(prose("p\n\n    [[x]]\n    [[y]]\nq\n"), "p\n\n|q\n");
        assert_eq!(prose("\t[[x]]\nq\n"), "q\n");
    }

    #[test]
    fn an_indented_line_continuing_a_paragraph_is_prose() {
        assert_eq!(prose("p\n    [[x]]\n"), "p\n    [[x]]\n");
    }

    #[test]
    fn an_indented_line_after_a_list_item_is_the_item_not_code() {
        assert_eq!(prose("- item\n\n    [[x]]\n"), "- item\n\n    [[x]]\n");
        assert_eq!(prose("1. item\n\n    [[x]]\n"), "1. item\n\n    [[x]]\n");
    }

    #[test]
    fn indented_code_may_contain_blank_lines() {
        assert_eq!(prose("\n    a\n\n    b\nq"), "\n|\n|q");
    }

    #[test]
    fn a_list_marker_inside_indented_code_does_not_reopen_a_list() {
        assert_eq!(prose("\n    - a\n\n    [[x]]\n"), "\n|\n");
    }

    #[test]
    fn replace_in_prose_splices_code_back_verbatim() {
        let out = replace_in_prose("a `b` c\n```\nd\n```\ne", |s| s.to_uppercase());
        assert_eq!(out, "A `b` C\n```\nd\n```\nE");
    }
}
