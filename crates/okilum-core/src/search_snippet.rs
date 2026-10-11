//! Plain-text search snippets (#654).
//!
//! The index stores raw Markdown, so Tantivy's snippet is a window of source:
//! `**bold**`, `[[target|alias]]`, `[text](url)`, heading hashes, list
//! markers and code fences. Search results show readable text instead, with
//! the matched terms still marked.
//!
//! The input is the snippet HTML (`SearchHit::snippet_html`): escaped text
//! with `<b>`..`</b>` around matches. Tantivy escapes the fragment itself, so
//! the only tags in it are its own marks. Every source byte is mapped to an
//! output position while stripping, so a match keeps pointing at the same
//! word. A match that falls on text the snippet hides (a wikilink target
//! behind its alias, a link URL) marks the visible text that stands for it,
//! rather than disappearing.
//!
//! Hand-rolled for the same reason as `render::strip_inline_markdown`: the
//! fragment starts and ends mid-construct, and the goal is readable text, not
//! a faithful AST.

use std::ops::Range;

mod source;
pub(crate) use source::source_snippet;

/// A snippet ready to show: plain text plus byte ranges of the matches in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlainSnippet {
    pub text: String,
    /// Sorted, non-overlapping, non-empty, on char boundaries of `text`.
    pub highlights: Vec<Range<usize>>,
    /// One secondary reason line, only for matches in hidden link destinations.
    pub hidden_match: Option<MatchContext>,
    /// A match in confirmed leading frontmatter, formatted independently of prose.
    pub property_match: Option<MatchContext>,
    /// A match only in the file name, when the displayed title (the first H1)
    /// differs from it. Set by the presentation layer, never by the engine.
    pub name_match: Option<MatchContext>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MatchContext {
    pub text: String,
    pub highlights: Vec<Range<usize>>,
    /// Local destinations whose display labels can use the caller's title cache.
    pub links: Vec<LinkMatch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkMatch {
    pub target: String,
    pub wiki: bool,
    pub label: Range<usize>,
}

impl MatchContext {
    /// Replace labels from right to left so all remaining source byte ranges stay valid.
    /// A hidden-target match marks its human title, just as an alias stands for a target.
    pub fn resolve_link_labels(&mut self, mut label: impl FnMut(&str, bool) -> String) {
        for link in self.links.iter().rev() {
            let title = label(&link.target, link.wiki);
            let old = link.label.clone();
            let marked = self
                .highlights
                .iter()
                .any(|r| r.start < old.end && r.end > old.start);
            self.text.replace_range(old.clone(), &title);
            self.highlights = self
                .highlights
                .iter()
                .filter_map(|r| {
                    if r.end <= old.start {
                        Some(r.clone())
                    } else if r.start >= old.end {
                        Some(r.start - old.len() + title.len()..r.end - old.len() + title.len())
                    } else {
                        None
                    }
                })
                .collect();
            if marked && !title.is_empty() {
                self.highlights.push(old.start..old.start + title.len());
            }
        }
        self.highlights.sort_by_key(|r| r.start);
        self.links.clear();
    }
}

#[derive(Clone)]
struct HiddenDestination {
    range: Range<usize>,
    url: bool,
    wiki: bool,
}

fn hidden_context(
    raw: &str,
    marks: &[Range<usize>],
    destinations: &[HiddenDestination],
) -> Option<MatchContext> {
    let mut result = MatchContext::default();
    for destination in destinations {
        let span = &destination.range;
        let hits: Vec<_> = marks
            .iter()
            .filter(|m| m.start < span.end && m.end > span.start)
            .collect();
        if hits.is_empty() {
            continue;
        }
        if !result.text.is_empty() {
            result.text.push_str(" · ");
        }
        result.text.push_str(if destination.url {
            "Link URL: "
        } else {
            "Link target: "
        });
        let source = &raw[span.clone()];
        let label_start = result.text.len();
        let end = source.find(['#', '^']).unwrap_or(source.len());
        let extension = (!destination.url && source[..end].ends_with(".md"))
            .then_some(end.saturating_sub(3)..end);
        // Keep long URLs bounded around the first matching character; byte
        // positions are still mapped from the exact unescaped source fragment.
        let chars: Vec<_> = source.char_indices().collect();
        let first = chars
            .iter()
            .position(|(i, c)| span.start + i + c.len_utf8() > hits[0].start)
            .unwrap_or(0);
        let from = if destination.url {
            first.saturating_sub(32)
        } else {
            0
        };
        let to = if destination.url {
            (from + 120).min(chars.len())
        } else {
            chars.len()
        };
        if from > 0 {
            result.text.push('…');
        }
        for &(i, c) in &chars[from..to] {
            if extension.as_ref().is_some_and(|r| r.contains(&i)) {
                continue;
            }
            let start = result.text.len();
            result.text.push(if c.is_whitespace() { ' ' } else { c });
            if hits
                .iter()
                .any(|m| m.start < span.start + i + c.len_utf8() && m.end > span.start + i)
            {
                let end = result.text.len();
                if let Some(last) = result.highlights.last_mut().filter(|r| r.end == start) {
                    last.end = end;
                } else {
                    result.highlights.push(start..end);
                }
            }
        }
        if to < chars.len() {
            result.text.push('…');
        }
        if !destination.url {
            result.links.push(LinkMatch {
                target: source.to_owned(),
                wiki: destination.wiki,
                label: label_start..result.text.len(),
            });
        }
    }
    (!result.text.is_empty()).then_some(result)
}

/// Snippet HTML (`<b>` marks, escaped text) to a plain-text snippet.
pub fn plain_snippet(html: &str) -> PlainSnippet {
    let (raw, marks) = parse_marked(html);
    plain_marked(&raw, &marks)
}

fn plain_marked(raw: &str, marks: &[Range<usize>]) -> PlainSnippet {
    let stripped = strip(raw);
    let mut highlights: Vec<Range<usize>> = Vec::new();
    let hidden_match = hidden_context(raw, marks, &stripped.hidden);
    for mark in marks {
        let range = stripped.map(mark.clone());
        if range.is_empty() {
            continue;
        }
        match highlights.last_mut() {
            Some(last) if range.start <= last.end => {
                last.start = last.start.min(range.start);
                last.end = last.end.max(range.end);
            }
            _ => highlights.push(range),
        }
    }
    PlainSnippet {
        text: stripped.text,
        highlights,
        hidden_match,
        property_match: None,
        name_match: None,
    }
}

/// Unescapes Tantivy's snippet HTML, returning the source fragment and the
/// byte ranges of its `<b>` marks in that fragment.
fn parse_marked(html: &str) -> (String, Vec<Range<usize>>) {
    const ENTITIES: [(&str, char); 6] = [
        ("&amp;", '&'),
        ("&lt;", '<'),
        ("&gt;", '>'),
        ("&quot;", '"'),
        ("&#x27;", '\''),
        ("&#39;", '\''),
    ];
    let mut raw = String::with_capacity(html.len());
    let mut marks = Vec::new();
    let mut open = None;
    let mut rest = html;
    while let Some(c) = rest.chars().next() {
        if let Some(r) = rest.strip_prefix("<b>") {
            open = Some(raw.len());
            rest = r;
        } else if let Some(r) = rest.strip_prefix("</b>") {
            if let Some(start) = open.take() {
                marks.push(start..raw.len());
            }
            rest = r;
        } else if let Some((entity, ch)) = ENTITIES.iter().find(|(e, _)| rest.starts_with(e)) {
            raw.push(*ch);
            rest = &rest[entity.len()..];
        } else {
            raw.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    (raw, marks)
}

/// Stripped text plus, for every source byte offset (and the end), where a
/// range starting or ending there lands in `text`.
struct Stripped {
    hidden: Vec<HiddenDestination>,
    text: String,
    start: Vec<usize>,
    end: Vec<usize>,
}

impl Stripped {
    fn map(&self, range: Range<usize>) -> Range<usize> {
        let start = self.start[range.start.min(self.start.len() - 1)];
        let end = self.end[range.end.min(self.end.len() - 1)];
        let (start, end) = (start.min(end), end);
        // Collapsed whitespace and trimming never leave a range covering
        // only spaces, and never one that starts or ends on a space.
        let slice = &self.text[start..end];
        let lead = slice.len() - slice.trim_start().len();
        let trail = slice.len() - slice.trim_end().len();
        if lead + trail >= slice.len() {
            return start..start;
        }
        start + lead..end - trail
    }
}

struct Stripper<'a> {
    hidden: Vec<HiddenDestination>,
    src: &'a str,
    out: String,
    start: Vec<usize>,
    end: Vec<usize>,
    /// The last thing emitted was whitespace (or nothing yet): collapse.
    space: bool,
}

impl<'a> Stripper<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            hidden: Vec::new(),
            src,
            out: String::with_capacity(src.len()),
            start: vec![0; src.len() + 1],
            end: vec![0; src.len() + 1],
            space: true,
        }
    }

    /// `src[from..to]` produces no text (a marker): a range edge anywhere in
    /// it lands where the output stands now.
    fn skip(&mut self, from: usize, to: usize) {
        let here = self.out.len();
        for i in from..to {
            self.start[i] = here;
            self.end[i] = here;
        }
    }

    /// `src[from..to]` is hidden but stands for `shown` (already emitted): a
    /// match inside it marks all of `shown`.
    fn hide(&mut self, from: usize, to: usize, shown: Range<usize>) {
        for i in from..to {
            self.start[i] = shown.start;
            self.end[i] = shown.end;
        }
    }

    /// Copy one char at `i`, collapsing whitespace runs to one space.
    fn copy(&mut self, i: usize) -> usize {
        let c = self.src[i..].chars().next().expect("char at offset");
        let len = c.len_utf8();
        if c.is_whitespace() {
            self.skip(i, i + len);
            if !self.space {
                self.out.push(' ');
                self.space = true;
            }
        } else {
            for j in i..i + len {
                self.start[j] = self.out.len();
                self.end[j] = self.out.len();
            }
            self.out.push(c);
            self.space = false;
        }
        i + len
    }

    /// A line separator: one space between lines' text.
    fn newline(&mut self, i: usize) -> usize {
        self.skip(i, i + 1);
        if !self.space {
            self.out.push(' ');
            self.space = true;
        }
        i + 1
    }
}

