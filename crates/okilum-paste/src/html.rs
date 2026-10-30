//! HTML → Markdown.
//!
//! The DOM comes from html5ever (the parser gpui-component already uses).
//! The walk recurses per element but stops at [`MAX_DEPTH`]; anything deeper
//! is collected as text by an explicit stack, so hostile nesting cannot
//! overflow the stack. Dropping the DOM is iterative in `markup5ever_rcdom`.

use std::rc::Rc;

use html5ever::tendril::TendrilSink;
use html5ever::{parse_document, ParseOpts};
use markup5ever_rcdom::{Handle, NodeData, RcDom};

use crate::css::{self, Decl, Sheet};
use crate::render::{self, Block, InlineMode, Item, Run, Style};
use crate::text::{guess_language, normalize_language};
use crate::{Asset, AssetSource, Converted, HtmlInput};

const MAX_DEPTH: usize = 160;
const ASSET_SCHEME: &str = "okilum-paste-asset:";

pub fn html_to_markdown(input: &HtmlInput) -> Converted {
    let html = flatten_deep(input.html);
    let dom = parse_document(RcDom::default(), ParseOpts::default()).one(&*html);
    let root = dom.document.clone();
    let mut conv = Conv::new(&root, input.base_url);
    let body = find_body(&root).unwrap_or_else(|| root.clone());
    let mut blocks = conv.blocks_of(&body, &Style::default(), 0);
    let monospace = conv.text_chars > 0 && conv.text_chars == conv.mono_chars;

    let mut code_language = None;
    let content: Vec<&Block> = blocks
        .iter()
        .filter(|b| !matches!(b, Block::Empty))
        .collect();
    if let [Block::Code { lang, .. }] = content.as_slice() {
        code_language = Some(lang.clone().unwrap_or_default());
    } else if monospace
        && conv.assets.is_empty()
        && !conv.linked
        && content.iter().all(|b| matches!(b, Block::Para(_)))
    {
        let lines = raw_lines(&body);
        if lines.len() >= 2 {
            let text = lines.join("\n");
            let lang = guess_language(&text).map(str::to_string);
            code_language = Some(lang.clone().unwrap_or_default());
            blocks = vec![Block::Code { lang, text }];
        }
    }

    let markdown = render::render_blocks(&blocks);
    let plain = conv.assets.is_empty() && blocks.iter().all(is_plain_block);
    let block = blocks
        .iter()
        .any(|b| !matches!(b, Block::Para(_) | Block::Empty));
    // Assets whose placeholder did not survive (an image in a dropped
    // chrome paragraph) are not reported.
    let assets = surviving_assets(conv.assets, &markdown);
    Converted {
        markdown,
        assets,
        plain,
        block,
        code_language,
        monospace,
    }
}

/// The assets whose placeholder occurs in `markdown`, in one pass over it
/// (a page with a thousand images must not scan the text a thousand times).
fn surviving_assets(assets: Vec<Asset>, markdown: &str) -> Vec<Asset> {
    match assets.len() {
        0 => assets,
        1..=4 => assets
            .into_iter()
            .filter(|a| markdown.contains(&a.placeholder))
            .collect(),
        _ => {
            let Ok(matcher) =
                aho_corasick::AhoCorasick::new(assets.iter().map(|a| a.placeholder.as_str()))
            else {
                return assets
                    .into_iter()
                    .filter(|a| markdown.contains(&a.placeholder))
                    .collect();
            };
            let mut seen = vec![false; assets.len()];
            for m in matcher.find_overlapping_iter(markdown) {
                seen[m.pattern().as_usize()] = true;
            }
            assets
                .into_iter()
                .zip(seen)
                .filter_map(|(a, s)| s.then_some(a))
                .collect()
        }
    }
}

/// Nesting deeper than this is flattened before parsing: html5ever's scope
/// checks walk the stack of open elements, so 10 000 nested lists take
/// seconds; no real clipboard comes near this depth.
const PARSE_DEPTH: usize = 256;
/// Deepest Slack-style `data-indent` list level (Word levels stop at 8 too).
const MAX_DATA_INDENT: u32 = 8;

/// Drops tags nested deeper than [`PARSE_DEPTH`] (their text stays), in one
/// linear scan. Borrowed when nothing is that deep, which is always the case
/// for real content.
fn flatten_deep(html: &str) -> std::borrow::Cow<'_, str> {
    if scan_tags(html, None) <= PARSE_DEPTH {
        return std::borrow::Cow::Borrowed(html);
    }
    let mut out = String::with_capacity(html.len());
    scan_tags(html, Some(&mut out));
    std::borrow::Cow::Owned(out)
}

/// Returns the maximum nesting; with `out`, copies `html` minus tags that
/// are deeper than [`PARSE_DEPTH`].
fn scan_tags(html: &str, mut out: Option<&mut String>) -> usize {
    const VOID: [&str; 16] = [
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
        "source", "track", "wbr", "keygen", "basefont",
    ];
    // End tags are optional: a sibling closes them, they do not nest.
    const OPTIONAL: [&str; 14] = [
        "p", "li", "dt", "dd", "tr", "td", "th", "option", "optgroup", "thead", "tbody", "tfoot",
        "colgroup", "rt",
    ];
    const RAW: [&str; 6] = ["script", "style", "textarea", "title", "xmp", "plaintext"];
    // Raw text the converter shows as text; the rest of RAW is skipped.
    const LITERAL: [&str; 2] = ["xmp", "plaintext"];
    let bytes = html.as_bytes();
    let n = bytes.len();
    let mut depth = 0usize;
    // Open <svg>/<math>: only foreign elements self-close; HTML ignores the
    // slash of `<div/>`, which opens and nests like `<div>`.
    let mut foreign = 0usize;
    let mut max = 0usize;
    let mut copied = 0usize; // bytes of `html` already handled for `out`
    let mut i = 0usize;
    let find = |from: usize, needle: &str| {
        html.get(from..)
            .and_then(|h| h.find(needle))
            .map(|k| from + k)
    };
    while i < n {
        let Some(lt) = bytes[i..].iter().position(|b| *b == b'<').map(|k| i + k) else {
            break;
        };
        i = lt + 1;
        if i >= n {
            break;
        }
        let c = bytes[i];
        let end;
        let mut keep = true;
        if html[lt..].starts_with("<!--") {
            end = find(lt + 4, "-->").map(|k| k + 3).unwrap_or(n);
        } else if c == b'!' || c == b'?' {
            end = find(i, ">").map(|k| k + 1).unwrap_or(n);
        } else if c == b'/' || c.is_ascii_alphabetic() {
            let closing = c == b'/';
            let name_start = if closing { i + 1 } else { i };
            let name_end = bytes[name_start..]
                .iter()
                .position(|b| !(b.is_ascii_alphanumeric() || *b == b'-' || *b == b':'))
                .map(|k| name_start + k)
                .unwrap_or(n);
            let name = html[name_start..name_end].to_ascii_lowercase();
            // Skip attributes, honouring quotes.
            let mut k = name_end;
            let mut quote = 0u8;
            while k < n {
                let b = bytes[k];
                if quote != 0 {
                    if b == quote {
                        quote = 0;
                    }
                } else if b == b'"' || b == b'\'' {
                    quote = b;
                } else if b == b'>' {
                    break;
                }
                k += 1;
            }
            end = (k + 1).min(n);
            let self_closing = k > 0 && k <= n && bytes.get(k.wrapping_sub(1)) == Some(&b'/');
            let counted = !name.is_empty()
                && !VOID.contains(&name.as_str())
                && !OPTIONAL.contains(&name.as_str());
            let is_foreign = name == "svg" || name == "math";
            if closing {
                if counted {
                    keep = depth <= PARSE_DEPTH;
                    depth = depth.saturating_sub(1);
                    if is_foreign {
                        foreign = foreign.saturating_sub(1);
                    }
                }
            } else if counted && !(self_closing && (foreign > 0 || is_foreign)) {
                if is_foreign {
                    foreign += 1;
                }
                depth += 1;
                max = max.max(depth);
                keep = depth <= PARSE_DEPTH;
                if RAW.contains(&name.as_str()) {
                    // Raw text runs to its end tag.
                    let mut stop = n;
                    let mut from = end;
                    while let Some(p) = find(from, "</") {
                        let tail = &bytes[p + 2..];
                        if tail.len() >= name.len()
                            && tail[..name.len()].eq_ignore_ascii_case(name.as_bytes())
                        {
                            stop = p;
                            break;
                        }
                        from = p + 2;
                    }
                    if let Some(out) = out.as_deref_mut() {
                        if keep {
                            out.push_str(&html[copied..stop]);
                        } else {
                            // Without its wrapper the body would be markup:
                            // literal bodies stay text, the others are
                            // dropped as `skipped` drops them when shallow.
                            out.push_str(&html[copied..lt]);
                            if LITERAL.contains(&name.as_str()) {
                                escape_text(out, &html[end..stop]);
                            }
                        }
                        copied = stop;
                    }
                    i = stop;
                    continue;
                }
            }
        } else {
            continue; // a lone '<' in text
        }
        if let Some(out) = out.as_deref_mut() {
            if keep {
                out.push_str(&html[copied..end]);
            } else {
                out.push_str(&html[copied..lt]);
            }
            copied = end;
        }
        i = end;
    }
    if let Some(out) = out {
        out.push_str(&html[copied.min(n)..]);
    }
    max
}

