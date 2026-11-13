//! XML Format code (#2912). A streaming two-pass pretty-printer over the
//! original bytes: no tree, no entity expansion, no DTD processing.
//!
//! The first pass checks well-formedness (matched tags, one root, quoted and
//! unique attributes, references, no text outside the root) and decides, for
//! every element in document order, whether its content may be re-indented.
//! The second pass copies tokens byte for byte and changes only the
//! whitespace between them:
//!
//! - an element whose content is only child elements, comments or processing
//!   instructions separated by whitespace gets one child per line, indented
//!   one level deeper;
//! - every other element is copied verbatim from its start tag to its end
//!   tag: text-only leaves, mixed content (text next to elements, as in
//!   XHTML or DocBook), CDATA and `xml:space="preserve"` subtrees;
//! - start and end tags are copied as written, so attributes keep their
//!   order, quotes and spacing; comments, CDATA, processing instructions,
//!   the declaration and DOCTYPE are copied as written;
//! - one blank line before a child survives where the source had one;
//! - the final newline, a byte order mark and the newline style stay as the
//!   source had them.
//!
//! Whitespace-only text between elements is treated as insignificant, as
//! every XML pretty-printer does; an element that relies on it without any
//! other text of its own is the one documented limit.

use super::{Formatted, Invalid, LineEnding, MAX_OUTPUT_BYTES};
use std::ops::Range;

/// Cancellation is polled at least this often while scanning.
const CHECK_EVERY: usize = 16 * 1024;
/// Duplicate attributes are found by a linear scan up to this many.
const LINEAR_ATTRIBUTES: usize = 32;

pub(super) enum Failure {
    Invalid(Invalid),
    Limit,
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

struct Lexer<'a> {
    text: &'a str,
    bytes: &'a [u8],
    at: usize,
    next_check: usize,
    cancelled: &'a dyn Fn() -> bool,
    attributes: Vec<Range<usize>>,
    seen: std::collections::HashSet<&'a str>,
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

    fn token(&mut self, kind: Kind, span: Range<usize>, name: Range<usize>) -> Option<Token> {
        self.at = span.end;
        Some(Token { kind, span, name })
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
            let end = self
                .find(start + 4, "-->")
                .ok_or_else(|| self.invalid(start, "comment is not closed"))?;
            return Ok(self.token(Kind::Comment, start..end + 3, 0..0));
        }
        if rest.starts_with(b"<![CDATA[") {
            let end = self
                .find(start + 9, "]]>")
                .ok_or_else(|| self.invalid(start, "CDATA section is not closed"))?;
            return Ok(self.token(Kind::Cdata, start..end + 3, 0..0));
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
        Ok(self.token(Kind::Text { blank, newlines }, start..end, 0..0))
    }

