//! Obsidian syntax the Reader shows the way Obsidian does (#651).
//!
//! Every pass here rewrites Markdown *source* into Markdown the shell's
//! renderer already understands, the same way `==highlight==` becomes
//! `<mark>` ([`crate::render::rewrite_highlights`]). Code spans and blocks are
//! never touched: `%%`, `$`, `[^1]` and `^id` inside code are text about the
//! syntax, not uses of it ([`crate::prose`] draws that boundary).
//!
//! - `%%comments%%` are removed; a comment that fills its lines removes the
//!   lines, so it cannot split the paragraph around it. An unpaired `%%` is
//!   text: hiding the rest of a note behind a stray marker would hide content
//!   the user cannot see is there.
//! - Math has no renderer in the shell. Inline `$x$` becomes a code span and
//!   display `$$…$$` a fence tagged [`MATH_LANG`]: the TeX source, legible and
//!   copyable, instead of a formula that silently looks like prose.
//! - Footnotes are numbered in order of first reference, as Obsidian numbers
//!   them. A reference becomes a superscript link to the definition; each
//!   definition moves to the end of the note inside a fence tagged
//!   [`FOOTNOTE_LANG`], one top-level block per footnote, so both directions
//!   can land on an exact block.
//! - A block ID (` ^id` ending a line, or `^id` alone on a line after a
//!   block) becomes an invisible HTML comment marker ([`block_marker`]) that
//!   the heading inventory reads to land `[[note#^id]]` links.

use crate::prose::{prose_spans, replace_in_prose};
use comrak::nodes::{AstNode, NodeValue};
use regex::Regex;
use std::ops::Range;

/// Info-string tag of the fence a display formula is wrapped in.
pub const MATH_LANG: &str = "math";
/// Info-string tag of a footnote definition fence: `footnote <n> <id>`, the id
/// percent-encoded as in a `tessera://` URL.
pub const FOOTNOTE_LANG: &str = "footnote";
/// A footnote reference: lands on the definition.
pub const FOOTNOTE_SCHEME: &str = "tessera://footnote/";
/// A footnote definition's back-link: lands on the first reference.
pub const FOOTNOTE_BACK_SCHEME: &str = "tessera://footnote-back/";

/// Hide `%%comments%%`, render math as its source, number footnotes and mark
/// block IDs. The shell's Reader runs this; the MCP `read_note` does not, so
/// an agent still reads the note as written.
///
/// Split in two because the passes straddle link rewriting: comments and math
/// must go first (a link inside either is not a link), footnotes and block
/// IDs last (their bodies must be rewritten first, and their output must not
/// be).
pub fn before_links(text: &str) -> String {
    rewrite_math(&strip_comments(text))
}

/// The second half of [`before_links`]'s pipeline.
pub fn after_links(text: &str) -> String {
    rewrite_block_ids(&rewrite_footnotes(text))
}

fn is_prose(prose: &[Range<usize>], at: usize) -> bool {
    let i = prose.partition_point(|r| r.end <= at);
    prose.get(i).is_some_and(|r| r.contains(&at))
}