/// Appends raw text as HTML text that parses back to the same characters.
fn escape_text(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
}

fn is_plain_block(block: &Block) -> bool {
    match block {
        Block::Empty => true,
        Block::Para(runs) => runs.iter().all(|r| match r {
            Run::Break => true,
            Run::Text { style, raw, .. } => style.is_plain() && !raw,
            Run::Image(_) => false,
        }),
        _ => false,
    }
}

// ---------------------------------------------------------------- DOM helpers

fn tag(node: &Handle) -> Option<&str> {
    match &node.data {
        NodeData::Element { name, .. } => Some(&name.local),
        _ => None,
    }
}

fn attr(node: &Handle, key: &str) -> Option<String> {
    match &node.data {
        NodeData::Element { attrs, .. } => attrs
            .borrow()
            .iter()
            .find(|a| &*a.name.local == key)
            .map(|a| a.value.to_string()),
        _ => None,
    }
}

/// Runs `f` on an attribute value without copying it.
fn with_attr<R>(node: &Handle, key: &str, f: impl FnOnce(Option<&str>) -> R) -> R {
    match &node.data {
        NodeData::Element { attrs, .. } => {
            let attrs = attrs.borrow();
            f(attrs
                .iter()
                .find(|a| &*a.name.local == key)
                .map(|a| &*a.value))
        }
        _ => f(None),
    }
}

fn has_attr(node: &Handle, key: &str) -> bool {
    with_attr(node, key, |v| v.is_some())
}

fn has_class(node: &Handle, class: &str) -> bool {
    with_attr(node, "class", |c| {
        c.is_some_and(|c| c.split_ascii_whitespace().any(|t| t == class))
    })
}

fn find_body(root: &Handle) -> Option<Handle> {
    let html = root
        .children
        .borrow()
        .iter()
        .find(|c| tag(c) == Some("html"))
        .cloned()?;
    let body = html
        .children
        .borrow()
        .iter()
        .find(|c| tag(c) == Some("body"))
        .cloned();
    body
}

/// Iterative pre-order traversal of element descendants.
fn for_each_element(root: &Handle, mut f: impl FnMut(&Handle) -> bool) {
    let mut stack: Vec<Handle> = root.children.borrow().iter().rev().cloned().collect();
    while let Some(node) = stack.pop() {
        if tag(&node).is_some() {
            if !f(&node) {
                continue;
            }
            stack.extend(node.children.borrow().iter().rev().cloned());
        }
    }
}

