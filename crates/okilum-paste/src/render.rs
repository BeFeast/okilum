//! Markdown emission: inline runs with minimal escaping, blocks, GFM tables
//! and code fences.

use std::rc::Rc;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub mark: bool,
    pub code: bool,
    pub link: Option<Rc<str>>,
}

impl Style {
    fn intersect(&self, other: &Style) -> Style {
        Style {
            bold: self.bold && other.bold,
            italic: self.italic && other.italic,
            strike: self.strike && other.strike,
            mark: self.mark && other.mark,
            code: self.code && other.code,
            link: if self.link == other.link {
                self.link.clone()
            } else {
                None
            },
        }
    }

    pub fn is_plain(&self) -> bool {
        !self.bold
            && !self.italic
            && !self.strike
            && !self.mark
            && !self.code
            && self.link.is_none()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Run {
    Text {
        text: String,
        style: Style,
        /// Emitted verbatim: a bare URL or a math span.
        raw: bool,
    },
    Break,
    /// Rendered image Markdown (an asset placeholder).
    Image(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Para(Vec<Run>),
    /// An empty paragraph; only counted when monospace text is rebuilt.
    Empty,
    Heading(u8, Vec<Run>),
    Code {
        lang: Option<String>,
        text: String,
    },
    List {
        ordered: bool,
        start: u64,
        items: Vec<Item>,
        /// Slack's `data-indent`: sibling lists that are really nested.
        indent: u32,
    },
    Quote(Vec<Block>),
    Table {
        rows: Vec<Vec<String>>,
    },
    Hr,
    /// Word list paragraph, grouped into lists after the walk.
    MsoItem {
        level: u32,
        ordered: bool,
        start: u64,
        runs: Vec<Run>,
    },
}

// ---------------------------------------------------------------- inline

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InlineMode {
    Normal,
    Heading,
    Cell,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Attr {
    Link,
    Bold,
    Italic,
    Strike,
    Mark,
}

struct Seg<'a> {
    text: std::borrow::Cow<'a, str>,
    style: Style,
    raw: bool,
    ws: bool,
}

pub fn render_inline(runs: &[Run], mode: InlineMode) -> String {
    let mut out = String::new();
    let mut first = true;
    for line in runs.split(|r| matches!(r, Run::Break)) {
        if line.is_empty() {
            continue;
        }
        let rendered = render_line(line, mode);
        if rendered.is_empty() {
            continue;
        }
        if !first {
            out.push_str(match mode {
                InlineMode::Normal => "  \n",
                InlineMode::Heading => " ",
                InlineMode::Cell => "<br>",
            });
        }
        first = false;
        out.push_str(&rendered);
    }
    out
}

fn render_line(runs: &[Run], mode: InlineMode) -> String {
    // 1. Split styled text into leading whitespace, core and trailing
    //    whitespace so delimiters never sit next to a space.
    let mut segs: Vec<Seg> = Vec::new();
    let mut images: Vec<(usize, &str)> = Vec::new();
    for run in runs {
        match run {
            Run::Text { text, style, raw } => {
                if text.is_empty() {
                    continue;
                }
                if *raw {
                    segs.push(Seg {
                        text: text.as_str().into(),
                        style: style.clone(),
                        raw: *raw,
                        ws: false,
                    });
                    continue;
                }
                let core = text.trim_matches(' ');
                if core.is_empty() {
                    segs.push(Seg {
                        text: text.as_str().into(),
                        // Keeps `code` so a space inside a code element stays
                        // in the span (step 2).
                        style: Style {
                            code: style.code,
                            ..Style::default()
                        },
                        raw: false,
                        ws: true,
                    });
                    continue;
                }
                let lead = text.len() - text.trim_start_matches(' ').len();
                let trail = text.len() - text.trim_end_matches(' ').len();
                if lead > 0 {
                    segs.push(Seg {
                        text: text[..lead].into(),
                        style: Style::default(),
                        raw: false,
                        ws: true,
                    });
                }
                segs.push(Seg {
                    text: core.into(),
                    style: style.clone(),
                    raw: false,
                    ws: false,
                });
                if trail > 0 {
                    segs.push(Seg {
                        text: text[text.len() - trail..].into(),
                        style: Style::default(),
                        raw: false,
                        ws: true,
                    });
                }
            }
            Run::Image(md) => {
                images.push((segs.len(), md.as_str()));
                // A zero-width marker segment keeps the position.
                segs.push(Seg {
                    text: "".into(),
                    style: Style::default(),
                    raw: true,
                    ws: false,
                });
            }
            Run::Break => {}
        }
    }
    // 2. Whitespace takes the styles its neighbours share.
    for i in 0..segs.len() {
        if !segs[i].ws {
            continue;
        }
        let prev = segs[..i].iter().rev().find(|s| !s.ws).map(|s| &s.style);
        let next = segs[i + 1..].iter().find(|s| !s.ws).map(|s| &s.style);
        let own_code = segs[i].style.code;
        let mut style = match (prev, next) {
            (Some(p), Some(n)) => p.intersect(n),
            _ => Style::default(),
        };
        // Two code elements with a plain space between stay two spans.
        style.code &= own_code;
        segs[i].style = style;
    }
    // 3. Neighbours with the same style become one segment (one code span,
    //    one emphasis).
    let mut merged: Vec<Seg> = Vec::with_capacity(segs.len());
    let mut images_at: Vec<(usize, &str)> = Vec::with_capacity(images.len());
    let mut image_iter = images.into_iter().peekable();
    for (i, seg) in segs.into_iter().enumerate() {
        if image_iter.peek().is_some_and(|(at, _)| *at == i) {
            let (_, md) = image_iter.next().unwrap();
            images_at.push((merged.len(), md));
            merged.push(seg);
            continue;
        }
        let image_marker = images_at
            .last()
            .is_some_and(|(at, _)| *at + 1 == merged.len());
        if let Some(last) = merged.last_mut() {
            if !image_marker && !last.raw && !seg.raw && last.style == seg.style {
                last.text.to_mut().push_str(&seg.text);
                last.ws &= seg.ws;
                continue;
            }
        }
        merged.push(seg);
    }
    let segs = merged;
    let images = images_at;
    // 4. Emit.
    let mut out = String::new();
    let mut open: Vec<Attr> = Vec::new();
    let mut open_link: Option<Rc<str>> = None;
    let mut image_iter = images.into_iter().peekable();
    let mut line_start = true;
    let table = mode == InlineMode::Cell;
    let n = segs.len();
    for i in 0..n {
        if image_iter.peek().is_some_and(|(at, _)| *at == i) {
            let (_, md) = image_iter.next().unwrap();
            close_to(&mut out, &mut open, &mut open_link, 0);
            out.push_str(md);
            line_start = false;
            continue;
        }
        let seg = &segs[i];
        if seg.text.is_empty() {
            continue;
        }
        let want = attrs(&seg.style);
        let keep = common_prefix(
            &open,
            &want,
            open_link.as_deref(),
            seg.style.link.as_deref(),
        );
        close_to(&mut out, &mut open, &mut open_link, keep);
        for attr in &want[keep..] {
            out.push_str(match attr {
                Attr::Link => "[",
                Attr::Bold => "**",
                Attr::Italic => "*",
                Attr::Strike => "~~",
                Attr::Mark => "==",
            });
            if *attr == Attr::Link {
                open_link = seg.style.link.clone();
            }
            open.push(*attr);
            line_start = false;
        }
        if seg.raw {
            out.push_str(&seg.text);
        } else if seg.style.code {
            out.push_str(&code_span(&seg.text, table));
        } else {
            let next = segs[i + 1..]
                .iter()
                .find(|s| !s.text.is_empty())
                .and_then(|s| s.text.chars().next());
            let prev = out.chars().next_back();
            let in_link = open.contains(&Attr::Link);
            escape_into(&mut out, &seg.text, prev, next, line_start, in_link, table);
        }
        line_start = false;
    }
    close_to(&mut out, &mut open, &mut open_link, 0);
    // Images left after the last segment.
    for (_, md) in image_iter {
        out.push_str(md);
    }
    out.trim_matches(' ').to_string()
}

fn attrs(style: &Style) -> Vec<Attr> {
    let mut v = Vec::with_capacity(5);
    if style.link.is_some() {
        v.push(Attr::Link);
    }
    if style.bold {
        v.push(Attr::Bold);
    }
    if style.italic {
        v.push(Attr::Italic);
    }
    if style.strike {
        v.push(Attr::Strike);
    }
    if style.mark {
        v.push(Attr::Mark);
    }
    v
}

fn common_prefix(
    open: &[Attr],
    want: &[Attr],
    open_link: Option<&str>,
    want_link: Option<&str>,
) -> usize {
    let mut k = 0;
    while k < open.len() && k < want.len() && open[k] == want[k] {
        if open[k] == Attr::Link && open_link != want_link {
            break;
        }
        k += 1;
    }
    k
}

fn close_to(out: &mut String, open: &mut Vec<Attr>, open_link: &mut Option<Rc<str>>, keep: usize) {
    while open.len() > keep {
        match open.pop().unwrap() {
            Attr::Link => {
                let url = open_link.take().unwrap_or_default();
                out.push_str("](");
                out.push_str(&link_dest(&url));
                out.push(')');
            }
            Attr::Bold => out.push_str("**"),
            Attr::Italic => out.push('*'),
            Attr::Strike => out.push_str("~~"),
            Attr::Mark => out.push_str("=="),
        }
    }
}

/// A link destination that survives Markdown: spaces and angle brackets
/// percent-encoded, parentheses encoded when unbalanced.
pub fn link_dest(url: &str) -> String {
    let mut depth = 0i32;
    let mut balanced = true;
    for c in url.chars() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    balanced = false;
                }
            }
            _ => {}
        }
    }
    balanced &= depth == 0;
    let mut out = String::with_capacity(url.len());
    for c in url.chars() {
        match c {
            ' ' => out.push_str("%20"),
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            '(' if !balanced => out.push_str("%28"),
            ')' if !balanced => out.push_str("%29"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// `` `code` `` with a delimiter longer than any backtick run inside.
pub fn code_span(text: &str, table: bool) -> String {
    let text = text.replace('\n', " ");
    let longest = longest_run(&text, '`');
    let fence = "`".repeat(longest + 1);
    let pad = text.starts_with('`')
        || text.ends_with('`')
        || (text.starts_with(' ') && text.ends_with(' ') && !text.trim().is_empty());
    let body = if table {
        text.replace('|', "\\|")
    } else {
        text
    };
    if pad {
        format!("{fence} {body} {fence}")
    } else {
        format!("{fence}{body}{fence}")
    }
}

fn longest_run(text: &str, ch: char) -> usize {
    let mut best = 0;
    let mut cur = 0;
    for c in text.chars() {
        if c == ch {
            cur += 1;
            best = best.max(cur);
        } else {
            cur = 0;
        }
    }
    best
}

/// Escapes only what would otherwise become Markdown.
pub fn escape_into(
    out: &mut String,
    text: &str,
    prev: Option<char>,
    next: Option<char>,
    line_start: bool,
    in_link: bool,
    table: bool,
) {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut i = 0;
    // Line-start block markers.
    if line_start {
        i = escape_line_start(out, &chars);
    }
    let mut url_end = 0usize;
    let last_close = chars.iter().rposition(|c| *c == ']');
    while i < n {
        let c = chars[i];
        let before = if i == 0 { prev } else { Some(chars[i - 1]) };
        let after = if i + 1 < n { Some(chars[i + 1]) } else { next };
        // Bare URLs keep their characters: an escape would end up in the link.
        if i >= url_end
            && matches!(c, 'h' | 'H' | 'w' | 'W')
            && starts_url(&chars[i..])
            && before.is_none_or(|b| !b.is_alphanumeric())
        {
            url_end = i + chars[i..]
                .iter()
                .position(|c| c.is_whitespace())
                .unwrap_or(n - i);
        }
        if i < url_end {
            if table && c == '|' {
                out.push_str("\\|");
            } else {
                out.push(c);
            }
            i += 1;
            continue;
        }
        let ws = |c: Option<char>| c.is_none_or(char::is_whitespace);
        let alnum = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
        let escape = match c {
            '\\' => after.is_some_and(|a| a.is_ascii_punctuation()) || after.is_none(),
            '`' => true,
            '*' | '~' => !(ws(before) && ws(after)),
            '_' => !(ws(before) && ws(after)) && !(alnum(before) && alnum(after)),
            '=' => {
                after == Some('=') && before != Some('=') && !ws(chars.get(i + 2).copied().or(None))
            }
            '[' => in_link || last_close.is_some_and(|k| k > i),
            ']' => in_link,
            '<' => after.is_some_and(|a| a.is_ascii_alphabetic() || matches!(a, '/' | '!' | '?')),
            '&' => looks_like_entity(&chars[i..]),
            '|' => table,
            '$' => false,
            _ => false,
        };
        if escape {
            out.push('\\');
        }
        out.push(c);
        i += 1;
    }
}

fn starts_url(chars: &[char]) -> bool {
    let has = |prefix: &str| {
        prefix
            .chars()
            .enumerate()
            .all(|(k, p)| chars.get(k).is_some_and(|c| c.to_ascii_lowercase() == p))
    };
    has("http://") || has("https://") || has("www.")
}

fn looks_like_entity(chars: &[char]) -> bool {
    let mut k = 1;
    while k < chars.len()
        && k < 33
        && (chars[k].is_ascii_alphanumeric() || (k == 1 && chars[k] == '#'))
    {
        k += 1;
    }
    k > 1 && chars.get(k) == Some(&';')
}

/// Escapes a block marker at the start of a line; returns how many chars
/// were consumed.
fn escape_line_start(out: &mut String, chars: &[char]) -> usize {
    let n = chars.len();
    let sp_after = |k: usize| k >= n || chars[k] == ' ' || chars[k] == '\t';
    match chars.first() {
        Some('#') => {
            let hashes = chars.iter().take_while(|c| **c == '#').count();
            if hashes <= 6 && sp_after(hashes) {
                out.push('\\');
            }
            0
        }
        Some('>') => {
            out.push('\\');
            0
        }
        Some('-' | '+' | '*') if sp_after(1) => {
            out.push('\\');
            out.push(chars[0]);
            1
        }
        Some('=' | '-') if chars.iter().all(|c| *c == chars[0] || *c == ' ') => {
            out.push('\\');
            out.push(chars[0]);
            1
        }
        Some(c) if c.is_ascii_digit() => {
            let digits = chars.iter().take_while(|c| c.is_ascii_digit()).count();
            if digits <= 9 && matches!(chars.get(digits), Some('.' | ')')) && sp_after(digits + 1) {
                out.extend(&chars[..digits]);
                out.push('\\');
                out.push(chars[digits]);
                return digits + 1;
            }
            0
        }
        _ => 0,
    }
}

pub fn escape_alt(alt: &str) -> String {
    let mut out = String::with_capacity(alt.len());
    for c in alt.chars() {
        match c {
            '[' | ']' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\n' | '\r' | '\t' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Link text: same rules as text plus brackets.
pub fn escape_link_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    escape_into(&mut out, text, None, None, false, true, false);
    out
}

// ---------------------------------------------------------------- blocks

pub fn render_blocks(blocks: &[Block]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        let s = render_block(block);
        if !s.is_empty() {
            parts.push(s);
        }
    }
    parts.join("\n\n")
}

fn render_block(block: &Block) -> String {
    match block {
        Block::Para(runs) => render_inline(runs, InlineMode::Normal),
        Block::Empty | Block::MsoItem { .. } => String::new(),
        Block::Heading(level, runs) => {
            let text = render_inline(runs, InlineMode::Heading);
            if text.is_empty() {
                return text;
            }
            let mut text = text;
            // A trailing run of `#` would be read as the closing sequence.
            if text.ends_with('#') && !text.ends_with("\\#") {
                let pos = text.trim_end_matches('#').len();
                if text[..pos].ends_with(' ') {
                    text.insert(pos, '\\');
                }
            }
            format!("{} {}", "#".repeat((*level).clamp(1, 6) as usize), text)
        }
        Block::Code { lang, text } => fence_code(text, lang.as_deref()),
        Block::List {
            ordered,
            start,
            items,
            ..
        } => render_list(*ordered, *start, items),
        Block::Quote(inner) => {
            let body = render_blocks(inner);
            prefix_lines(&body, "> ", ">")
        }
        Block::Table { rows } => render_table(rows),
        Block::Hr => "---".to_string(),
    }
}

fn render_list(ordered: bool, start: u64, items: &[Item]) -> String {
    let mut out = String::new();
    for (k, item) in items.iter().enumerate() {
        let marker = if ordered {
            format!("{}. ", start.saturating_add(k as u64))
        } else {
            "- ".to_string()
        };
        let indent = " ".repeat(marker.len());
        let task = match item.task {
            Some(true) => "[x] ",
            Some(false) => "[ ] ",
            None => "",
        };
        let body = render_item_blocks(&item.blocks);
        if k > 0 {
            out.push('\n');
        }
        if body.is_empty() {
            out.push_str((marker.clone() + task).trim_end());
            continue;
        }
        let starts_with_list = matches!(
            item.blocks.iter().find(|b| !is_blank(b)),
            Some(Block::List { .. })
        );
        if starts_with_list && task.is_empty() {
            out.push_str(marker.trim_end());
            out.push('\n');
            out.push_str(&prefix_lines(&body, &indent, ""));
            continue;
        }
        out.push_str(&marker);
        out.push_str(task);
        let mut lines = body.split('\n');
        out.push_str(lines.next().unwrap_or(""));
        for line in lines {
            out.push('\n');
            if !line.is_empty() {
                out.push_str(&indent);
                out.push_str(line);
            }
        }
    }
    out
}

fn is_blank(block: &Block) -> bool {
    matches!(block, Block::Empty | Block::MsoItem { .. })
}

fn render_item_blocks(blocks: &[Block]) -> String {
    let mut out = String::new();
    let mut prev_para = false;
    for block in blocks {
        let s = render_block(block);
        if s.is_empty() {
            continue;
        }
        if !out.is_empty() {
            let tight = prev_para && matches!(block, Block::List { .. });
            out.push_str(if tight { "\n" } else { "\n\n" });
        }
        prev_para = matches!(block, Block::Para(_));
        out.push_str(&s);
    }
    out
}

pub fn prefix_lines(body: &str, prefix: &str, empty_prefix: &str) -> String {
    let mut out = String::with_capacity(body.len() + 16);
    for (k, line) in body.split('\n').enumerate() {
        if k > 0 {
            out.push('\n');
        }
        if line.is_empty() {
            out.push_str(empty_prefix);
        } else {
            out.push_str(prefix);
            out.push_str(line);
        }
    }
    out
}

/// A fenced code block; the fence is longer than any backtick run inside.
/// One trailing newline of `code` is dropped.
pub fn fence_code(code: &str, language: Option<&str>) -> String {
    let code = code.replace("\r\n", "\n");
    let code = code.strip_suffix('\n').unwrap_or(&code);
    let fence = "`".repeat((longest_run(code, '`') + 1).max(3));
    let lang: String = language
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '`')
        .collect();
    format!("{fence}{lang}\n{code}\n{fence}")
}

// ---------------------------------------------------------------- tables

/// `rows[0]` is the header. Cells are already-rendered inline Markdown.
pub fn render_table(rows: &[Vec<String>]) -> String {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return String::new();
    }
    let cell = |r: usize, c: usize| rows[r].get(c).map(String::as_str).unwrap_or("");
    let aligns: Vec<Align> = (0..cols)
        .map(|c| {
            let body: Vec<&str> = (1..rows.len())
                .map(|r| cell(r, c))
                .filter(|s| !s.is_empty())
                .collect();
            if !body.is_empty() && body.iter().all(|s| is_numeric(s)) {
                Align::Right
            } else {
                Align::Left
            }
        })
        .collect();
    let width = |s: &str| s.chars().count();
    let mut widths: Vec<usize> = (0..cols)
        .map(|c| {
            (0..rows.len())
                .map(|r| width(cell(r, c)))
                .max()
                .unwrap_or(0)
                .max(3)
        })
        .collect();
    let pad = widths.iter().sum::<usize>() + cols * 3 <= 160;
    if !pad {
        widths.iter_mut().for_each(|w| *w = 0);
    }
    let mut out = String::new();
    let line = |out: &mut String, r: usize| {
        out.push('|');
        for c in 0..cols {
            let s = cell(r, c);
            let fill = widths[c].saturating_sub(width(s));
            out.push(' ');
            if aligns[c] == Align::Right && pad {
                out.extend(std::iter::repeat_n(' ', fill));
                out.push_str(s);
            } else {
                out.push_str(s);
                out.extend(std::iter::repeat_n(' ', fill));
            }
            out.push_str(" |");
        }
    };
    line(&mut out, 0);
    out.push_str("\n|");
    for c in 0..cols {
        let w = widths[c].max(3);
        out.push(' ');
        match aligns[c] {
            Align::Left => out.push_str(&"-".repeat(w)),
            Align::Right => {
                out.push_str(&"-".repeat(w - 1));
                out.push(':');
            }
        }
        out.push_str(" |");
    }
    for r in 1..rows.len() {
        out.push('\n');
        line(&mut out, r);
    }
    // Empty cells leave "|  |" when unpadded; trailing spaces never occur
    // because every line ends with '|'.
    out
}

fn is_numeric(s: &str) -> bool {
    let s = s.trim();
    let s = s.trim_start_matches(['-', '+', '(', '−']);
    let s = s.trim_start_matches(['$', '€', '£', '¥', '₽', '₪', ' ']);
    let s = s.trim_end_matches(['%', ')', ' ']);
    !s.is_empty()
        && s.chars().any(|c| c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ',' | '.' | ' ' | '\'' | '\u{a0}'))
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c == '.')
}