fn strip(src: &str) -> Stripped {
    let mut s = Stripper::new(src);
    let mut fenced = false;
    let mut line_start = 0;
    while line_start < src.len() {
        let line_end = src[line_start..]
            .find('\n')
            .map_or(src.len(), |n| line_start + n);
        let line = &src[line_start..line_end];
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            // The fence line and its info string are syntax; the code
            // between fences is shown as written.
            fenced = !fenced;
            s.skip(line_start, line_end);
        } else if fenced {
            let mut i = line_start;
            while i < line_end {
                i = s.copy(i);
            }
        } else if is_rule(trimmed) {
            s.skip(line_start, line_end);
        } else {
            let body = line_start + block_markers(line);
            s.skip(line_start, body);
            inline(&mut s, body, line_end);
        }
        if line_end < src.len() {
            s.newline(line_end);
        }
        line_start = line_end + 1;
    }
    let len = src.len();
    s.skip(len, len + 1);
    // Trim: a trailing collapsed space never belongs to the text.
    if s.out.ends_with(' ') {
        s.out.pop();
    }
    let end = s.out.len();
    for v in s.start.iter_mut().chain(s.end.iter_mut()) {
        *v = (*v).min(end);
    }
    Stripped {
        hidden: s.hidden,
        text: s.out,
        start: s.start,
        end: s.end,
    }
}

