//! XML Format code (#2912). A streaming two-pass pretty-printer over the
//! original bytes: no tree, no entity expansion, no DTD processing.
//!
//! The first pass checks well-formedness: matched tags, one root, quoted and
//! unique attributes, legal characters, character references to legal
//! characters, entity references that are predefined or declared in the
//! internal subset, comments without `--`, no text outside the root, and at
//! most [`MAX_DEPTH`] nesting levels. It is the authority for Format and for
//! Validate; the highlighting grammar only colours. The pass also decides,
//! for every element in document order, whether its content may be
//! re-indented. The second pass copies tokens byte for byte and changes only
//! the whitespace between them.
//!
//! An element's content is re-indented, one child per line one level deeper,
//! only when that cannot change its text:
//!
//! - it has child elements, comments or processing instructions and no
//!   other character content (no non-whitespace text, no CDATA, no entity or
//!   character reference);
//! - it is not under `xml:space` other than `default` (the value is resolved
//!   first; a value that cannot be resolved counts as `preserve`), and a
//!   `default` inside a preserved subtree keeps the subtree frozen;
//! - if its vocabulary gives it mixed content — XHTML, DocBook, TEI, SVG text
//!   elements, and in no namespace the HTML, DocBook and DITA names that
//!   carry prose (`p`, `b`, `i`, `li`, `para`, `emphasis`, `ph`, …) — every
//!   boundary between its tags and children must already hold whitespace.
//!   `<p><b>a</b><i>b</i></p>` reads «ab», so it stays byte for byte;
//! - data vocabularies (a Maven POM, Atom, configuration files) are
//!   element-only, so a one-line file gets indented.
//!
//! Every other element is copied verbatim from its start tag to its end tag.
//! Start and end tags are copied as written, so attributes keep their order,
//! quotes and spacing; comments, CDATA, processing instructions, the
//! declaration and DOCTYPE are copied as written. One blank line before a
//! child survives; the final newline, a byte order mark and the newline
//! style stay as the source had them.

use super::{Formatted, Invalid, LineEnding, MAX_OUTPUT_BYTES};
use std::collections::HashSet;
use std::ops::Range;

/// Cancellation is polled at least this often while scanning.
const CHECK_EVERY: usize = 16 * 1024;
/// Duplicate attributes are found by a linear scan up to this many.
const LINEAR_ATTRIBUTES: usize = 32;
/// Open elements kept as scratch: deeper input is refused before it can
/// allocate frames for millions of levels.
pub const MAX_DEPTH: usize = 10_000;

pub(super) enum Failure {
    Invalid(Invalid),
    Limit,
    Depth,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Declaration,
    Instruction,
    Comment,
    Cdata,
    Doctype,
    Start { empty: bool, preserve: bool },
    End,
    Text { blank: bool, newlines: usize },
}

struct Token {
    kind: Kind,
    span: Range<usize>,
    /// The tag name of a start or end tag.
    name: Range<usize>,
}

/// The namespace an element belongs to, as far as layout is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Vocabulary {
    None,
    Html,
    DocBook,
    Tei,
    Svg,
    Data,
}

impl Vocabulary {
    fn of(uri: Option<&str>) -> Self {
        match uri {
            Some("") => Self::None,
            Some("http://www.w3.org/1999/xhtml") => Self::Html,
            Some("http://docbook.org/ns/docbook") => Self::DocBook,
            Some("http://www.tei-c.org/ns/1.0") => Self::Tei,
            Some("http://www.w3.org/2000/svg") => Self::Svg,
            _ => Self::Data,
        }
    }

    /// Whether whitespace between this element's children may be text.
    fn mixed(self, local: &str) -> bool {
        let named = |names: &[&str]| names.iter().any(|n| n.eq_ignore_ascii_case(local));
        match self {
            Self::None => named(HTML_PROSE) || named(DOCBOOK_PROSE) || named(DITA_PROSE),
            Self::Html => named(HTML_PROSE),
            Self::DocBook => named(DOCBOOK_PROSE),
            Self::Tei => true,
            Self::Svg => named(&["text", "tspan", "textPath", "a", "title", "desc"]),
            Self::Data => false,
        }
    }
}