/// Remove `%%…%%` comments outside code. Markers pair in order; a trailing
/// unpaired marker is left as text.
pub fn strip_comments(text: &str) -> String {
    if !text.contains("%%") {
        return text.to_string();
    }
    let prose = prose_spans(text);
    let mut marks = Vec::new();
    let mut i = 0;
    while let Some(p) = text[i..].find("%%") {
        let at = i + p;
        if is_prose(&prose, at) && is_prose(&prose, at + 1) {
            marks.push(at);
            i = at + 2;
        } else {
            i = at + 1;
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for pair in marks.as_chunks::<2>().0 {
        let (mut start, mut end) = (pair[0], pair[1] + 2);
        let line_start = text[..start].rfind('\n').map_or(0, |n| n + 1);
        let line_end = text[end..].find('\n').map_or(text.len(), |n| end + n + 1);
        if line_start >= pos
            && text[line_start..start].trim().is_empty()
            && text[end..line_end].trim().is_empty()
        {
            start = line_start;
            end = line_end;
        }
        out.push_str(&text[pos..start.max(pos)]);
        pos = end.max(pos);
    }
    out.push_str(&text[pos..]);
    out
}

/// Container prefix of a line: blockquote markers and up to three spaces of
/// indentation, in any order Markdown allows them.
fn container_prefix(line: &str) -> usize {
    let mut i = 0;
    let bytes = line.as_bytes();
    loop {
        let spaces = bytes[i..]
            .iter()
            .take(3)
            .take_while(|&&b| b == b' ')
            .count();
        if bytes.get(i + spaces) == Some(&b'>') {
            i += spaces + 1;
            if bytes.get(i) == Some(&b' ') {
                i += 1;
            }
        } else {
            // List-item content indentation is kept as prefix too, so a
            // formula inside a list item stays inside it.
            let indent = bytes[i..].iter().take_while(|&&b| b == b' ').count();
            return i + indent;
        }
    }
}

/// A code span holding `s` verbatim, whatever backticks it contains.
fn code_span(s: &str) -> String {
    let longest = s.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let ticks = "`".repeat(longest + 1);
    let pad = if s.starts_with('`') || s.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{ticks}{pad}{s}{pad}{ticks}")
}

/// A tilde fence longer than any tilde run that starts a line of `body`.
fn fence_for(body: &str) -> String {
    let longest = body
        .lines()
        .map(|l| l.trim_start().chars().take_while(|&c| c == '~').count())
        .max()
        .unwrap_or(0);
    "~".repeat(longest.max(3) + 1)
}

/// Rewrite display `$$…$$` into [`MATH_LANG`] fences and inline `$…$` into
/// code spans. Obsidian's (Pandoc's) inline rule: the opening `$` is not
/// followed by whitespace, the closing one is not preceded by whitespace nor
/// followed by a digit, and both are on one line — so `$5 and $10` is money.
pub fn rewrite_math(text: &str) -> String {
    if !text.contains('$') {
        return text.to_string();
    }
    let prose = prose_spans(text);
    let lines: Vec<(usize, &str)> = text
        .split_inclusive('\n')
        .scan(0, |off, line| {
            let at = *off;
            *off += line.len();
            Some((at, line))
        })
        .collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < lines.len() {
        let (at, line) = lines[i];
        let content = line.trim_end_matches(['\n', '\r']);
        let prefix_len = container_prefix(content);
        let rest = &content[prefix_len..];
        if !rest.starts_with("$$") || !is_prose(&prose, at + prefix_len) {
            out.push_str(line);
            i += 1;
            continue;
        }
        let prefix = &content[..prefix_len];
        let after = &rest[2..];
        let mut body: Vec<&str> = Vec::new();
        let mut close = None;
        if let Some(end) = after.find("$$") {
            if after[end + 2..].trim().is_empty() {
                body.push(after[..end].trim());
                close = Some(i);
            }
        } else {
            if !after.trim().is_empty() {
                body.push(after.trim());
            }
            for (j, (_, next)) in lines.iter().enumerate().skip(i + 1) {
                let next = next.trim_end_matches(['\n', '\r']);
                let inner = next.strip_prefix(prefix).unwrap_or_else(|| {
                    let p = container_prefix(next);
                    &next[p..]
                });
                if let Some(end) = inner.find("$$") {
                    if !inner[end + 2..].trim().is_empty() {
                        break;
                    }
                    if !inner[..end].trim().is_empty() {
                        body.push(inner[..end].trim_end());
                    }
                    close = Some(j);
                    break;
                }
                body.push(inner);
            }
        }
        let Some(close) = close.filter(|_| body.iter().any(|l| !l.trim().is_empty())) else {
            out.push_str(line);
            i += 1;
            continue;
        };
        let source = body.join("\n");
        let fence = fence_for(&source);
        out.push_str(&format!("{prefix}{fence}{MATH_LANG}\n"));
        for l in source.lines() {
            out.push_str(if l.is_empty() {
                prefix.trim_end()
            } else {
                prefix
            });
            out.push_str(l);
            out.push('\n');
        }
        out.push_str(&format!("{prefix}{fence}\n"));
        i = close + 1;
    }
    replace_in_prose(&out, inline_math)
}

fn escaped(s: &str, at: usize) -> bool {
    s[..at].bytes().rev().take_while(|&b| b == b'\\').count() % 2 == 1
}

fn inline_math(seg: &str) -> String {
    let b = seg.as_bytes();
    let mut out = String::with_capacity(seg.len());
    let mut pos = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'$' || escaped(seg, i) {
            i += 1;
            continue;
        }
        let line_end = seg[i..].find('\n').map_or(seg.len(), |n| i + n);
        let found = if b.get(i + 1) == Some(&b'$') {
            // `$$x$$` inside a line: display math written inline.
            seg[i + 2..line_end]
                .find("$$")
                .map(|n| (i + 2, i + 2 + n, i + 4 + n))
                .filter(|(s, e, _)| !seg[*s..*e].trim().is_empty())
        } else if seg[i + 1..]
            .chars()
            .next()
            .is_some_and(|c| !c.is_whitespace())
        {
            (i + 2..line_end)
                .filter(|&j| {
                    b[j] == b'$'
                        && !escaped(seg, j)
                        && !b[j - 1].is_ascii_whitespace()
                        && !b
                            .get(j + 1)
                            .is_some_and(|c| c.is_ascii_digit() || *c == b'$')
                })
                .map(|j| (i + 1, j, j + 1))
                .next()
        } else {
            None
        };
        match found {
            Some((start, end, next)) => {
                out.push_str(&seg[pos..i]);
                out.push_str(&code_span(seg[start..end].trim()));
                pos = next;
                i = next;
            }
            None => i += 1,
        }
    }
    out.push_str(&seg[pos..]);
    out
}

const SUPERSCRIPT: [char; 10] = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];