/// `---`, `***`, `___` (frontmatter fences and thematic breaks).
fn is_rule(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 3
        && ['-', '*', '_']
            .iter()
            .any(|&m| t.chars().all(|c| c == m || c == ' ') && t.starts_with(m))
}

/// Byte length of the leading block markers of a line: indentation, quotes,
/// headings, bullets, ordered-list numbers and task boxes, nested.
fn block_markers(line: &str) -> usize {
    let mut rest = line;
    loop {
        let t = rest.trim_start();
        let next = if let Some(r) = t.strip_prefix('>') {
            Some(r)
        } else if t.starts_with('#') {
            let hashes = t.len() - t.trim_start_matches('#').len();
            let r = &t[hashes..];
            (hashes <= 6 && (r.is_empty() || r.starts_with(' '))).then_some(r)
        } else if let Some(r) = ["- ", "* ", "+ "].iter().find_map(|m| t.strip_prefix(m)) {
            Some(r)
        } else if t.starts_with(|c: char| c.is_ascii_digit()) {
            let r = t.trim_start_matches(|c: char| c.is_ascii_digit());
            r.strip_prefix(". ").or_else(|| r.strip_prefix(") "))
        } else {
            ["[ ] ", "[x] ", "[X] "]
                .iter()
                .find_map(|m| t.strip_prefix(m))
        };
        match next {
            Some(r) if r.len() < rest.len() => rest = r,
            _ => return line.len() - t.len(),
        }
    }
}