const HTML_PROSE: &[&str] = &[
    "a",
    "abbr",
    "address",
    "b",
    "bdi",
    "bdo",
    "big",
    "blockquote",
    "button",
    "caption",
    "cite",
    "code",
    "dd",
    "del",
    "dfn",
    "div",
    "dt",
    "em",
    "figcaption",
    "font",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "i",
    "ins",
    "kbd",
    "label",
    "legend",
    "li",
    "mark",
    "p",
    "pre",
    "q",
    "rp",
    "rt",
    "ruby",
    "s",
    "samp",
    "small",
    "span",
    "strike",
    "strong",
    "sub",
    "summary",
    "sup",
    "td",
    "th",
    "title",
    "tt",
    "u",
    "var",
];

const DOCBOOK_PROSE: &[&str] = &[
    "abbrev",
    "acronym",
    "address",
    "application",
    "attribution",
    "citation",
    "citetitle",
    "classname",
    "command",
    "computeroutput",
    "email",
    "emphasis",
    "entry",
    "filename",
    "firstterm",
    "foreignphrase",
    "function",
    "glossterm",
    "guilabel",
    "guimenu",
    "keycap",
    "lineannotation",
    "link",
    "literal",
    "literallayout",
    "markup",
    "member",
    "methodname",
    "option",
    "para",
    "parameter",
    "phrase",
    "programlisting",
    "quote",
    "remark",
    "replaceable",
    "screen",
    "simpara",
    "subscript",
    "subtitle",
    "superscript",
    "systemitem",
    "tag",
    "term",
    "title",
    "ulink",
    "uri",
    "userinput",
    "varname",
    "wordasword",
];

const DITA_PROSE: &[&str] = &[
    "apiname",
    "cmd",
    "codeph",
    "filepath",
    "info",
    "keyword",
    "menucascade",
    "parmname",
    "ph",
    "shortdesc",
    "systemoutput",
    "tm",
    "uicontrol",
    "wintitle",
    "xref",
];

struct Lexer<'a> {
    text: &'a str,
    bytes: &'a [u8],
    at: usize,
    next_check: usize,
    cancelled: &'a dyn Fn() -> bool,
    /// The last start tag's attributes: name and value (inside the quotes).
    attributes: Vec<(Range<usize>, Range<usize>)>,
    seen: HashSet<&'a str>,
    /// General entities the internal subset declares.
    entities: HashSet<&'a str>,
    /// An external subset or a parameter entity reference may declare more.
    open_entities: bool,
}

fn space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

fn name_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || matches!(b, b'_' | b':') || b >= 0x80
}

fn name_char(b: u8) -> bool {
    name_start(b) || b.is_ascii_digit() || matches!(b, b'-' | b'.')
}

/// XML 1.0 `Char`.
fn legal(c: u32) -> bool {
    matches!(c, 0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF)
}

const PREDEFINED: [(&str, char); 5] = [
    ("lt", '<'),
    ("gt", '>'),
    ("amp", '&'),
    ("apos", '\''),
    ("quot", '"'),
];

/// Line and byte column, both 1-based, as Validate and Go to error use them.
fn position(text: &str, at: usize) -> (usize, usize) {
    let before = &text.as_bytes()[..at.min(text.len())];
    let line = 1 + before.iter().filter(|b| **b == b'\n').count();
    let start = before
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |at| at + 1);
    (line, before.len() - start + 1)
}

fn shown(name: &str) -> String {
    name.chars().take(40).collect()
}

impl<'a> Lexer<'a> {
    fn new(text: &'a str, cancelled: &'a dyn Fn() -> bool) -> Self {
        Self {
            text,
            bytes: text.as_bytes(),
            at: 0,
            next_check: 0,
            cancelled,
            attributes: Vec::new(),
            seen: Default::default(),
            entities: Default::default(),
            open_entities: false,
        }
    }

    fn invalid(&self, at: usize, message: impl Into<String>) -> Failure {
        let (line, column) = position(self.text, at);
        Failure::Invalid(Invalid {
            line,
            column,
            message: message.into(),
        })
    }