fn superscript(n: usize) -> String {
    n.to_string()
        .chars()
        .map(|d| SUPERSCRIPT[d.to_digit(10).unwrap_or(0) as usize])
        .collect()
}

fn is_indented(line: &str) -> bool {
    line.starts_with("    ") || line.starts_with('\t')
}

fn deindent(line: &str) -> &str {
    line.strip_prefix("    ")
        .or_else(|| line.strip_prefix('\t'))
        .unwrap_or(line)
}

/// A line that cannot lazily continue a footnote paragraph: it starts a new
/// block of its own.
fn starts_block(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with('#')
        || t.starts_with('>')
        || t.starts_with("```")
        || t.starts_with("~~~")
        || t.starts_with("- ")
        || t.starts_with("* ")
        || t.starts_with("+ ")
        || t.starts_with("[^")
        || t.split_once(['.', ')']).is_some_and(|(n, rest)| {
            !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) && rest.starts_with(' ')
        })
}

struct Footnotes {
    /// Definitions by normalised label: `(id as written, body)`.
    defs: std::collections::HashMap<String, (String, String)>,
    /// Footnotes in number order: `(id, body)`.
    used: Vec<(String, String)>,
    numbers: std::collections::HashMap<String, usize>,
}

impl Footnotes {
    fn number(&mut self, key: &str) -> Option<(usize, String)> {
        if let Some(&n) = self.numbers.get(key) {
            return Some((n, self.used[n - 1].0.clone()));
        }
        let (id, body) = self.defs.get(key)?.clone();
        self.used.push((id.clone(), body));
        let n = self.used.len();
        self.numbers.insert(key.to_string(), n);
        Some((n, id))
    }

    fn inline(&mut self, body: &str) -> (usize, String) {
        let n = self.used.len() + 1;
        let id = format!("inline-{n}");
        self.used.push((id.clone(), body.to_string()));
        (n, id)
    }

    fn reference(n: usize, id: &str) -> String {
        format!(
            "[{}]({FOOTNOTE_SCHEME}{})",
            superscript(n),
            crate::document_links::encode(id)
        )
    }