    /// Every `&` in `start..end` starts `&name;`, `&#digits;` or `&#xhex;`.
    fn references(&self, start: usize, end: usize) -> Result<(), Failure> {
        let mut from = start;
        while let Some(at) = self.find_in(from, end, "&") {
            let mut i = at + 1;
            let valid = if self.bytes.get(i) == Some(&b'#') {
                i += 1;
                let hex = self.bytes.get(i) == Some(&b'x');
                if hex {
                    i += 1;
                }
                let digits = i;
                while i < end
                    && (if hex {
                        self.bytes[i].is_ascii_hexdigit()
                    } else {
                        self.bytes[i].is_ascii_digit()
                    })
                {
                    i += 1;
                }
                i > digits
            } else {
                let name = i;
                i = self.name_end(i).min(end);
                i > name
            };
            if !valid || i >= end || self.bytes[i] != b';' {
                return Err(self.invalid(
                    at,
                    "'&' must start an entity or character reference; write &amp; for a literal '&'",
                ));
            }
            from = i + 1;
        }
        Ok(())
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
        Ok(self.token(kind, start..end + 2, target))
    }

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
            match b {
                b'"' | b'\'' => {
                    let quote = if b == b'"' { "\"" } else { "'" };
                    at = self.find(at + 1, quote).ok_or_else(|| unclosed(self))? + 1;
                    continue;
                }
                b'[' if !subset => subset = true,
                b']' if subset => subset = false,
                b'<' if subset && self.bytes[at..].starts_with(b"<!--") => {
                    at = self.find(at + 4, "-->").ok_or_else(|| unclosed(self))? + 3;
                    continue;
                }
                b'<' if subset && self.bytes[at..].starts_with(b"<?") => {
                    at = self.find(at + 2, "?>").ok_or_else(|| unclosed(self))? + 2;
                    continue;
                }
                b'>' if !subset => return Ok(self.token(Kind::Doctype, start..at + 1, 0..0)),
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
        Ok(self.token(Kind::End, start..at + 1, name))
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
            let value_end = self
                .find(at + 1, quote)
                .ok_or_else(|| self.invalid(at, "attribute value is not closed"))?;
            if let Some(lt) = self.find_in(at + 1, value_end, "<") {
                return Err(self.invalid(lt, "'<' is not allowed in an attribute value"));
            }
            self.references(at + 1, value_end)?;
            let text = self.text;
            let key = &text[attribute.clone()];
            if self.duplicate(key) {
                return Err(self.invalid(
                    attribute.start,
                    format!("duplicate attribute {}", shown(key)),
                ));
            }
            if key == "xml:space" && &text[at + 1..value_end] == "preserve" {
                preserve = true;
            }
            self.attributes.push(attribute);
            at = value_end + 1;
        };
        Ok(self.token(Kind::Start { empty, preserve }, start..at, name))
    }

    fn duplicate(&mut self, key: &'a str) -> bool {
        let text = self.text;
        if self.attributes.len() < LINEAR_ATTRIBUTES {
            return self.attributes.iter().any(|a| &text[a.clone()] == key);
        }
        if self.seen.is_empty() {
            self.seen
                .extend(self.attributes.iter().map(|a| &text[a.clone()]));
        }
        !self.seen.insert(key)
    }
}

/// Pass one: well-formedness, and for every element in document order
/// whether its content is re-indented (only markup and whitespace inside).
pub(super) fn check(text: &str, cancelled: &dyn Fn() -> bool) -> Result<Vec<bool>, Failure> {
    struct Frame {
        name: Range<usize>,
        start: usize,
        index: usize,
        markup: bool,
        text: bool,
        preserve: bool,
    }
    let mut lexer = Lexer::new(text, cancelled);
    let mut stack: Vec<Frame> = Vec::new();
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
                    frame.markup = true;
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
            Kind::Text { blank: true, .. } => {}
            Kind::Start { empty, preserve } => {
                match stack.last_mut() {
                    Some(frame) => frame.markup = true,
                    None if closed => {
                        return Err(lexer.invalid(at, "a document has only one root element"))
                    }
                    None => root = true,
                }
                let index = flags.len();
                flags.push(false);
                if !empty {
                    stack.push(Frame {
                        name: token.name,
                        start: at,
                        index,
                        markup: false,
                        text: false,
                        preserve,
                    });
                } else if stack.is_empty() {
                    closed = true;
                }
            }
            Kind::End => {
                let Some(frame) = stack.pop() else {
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
                flags[frame.index] = !frame.preserve && frame.markup && !frame.text;
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

/// Pass two: copy every token, re-indenting the markup-only elements.
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
        Err(Failure::Limit) => Formatted::Unchanged(format!(
            "Format supports results up to 64 MiB; this file is {} bytes.",
            source.len()
        )),
        Ok(result) if result == source => Formatted::Unchanged("Already formatted.".into()),
        Ok(result) => Formatted::Changed(result),
    })
}

/// Well-formedness only; `None` means cancelled.
pub(super) fn validation(
    source: &str,
    cancelled: &dyn Fn() -> bool,
) -> Option<Result<(), Invalid>> {
    let text = source.strip_prefix('\u{feff}').unwrap_or(source);
    match check(text, cancelled) {
        Ok(_) | Err(Failure::Limit) => Some(Ok(())),
        Err(Failure::Invalid(error)) => Some(Err(error)),
        Err(Failure::Cancelled) => None,
    }
}

#[cfg(test)]
mod tests;