    fn find(&self, from: usize, needle: &str) -> Option<usize> {
        self.text
            .get(from..)
            .and_then(|rest| rest.find(needle))
            .map(|at| from + at)
    }

    /// `needle` within `from..end` only: a miss never scans the rest of
    /// the document, so many small runs stay linear.
    fn find_in(&self, from: usize, end: usize, needle: &str) -> Option<usize> {
        self.text[from..end].find(needle).map(|at| from + at)
    }

    fn name_end(&self, from: usize) -> usize {
        if !self.bytes.get(from).is_some_and(|b| name_start(*b)) {
            return from;
        }
        let mut at = from + 1;
        while self.bytes.get(at).is_some_and(|b| name_char(*b)) {
            at += 1;
        }
        at
    }

    fn skip_space(&self, mut at: usize) -> usize {
        while self.bytes.get(at).is_some_and(|b| space(*b)) {
            at += 1;
        }
        at
    }

    /// C0 controls other than tab, newline and return, and U+FFFE/U+FFFF,
    /// are not XML characters anywhere in a document.
    fn characters(&self, span: &Range<usize>) -> Result<(), Failure> {
        let bytes = &self.bytes[span.clone()];
        let control = bytes
            .iter()
            .position(|b| *b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r'));
        let noncharacter = bytes
            .windows(3)
            .position(|w| w[0] == 0xEF && w[1] == 0xBF && matches!(w[2], 0xBE | 0xBF));
        match control.into_iter().chain(noncharacter).min() {
            Some(at) => Err(self.invalid(span.start + at, "character not allowed in XML")),
            None => Ok(()),
        }
    }

    fn token(
        &mut self,
        kind: Kind,
        span: Range<usize>,
        name: Range<usize>,
    ) -> Result<Option<Token>, Failure> {
        self.characters(&span)?;
        self.at = span.end;
        Ok(Some(Token { kind, span, name }))
    }

    fn next(&mut self) -> Result<Option<Token>, Failure> {
        if self.at >= self.bytes.len() {
            return Ok(None);
        }
        if self.at >= self.next_check {
            self.next_check = self.at + CHECK_EVERY;
            if (self.cancelled)() {
                return Err(Failure::Cancelled);
            }
        }
        let start = self.at;
        let rest = &self.bytes[start..];
        if rest[0] != b'<' {
            return self.text_run(start);
        }
        if rest.starts_with(b"<?") {
            return self.instruction(start);
        }
        if rest.starts_with(b"<!--") {
            let end = self.comment(start)?;
            return self.token(Kind::Comment, start..end, 0..0);
        }
        if rest.starts_with(b"<![CDATA[") {
            let end = self
                .find(start + 9, "]]>")
                .ok_or_else(|| self.invalid(start, "CDATA section is not closed"))?;
            return self.token(Kind::Cdata, start..end + 3, 0..0);
        }
        if rest.starts_with(b"<!DOCTYPE") {
            return self.doctype(start);
        }
        if rest.starts_with(b"<!") {
            return Err(self.invalid(start, "unexpected markup declaration"));
        }
        if rest.starts_with(b"</") {
            return self.end_tag(start);
        }
        self.start_tag(start)
    }

    /// The end of the comment at `start`; `--` may not occur inside it.
    fn comment(&self, start: usize) -> Result<usize, Failure> {
        let body = start + 4;
        let close = self
            .find(body, "-->")
            .ok_or_else(|| self.invalid(start, "comment is not closed"))?;
        if let Some(at) = self.find_in(body, close, "--") {
            return Err(self.invalid(at, "'--' is not allowed inside a comment"));
        }
        if close > body && self.bytes[close - 1] == b'-' {
            return Err(self.invalid(close - 1, "a comment may not end with '--->'"));
        }
        Ok(close + 3)
    }

    fn text_run(&mut self, start: usize) -> Result<Option<Token>, Failure> {
        let end = self.find(start, "<").unwrap_or(self.bytes.len());
        let run = &self.bytes[start..end];
        let blank = run.iter().all(|b| space(*b));
        if !blank {
            if let Some(at) = self.find_in(start, end, "]]>") {
                return Err(self.invalid(at, "']]>' is not allowed in text"));
            }
            self.references(start, end)?;
        }
        let newlines = if blank {
            run.iter().filter(|b| **b == b'\n').count()
        } else {
            0
        };
        self.token(Kind::Text { blank, newlines }, start..end, 0..0)
    }

    /// The reference starting with `&` at `at`, ending before `end`: its end
    /// (after `;`) and what it stands for — a character, a predefined
    /// entity's character, or `None` for a declared entity.
    fn reference(&self, at: usize, end: usize) -> Result<(usize, Option<char>), Failure> {
        let malformed = || {
            self.invalid(
                at,
                "'&' must start an entity or character reference; write &amp; for a literal '&'",
            )
        };
        let mut i = at + 1;
        if self.bytes.get(i) == Some(&b'#') {
            i += 1;
            let hex = self.bytes.get(i) == Some(&b'x');
            i += usize::from(hex);
            let digits = i;
            let mut value: u32 = 0;
            while i < end {
                let digit = match (hex, self.bytes[i]) {
                    (true, b) if b.is_ascii_hexdigit() => (b as char).to_digit(16),
                    (false, b) if b.is_ascii_digit() => (b as char).to_digit(10),
                    _ => break,
                };
                let radix = if hex { 16 } else { 10 };
                value = value
                    .saturating_mul(radix)
                    .saturating_add(digit.unwrap_or(0));
                i += 1;
            }
            if i == digits || i >= end || self.bytes[i] != b';' {
                return Err(malformed());
            }
            let ch = char::from_u32(value).filter(|c| legal(*c as u32));
            let Some(ch) = ch else {
                return Err(
                    self.invalid(at, "character reference to a character not allowed in XML")
                );
            };
            return Ok((i + 1, Some(ch)));
        }
        let name = i..self.name_end(i).min(end);
        if name.is_empty() || name.end >= end || self.bytes[name.end] != b';' {
            return Err(malformed());
        }
        let key = &self.text[name.clone()];
        if let Some((_, ch)) = PREDEFINED.iter().find(|(n, _)| *n == key) {
            return Ok((name.end + 1, Some(*ch)));
        }
        if !self.entities.contains(key) && !self.open_entities {
            return Err(self.invalid(at, format!("entity &{}; is not declared", shown(key))));
        }
        Ok((name.end + 1, None))
    }

    /// Every `&` in `start..end` starts a well-formed reference.
    fn references(&self, start: usize, end: usize) -> Result<(), Failure> {
        let mut from = start;
        while let Some(at) = self.find_in(from, end, "&") {
            from = self.reference(at, end)?.0;
        }
        Ok(())
    }

    /// An attribute value as a processor sees it for a tokenized type:
    /// references resolved, whitespace collapsed. `None` when it needs a
    /// declared entity this formatter does not expand.
    fn resolved(&self, value: Range<usize>) -> Option<String> {
        let mut out = String::new();
        let mut at = value.start;
        while at < value.end {
            if self.bytes[at] == b'&' {
                let (end, ch) = self.reference(at, value.end).ok()?;
                out.push(ch?);
                at = end;
                continue;
            }
            let ch = self.text[at..value.end].chars().next()?;
            out.push(if matches!(ch, '\t' | '\n' | '\r') {
                ' '
            } else {
                ch
            });
            at += ch.len_utf8();
        }
        Some(
            out.split(' ')
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
        )
    }

    fn instruction(&mut self, start: usize) -> Result<Option<Token>, Failure> {
        let end = self
            .find(start + 2, "?>")
            .ok_or_else(|| self.invalid(start, "processing instruction is not closed"))?;
        let target = start + 2..self.name_end(start + 2);
        if target.is_empty() {
            return Err(self.invalid(start + 2, "processing instruction needs a target"));
        }
        if target.end < end && !space(self.bytes[target.end]) {
            return Err(self.invalid(target.end, "expected a space after the target"));
        }
        let name = &self.text[target.clone()];
        let kind = if name.eq_ignore_ascii_case("xml") {
            if name != "xml" || start != 0 {
                return Err(self.invalid(
                    start,
                    "the XML declaration must be the first thing in the document",
                ));
            }
            Kind::Declaration
        } else {
            Kind::Instruction
        };
        self.token(kind, start..end + 2, target)
    }

    /// DOCTYPE: copied as written. Its internal subset is scanned for
    /// general entity declarations; an external subset or a parameter entity
    /// reference leaves undeclared entities possible.
    fn doctype(&mut self, start: usize) -> Result<Option<Token>, Failure> {
        let unclosed = |lexer: &Self| lexer.invalid(start, "DOCTYPE is not closed");
        let mut at = start + "<!DOCTYPE".len();
        if !self.bytes.get(at).is_some_and(|b| space(*b)) {
            return Err(self.invalid(at, "expected a space after DOCTYPE"));
        }
        let mut subset = false;
        loop {
            let Some(&b) = self.bytes.get(at) else {
                return Err(unclosed(self));
            };
            let rest = &self.bytes[at..];
            match b {
                b'"' | b'\'' => {
                    let quote = if b == b'"' { "\"" } else { "'" };
                    at = self.find(at + 1, quote).ok_or_else(|| unclosed(self))? + 1;
                    continue;
                }
                b'S' | b'P'
                    if !subset && (rest.starts_with(b"SYSTEM") || rest.starts_with(b"PUBLIC")) =>
                {
                    self.open_entities = true;
                }
                b'[' if !subset => subset = true,
                b']' if subset => subset = false,
                b'%' if subset && self.bytes.get(at + 1).is_some_and(|b| name_start(*b)) => {
                    self.open_entities = true;
                }
                b'<' if subset && rest.starts_with(b"<!--") => {
                    at = self.comment(at)?;
                    continue;
                }
                b'<' if subset && rest.starts_with(b"<?") => {
                    at = self.find(at + 2, "?>").ok_or_else(|| unclosed(self))? + 2;
                    continue;
                }
                b'<' if subset && rest.starts_with(b"<!ENTITY") => {
                    at = self.skip_space(at + "<!ENTITY".len());
                    if self.bytes.get(at) != Some(&b'%') {
                        let name = at..self.name_end(at);
                        let text = self.text;
                        self.entities.insert(&text[name.clone()]);
                        at = name.end.max(at + 1);
                    }
                    continue;
                }
                b'>' if !subset => return self.token(Kind::Doctype, start..at + 1, 0..0),
                _ => {}
            }
            at += 1;
        }
    }

    fn end_tag(&mut self, start: usize) -> Result<Option<Token>, Failure> {
        let name = start + 2..self.name_end(start + 2);
        if name.is_empty() {
            return Err(self.invalid(start + 2, "expected a closing tag name"));
        }
        let at = self.skip_space(name.end);
        if self.bytes.get(at) != Some(&b'>') {
            return Err(self.invalid(at, "expected '>' to close the tag"));
        }
        self.token(Kind::End, start..at + 1, name)
    }

    fn start_tag(&mut self, start: usize) -> Result<Option<Token>, Failure> {
        let name = start + 1..self.name_end(start + 1);
        if name.is_empty() {
            return Err(self.invalid(start, "'<' must start a tag; write &lt; for a literal '<'"));
        }
        self.attributes.clear();
        self.seen.clear();
        let mut preserve = false;
        let mut at = name.end;
        let empty = loop {
            let gap = at;
            at = self.skip_space(at);
            match self.bytes.get(at) {
                None => return Err(self.invalid(start, "tag is not closed")),
                Some(b'>') => {
                    at += 1;
                    break false;
                }
                Some(b'/') if self.bytes.get(at + 1) == Some(&b'>') => {
                    at += 2;
                    break true;
                }
                Some(_) if at == gap => {
                    return Err(self.invalid(at, "attributes must be separated by a space"))
                }
                Some(_) => {}
            }
            let attribute = at..self.name_end(at);
            if attribute.is_empty() {
                return Err(self.invalid(at, "expected an attribute name"));
            }
            at = self.skip_space(attribute.end);
            if self.bytes.get(at) != Some(&b'=') {
                return Err(self.invalid(at, "expected '=' after the attribute name"));
            }
            at = self.skip_space(at + 1);
            let quote = match self.bytes.get(at) {
                Some(b'"') => "\"",
                Some(b'\'') => "'",
                _ => return Err(self.invalid(at, "attribute values must be quoted")),
            };
            let value = at + 1
                ..self
                    .find(at + 1, quote)
                    .ok_or_else(|| self.invalid(at, "attribute value is not closed"))?;
            if let Some(lt) = self.find_in(value.start, value.end, "<") {
                return Err(self.invalid(lt, "'<' is not allowed in an attribute value"));
            }
            self.references(value.start, value.end)?;
            let text = self.text;
            let key = &text[attribute.clone()];
            if self.duplicate(key) {
                return Err(self.invalid(
                    attribute.start,
                    format!("duplicate attribute {}", shown(key)),
                ));
            }
            // Only a value that resolves to `default` re-enables layout.
            if key == "xml:space" && self.resolved(value.clone()).as_deref() != Some("default") {
                preserve = true;
            }
            at = value.end + 1;
            self.attributes.push((attribute, value));
        };
        self.token(Kind::Start { empty, preserve }, start..at, name)
    }

    fn duplicate(&mut self, key: &'a str) -> bool {
        let text = self.text;
        if self.attributes.len() < LINEAR_ATTRIBUTES {
            return self.attributes.iter().any(|(a, _)| &text[a.clone()] == key);
        }
        if self.seen.is_empty() {
            self.seen
                .extend(self.attributes.iter().map(|(a, _)| &text[a.clone()]));
        }
        !self.seen.insert(key)
    }
}

/// Pass one: well-formedness, and for every element in document order
/// whether its content is re-indented (see the module rules).
pub(super) fn check(text: &str, cancelled: &dyn Fn() -> bool) -> Result<Vec<bool>, Failure> {
    struct Frame {
        name: Range<usize>,
        start: usize,
        index: usize,
        markup: bool,
        text: bool,
        preserve: bool,
        mixed: bool,
        /// Some boundary between this element's tags and children had no
        /// whitespace; the last boundary seen had whitespace.
        adjacent: bool,
        spaced: bool,
        /// The default namespace for children and the prefix bindings
        /// in force before this element's own.
        default: Vocabulary,
        bindings: usize,
    }
    impl Frame {
        fn child(&mut self) {
            self.markup = true;
            self.adjacent |= !self.spaced;
            self.spaced = false;
        }
    }
    let mut lexer = Lexer::new(text, cancelled);
    let mut stack: Vec<Frame> = Vec::new();
    let mut bindings: Vec<(&str, Vocabulary)> = Vec::new();
    let mut flags = Vec::new();
    let (mut root, mut closed, mut doctype) = (false, false, false);
    while let Some(token) = lexer.next()? {
        let at = token.span.start;
        match token.kind {
            Kind::Declaration => {}
            Kind::Doctype => {
                if root || doctype {
                    return Err(
                        lexer.invalid(at, "DOCTYPE must come once, before the root element")
                    );
                }
                doctype = true;
            }
            Kind::Instruction | Kind::Comment => {
                if let Some(frame) = stack.last_mut() {
                    frame.child();
                }
            }
            Kind::Cdata => match stack.last_mut() {
                Some(frame) => frame.text = true,
                None => {
                    return Err(lexer.invalid(at, "CDATA is only allowed inside the root element"))
                }
            },
            Kind::Text { blank: false, .. } => match stack.last_mut() {
                Some(frame) => frame.text = true,
                None => {
                    let at = lexer.skip_space(at);
                    return Err(lexer.invalid(at, "text is not allowed outside the root element"));
                }
            },
            Kind::Text { blank: true, .. } => {
                if let Some(frame) = stack.last_mut() {
                    frame.spaced = true;
                }
            }
            Kind::Start { empty, preserve } => {
                match stack.last_mut() {
                    Some(frame) => frame.child(),
                    None if closed => {
                        return Err(lexer.invalid(at, "a document has only one root element"))
                    }
                    None => root = true,
                }
                if stack.len() >= MAX_DEPTH {
                    return Err(Failure::Depth);
                }
                // Namespaces: this tag's declarations apply to itself.
                let before = bindings.len();
                let mut default = stack.last().map_or(Vocabulary::None, |f| f.default);
                for (name, value) in &lexer.attributes {
                    let name = &text[name.clone()];
                    let uri = lexer.resolved(value.clone());
                    if name == "xmlns" {
                        default = Vocabulary::of(uri.as_deref());
                    } else if let Some(prefix) = name.strip_prefix("xmlns:") {
                        bindings.push((prefix, Vocabulary::of(uri.as_deref())));
                    }
                }
                let tag = &text[token.name.clone()];
                let (vocabulary, local) = match tag.split_once(':') {
                    Some((prefix, local)) => (
                        bindings
                            .iter()
                            .rev()
                            .find(|(p, _)| *p == prefix)
                            .map_or(Vocabulary::Data, |(_, v)| *v),
                        local,
                    ),
                    None => (default, tag),
                };
                let index = flags.len();
                flags.push(false);
                if empty {
                    bindings.truncate(before);
                    closed |= stack.is_empty();
                    continue;
                }
                let preserve = preserve || stack.last().is_some_and(|f| f.preserve);
                stack.push(Frame {
                    name: token.name,
                    start: at,
                    index,
                    markup: false,
                    text: false,
                    preserve,
                    mixed: vocabulary.mixed(local),
                    adjacent: false,
                    spaced: false,
                    default,
                    bindings: before,
                });
            }
            Kind::End => {
                let Some(mut frame) = stack.pop() else {
                    return Err(lexer.invalid(at, "closing tag without an open element"));
                };
                if text[frame.name.clone()] != text[token.name.clone()] {
                    return Err(lexer.invalid(
                        at,
                        format!(
                            "</{}> does not close <{}>",
                            shown(&text[token.name]),
                            shown(&text[frame.name])
                        ),
                    ));
                }
                frame.adjacent |= !frame.spaced;
                bindings.truncate(frame.bindings);
                flags[frame.index] = !frame.preserve
                    && frame.markup
                    && !frame.text
                    && (!frame.mixed || !frame.adjacent);
                closed = stack.is_empty();
            }
        }
    }
    if let Some(frame) = stack.last() {
        return Err(lexer.invalid(
            frame.start,
            format!("<{}> is not closed", shown(&text[frame.name.clone()])),
        ));
    }
    if !root {
        return Err(lexer.invalid(text.len(), "no root element"));
    }
    Ok(flags)
}

/// Output under the shared 64 MiB ceiling; growth never reserves past it.
struct Out(String);
impl Out {
    fn push(&mut self, text: &str) -> Result<(), Failure> {
        let needed = self.0.len().saturating_add(text.len());
        if needed > MAX_OUTPUT_BYTES {
            return Err(Failure::Limit);
        }
        if needed > self.0.capacity() {
            let capacity = needed
                .max(self.0.capacity().saturating_mul(2))
                .min(MAX_OUTPUT_BYTES);
            self.0.reserve_exact(capacity - self.0.len());
        }
        self.0.push_str(text);
        Ok(())
    }
}

/// Pass two: copy every token, re-indenting the elements pass one allowed.
fn emit(
    text: &str,
    flags: &[bool],
    prefix: &str,
    indent: &str,
    newline: &str,
    cancelled: &dyn Fn() -> bool,
) -> Result<String, Failure> {
    let mut out = Out(String::with_capacity(
        prefix
            .len()
            .saturating_add(text.len())
            .saturating_add(text.len() / 4)
            .min(MAX_OUTPUT_BYTES),
    ));
    out.push(prefix)?;
    let mut lexer = Lexer::new(text, cancelled);
    let (mut depth, mut index) = (0usize, 0usize);
    // Something was written; the source had a blank line before the next
    // token. A blank line before an end tag or at either end is dropped.
    let (mut started, mut blank) = (false, false);
    let mut line = |out: &mut Out, depth: usize, blank: &mut bool| -> Result<(), Failure> {
        if started {
            out.push(newline)?;
            if *blank {
                out.push(newline)?;
            }
        }
        *blank = false;
        started = true;
        for _ in 0..depth {
            out.push(indent)?;
        }
        Ok(())
    };
    let unexpected = |lexer: &Lexer, at: usize| lexer.invalid(at, "unexpected content");
    while let Some(token) = lexer.next()? {
        match token.kind {
            Kind::Text {
                blank: true,
                newlines,
            } => blank |= newlines >= 2,
            Kind::Text { blank: false, .. } | Kind::Cdata => {
                return Err(unexpected(&lexer, token.span.start))
            }
            Kind::Declaration | Kind::Instruction | Kind::Comment | Kind::Doctype => {
                line(&mut out, depth, &mut blank)?;
                out.push(&text[token.span])?;
            }
            Kind::Start { empty, .. } => {
                let indented = flags[index];
                index += 1;
                line(&mut out, depth, &mut blank)?;
                if indented {
                    out.push(&text[token.span])?;
                    depth += 1;
                    continue;
                }
                // Verbatim through the matching end tag.
                let mut end = token.span.end;
                let mut open = usize::from(!empty);
                while open > 0 {
                    let Some(inner) = lexer.next()? else {
                        return Err(unexpected(&lexer, text.len()));
                    };
                    match inner.kind {
                        Kind::Start { empty, .. } => {
                            index += 1;
                            open += usize::from(!empty);
                        }
                        Kind::End => open -= 1,
                        _ => {}
                    }
                    end = inner.span.end;
                }
                out.push(&text[token.span.start..end])?;
            }
            Kind::End => {
                blank = false;
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| unexpected(&lexer, token.span.start))?;
                line(&mut out, depth, &mut blank)?;
                out.push(&text[token.span])?;
            }
        }
    }
    if text.ends_with('\n') {
        out.push(newline)?;
    }
    if cancelled() {
        return Err(Failure::Cancelled);
    }
    Ok(out.0)
}