fn clean_text(s: &str) -> String {
    // nbsp → space; zero-width characters removed (ZWJ/ZWNJ kept between
    // non-ASCII letters or emoji, where they carry meaning).
    if s.is_ascii()
        || !s.contains([
            '\u{a0}', '\u{202f}', '\u{2007}', '\u{200b}', '\u{feff}', '\u{2060}', '\u{00ad}',
            '\u{200c}', '\u{200d}',
        ])
    {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '\u{a0}' | '\u{202f}' | '\u{2007}' => out.push(' '),
            '\u{200b}' | '\u{feff}' | '\u{2060}' | '\u{00ad}' => {}
            '\u{200c}' | '\u{200d}' => {
                let prev = i.checked_sub(1).and_then(|p| chars.get(p));
                let next = chars.get(i + 1);
                if prev.is_some_and(|p| !p.is_ascii()) && next.is_some_and(|n| !n.is_ascii()) {
                    out.push(c);
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// All text under `node`, iteratively; `<br>` and block boundaries become
/// newlines when `lines` is set, otherwise spaces.
fn gather_text(node: &Handle, sheet: &Sheet, lines: bool) -> String {
    enum Step {
        Enter(Handle),
        Leave(bool),
    }
    let mut out = String::new();
    let mut stack = vec![Step::Enter(node.clone())];
    let mut root = true;
    while let Some(step) = stack.pop() {
        match step {
            Step::Leave(block) => {
                if block {
                    out.push(if lines { '\n' } else { ' ' });
                }
            }
            Step::Enter(n) => {
                match &n.data {
                    NodeData::Text { contents } => out.push_str(&contents.borrow()),
                    NodeData::Element { .. } => {
                        let t = tag(&n).unwrap_or("");
                        if !root && (skipped(t) || is_hidden(&n, t, sheet)) {
                            continue;
                        }
                        if t == "br" {
                            out.push(if lines { '\n' } else { ' ' });
                            continue;
                        }
                        let block = !root && is_block_tag(t);
                        if block && !out.is_empty() && !out.ends_with('\n') {
                            out.push(if lines { '\n' } else { ' ' });
                        }
                        stack.push(Step::Leave(block));
                        for c in n.children.borrow().iter().rev() {
                            stack.push(Step::Enter(c.clone()));
                        }
                    }
                    NodeData::Document => {
                        for c in n.children.borrow().iter().rev() {
                            stack.push(Step::Enter(c.clone()));
                        }
                    }
                    _ => {}
                }
                root = false;
            }
        }
    }
    clean_text(&out)
}

/// Lines of text as they were laid out: block boundaries and `<br>` end
/// lines, whitespace kept. Used to rebuild code from all-monospace HTML
/// (Xcode/Terminal RTF), where indentation matters.
fn raw_lines(body: &Handle) -> Vec<String> {
    enum Step {
        Enter(Handle),
        Leave(bool, usize),
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut stack: Vec<Step> = body
        .children
        .borrow()
        .iter()
        .rev()
        .map(|c| Step::Enter(c.clone()))
        .collect();
    while let Some(step) = stack.pop() {
        match step {
            Step::Leave(block, before) => {
                if block && (!cur.is_empty() || lines.len() == before) {
                    lines.push(std::mem::take(&mut cur));
                }
            }
            Step::Enter(n) => match &n.data {
                NodeData::Text { contents } => {
                    let text = contents.borrow();
                    if text.trim().is_empty() && text.contains('\n') {
                        continue;
                    }
                    for (k, part) in text.split('\n').enumerate() {
                        if k > 0 {
                            lines.push(std::mem::take(&mut cur));
                        }
                        cur.push_str(part);
                    }
                }
                NodeData::Element { .. } => {
                    let t = tag(&n).unwrap_or("");
                    if skipped(t) || attr(&n, "aria-hidden").as_deref() == Some("true") {
                        continue;
                    }
                    if t == "br" {
                        lines.push(std::mem::take(&mut cur));
                        continue;
                    }
                    let block = is_block_tag(t);
                    if block && !cur.is_empty() {
                        lines.push(std::mem::take(&mut cur));
                    }
                    stack.push(Step::Leave(block, lines.len()));
                    for c in n.children.borrow().iter().rev() {
                        stack.push(Step::Enter(c.clone()));
                    }
                }
                _ => {}
            },
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    let mut lines: Vec<String> = lines
        .into_iter()
        .map(|l| clean_text(&l).trim_end().replace('\r', ""))
        .collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let lead = lines.iter().take_while(|l| l.is_empty()).count();
    lines.drain(..lead);
    lines
}

fn skipped(t: &str) -> bool {
    matches!(
        t,
        "script"
            | "style"
            | "noscript"
            | "template"
            | "svg"
            | "button"
            | "head"
            | "title"
            | "meta"
            | "link"
            | "iframe"
            | "object"
            | "embed"
            | "canvas"
            | "select"
            | "textarea"
            | "o:p"
            | "xml"
            | "clipboard-copy"
            | "base"
            | "map"
            | "area"
            | "audio"
            | "video"
            | "source"
            | "track"
            | "dialog"
    ) || t.starts_with("v:")
        || t.starts_with("w:")
}

fn is_hidden(node: &Handle, t: &str, sheet: &Sheet) -> bool {
    if hidden_by_attrs(node, t) {
        return true;
    }
    let decl = element_decl(node, t, sheet);
    decl.hidden || decl.mso_ignore
}

/// `hidden`, `aria-hidden`, and Wikipedia's edit links and reference marks.
fn hidden_by_attrs(node: &Handle, t: &str) -> bool {
    if has_attr(node, "hidden") || with_attr(node, "aria-hidden", |v| v == Some("true")) {
        // KaTeX hides its visual HTML and keeps MathML; other aria-hidden
        // content is decoration.
        return true;
    }
    (t == "span" && has_class(node, "mw-editsection"))
        || (t == "sup" && has_class(node, "reference"))
}

/// Class rules from `<style>`, then the inline `style` attribute.
fn element_decl(node: &Handle, t: &str, sheet: &Sheet) -> Decl {
    let mut decl = Decl::default();
    if !sheet.is_empty() {
        with_attr(node, "class", |classes| {
            sheet.apply(t, classes.unwrap_or(""), &mut decl)
        });
    }
    with_attr(node, "style", |style| {
        if let Some(style) = style {
            css::parse_decls(style, &mut decl);
        }
    });
    decl
}

fn is_block_tag(t: &str) -> bool {
    matches!(
        t,
        "p" | "div"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "main"
            | "nav"
            | "aside"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "li"
            | "dl"
            | "dt"
            | "dd"
            | "blockquote"
            | "pre"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "td"
            | "th"
            | "caption"
            | "hr"
            | "figure"
            | "figcaption"
            | "details"
            | "summary"
            | "address"
            | "center"
            | "form"
            | "fieldset"
            | "legend"
            | "body"
            | "html"
            | "hgroup"
            | "picture"
    )
}

// ---------------------------------------------------------------- builder

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    /// Table cells, headings: block boundaries become `<br>` / spaces.
    Inline {
        breaks: bool,
    },
}

struct Builder {
    mode: Mode,
    blocks: Vec<Block>,
    runs: Vec<Run>,
    ws_pending: bool,
    task: Option<bool>,
}

impl Builder {
    fn new(mode: Mode) -> Self {
        Builder {
            mode,
            blocks: Vec::new(),
            runs: Vec::new(),
            ws_pending: false,
            task: None,
        }
    }

    fn at_line_start(&self) -> bool {
        self.runs.is_empty() || matches!(self.runs.last(), Some(Run::Break))
    }

    fn untouched(&self) -> bool {
        self.blocks.is_empty() && self.runs.is_empty()
    }

    fn push_text(&mut self, text: &str, style: &Style, pre: bool) {
        let text = clean_text(text);
        let mut buf = String::new();
        for c in text.chars() {
            if pre && c == '\n' {
                self.emit(&mut buf, style);
                self.push_break();
                continue;
            }
            if c.is_ascii_whitespace() {
                self.ws_pending = true;
                continue;
            }
            if self.ws_pending {
                if !(buf.is_empty() && self.at_line_start()) {
                    buf.push(' ');
                }
                self.ws_pending = false;
            }
            buf.push(c);
        }
        self.emit(&mut buf, style);
    }

    fn emit(&mut self, buf: &mut String, style: &Style) {
        if buf.is_empty() {
            return;
        }
        let mut text = std::mem::take(buf);
        if let Some(Run::Text {
            text: last,
            style: s,
            raw: false,
        }) = self.runs.last_mut()
        {
            if s == style {
                if style.code && text.starts_with(' ') {
                    // `<code>a</code> <kbd>b</kbd>`: the space between two
                    // code elements is not code, keep two spans.
                    self.runs.push(Run::Text {
                        text: " ".into(),
                        style: Style {
                            code: false,
                            ..style.clone()
                        },
                        raw: false,
                    });
                    text.remove(0);
                    if text.is_empty() {
                        return;
                    }
                } else {
                    last.push_str(&text);
                    return;
                }
            }
        }
        self.runs.push(Run::Text {
            text,
            style: style.clone(),
            raw: false,
        });
    }

    fn push_raw(&mut self, text: &str, style: &Style) {
        if self.ws_pending && !self.at_line_start() {
            self.runs.push(Run::Text {
                text: " ".into(),
                style: Style::default(),
                raw: false,
            });
        }
        self.ws_pending = false;
        self.runs.push(Run::Text {
            text: text.to_string(),
            style: style.clone(),
            raw: true,
        });
    }

    fn push_image(&mut self, md: String) {
        if self.ws_pending && !self.at_line_start() {
            self.runs.push(Run::Text {
                text: " ".into(),
                style: Style::default(),
                raw: false,
            });
        }
        self.ws_pending = false;
        self.runs.push(Run::Image(md));
    }

    /// An explicit `<br>`.
    fn push_break(&mut self) {
        self.ws_pending = false;
        match self.mode {
            Mode::Inline { breaks: false } => self.ws_pending = !self.runs.is_empty(),
            _ => {
                if !self.runs.is_empty() {
                    self.runs.push(Run::Break);
                }
            }
        }
    }

    /// A `<div>`-like boundary: a line break inside a paragraph.
    fn soft_boundary(&mut self) {
        self.ws_pending = false;
        if self.runs.is_empty() || matches!(self.runs.last(), Some(Run::Break)) {
            return;
        }
        match self.mode {
            Mode::Inline { breaks: false } => self.ws_pending = true,
            _ => self.runs.push(Run::Break),
        }
    }

    /// Ends the paragraph (Normal) or separates content (Inline).
    fn flush(&mut self) {
        match self.mode {
            Mode::Inline { .. } => self.soft_boundary(),
            Mode::Normal => {
                self.ws_pending = false;
                let runs = std::mem::take(&mut self.runs);
                push_paragraphs(&mut self.blocks, runs);
            }
        }
    }

    fn take_runs(&mut self) -> Vec<Run> {
        self.ws_pending = false;
        let mut runs = std::mem::take(&mut self.runs);
        trim_runs(&mut runs);
        runs
    }

    fn push_block(&mut self, block: Block) {
        self.flush();
        self.blocks.push(block);
    }

    fn finish(mut self) -> (Vec<Block>, Vec<Run>, Option<bool>) {
        if self.mode == Mode::Normal {
            self.flush();
        }
        let runs = self.take_runs();
        (self.blocks, runs, self.task)
    }
}

fn trim_runs(runs: &mut Vec<Run>) {
    while matches!(runs.last(), Some(Run::Break)) {
        runs.pop();
    }
    while matches!(runs.first(), Some(Run::Break)) {
        runs.remove(0);
    }
    if let Some(Run::Text {
        text, raw: false, ..
    }) = runs.last_mut()
    {
        let t = text.trim_end_matches(' ').len();
        text.truncate(t);
    }
    if let Some(Run::Text {
        text, raw: false, ..
    }) = runs.first_mut()
    {
        let lead = text.len() - text.trim_start_matches(' ').len();
        text.drain(..lead);
    }
    runs.retain(|r| !matches!(r, Run::Text { text, .. } if text.is_empty()));
}

/// Splits on two or more consecutive breaks (a blank line).
fn push_paragraphs(blocks: &mut Vec<Block>, mut runs: Vec<Run>) {
    trim_runs(&mut runs);
    if runs.is_empty() {
        return;
    }
    let mut cur: Vec<Run> = Vec::new();
    let mut breaks = 0usize;
    for run in runs {
        if matches!(run, Run::Break) {
            breaks += 1;
            continue;
        }
        if breaks >= 2 {
            let mut para = std::mem::take(&mut cur);
            trim_runs(&mut para);
            if !para.is_empty() {
                blocks.push(Block::Para(para));
            }
            for _ in 1..breaks {
                blocks.push(Block::Empty);
            }
        } else if breaks == 1 {
            cur.push(Run::Break);
        }
        breaks = 0;
        cur.push(run);
    }
    trim_runs(&mut cur);
    if !cur.is_empty() {
        blocks.push(Block::Para(cur));
    }
}

// ---------------------------------------------------------------- converter

struct Conv {
    sheet: Sheet,
    base: Option<url::Url>,
    assets: Vec<Asset>,
    /// Placeholders of the remote assets already listed.
    remote_seen: std::collections::HashSet<String>,
    text_chars: usize,
    mono_chars: usize,
    linked: bool,
}

impl Conv {
    fn new(root: &Handle, base_url: Option<&str>) -> Self {
        let mut sheet = Sheet::default();
        let mut base = base_url.and_then(|b| url::Url::parse(b).ok());
        for_each_element(root, |n| match tag(n) {
            Some("style") => {
                let css = gather_style_text(n);
                sheet.add(&css);
                false
            }
            Some("base") => {
                if let Some(href) = attr(n, "href") {
                    let joined = match &base {
                        Some(b) => b.join(href.trim()).ok(),
                        None => url::Url::parse(href.trim()).ok(),
                    };
                    if joined.is_some() {
                        base = joined;
                    }
                }
                false
            }
            Some("body") => false,
            _ => true,
        });
        Conv {
            sheet,
            base,
            assets: Vec::new(),
            remote_seen: Default::default(),
            text_chars: 0,
            mono_chars: 0,
            linked: false,
        }
    }

    fn blocks_of(&mut self, node: &Handle, style: &Style, depth: usize) -> Vec<Block> {
        let mut b = Builder::new(Mode::Normal);
        self.walk_children(node, &mut b, style, false, depth);
        let (blocks, _, _) = b.finish();
        postprocess(blocks)
    }

    fn walk_children(
        &mut self,
        node: &Handle,
        b: &mut Builder,
        style: &Style,
        pre: bool,
        depth: usize,
    ) {
        if depth > MAX_DEPTH {
            let text = gather_text(node, &self.sheet, false);
            self.count(&text, style);
            b.push_text(&text, style, false);
            return;
        }
        // The DOM is not mutated while converting: holding the borrow is safe.
        let children = node.children.borrow();
        for child in children.iter() {
            match &child.data {
                NodeData::Text { contents } => {
                    let text = contents.borrow();
                    self.count(&text, style);
                    b.push_text(&text, style, pre);
                }
                NodeData::Element { .. } => self.element(child, b, style, pre, depth + 1),
                _ => {}
            }
        }
    }

    fn count(&mut self, text: &str, style: &Style) {
        let n = text
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '\u{a0}')
            .count();
        self.text_chars += n;
        if style.code {
            self.mono_chars += n;
        }
    }

    fn element(&mut self, node: &Handle, b: &mut Builder, style: &Style, pre: bool, depth: usize) {
        let t = tag(node).unwrap_or("");
        if skipped(t) {
            return;
        }
        // Checkboxes and Notion's checkbox divs make the item a task.
        if t == "input" {
            if attr(node, "type").is_some_and(|v| v.eq_ignore_ascii_case("checkbox"))
                && b.untouched()
            {
                b.task = Some(attr(node, "checked").is_some());
            }
            return;
        }
        if (has_class(node, "checkbox-on") || has_class(node, "checkbox-off")) && b.untouched() {
            b.task = Some(has_class(node, "checkbox-on"));
            return;
        }
        if t == "br" {
            return b.push_break();
        }
        if hidden_by_attrs(node, t) {
            return;
        }
        let decl = element_decl(node, t, &self.sheet);
        if decl.hidden || decl.mso_ignore {
            return;
        }

        // Word list paragraph.
        if let Some(level) = decl.mso_level {
            if matches!(
                t,
                "p" | "li" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
            ) && b.mode == Mode::Normal
            {
                let glyph = mso_glyph(node, &self.sheet);
                let (ordered, start) = glyph_kind(&glyph);
                b.flush();
                self.walk_children(node, b, &self.child_style(style, t, &decl), false, depth);
                let runs = b.take_runs();
                b.blocks.push(Block::MsoItem {
                    level: level.max(1),
                    ordered,
                    start,
                    runs,
                });
                return;
            }
        }

        let s = self.child_style(style, t, &decl);
        let pre_here = pre || decl.pre.unwrap_or(false);

        match t {
            "br" => b.push_break(),
            "img" => self.image(node, b),
            "math" => self.math(node, b, &s),
            "a" => self.link(node, b, &s, pre_here, depth),
            "code" | "kbd" | "samp" | "tt" if !pre => {
                let text = gather_text(node, &self.sheet, false);
                let text = collapse(&text);
                self.count(&text, &s);
                if text.trim().is_empty() {
                    b.push_text(&text, style, false);
                } else {
                    b.push_text(&text, &s, false);
                }
            }
            "pre" => self.pre(node, b, style, depth),
            "p" | "dt" | "dd" | "address" | "legend" | "caption" | "figcaption" => {
                let classes = attr(node, "class").unwrap_or_default();
                if classes.contains("MsoTitle") && b.mode == Mode::Normal {
                    return self.heading(node, b, 1, &s, depth);
                }
                if (classes.contains("MsoQuote") || classes.contains("MsoIntenseQuote"))
                    && b.mode == Mode::Normal
                {
                    let inner = self.blocks_of(node, &s, depth);
                    b.push_block(Block::Quote(inner));
                    return;
                }
                b.flush();
                let before = b.blocks.len();
                self.walk_children(node, b, &s, pre_here, depth);
                b.flush();
                if b.mode == Mode::Normal && b.blocks.len() == before && t == "p" {
                    b.blocks.push(Block::Empty);
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = t.as_bytes()[1] - b'0';
                self.heading(node, b, level, &s, depth)
            }
            "ul" | "ol" => self.list(node, b, &s, depth),
            "li" => {
                // A stray item outside a list.
                if b.mode == Mode::Normal {
                    let item = self.item(node, &s, depth);
                    b.push_block(Block::List {
                        ordered: false,
                        start: 1,
                        items: vec![item],
                        indent: 0,
                    });
                } else {
                    b.flush();
                    self.walk_children(node, b, &s, pre_here, depth);
                    b.flush();
                }
            }
            "blockquote" if b.mode == Mode::Normal => {
                let inner = self.blocks_of(node, &s, depth);
                b.push_block(Block::Quote(inner));
            }
            "figure" | "aside"
                if b.mode == Mode::Normal && (t == "aside" || has_class(node, "callout")) =>
            {
                let mut inner = self.blocks_of(node, &s, depth);
                // Notion callouts: the icon sits in its own div; keep it on
                // the text's line.
                if let Some(Block::Para(runs)) = inner.first_mut() {
                    let icon = matches!(runs.first(), Some(Run::Text { text, .. })
                        if text.chars().count() <= 3 && !text.chars().any(char::is_alphanumeric));
                    if icon && matches!(runs.get(1), Some(Run::Break)) {
                        runs[1] = Run::Text {
                            text: " ".into(),
                            style: Style::default(),
                            raw: false,
                        };
                    }
                }
                b.push_block(Block::Quote(inner));
            }
            "hr" => {
                if b.mode == Mode::Normal {
                    b.push_block(Block::Hr);
                } else {
                    b.flush();
                }
            }
            "table" => self.table(node, b, &s, depth),
            "summary" => {
                b.flush();
                self.walk_children(node, b, &s, pre_here, depth);
                b.flush();
            }
            _ if is_block_tag(t) => {
                b.soft_boundary();
                self.walk_children(node, b, &s, pre_here, depth);
                b.soft_boundary();
            }
            _ => {
                // Inline: span, b, i, font, sub, sup, unknown custom tags.
                self.walk_children(node, b, &s, pre_here, depth);
            }
        }
    }

    fn child_style(&self, style: &Style, t: &str, decl: &Decl) -> Style {
        let mut s = style.clone();
        match t {
            "b" | "strong" => s.bold = true,
            "i" | "em" | "cite" | "dfn" | "var" => s.italic = true,
            "s" | "strike" | "del" => s.strike = true,
            "mark" => s.mark = true,
            "code" | "kbd" | "samp" | "tt" => s.code = true,
            _ => {}
        }
        if let Some(bold) = decl.bold {
            s.bold = bold;
        }
        if let Some(italic) = decl.italic {
            s.italic = italic;
        }
        if decl.strike == Some(true) {
            s.strike = true;
        }
        if decl.mark == Some(true) {
            s.mark = true;
        }
        if let Some(mono) = decl.mono {
            if !matches!(t, "code" | "kbd" | "samp" | "tt") {
                s.code = mono;
            }
        }
        s
    }

    fn heading(&mut self, node: &Handle, b: &mut Builder, level: u8, style: &Style, depth: usize) {
        if b.mode != Mode::Normal {
            b.flush();
            self.walk_children(node, b, style, false, depth);
            b.flush();
            return;
        }
        let mut hb = Builder::new(Mode::Inline { breaks: false });
        let mut s = style.clone();
        s.bold = false;
        self.walk_children(node, &mut hb, &unbold(&s), false, depth);
        let (_, runs, _) = hb.finish();
        let runs: Vec<Run> = runs.into_iter().map(strip_bold).collect();
        if runs.iter().any(|r| !matches!(r, Run::Break)) {
            b.push_block(Block::Heading(level, runs));
        } else {
            b.flush();
        }
    }

    fn link(&mut self, node: &Handle, b: &mut Builder, style: &Style, pre: bool, depth: usize) {
        let href = attr(node, "href").and_then(|h| self.resolve_link(&h));
        let Some(href) = href else {
            return self.walk_children(node, b, style, pre, depth);
        };
        let text = collapse(&gather_text(node, &self.sheet, false));
        let text = text.trim();
        let has_img = {
            let mut found = false;
            for_each_element(node, |n| {
                if tag(n) == Some("img") {
                    found = true;
                }
                !found
            });
            found
        };
        if !has_img && !text.is_empty() && same_target(text, &href) {
            self.count(text, style);
            let mut s = style.clone();
            s.link = None;
            b.push_text(text, &s, false);
            return;
        }
        if text.is_empty() && !has_img {
            return;
        }
        let mut s = style.clone();
        s.link = Some(Rc::from(href.as_str()));
        s.code = style.code;
        if !text.is_empty() {
            self.linked = true;
        }
        self.walk_children(node, b, &s, pre, depth);
    }

    fn resolve_link(&self, href: &str) -> Option<String> {
        let href = href.trim();
        if href.is_empty() {
            return None;
        }
        let lower = href.to_ascii_lowercase();
        if lower.starts_with("javascript:")
            || lower.starts_with("vbscript:")
            || lower.starts_with("data:")
        {
            return None;
        }
        if let Ok(abs) = url::Url::parse(href) {
            if let Some(target) = unwrap_redirect(&abs) {
                return Some(target);
            }
            return allowed_scheme(abs.scheme()).then(|| href.to_string());
        }
        let base = self.base.as_ref()?;
        let joined = base.join(href).ok()?;
        allowed_scheme(joined.scheme()).then(|| joined.to_string())
    }

    fn image(&mut self, node: &Handle, b: &mut Builder) {
        let alt = collapse(&clean_text(&attr(node, "alt").unwrap_or_default()))
            .trim()
            .to_string();
        let classes = attr(node, "class").unwrap_or_default();
        let emoji = attr(node, "data-emoji")
            .or_else(|| attr(node, "data-stringify-emoji").map(|_| alt.clone()))
            .or_else(|| {
                (classes.to_ascii_lowercase().contains("emoji") && !alt.is_empty())
                    .then(|| alt.clone())
            });
        if let Some(e) = emoji.filter(|e| !e.is_empty()) {
            let s = Style::default();
            b.push_text(&e, &s, false);
            return;
        }
        if is_tracking_pixel(node) {
            return;
        }
        let src = attr(node, "src").unwrap_or_default();
        let src = src.trim();
        if src.is_empty() {
            return;
        }
        let lower = src.to_ascii_lowercase();
        let source = if lower.starts_with("data:") {
            match decode_data_uri(src) {
                Some((mime, bytes)) if mime.starts_with("image/") && !bytes.is_empty() => {
                    AssetSource::Data { mime, bytes }
                }
                _ => return,
            }
        } else if lower.starts_with("file:") {
            match file_url_path(src) {
                Some(path) => AssetSource::LocalFile(path),
                None => return,
            }
        } else {
            let abs = if lower.starts_with("http://") || lower.starts_with("https://") {
                Some(src.to_string())
            } else {
                self.base
                    .as_ref()
                    .and_then(|base| base.join(src).ok())
                    .map(|u| u.to_string())
            };
            match abs {
                Some(u) if u.starts_with("http://") || u.starts_with("https://") => {
                    AssetSource::Remote(u)
                }
                _ => return,
            }
        };
        let alt_md = render::escape_alt(&alt);
        let placeholder = match &source {
            AssetSource::Remote(u) => format!("![{alt_md}]({})", render::link_dest(u)),
            _ => format!("![{alt_md}]({ASSET_SCHEME}{})", self.assets.len()),
        };
        let duplicate = matches!(source, AssetSource::Remote(_))
            && !self.remote_seen.insert(placeholder.clone());
        if !duplicate {
            self.assets.push(Asset {
                source,
                alt,
                placeholder: placeholder.clone(),
            });
        }
        b.push_image(placeholder);
    }

    fn math(&mut self, node: &Handle, b: &mut Builder, style: &Style) {
        let mut tex = None;
        for_each_element(node, |n| {
            if tag(n) == Some("annotation")
                && attr(n, "encoding").is_some_and(|e| e.eq_ignore_ascii_case("application/x-tex"))
            {
                tex = Some(gather_text(n, &self.sheet, false));
                return false;
            }
            tex.is_none()
        });
        match tex {
            Some(tex) => {
                let tex = tex.trim();
                let display = attr(node, "display").as_deref() == Some("block");
                let md = if display {
                    format!("$${tex}$$")
                } else {
                    format!("${tex}$")
                };
                self.linked = true; // math is formatting
                b.push_raw(&md, &Style::default());
            }
            None => {
                let text = gather_text(node, &self.sheet, false);
                b.push_text(&text, style, false);
            }
        }
    }

    fn pre(&mut self, node: &Handle, b: &mut Builder, style: &Style, depth: usize) {
        let codes = code_elements(node);
        let mut text = String::new();
        if codes.is_empty() {
            text = gather_code(node, &self.sheet);
        } else {
            for code in &codes {
                text.push_str(&gather_code(code, &self.sheet));
            }
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let text = text.trim_end_matches([' ', '\n', '\t']).to_string();
        let text = text.trim_start_matches('\n').to_string();
        let lang = language_of(node)
            .or_else(|| codes.iter().find_map(language_of))
            .or_else(|| ancestors_language(node))
            .or_else(|| guess_language(&text).map(str::to_string));
        let n = text.chars().filter(|c| !c.is_whitespace()).count();
        self.text_chars += n;
        self.mono_chars += n;
        if b.mode != Mode::Normal {
            let mut s = style.clone();
            s.code = true;
            b.flush();
            for (k, line) in text.split('\n').enumerate() {
                if k > 0 {
                    b.push_break();
                }
                b.push_text(line, &s, false);
            }
            b.flush();
            return;
        }
        let _ = depth;
        if text.trim().is_empty() {
            b.flush();
            return;
        }
        b.push_block(Block::Code { lang, text });
    }

    fn list(&mut self, node: &Handle, b: &mut Builder, style: &Style, depth: usize) {
        if b.mode != Mode::Normal {
            b.flush();
            for child in node.children.borrow().iter() {
                if tag(child).is_some() {
                    b.flush();
                    let mut s = style.clone();
                    s.bold = style.bold;
                    self.walk_children(child, b, &s, false, depth + 1);
                    b.flush();
                }
            }
            return;
        }
        let ordered = tag(node) == Some("ol");
        let start = attr(node, "start")
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(1);
        // Clamped like Word list levels: sibling lists nest by this number,
        // so it bounds the nesting depth that the DOM depth guard can't see.
        let indent = attr(node, "data-indent")
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0)
            .min(MAX_DATA_INDENT);
        let mut items: Vec<Item> = Vec::new();
        let children: Vec<Handle> = node.children.borrow().iter().cloned().collect();
        for child in &children {
            match tag(child) {
                Some("li") => {
                    if is_hidden(child, "li", &self.sheet) {
                        continue;
                    }
                    let item = self.item(child, style, depth + 1);
                    items.push(item);
                }
                Some("ul") | Some("ol") => {
                    // Google Docs nests lists as siblings of the items.
                    let mut nb = Builder::new(Mode::Normal);
                    self.list(child, &mut nb, style, depth + 1);
                    let (nested, _, _) = nb.finish();
                    match items.last_mut() {
                        Some(last) => last.blocks.extend(nested),
                        None => items.push(Item {
                            task: None,
                            blocks: nested,
                        }),
                    }
                }
                Some(t) if skipped(t) => {}
                Some(_) => {
                    let blocks = self.blocks_of(child, style, depth + 1);
                    if blocks.iter().all(|b| matches!(b, Block::Empty)) {
                        continue;
                    }
                    match items.last_mut() {
                        Some(last) => last.blocks.extend(blocks),
                        None => items.push(Item { task: None, blocks }),
                    }
                }
                None => {
                    if let NodeData::Text { contents } = &child.data {
                        if !contents.borrow().trim().is_empty() {
                            let mut tb = Builder::new(Mode::Normal);
                            tb.push_text(&contents.borrow(), style, false);
                            let (blocks, _, _) = tb.finish();
                            match items.last_mut() {
                                Some(last) => last.blocks.extend(blocks),
                                None => items.push(Item { task: None, blocks }),
                            }
                        }
                    }
                }
            }
        }
        if items.is_empty() {
            b.flush();
            return;
        }
        b.push_block(Block::List {
            ordered,
            start,
            items,
            indent,
        });
    }

    fn item(&mut self, node: &Handle, style: &Style, depth: usize) -> Item {
        let mut ib = Builder::new(Mode::Normal);
        if attr(node, "role").as_deref() == Some("checkbox") {
            ib.task = Some(attr(node, "aria-checked").as_deref() == Some("true"));
        }
        let has_checked_class =
            has_class(node, "checked") || has_class(node, "task-list-item-checked");
        self.walk_children(node, &mut ib, style, false, depth);
        let (blocks, _, mut task) = ib.finish();
        if task.is_none()
            && (has_class(node, "task-list-item") || has_checked_class)
            && has_checked_class
        {
            task = Some(true);
        }
        Item {
            task,
            blocks: postprocess(blocks),
        }
    }

    fn table(&mut self, node: &Handle, b: &mut Builder, style: &Style, depth: usize) {
        let rows = table_rows(node);
        let cols = rows
            .iter()
            .map(|r| r.iter().map(colspan).sum::<usize>())
            .max()
            .unwrap_or(0);
        let nested = {
            let mut found = false;
            for_each_element(node, |n| {
                if tag(n) == Some("table") {
                    found = true;
                }
                !found
            });
            found
        };
        let role = attr(node, "role").unwrap_or_default();
        let layout = role == "presentation" || role == "none" || (nested && cols <= 1);
        if b.mode != Mode::Normal || layout || rows.is_empty() {
            // Flatten: rows are lines, cells are separated by spaces.
            b.flush();
            for row in &rows {
                for cell in row {
                    if layout && b.mode == Mode::Normal {
                        let inner = self.blocks_of(cell, style, depth + 1);
                        b.flush();
                        b.blocks.extend(inner);
                    } else {
                        b.push_text(" ", style, false);
                        self.walk_children(cell, b, style, false, depth + 1);
                        b.ws_pending = true;
                    }
                }
                b.flush();
            }
            return;
        }
        if rows.len() < 2 || cols < 2 {
            // A single cell, row or column (spreadsheet copies always carry
            // HTML): its cells' content, as text, never a header-only or
            // one-column table (#1611).
            b.flush();
            for row in &rows {
                for cell in row {
                    let inner = self.blocks_of(cell, style, depth + 1);
                    b.flush();
                    b.blocks.extend(inner);
                }
            }
            return;
        }
        let mut out: Vec<Vec<String>> = Vec::with_capacity(rows.len());
        // Rows still covered by a cell's rowspan, per column: those slots
        // stay empty so later cells keep their column (Wikipedia tables).
        let mut covered: Vec<usize> = Vec::new();
        for (r, row) in rows.iter().enumerate() {
            let mut cells = Vec::with_capacity(cols);
            let skip_covered = |cells: &mut Vec<String>, covered: &mut Vec<usize>| {
                while let Some(left) = covered.get_mut(cells.len()).filter(|left| **left > 0) {
                    *left -= 1;
                    cells.push(String::new());
                }
            };
            for cell in row {
                skip_covered(&mut cells, &mut covered);
                let mut cb = Builder::new(Mode::Inline { breaks: true });
                let mut s = style.clone();
                if tag(cell) == Some("th") {
                    s.bold = true;
                }
                let decl = element_decl(cell, tag(cell).unwrap_or("td"), &self.sheet);
                let s = self.child_style(&s, "td", &decl);
                self.walk_children(cell, &mut cb, &s, false, depth + 2);
                let (_, mut runs, _) = cb.finish();
                if r == 0 {
                    runs = runs.into_iter().map(strip_bold).collect();
                }
                let below = rowspan(cell, rows.len() - r) - 1;
                for k in 0..colspan(cell) {
                    if covered.len() <= cells.len() {
                        covered.resize(cells.len() + 1, 0);
                    }
                    covered[cells.len()] = below;
                    cells.push(if k == 0 {
                        render::render_inline(&runs, InlineMode::Cell)
                    } else {
                        String::new()
                    });
                }
            }
            // Covered slots after the row's last cell.
            while cells.len() < covered.len() {
                if covered[cells.len()] > 0 {
                    covered[cells.len()] -= 1;
                }
                cells.push(String::new());
            }
            out.push(cells);
        }
        let width = out.iter().map(Vec::len).max().unwrap_or(0).max(cols);
        for cells in &mut out {
            cells.resize(width, String::new());
        }
        b.push_block(Block::Table { rows: out });
    }
}

fn unbold(s: &Style) -> Style {
    let mut s = s.clone();
    s.bold = false;
    s
}

fn strip_bold(run: Run) -> Run {
    match run {
        Run::Text {
            text,
            mut style,
            raw,
        } => {
            style.bold = false;
            Run::Text { text, style, raw }
        }
        r => r,
    }
}

fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            ws = true;
        } else {
            if ws && !out.is_empty() {
                out.push(' ');
            }
            ws = false;
            out.push(c);
        }
    }
    if ws && !out.is_empty() {
        out.push(' ');
    }
    out
}

/// Google's and Outlook's click-tracking wrappers, unwrapped to the target.
fn unwrap_redirect(url: &url::Url) -> Option<String> {
    let host = url.host_str()?;
    let key = match (host, url.path()) {
        ("www.google.com" | "google.com", "/url") => "q",
        (h, _) if h.ends_with("safelinks.protection.outlook.com") => "url",
        _ => return None,
    };
    let target = url.query_pairs().find(|(k, _)| k == key)?.1.into_owned();
    let parsed = url::Url::parse(&target).ok()?;
    matches!(parsed.scheme(), "http" | "https").then_some(target)
}

fn same_target(text: &str, href: &str) -> bool {
    let strip = |s: &str| s.trim_end_matches('/').to_string();
    let t = strip(text);
    let h = strip(href);
    t == h || h.strip_prefix("mailto:").is_some_and(|m| m == t)
}

fn allowed_scheme(scheme: &str) -> bool {
    matches!(
        scheme,
        "http"
            | "https"
            | "mailto"
            | "obsidian"
            | "file"
            | "ftp"
            | "tel"
            | "zotero"
            | "x-devonthink-item"
            | "message"
            | "things"
            | "raycast"
            | "slack"
            | "notion"
            | "vscode"
            | "okilum"
    )
}

fn is_tracking_pixel(node: &Handle) -> bool {
    let num = |v: Option<String>| {
        v.and_then(|s| s.trim().trim_end_matches("px").trim().parse::<f32>().ok())
    };
    let w = num(attr(node, "width"));
    let h = num(attr(node, "height"));
    if w.is_some_and(|w| w <= 1.0) && h.is_some_and(|h| h <= 1.0) {
        return true;
    }
    if let Some(style) = attr(node, "style") {
        // Exact `width`/`height` declarations of at most 1px, with or without
        // a trailing ';' (not `max-width`).
        let tiny = |property: &str| {
            style.split(';').any(|declaration| {
                declaration.split_once(':').is_some_and(|(name, value)| {
                    name.trim().eq_ignore_ascii_case(property)
                        && num(Some(value.to_ascii_lowercase())).is_some_and(|v| v <= 1.0)
                })
            })
        };
        if tiny("width") && tiny("height") {
            return true;
        }
    }
    false
}

fn decode_data_uri(src: &str) -> Option<(String, Vec<u8>)> {
    let rest = src.get(5..)?;
    let (meta, data) = rest.split_once(',')?;
    let mut parts = meta.split(';');
    let mime = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let base64 = meta
        .to_ascii_lowercase()
        .split(';')
        .any(|p| p.trim() == "base64");
    let bytes = if base64 {
        let cleaned: String = percent_encoding::percent_decode_str(data)
            .decode_utf8_lossy()
            .into_owned();
        decode_base64(&cleaned)?
    } else {
        percent_encoding::percent_decode_str(data).collect()
    };
    Some((
        if mime.is_empty() {
            "text/plain".into()
        } else {
            mime
        },
        bytes,
    ))
}

fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn file_url_path(src: &str) -> Option<std::path::PathBuf> {
    let rest = &src[5..];
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    // Skip the host ("localhost" or empty).
    let path = match rest.find('/') {
        Some(0) => rest,
        Some(k) if rest[..k].eq_ignore_ascii_case("localhost") => &rest[k..],
        Some(_) => return None,
        None => return None,
    };
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let decoded = percent_encoding::percent_decode_str(path)
        .decode_utf8_lossy()
        .into_owned();
    // file:///C:/Users/... → C:/Users/...
    let bytes = decoded.as_bytes();
    let decoded = if bytes.len() > 3
        && bytes[0] == b'/'
        && bytes[2] == b':'
        && bytes[1].is_ascii_alphabetic()
    {
        decoded[1..].to_string()
    } else {
        decoded
    };
    if decoded.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(decoded))
}

fn gather_style_text(node: &Handle) -> String {
    let mut s = String::new();
    for c in node.children.borrow().iter() {
        if let NodeData::Text { contents } = &c.data {
            s.push_str(&contents.borrow());
        }
    }
    s
}

fn mso_glyph(node: &Handle, sheet: &Sheet) -> String {
    let mut glyph = String::new();
    for_each_element(node, |n| {
        if !glyph.is_empty() {
            return false;
        }
        let mut d = Decl::default();
        if let Some(style) = attr(n, "style") {
            css::parse_decls(&style, &mut d);
        }
        if d.mso_ignore {
            glyph = collapse(&gather_text_unfiltered(n)).trim().to_string();
            if glyph.is_empty() {
                glyph.push(' ');
            }
            return false;
        }
        let _ = sheet;
        true
    });
    glyph.trim().to_string()
}

fn gather_text_unfiltered(node: &Handle) -> String {
    let mut out = String::new();
    let mut stack = vec![node.clone()];
    while let Some(n) = stack.pop() {
        if let NodeData::Text { contents } = &n.data {
            out.push_str(&contents.borrow());
        }
        stack.extend(n.children.borrow().iter().rev().cloned());
    }
    clean_text(&out)
}

fn glyph_kind(glyph: &str) -> (bool, u64) {
    let g = glyph.trim();
    let core = g.trim_end_matches(['.', ')']).trim_start_matches('(');
    let punct = core.len() < g.len();
    if core.is_empty() {
        return (false, 1);
    }
    if core.chars().all(|c| c.is_ascii_digit()) {
        return (true, core.parse().unwrap_or(1));
    }
    let roman = core.chars().all(|c| "ivxlcdmIVXLCDM".contains(c))
        && (core.len() > 1 || "ivxIVX".contains(core));
    let letter = core.len() == 1 && core.chars().all(|c| c.is_ascii_alphabetic());
    if punct && (roman || letter) {
        return (true, 1);
    }
    (false, 1)
}

fn code_elements(pre: &Handle) -> Vec<Handle> {
    let mut codes = Vec::new();
    for_each_element(pre, |n| {
        if tag(n) == Some("code") {
            codes.push(n.clone());
            return false;
        }
        true
    });
    codes
}

/// Text of a code block: whitespace preserved, `<br>` and line `<div>`s are
/// newlines, buttons and hidden chrome dropped.
fn gather_code(node: &Handle, sheet: &Sheet) -> String {
    enum Step {
        Enter(Handle),
        Leave,
    }
    let mut out = String::new();
    let mut stack = vec![Step::Enter(node.clone())];
    let mut root = true;
    while let Some(step) = stack.pop() {
        match step {
            Step::Leave => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            Step::Enter(n) => {
                match &n.data {
                    NodeData::Text { contents } => out.push_str(&contents.borrow()),
                    NodeData::Element { .. } => {
                        let t = tag(&n).unwrap_or("");
                        if !root && (skipped(t) || is_hidden(&n, t, sheet) || is_line_number(&n)) {
                            continue;
                        }
                        if t == "br" {
                            out.push('\n');
                            continue;
                        }
                        if !root && matches!(t, "div" | "p" | "li" | "tr") {
                            if !out.is_empty() && !out.ends_with('\n') {
                                out.push('\n');
                            }
                            stack.push(Step::Leave);
                        }
                        for c in n.children.borrow().iter().rev() {
                            stack.push(Step::Enter(c.clone()));
                        }
                    }
                    _ => {}
                }
                root = false;
            }
        }
    }
    clean_text(&out)
}

fn is_line_number(node: &Handle) -> bool {
    attr(node, "class").is_some_and(|c| {
        c.split_ascii_whitespace().any(|t| {
            matches!(
                t,
                "linenumber"
                    | "line-number"
                    | "line-numbers"
                    | "lineno"
                    | "react-syntax-highlighter-line-number"
                    | "blob-num"
            )
        })
    }) || attr(node, "data-line-number").is_some() && tag(node) == Some("td")
}

fn language_of(node: &Handle) -> Option<String> {
    for key in ["data-language", "data-lang", "lang"] {
        if let Some(v) = attr(node, key) {
            if key == "lang" && tag(node) != Some("code") {
                continue; // `lang` on most elements is a human language
            }
            if let Some(l) = normalize_language(&v) {
                return Some(l);
            }
        }
    }
    let classes = attr(node, "class")?;
    let mut tokens = classes.split_ascii_whitespace().peekable();
    while let Some(tok) = tokens.next() {
        for prefix in [
            "language-",
            "lang-",
            "highlight-source-",
            "highlight-",
            "sourceCode-",
        ] {
            if let Some(rest) = tok.strip_prefix(prefix) {
                if let Some(l) = normalize_language(rest) {
                    return Some(l);
                }
            }
        }
        if tok == "brush:" {
            if let Some(next) = tokens.peek() {
                return normalize_language(next.trim_end_matches(';'));
            }
        }
        if let Some(rest) = tok.strip_prefix("brush:") {
            return normalize_language(rest.trim_end_matches(';'));
        }
    }
    None
}

fn ancestors_language(node: &Handle) -> Option<String> {
    let mut cur = node.parent.take();
    node.parent.set(cur.clone());
    for _ in 0..4 {
        let parent = cur.as_ref()?.upgrade()?;
        if let Some(l) = language_of(&parent) {
            return Some(l);
        }
        let next = parent.parent.take();
        parent.parent.set(next.clone());
        cur = next;
    }
    None
}

fn table_rows(table: &Handle) -> Vec<Vec<Handle>> {
    let mut rows = Vec::new();
    let mut sections: Vec<Handle> = vec![table.clone()];
    let mut head_rows: Vec<Vec<Handle>> = Vec::new();
    let mut i = 0;
    while i < sections.len() {
        let section = sections[i].clone();
        let is_head = tag(&section) == Some("thead");
        for child in section.children.borrow().iter() {
            match tag(child) {
                Some("thead") | Some("tbody") | Some("tfoot") => sections.push(child.clone()),
                Some("tr") => {
                    let cells: Vec<Handle> = child
                        .children
                        .borrow()
                        .iter()
                        .filter(|c| matches!(tag(c), Some("td") | Some("th")))
                        .cloned()
                        .collect();
                    if cells.is_empty() {
                        continue;
                    }
                    if is_head {
                        head_rows.push(cells);
                    } else {
                        rows.push(cells);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    // thead first wherever it appeared.
    let mut all = head_rows;
    all.extend(rows);
    all
}

/// Rows a cell spans, at most `left` (the rows from this one to the end);
/// `rowspan="0"` spans them all.
fn rowspan(cell: &Handle, left: usize) -> usize {
    let span = attr(cell, "rowspan")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(1);
    let span = if span == 0 { left } else { span };
    span.clamp(1, left.max(1))
}

fn colspan(cell: &Handle) -> usize {
    attr(cell, "colspan")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 64)
}

// ---------------------------------------------------------------- postprocess

fn postprocess(blocks: Vec<Block>) -> Vec<Block> {
    let blocks = group_mso(blocks);
    let blocks = merge_lists(blocks);
    strip_code_chrome(blocks)
}

/// Word list paragraphs → nested lists by `levelN`.
fn group_mso(blocks: Vec<Block>) -> Vec<Block> {
    if !blocks.iter().any(|b| matches!(b, Block::MsoItem { .. })) {
        return blocks;
    }
    let mut out = Vec::new();
    let mut pending: Vec<(u32, bool, u64, Vec<Run>)> = Vec::new();
    for block in blocks {
        match block {
            Block::MsoItem {
                level,
                ordered,
                start,
                runs,
            } => {
                // A bullet list followed by a numbered one (or the reverse)
                // at the outer level starts a new list.
                if pending
                    .first()
                    .is_some_and(|p| level <= p.0 && ordered != p.1)
                {
                    out.push(build_mso_list(&std::mem::take(&mut pending), 1));
                }
                pending.push((level, ordered, start, runs))
            }
            Block::Empty if !pending.is_empty() => {
                out.push(build_mso_list(&std::mem::take(&mut pending), 1));
            }
            other => {
                if !pending.is_empty() {
                    out.push(build_mso_list(&std::mem::take(&mut pending), 1));
                }
                out.push(other);
            }
        }
    }
    if !pending.is_empty() {
        out.push(build_mso_list(&pending, 1));
    }
    out
}

fn build_mso_list(items: &[(u32, bool, u64, Vec<Run>)], _level: u32) -> Block {
    // Iterative build with an explicit stack of open lists.
    struct Open {
        level: u32,
        ordered: bool,
        start: u64,
        items: Vec<Item>,
    }
    let base = items.iter().map(|i| i.0).min().unwrap_or(1);
    let mut stack: Vec<Open> = Vec::new();
    fn close(stack: &mut Vec<Open>) {
        let done = stack.pop().unwrap();
        let block = Block::List {
            ordered: done.ordered,
            start: done.start,
            items: done.items,
            indent: 0,
        };
        let parent = stack.last_mut().unwrap();
        if parent.items.is_empty() {
            parent.items.push(Item {
                task: None,
                blocks: Vec::new(),
            });
        }
        parent.items.last_mut().unwrap().blocks.push(block);
    }
    for (level, ordered, start, runs) in items {
        let level = (*level).max(base).min(base + 8);
        while stack.len() > 1 && stack.last().unwrap().level > level {
            close(&mut stack);
        }
        let need_new = match stack.last() {
            None => true,
            Some(top) => top.level < level,
        };
        if need_new {
            // Missing intermediate levels get empty items.
            let from = stack.last().map(|t| t.level + 1).unwrap_or(level);
            for l in from..=level {
                stack.push(Open {
                    level: l,
                    ordered: *ordered,
                    start: *start,
                    items: Vec::new(),
                });
            }
        }
        let mut blocks = Vec::new();
        if !runs.is_empty() {
            blocks.push(Block::Para(runs.clone()));
        }
        stack
            .last_mut()
            .unwrap()
            .items
            .push(Item { task: None, blocks });
    }
    while stack.len() > 1 {
        close(&mut stack);
    }
    let root = stack.pop().unwrap();
    Block::List {
        ordered: root.ordered,
        start: root.start,
        items: root.items,
        indent: 0,
    }
}

/// Adjacent lists merge (Markdown cannot keep them apart); Slack's
/// `data-indent` siblings nest.
fn merge_lists(blocks: Vec<Block>) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::with_capacity(blocks.len());
    for block in blocks {
        if let Block::List {
            ordered,
            start,
            items,
            indent,
        } = block
        {
            if let Some(Block::List {
                ordered: po,
                items: pitems,
                indent: pindent,
                ..
            }) = out.last_mut()
            {
                if indent > *pindent {
                    let nested = Block::List {
                        ordered,
                        start,
                        items,
                        indent,
                    };
                    nest_into(pitems, nested);
                    continue;
                }
                if *po == ordered && indent == *pindent {
                    pitems.extend(items);
                    continue;
                }
            }
            out.push(Block::List {
                ordered,
                start,
                items,
                indent,
            });
        } else {
            out.push(block);
        }
    }
    out
}

fn nest_into(items: &mut Vec<Item>, list: Block) {
    let Block::List {
        ordered, indent, ..
    } = &list
    else {
        return;
    };
    let (ordered, indent) = (*ordered, *indent);
    if items.is_empty() {
        items.push(Item {
            task: None,
            blocks: Vec::new(),
        });
    }
    let last = items.last_mut().unwrap();
    if let Some(Block::List {
        items: inner,
        indent: ii,
        ordered: io,
        ..
    }) = last.blocks.last_mut()
    {
        if indent > *ii {
            return nest_into(inner, list);
        }
        if indent == *ii && *io == ordered {
            if let Block::List {
                items: new_items, ..
            } = list
            {
                inner.extend(new_items);
            }
            return;
        }
    }
    last.blocks.push(list);
}

const CHROME: [&str; 10] = [
    "copy",
    "copy code",
    "copied",
    "copied!",
    "copy to clipboard",
    "edit",
    "run",
    "run code",
    "wrap",
    "download",
];

fn para_text(block: &Block) -> Option<String> {
    let Block::Para(runs) = block else {
        return None;
    };
    let mut s = String::new();
    for r in runs {
        match r {
            Run::Text { text, .. } => s.push_str(text),
            Run::Break => s.push('\n'),
            Run::Image(_) => return None,
        }
    }
    Some(s.trim().to_string())
}

/// Drops "Copy code" buttons and language labels next to code blocks
/// (ChatGPT, Claude, GitHub); a label also names an unknown language.
fn strip_code_chrome(mut blocks: Vec<Block>) -> Vec<Block> {
    if !blocks.iter().any(|b| matches!(b, Block::Code { .. })) {
        return blocks;
    }
    let mut drop = vec![false; blocks.len()];
    for i in 0..blocks.len() {
        if !matches!(blocks[i], Block::Code { .. }) {
            continue;
        }
        // Up to two chrome paragraphs before, one after.
        let mut j = i;
        let mut steps = 0;
        while j > 0 && steps < 2 {
            j -= 1;
            if matches!(blocks[j], Block::Empty) {
                continue;
            }
            steps += 1;
            let Some(text) = para_text(&blocks[j]) else {
                break;
            };
            let mut chrome = true;
            for line in text.split('\n') {
                let l = line.trim().to_lowercase();
                if CHROME.contains(&l.as_str()) {
                    continue;
                }
                let label = normalize_language(&l).filter(|_| is_known_label(&l));
                let Block::Code { lang, .. } = &mut blocks[i] else {
                    unreachable!()
                };
                match (&label, lang.as_ref()) {
                    (Some(l), Some(cur)) if l == cur => {}
                    (Some(l), None) => *lang = Some(l.clone()),
                    _ if matches!(l.as_str(), "text" | "plaintext" | "plain text") => {}
                    _ => {
                        chrome = false;
                        break;
                    }
                }
            }
            if !chrome {
                break;
            }
            drop[j] = true;
        }
        if let Some(next) = blocks.get(i + 1) {
            if para_text(next).is_some_and(|t| CHROME.contains(&t.to_lowercase().as_str())) {
                drop[i + 1] = true;
            }
        }
    }
    let mut k = 0;
    blocks.retain(|_| {
        let keep = !drop[k];
        k += 1;
        keep
    });
    blocks
}

fn is_known_label(l: &str) -> bool {
    crate::text::KNOWN_LANGUAGES.contains(&l)
}

#[cfg(test)]
mod tracking_pixel_tests {
    use super::*;

    fn first_img(html: &str) -> Handle {
        let dom = parse_document(RcDom::default(), ParseOpts::default()).one(html);
        let mut found = None;
        for_each_element(&dom.document, |node| {
            if found.is_none() && tag(node) == Some("img") {
                found = Some(node.clone());
            }
            found.is_none()
        });
        found.expect("an img")
    }

    #[test]
    fn style_declarations_with_or_without_semicolon() {
        for style in [
            "width:0;height:0",
            "width: 1px; height: 1px;",
            "height:0px;width:0",
        ] {
            let html = format!("<p><img src=\"https://t.example/p.gif\" style=\"{style}\"></p>");
            assert!(is_tracking_pixel(&first_img(&html)), "{style}");
        }
        for style in ["max-width:1px;max-height:1px", "width:100px;height:0"] {
            let html = format!("<p><img src=\"https://t.example/p.gif\" style=\"{style}\"></p>");
            assert!(!is_tracking_pixel(&first_img(&html)), "{style}");
        }
    }
}

#[cfg(test)]
mod flatten_tests {
    use super::*;

    fn deep(inner: &str) -> String {
        let n = PARSE_DEPTH + 44;
        format!("{}{inner}{}", "<div>".repeat(n), "</div>".repeat(n))
    }

    /// Literal markup inside a deep `<xmp>` or `<plaintext>` stays text in
    /// the flattened HTML, so reparsing it cannot nest past the limit.
    #[test]
    fn deep_literal_bodies_do_not_reintroduce_nesting() {
        for t in ["xmp", "plaintext"] {
            let html = deep(&format!("<{t}>{}</{t}>", "<div>".repeat(1_000)));
            assert!(
                scan_tags(&html, None) > PARSE_DEPTH,
                "{t}: positive control"
            );
            let flat = flatten_deep(&html);
            assert!(matches!(flat, std::borrow::Cow::Owned(_)), "{t}");
            assert!(scan_tags(&flat, None) <= PARSE_DEPTH, "{t}");
            assert_eq!(flat.matches("&lt;div&gt;").count(), 1_000, "{t}");
        }
    }

    #[test]
    fn deep_skipped_bodies_are_dropped() {
        for t in ["script", "style", "title", "textarea"] {
            let html = deep(&format!("<p>shown</p><{t}>hidden-{t}</{t}>"));
            let flat = flatten_deep(&html);
            assert!(flat.contains("shown"), "{t}");
            assert!(!flat.contains("hidden"), "{t}: {flat:.200}");
        }
    }
}