/// Inline Markdown in `src[from..to]` (one line).
fn inline(s: &mut Stripper, from: usize, to: usize) {
    let bytes = s.src.as_bytes();
    let mut i = from;
    while i < to {
        match bytes[i] {
            b'\\' if i + 1 < to && bytes[i + 1].is_ascii_punctuation() => {
                s.skip(i, i + 1);
                i = s.copy(i + 1);
            }
            b'`' => {
                // Code span: delimiters go, contents stay as written.
                let run = run_of(bytes, i, to, b'`');
                let close = find_run(bytes, i + run, to, b'`', run);
                s.skip(i, i + run);
                let inner_end = close.unwrap_or(to);
                let mut j = i + run;
                while j < inner_end {
                    j = s.copy(j);
                }
                match close {
                    Some(c) => {
                        s.skip(c, c + run);
                        i = c + run;
                    }
                    None => i = to,
                }
            }
            b'!' if i + 1 < to && bytes[i + 1] == b'[' => {
                // Embed or image: the bang is syntax, the link follows.
                s.skip(i, i + 1);
                i += 1;
            }
            b'[' if i + 1 < to && bytes[i + 1] == b'[' => i = wikilink(s, i, to),
            b']' if i + 1 < to && bytes[i + 1] == b']' => {
                // Closing half of a wikilink the fragment cut into.
                s.skip(i, i + 2);
                i += 2;
            }
            b'[' => i = markdown_link(s, i, to),
            b'*' | b'~' | b'=' => {
                let run = run_of(bytes, i, to, bytes[i]);
                let marker = bytes[i] == b'*' || run == 2;
                if marker {
                    s.skip(i, i + run);
                    i += run;
                } else {
                    for _ in 0..run {
                        i = s.copy(i);
                    }
                }
            }
            b'_' => {
                // Emphasis, unless inside a word (`snake_case`).
                let run = run_of(bytes, i, to, b'_');
                let prev_word = i > from && is_word_before(s.src, i);
                let next_word = s.src[i + run..to]
                    .chars()
                    .next()
                    .is_some_and(char::is_alphanumeric);
                if prev_word && next_word {
                    for _ in 0..run {
                        i = s.copy(i);
                    }
                } else {
                    s.skip(i, i + run);
                    i += run;
                }
            }
            _ => i = s.copy(i),
        }
    }
}