/// Format's refusal for XML that is not well-formed; Validate reports the
/// same position.
pub fn invalid_notice(line: usize, column: usize) -> String {
    format!(
        "XML is invalid at {line}:{column}; this formatter cannot safely recover its structure. Source unchanged."
    )
}

fn depth_notice(action: &str, bytes: usize) -> String {
    format!("{action} supports XML nesting up to {MAX_DEPTH} levels; this file is {bytes} bytes.")
}

pub(super) fn format(
    source: &str,
    ending: LineEnding,
    indent: &str,
    cancelled: &dyn Fn() -> bool,
) -> Option<Formatted> {
    if cancelled() {
        return None;
    }
    if ending == LineEnding::Mixed || super::line_ending(source) == LineEnding::Mixed {
        return Some(Formatted::Unchanged(
            "Mixed line endings; source unchanged.".into(),
        ));
    }
    let (prefix, text) = match source.strip_prefix('\u{feff}') {
        Some(text) => ("\u{feff}", text),
        None => ("", source),
    };
    let newline = if ending == LineEnding::Crlf {
        "\r\n"
    } else {
        "\n"
    };
    let result = check(text, cancelled)
        .and_then(|flags| emit(text, &flags, prefix, indent, newline, cancelled));
    Some(match result {
        Err(Failure::Cancelled) => return None,
        Err(Failure::Invalid(error)) => {
            Formatted::Unchanged(invalid_notice(error.line, error.column))
        }
        Err(Failure::Depth) => Formatted::Unchanged(depth_notice("Format", source.len())),
        Err(Failure::Limit) => Formatted::Unchanged(format!(
            "Format supports results up to 64 MiB; this file is {} bytes.",
            source.len()
        )),
        Ok(result) if result == source => Formatted::Unchanged("Already formatted.".into()),
        Ok(result) => Formatted::Changed(result),
    })
}

/// Well-formedness only. `None` means cancelled; `Err` a budget refusal
/// that is no verdict on the XML.
pub(super) fn validation(
    source: &str,
    cancelled: &dyn Fn() -> bool,
) -> Option<Result<Result<(), Invalid>, String>> {
    let text = source.strip_prefix('\u{feff}').unwrap_or(source);
    match check(text, cancelled) {
        Ok(_) | Err(Failure::Limit) => Some(Ok(Ok(()))),
        Err(Failure::Invalid(error)) => Some(Ok(Err(error))),
        Err(Failure::Depth) => Some(Err(depth_notice("Validate", source.len()))),
        Err(Failure::Cancelled) => None,
    }
}

#[cfg(test)]
mod tests;