    /// Replace `[^id]` references and `^[inline]` footnotes in one prose span.
    fn rewrite_refs(&mut self, seg: &str) -> String {
        let mut out = String::with_capacity(seg.len());
        let mut pos = 0;
        let mut i = 0;
        while i < seg.len() {
            let rest = &seg[i..];
            if rest.starts_with("[^") {
                if let Some(close) = rest.find(']') {
                    let label = &rest[2..close];
                    if !label.is_empty() && !label.contains(char::is_whitespace) {
                        if let Some((n, id)) = self.number(&label.to_lowercase()) {
                            out.push_str(&seg[pos..i]);
                            out.push_str(&Self::reference(n, &id));
                            i += close + 1;
                            pos = i;
                            continue;
                        }
                    }
                }
            } else if rest.starts_with("^[") && !escaped(seg, i) {
                let mut depth = 0;
                let mut end = None;
                for (k, c) in rest.char_indices().skip(1) {
                    match c {
                        '[' => depth += 1,
                        ']' => {
                            depth -= 1;
                            if depth == 0 {
                                end = Some(k);
                                break;
                            }
                        }
                        '\n' if rest[..k].ends_with('\n') => break,
                        _ => {}
                    }
                }
                if let Some(end) = end.filter(|&e| !rest[2..e].trim().is_empty()) {
                    let (n, id) = self.inline(rest[2..end].trim());
                    out.push_str(&seg[pos..i]);
                    out.push_str(&Self::reference(n, &id));
                    i += end + 1;
                    pos = i;
                    continue;
                }
            }
            i += rest.chars().next().map_or(1, char::len_utf8);
        }
        out.push_str(&seg[pos..]);
        out
    }
}

/// Number footnotes and move their definitions to the end of the note. A
/// definition nothing references is not shown, as in Obsidian; a reference
/// to a label nothing defines stays as written.
pub fn rewrite_footnotes(text: &str) -> String {
    if !text.contains("[^") && !text.contains("^[") {
        return text.to_string();
    }
    let def_re = Regex::new(r"^ {0,3}\[\^([^\]\s]+)\]:[ \t]?(.*)$").unwrap();
    let prose = prose_spans(text);
    let lines: Vec<(usize, &str)> = text
        .split_inclusive('\n')
        .scan(0, |off, line| {
            let at = *off;
            *off += line.len();
            Some((at, line))
        })
        .collect();
    let mut notes = Footnotes {
        defs: Default::default(),
        used: Vec::new(),
        numbers: Default::default(),
    };
    let mut body_text = String::with_capacity(text.len());
    let mut i = 0;
    while i < lines.len() {
        let (at, line) = lines[i];
        let content = line.trim_end_matches(['\n', '\r']);
        let Some(cap) = def_re.captures(content).filter(|_| is_prose(&prose, at)) else {
            body_text.push_str(line);
            i += 1;
            continue;
        };
        let id = cap[1].to_string();
        let mut def = vec![cap[2].to_string()];
        i += 1;
        while i < lines.len() {
            let next = lines[i].1.trim_end_matches(['\n', '\r']);
            if next.trim().is_empty() {
                let continues = lines[i..]
                    .iter()
                    .map(|(_, l)| l.trim_end_matches(['\n', '\r']))
                    .find(|l| !l.trim().is_empty())
                    .is_some_and(is_indented);
                if !continues {
                    break;
                }
                def.push(String::new());
            } else if is_indented(next) {
                def.push(deindent(next).to_string());
            } else if def.last().is_some_and(|l| !l.trim().is_empty()) && !starts_block(next) {
                def.push(next.to_string());
            } else {
                break;
            }
            i += 1;
        }
        notes
            .defs
            .entry(id.to_lowercase())
            .or_insert((id, def.join("\n").trim().to_string()));
    }
    let mut out = replace_in_prose(&body_text, |seg| notes.rewrite_refs(seg));
    // Footnotes referenced only from other footnotes are numbered as their
    // referencing bodies are rewritten, so walk a growing list.
    let mut k = 0;
    while k < notes.used.len() {
        let body = notes.used[k].1.clone();
        let rewritten = replace_in_prose(&body, |seg| notes.rewrite_refs(seg));
        notes.used[k].1 = rewritten;
        k += 1;
    }
    if notes.used.is_empty() {
        return out;
    }
    let trimmed = out.trim_end_matches(['\n', '\r', ' ', '\t']).len();
    out.truncate(trimmed);
    out.push_str("\n\n---\n\n");
    for (n, (id, body)) in notes.used.iter().enumerate() {
        let fence = fence_for(body);
        out.push_str(&format!(
            "{fence}{FOOTNOTE_LANG} {} {}\n{body}\n{fence}\n\n",
            n + 1,
            crate::document_links::encode(id)
        ));
    }
    out
}