/// `[[target]]`, `[[target#heading]]`, `[[target|alias]]` at `i`: the alias,
/// else the target, is shown. A link the fragment end cut into is read up
/// to the cut. Returns the offset after the link.
fn wikilink(s: &mut Stripper, i: usize, to: usize) -> usize {
    let open_end = i + 2;
    let found = s.src[open_end..to].find("]]").map(|n| open_end + n);
    if found.is_none() && to < s.src.len() {
        // Unclosed on a line that goes on: not a link, keep the words.
        s.skip(i, open_end);
        return open_end;
    }
    let close = found.unwrap_or(to);
    let inner = &s.src[open_end..close];
    s.skip(i, open_end);
    let (shown_from, shown_to, hidden) = match inner.find('|') {
        Some(bar) => (open_end + bar + 1, close, open_end..open_end + bar + 1),
        None => {
            let target_end = inner.find(['#', '^']).map_or(close, |n| open_end + n);
            (open_end, target_end, target_end..close)
        }
    };
    let shown_start = s.out.len();
    let mut j = shown_from;
    while j < shown_to {
        j = s.copy(j);
    }
    let shown = shown_start..s.out.len();
    if inner.contains('|') {
        s.hidden.push(HiddenDestination {
            range: hidden.start..hidden.end - 1,
            url: false,
            wiki: true,
        });
    }
    s.hide(hidden.start, hidden.end, shown);
    let end = if found.is_some() { close + 2 } else { close };
    s.skip(close, end);
    end
}

/// `[text](url)` at `i`: the text is shown (with its own inline Markdown
/// stripped), the URL stands behind it. A bracket that does not open a link
/// is left as written. Returns the offset after what was consumed.
fn markdown_link(s: &mut Stripper, i: usize, to: usize) -> usize {
    let found = s.src[i + 1..to]
        .find("](")
        .map(|n| i + 1 + n)
        .and_then(|mid| s.src[mid + 2..to].find(')').map(|n| (mid, mid + 2 + n)));
    let Some((mid, close)) = found else {
        return s.copy(i);
    };
    s.skip(i, i + 1);
    let shown_start = s.out.len();
    inline(s, i + 1, mid);
    let shown = shown_start..s.out.len();
    s.hidden.push(HiddenDestination {
        range: mid + 2..close,
        url: crate::document_links::is_external_url(&s.src[mid + 2..close]),
        wiki: false,
    });
    let end = if close < to { close + 1 } else { close };
    s.hide(mid, end, shown);
    end
}

fn run_of(bytes: &[u8], i: usize, to: usize, b: u8) -> usize {
    bytes[i..to].iter().take_while(|&&x| x == b).count()
}

/// The next run of exactly `len` `b` bytes at or after `from`.
fn find_run(bytes: &[u8], mut from: usize, to: usize, b: u8, len: usize) -> Option<usize> {
    while from < to {
        if bytes[from] == b {
            let run = run_of(bytes, from, to, b);
            if run == len {
                return Some(from);
            }
            from += run;
        } else {
            from += 1;
        }
    }
    None
}