/// Escapes a plain-text table cell (TSV): `|`, newlines → `<br>`.
pub fn plain_cell(text: &str) -> String {
    let mut out = String::new();
    escape_into(&mut out, text.trim(), None, None, false, false, true);
    out.replace("\r\n", "\n").replace('\n', "<br>")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Run {
        Run::Text {
            text: s.into(),
            style: Style::default(),
            raw: false,
        }
    }
    fn b(s: &str) -> Run {
        Run::Text {
            text: s.into(),
            style: Style {
                bold: true,
                ..Default::default()
            },
            raw: false,
        }
    }

    #[test]
    fn emphasis_moves_spaces_outside() {
        let runs = vec![t("a"), b(" bold "), t("b")];
        assert_eq!(render_inline(&runs, InlineMode::Normal), "a **bold** b");
        let runs = vec![b("x "), b("y")];
        assert_eq!(render_inline(&runs, InlineMode::Normal), "**x y**");
    }

    #[test]
    fn escapes_minimal() {
        let mut o = String::new();
        escape_into(
            &mut o,
            "# not heading, 2 * 3, snake_case, *x*, [a](b), <div>, a == b, ==hl==",
            None,
            None,
            true,
            false,
            false,
        );
        assert_eq!(
            o,
            "\\# not heading, 2 * 3, snake_case, \\*x\\*, \\[a](b), \\<div>, a == b, \\==hl=="
        );
        let mut o = String::new();
        escape_into(
            &mut o,
            "1. item see https://x.org/a_b_(c)",
            None,
            None,
            true,
            false,
            false,
        );
        assert_eq!(o, "1\\. item see https://x.org/a_b_(c)");
    }

    #[test]
    fn fences() {
        assert_eq!(fence_code("a\n", Some("rust")), "```rust\na\n```");
        assert_eq!(fence_code("x ```` y", None), "`````\nx ```` y\n`````");
        assert_eq!(code_span("a`b", false), "``a`b``");
        assert_eq!(code_span("`x", false), "`` `x ``");
    }
}