/// `(number, id)` from a [`FOOTNOTE_LANG`] fence's info string after the tag.
pub fn parse_footnote_info(meta: &str) -> Option<(usize, String)> {
    let mut parts = meta.split_whitespace();
    let n = parts.next()?.parse().ok()?;
    let id = crate::document_links::decode(parts.next()?);
    Some((n, id))
}

/// The invisible marker a block ID becomes.
pub fn block_marker(id: &str) -> String {
    format!("<!--^{id}-->")
}

fn is_block_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// The block ID a [`block_marker`] carries, given the raw HTML.
pub fn marker_id(html: &str) -> Option<&str> {
    let id = html.trim().strip_prefix("<!--^")?.strip_suffix("-->")?;
    is_block_id(id).then_some(id)
}

/// Replace Obsidian block IDs with [`block_marker`]s.
pub fn rewrite_block_ids(text: &str) -> String {
    if !text.contains('^') {
        return text.to_string();
    }
    let standalone = Regex::new(r"(?m)^([ \t>]*)\^([A-Za-z0-9-]+)[ \t]*(\r?)$").unwrap();
    let trailing = Regex::new(r"(?m)[ \t]+\^([A-Za-z0-9-]+)[ \t]*(\r?)$").unwrap();
    replace_in_prose(text, |seg| {
        let seg = standalone.replace_all(seg, |c: &regex::Captures| {
            format!("{}{}{}", &c[1], block_marker(&c[2]), &c[3])
        });
        trailing
            .replace_all(&seg, |c: &regex::Captures| {
                format!("{}{}", block_marker(&c[1]), &c[2])
            })
            .into_owned()
    })
}

fn list_item_indent(line: &str) -> Option<usize> {
    let indent = line.len() - line.trim_start().len();
    let t = line.trim_start();
    let bullet = ["- ", "* ", "+ "].iter().any(|b| t.starts_with(b));
    let ordered = t.split_once(['.', ')']).is_some_and(|(n, rest)| {
        !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) && rest.starts_with(' ')
    });
    (bullet || ordered).then_some(indent)
}