fn is_word_before(src: &str, i: usize) -> bool {
    src[..i]
        .chars()
        .next_back()
        .is_some_and(char::is_alphanumeric)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The highlighted words, in order.
    fn marked(snippet: &PlainSnippet) -> Vec<&str> {
        snippet
            .highlights
            .iter()
            .map(|r| &snippet.text[r.clone()])
            .collect()
    }

    fn check(html: &str, text: &str, words: &[&str]) {
        let snippet = plain_snippet(html);
        assert_eq!(snippet.text, text, "text of {html:?}");
        assert_eq!(marked(&snippet), words, "highlights of {html:?}");
        for r in &snippet.highlights {
            assert!(snippet.text.is_char_boundary(r.start));
            assert!(snippet.text.is_char_boundary(r.end));
            assert!(r.start < r.end);
        }
    }

    #[test]
    fn plain_text_keeps_matches() {
        check(
            "the <b>quick</b> brown <b>fox</b>",
            "the quick brown fox",
            &["quick", "fox"],
        );
        check("no match here", "no match here", &[]);
        check("", "", &[]);
    }

    #[test]
    fn entities_are_unescaped_before_offsets() {
        check(
            "a &amp; b &lt;c&gt; &quot;<b>d</b>&quot; it&#x27;s",
            "a & b <c> \"d\" it's",
            &["d"],
        );
    }

    #[test]
    fn emphasis_markers_go_and_offsets_follow() {
        check(
            "some **<b>bold</b>** and *it* and __<b>under</b>__ and ~~gone~~ ==hi==",
            "some bold and it and under and gone hi",
            &["bold", "under"],
        );
        // A match that includes its own markers is clipped to the word.
        check("x <b>**bold**</b> y", "x bold y", &["bold"]);
        // Word-internal underscores are not emphasis.
        check(
            "call <b>snake_case</b> now",
            "call snake_case now",
            &["snake_case"],
        );
    }

    #[test]
    fn wikilinks_show_alias_or_target() {
        check(
            "see [[Target Note|the <b>alias</b>]] here",
            "see the alias here",
            &["alias"],
        );
        check(
            "see [[<b>Target</b> Note#Heading]] here",
            "see Target Note here",
            &["Target"],
        );
        // A match on the hidden target marks the alias standing for it.
        check(
            "see [[<b>Target</b>|alias]] here",
            "see alias here",
            &["alias"],
        );
        check(
            "see [[Note#<b>Heading</b>]] here",
            "see Note here",
            &["Note"],
        );
        // Embeds lose the bang.
        check("![[<b>diagram</b>.png]]", "diagram.png", &["diagram"]);
    }

    #[test]
    fn markdown_links_keep_text() {
        check(
            "read [the **<b>guide</b>**](https://x.test/guide) now",
            "read the guide now",
            &["guide"],
        );
        // A match in the URL marks the link text.
        check(
            "read [docs](https://<b>example</b>.test) now",
            "read docs now",
            &["docs"],
        );
        check("![alt <b>pic</b>](a.png)", "alt pic", &["pic"]);
        // A bracket that opens no link is text.
        check("array[<b>0</b>] ok", "array[0] ok", &["0"]);
    }

    #[test]
    fn block_markers_and_lines() {
        check(
            "## <b>Heading</b>\n- item one\n  * [ ] task <b>two</b>\n3. third\n> quoted",
            "Heading item one task two third quoted",
            &["Heading", "two"],
        );
        // A hash glued to a word is a tag, not a heading.
        check("#<b>tag</b> text", "#tag text", &["tag"]);
        check("---\ntitle: X\n---\nbody", "title: X body", &[]);
    }

    #[test]
    fn code_fences_and_spans() {
        check(
            "before\n```rust\nlet <b>x</b> = *y*;\n```\nafter",
            "before let x = *y*; after",
            &["x"],
        );
        check(
            "use `<b>cargo</b> **fmt**` please",
            "use cargo **fmt** please",
            &["cargo"],
        );
    }

    #[test]
    fn fragments_cut_mid_construct() {
        // Tantivy windows start and end anywhere.
        check("alias]] and <b>more</b>", "alias and more", &["more"]);
        check("a [[half <b>link</b>", "a half link", &["link"]);
        check(
            "near [[start|the <b>start</b>",
            "near the start",
            &["start"],
        );
        check("near [[<b>start</b>|the st", "near the st", &["the st"]);
        check("[[a <b>b</b>\nnext", "a b next", &["b"]);
        check("**open <b>bold</b>", "open bold", &["bold"]);
    }

    #[test]
    fn whitespace_collapses_without_shifting_matches() {
        check(
            "  lead   <b>gap</b>\n\n\n  <b>next</b>  ",
            "lead gap next",
            &["gap", "next"],
        );
    }

    #[test]
    fn unicode_offsets_stay_on_char_boundaries() {
        check(
            "**Заметка** о <b>поиске</b> — [[Работа|<b>работа</b>]]",
            "Заметка о поиске — работа",
            &["поиске", "работа"],
        );
    }

    #[test]
    fn adjacent_matches_merge() {
        check("[[<b>a</b>|x]][[<b>b</b>|x]]", "xx", &["xx"]);
    }
    #[test]
    fn hidden_destinations_explain_only_actual_hidden_matches() {
        let snippet = plain_snippet("см. [[Inbox|зебрамарс]] и [[<b>зебравенера</b>.md|текст]]");
        assert_eq!(&snippet.text[snippet.highlights[0].clone()], "текст");
        let reason = snippet.hidden_match.unwrap();
        assert_eq!(reason.text, "Link target: зебравенера");
        assert_eq!(&reason.text[reason.highlights[0].clone()], "зебравенера");
        for input in [
            "[[target|<b>alias</b>]]",
            "<b>ordinary</b> text",
            "[[<b>visible</b>]]",
        ] {
            assert!(plain_snippet(input).hidden_match.is_none(), "{input}");
        }
    }

    #[test]
    fn hidden_context_handles_entities_multiple_links_and_truncation() {
        let snippet =
            plain_snippet("[[<b>one</b>|alias]] [label](https://x.test/<b>שלום</b>?q=a&amp;b=c)");
        let reason = snippet.hidden_match.unwrap();
        assert_eq!(
            reason.text,
            "Link target: one · Link URL: https://x.test/שלום?q=a&b=c"
        );
        let hits: Vec<_> = reason
            .highlights
            .iter()
            .map(|r| &reason.text[r.clone()])
            .collect();
        assert_eq!(hits, ["one", "שלום"]);
        let reason = plain_snippet("[[<b>target</b>|alias").hidden_match.unwrap();
        assert_eq!(reason.text, "Link target: target");
        let long = format!(
            "[label](https://x.test/{}<b>שלום</b>{})",
            "я".repeat(150),
            "z".repeat(150)
        );
        let reason = plain_snippet(&long).hidden_match.unwrap();
        assert!(reason.text.starts_with("Link URL: …"));
        assert!(reason.text.ends_with('…'));
        assert_eq!(&reason.text[reason.highlights[0].clone()], "שלום");
        assert!(reason.text.chars().count() < 140);
    }
    #[test]
    fn malformed_link_keeps_unclosed_destination_visible() {
        let snippet = plain_snippet("literal [example](ordinary <b>words</b>");
        assert!(snippet.text.contains("ordinary words"));
        assert_eq!(&snippet.text[snippet.highlights[0].clone()], "words");
        assert!(snippet.hidden_match.is_none());
    }

    #[test]
    fn human_link_titles_keep_highlights_and_do_not_rewrite_visible_aliases() {
        let snippet = plain_snippet("[[Projects/<b>שלום</b>.md|overview]] and [guide](../<b>other</b>.md) [web](https://x.test/<b>needle</b>)");
        assert_eq!(snippet.text, "overview and guide web");
        let mut reason = snippet.hidden_match.unwrap();
        reason.resolve_link_labels(|target, wiki| match (target, wiki) {
            ("Projects/שלום.md", true) => "Human Hebrew title".into(),
            ("../other.md", false) => "Other guide".into(),
            _ => panic!("unexpected local destination {target}"),
        });
        assert_eq!(reason.text, "Link target: Human Hebrew title · Link target: Other guide · Link URL: https://x.test/needle");
        let words: Vec<_> = reason
            .highlights
            .iter()
            .map(|r| &reason.text[r.clone()])
            .collect();
        assert_eq!(words, ["Human Hebrew title", "Other guide", "needle"]);
        assert!(reason.links.is_empty());
    }

    #[test]
    fn colon_in_local_target_does_not_bypass_human_link_context() {
        let mut reason =
            plain_snippet("[alias](notes/<b>12:30</b>.md) [web](https://example.test/<b>term</b>)")
                .hidden_match
                .unwrap();
        reason.resolve_link_labels(|target, wiki| {
            assert!(!wiki);
            assert_eq!(target, "notes/12:30.md");
            "Meeting note".into()
        });
        assert_eq!(
            reason.text,
            "Link target: Meeting note · Link URL: https://example.test/term"
        );
        assert_eq!(&reason.text[reason.highlights[0].clone()], "Meeting note");
    }
}