/// The block `![[note#^id]]` embeds, from the note's source: the paragraph or
/// list item (with its children) the ID ends, or for an ID alone on its line
/// the block before it. `None` when the ID is missing or ambiguous.
pub fn block_section(body: &str, id: &str) -> Option<String> {
    let prose = prose_spans(body);
    let lines: Vec<(usize, &str)> = body
        .split_inclusive('\n')
        .scan(0, |off, line| {
            let at = *off;
            *off += line.len();
            Some((at, line.trim_end_matches(['\n', '\r'])))
        })
        .collect();
    let mut matches = lines.iter().enumerate().filter_map(|(k, &(at, line))| {
        let t = line.trim_end();
        let (marker, found) = t.rsplit_once('^')?;
        if !is_block_id(found) || !found.eq_ignore_ascii_case(id) {
            return None;
        }
        let at_marker = at + marker.len();
        if !is_prose(&prose, at_marker) {
            return None;
        }
        if marker.trim_start_matches([' ', '\t', '>']).is_empty() {
            Some((k, true))
        } else if marker.ends_with([' ', '\t']) {
            Some((k, false))
        } else {
            None
        }
    });
    let (k, standalone) = matches.next()?;
    // The heading inventory rejects duplicate IDs case-insensitively. Embeds
    // must use that same ambiguity contract rather than choose the first block.
    if matches.next().is_some() {
        return None;
    }
    let blank = |l: &str| l.trim().is_empty();
    let stops = |l: &str| {
        let t = l.trim_start();
        blank(l) || t.starts_with('#') || t.starts_with("```") || t.starts_with("~~~")
    };
    let (start, end) = if standalone {
        let mut end = k;
        while end > 0 && blank(lines[end - 1].1) {
            end -= 1;
        }
        if end == 0 {
            return None;
        }
        let mut start = end - 1;
        while start > 0 && !blank(lines[start - 1].1) {
            start -= 1;
        }
        (start, end)
    } else if let Some(indent) = list_item_indent(lines[k].1) {
        let mut end = k + 1;
        while end < lines.len() {
            let l = lines[end].1;
            if blank(l) || l.len() - l.trim_start().len() <= indent {
                break;
            }
            end += 1;
        }
        let section: Vec<&str> = lines[k..end]
            .iter()
            .map(|(_, l)| l.get(indent..).unwrap_or(l.trim_start()))
            .collect();
        return Some(section.join("\n"));
    } else {
        let mut start = k;
        while start > 0 && !stops(lines[start - 1].1) && !stops(lines[start].1) {
            start -= 1;
        }
        (start, k + 1)
    };
    Some(
        lines[start..end]
            .iter()
            .map(|(_, l)| *l)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Top-level block index of the definition footnote `id` lands on, in the
/// rendered source the Reader shows.
pub fn footnote_block(source: &str, id: &str) -> Option<usize> {
    let arena = comrak::Arena::new();
    let root = comrak::parse_document(&arena, source, &crate::render::comrak_options());
    root.children().position(|top| {
        matches!(&top.data.borrow().value, NodeValue::CodeBlock(cb)
            if cb.info.split_whitespace().next() == Some(FOOTNOTE_LANG)
                && parse_footnote_info(cb.info[FOOTNOTE_LANG.len()..].trim())
                    .is_some_and(|(_, found)| found == id))
    })
}

/// Top-level block index of the first reference to footnote `id`.
pub fn footnote_reference_block(source: &str, id: &str) -> Option<usize> {
    let url = format!("{FOOTNOTE_SCHEME}{}", crate::document_links::encode(id));
    let arena = comrak::Arena::new();
    let root = comrak::parse_document(&arena, source, &crate::render::comrak_options());
    root.children().position(|top: &AstNode<'_>| {
        top.descendants()
            .any(|n| matches!(&n.data.borrow().value, NodeValue::Link(l) if l.url == url))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_inline_block_and_code() {
        assert_eq!(strip_comments("a %%hidden%% b"), "a  b");
        assert_eq!(
            strip_comments("para one\n%%\nhidden\nlines\n%%\npara two\n"),
            "para one\npara two\n"
        );
        assert_eq!(
            strip_comments("keep `%%code%%` here"),
            "keep `%%code%%` here"
        );
        assert_eq!(
            strip_comments("```\n%%\n```\n%% x %%\n"),
            "```\n%%\n```\n",
            "fenced %% is code; the prose comment is still removed"
        );
        assert_eq!(strip_comments("100%% sure"), "100%% sure", "unpaired");
        assert_eq!(
            strip_comments("%% a `%%` b %%\nshown\n"),
            "shown\n",
            "code inside a comment is hidden with it"
        );
        assert_eq!(strip_comments("%%a%% x %%b%%"), " x ");
    }

    #[test]
    fn inline_math_follows_the_dollar_rules() {
        assert_eq!(rewrite_math("e $x^2$ f"), "e `x^2` f");
        assert_eq!(rewrite_math("$5 and $10"), "$5 and $10");
        assert_eq!(rewrite_math("$ x $"), "$ x $");
        assert_eq!(rewrite_math("$x$5"), "$x$5");
        assert_eq!(rewrite_math(r"\$x$ and $y\$"), r"\$x$ and $y\$");
        assert_eq!(rewrite_math("`$x$`"), "`$x$`");
        assert_eq!(rewrite_math("a $$x+y$$ b"), "a `x+y` b");
        assert_eq!(rewrite_math("$a`b$"), "``a`b``");
    }

    #[test]
    fn display_math_becomes_a_fence() {
        assert_eq!(
            rewrite_math("Before\n$$\na^2 + b^2\n= c^2\n$$\nAfter\n"),
            "Before\n~~~~math\na^2 + b^2\n= c^2\n~~~~\nAfter\n"
        );
        assert_eq!(rewrite_math("$$E=mc^2$$\n"), "~~~~math\nE=mc^2\n~~~~\n");
        assert_eq!(
            rewrite_math("> [!note]\n> $$\n> x\n> $$\n"),
            "> [!note]\n> ~~~~math\n> x\n> ~~~~\n"
        );
        assert_eq!(rewrite_math("$$\nnever closed\n"), "$$\nnever closed\n");
        assert_eq!(
            rewrite_math("```\n$$\nx\n$$\n```\n"),
            "```\n$$\nx\n$$\n```\n"
        );
    }

    #[test]
    fn footnotes_number_by_first_reference() {
        let out = rewrite_footnotes(
            "One[^b] two[^a] again[^b] none[^x].\n\n[^a]: Alpha.\n[^b]: Beta\n    continued.\n\nTail.\n",
        );
        assert!(out.starts_with(&format!(
            "One[¹]({FOOTNOTE_SCHEME}b) two[²]({FOOTNOTE_SCHEME}a) again[¹]({FOOTNOTE_SCHEME}b) none[^x].\n\n\nTail."
        )));
        assert!(out.contains("\n\n---\n\n~~~~footnote 1 b\nBeta\ncontinued.\n~~~~\n"));
        assert!(out.contains("~~~~footnote 2 a\nAlpha.\n~~~~\n"));
        assert!(!out.contains("[^a]:"), "definitions moved");
    }

    #[test]
    fn inline_and_nested_footnotes() {
        let out =
            rewrite_footnotes("Text^[an [[x]] note] and[^n].\n\n[^n]: See[^m].\n[^m]: Deep.\n");
        assert!(out.starts_with(&format!(
            "Text[¹]({FOOTNOTE_SCHEME}inline-1) and[²]({FOOTNOTE_SCHEME}n)."
        )));
        assert!(out.contains("footnote 1 inline-1\nan [[x]] note\n"));
        assert!(out.contains(&format!("footnote 2 n\nSee[³]({FOOTNOTE_SCHEME}m).\n")));
        assert!(out.contains("footnote 3 m\nDeep.\n"));
    }

    #[test]
    fn footnotes_in_code_are_text() {
        let src = "`[^1]`\n\n```\n[^1]: no\n```\n";
        assert_eq!(rewrite_footnotes(src), src);
        let unreferenced = "Body.\n\n[^1]: Orphan.\n";
        assert_eq!(rewrite_footnotes(unreferenced), "Body.\n\n");
    }

    #[test]
    fn footnote_landing_blocks() {
        let out = rewrite_footnotes("# T\n\nSee[^1].\n\n[^1]: Note.\n");
        assert_eq!(footnote_reference_block(&out, "1"), Some(1));
        // heading, paragraph, rule, footnote
        assert_eq!(footnote_block(&out, "1"), Some(3));
        assert_eq!(footnote_block(&out, "2"), None);
    }

    #[test]
    fn block_ids_become_markers() {
        assert_eq!(
            rewrite_block_ids("A paragraph. ^p1\n"),
            "A paragraph.<!--^p1-->\n"
        );
        assert_eq!(rewrite_block_ids("- item ^li\r\n"), "- item<!--^li-->\r\n");
        assert_eq!(
            rewrite_block_ids("| a |\n|---|\n\n^table-1\n"),
            "| a |\n|---|\n\n<!--^table-1-->\n"
        );
        assert_eq!(rewrite_block_ids("`x ^code`\n"), "`x ^code`\n");
        assert_eq!(rewrite_block_ids("x^2 ^ y\n"), "x^2 ^ y\n");
        assert_eq!(marker_id("<!--^p1-->\n"), Some("p1"));
        assert_eq!(marker_id("<!-- p1 -->"), None);
    }

    #[test]
    fn block_sections() {
        let body = "# H\n\nFirst line\nsecond line ^para\n\n- one\n- two ^item\n  - child\n- three\n\n| a |\n|---|\n\n^tbl\n";
        assert_eq!(
            block_section(body, "para").as_deref(),
            Some("First line\nsecond line ^para")
        );
        assert_eq!(
            block_section(body, "item").as_deref(),
            Some("- two ^item\n  - child")
        );
        assert_eq!(block_section(body, "tbl").as_deref(), Some("| a |\n|---|"));
        assert_eq!(block_section(body, "nope"), None);
        assert_eq!(block_section("`a ^c`\n", "c"), None);
    }
}
