//! One request's details as display rows, built on a worker. Bodies are
//! decoded within [`BODY_PREVIEW`]; HTML and other markup is only ever text.
use super::model;
use gpui::SharedString;
use okilum_core::har::{self, Body, Cookie, DetailError, Details, Pair, Revision, Text};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Decoded bytes of a body shown in details.
pub(crate) const BODY_PREVIEW: usize = 256 * 1024;
/// Characters per displayed text row; longer lines continue in further rows.
const LINE: usize = 2000;
/// Displayed characters of one header, cookie or parameter value.
const VALUE: usize = 4000;
/// Rows of one body.
const BODY_ROWS: usize = 20_000;
/// Deepest nesting the indented JSON view shows; deeper bodies stay as written.
pub(crate) const PRETTY_DEPTH: usize = 64;
/// Largest indented JSON; a body that would indent larger stays as written.
pub(crate) const PRETTY_BYTES: usize = 4 * BODY_PREVIEW;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Section {
    General,
    Timing,
    RequestHeaders,
    Query,
    RequestCookies,
    RequestBody,
    ResponseHeaders,
    ResponseCookies,
    ResponseBody,
}

impl Section {
    pub(crate) fn title(self) -> &'static str {
        match self {
            Section::General => "General",
            Section::Timing => "Timing",
            Section::RequestHeaders => "Request headers",
            Section::Query => "Query string",
            Section::RequestCookies => "Request cookies",
            Section::RequestBody => "Request body",
            Section::ResponseHeaders => "Response headers",
            Section::ResponseCookies => "Response cookies",
            Section::ResponseBody => "Response body",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Row {
    Section {
        section: Section,
        count: Option<usize>,
        /// A body section's decoded text, for copying in full (#2888).
        body: Option<Arc<str>>,
    },
    Field {
        name: SharedString,
        value: SharedString,
        /// The whole value when `value` shows only part of it (#2888).
        full: Option<Text>,
    },
    /// A timing phase: its duration and its bar inside the request's total.
    Timing {
        name: SharedString,
        ms: f64,
        start: f32,
        width: f32,
    },
    /// Body text, monospace, as written (never rendered as markup).
    Text(SharedString),
    Note(SharedString),
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DetailRows {
    pub rows: Vec<Row>,
}

impl DetailRows {
    /// Indices of rows to show with `collapsed` sections folded to their title.
    pub(crate) fn visible(&self, collapsed: &dyn Fn(Section) -> bool) -> Vec<usize> {
        let mut visible = Vec::with_capacity(self.rows.len());
        let mut hidden = false;
        for (index, row) in self.rows.iter().enumerate() {
            if let Row::Section { section, .. } = row {
                hidden = collapsed(*section);
                visible.push(index);
            } else if !hidden {
                visible.push(index);
            }
        }
        visible
    }
}

/// Builds the rows for `details`, reading bodies from `path` (the indexed
/// `revision`) within the preview bound. `capture_start` is the first
/// request's start, for offsets. Stops on cancellation or a changed file.
pub(crate) fn build(
    path: &Path,
    details: &Details,
    capture_start: Option<f64>,
    revision: Option<Revision>,
    cancel: &AtomicBool,
) -> Result<DetailRows, DetailError> {
    let stop = || {
        if cancel.load(Ordering::Relaxed) {
            Err(DetailError::Cancelled)
        } else {
            Ok(())
        }
    };
    let mut rows = Vec::new();
    general(details, capture_start, &mut rows);
    timing(details, &mut rows);
    section(&mut rows, Section::RequestHeaders, &details.request_headers);
    section(&mut rows, Section::Query, &details.query);
    cookies(&mut rows, Section::RequestCookies, &details.request_cookies);
    stop()?;
    let request = details.request_body.as_ref();
    body(
        path,
        Section::RequestBody,
        request,
        revision,
        cancel,
        &mut rows,
    )?;
    section(
        &mut rows,
        Section::ResponseHeaders,
        &details.response_headers,
    );
    cookies(
        &mut rows,
        Section::ResponseCookies,
        &details.response_cookies,
    );
    stop()?;
    let response = details.response_body.as_ref();
    body(
        path,
        Section::ResponseBody,
        response,
        revision,
        cancel,
        &mut rows,
    )?;
    for (label, total) in &details.cut_lists {
        rows.push(Row::Note(
            format!(
                "{label}: the first {} of {} are shown.",
                model::count(har::MAX_ITEMS),
                model::count(*total)
            )
            .into(),
        ));
    }
    if details.incomplete {
        rows.push(Row::Note(
            "This request is larger than details can show; the rest of it was skipped. Source shows the file as written."
                .into(),
        ));
    }
    Ok(DetailRows { rows })
}

fn field(rows: &mut Vec<Row>, name: &str, value: impl Into<String>) {
    let value = value.into();
    if !value.is_empty() {
        rows.push(field_row(name, Text::Inline(value)));
    }
}

/// A name/value row; `full` keeps the whole value when the row shows part.
fn field_row(name: &str, value: Text) -> Row {
    let display = bounded(&shown(&value), VALUE);
    let full = match &value {
        Text::Long { .. } => Some(value),
        Text::Inline(text) if display != *text => Some(value),
        Text::Inline(_) => None,
    };
    Row::Field {
        name: bounded(name, 200).into(),
        value: display.into(),
        full,
    }
}

/// At most `limit` characters, cut at a character boundary with an ellipsis.
pub(crate) fn bounded(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

fn shown(text: &Text) -> String {
    match text {
        Text::Inline(text) => text.clone(),
        Text::Long { prefix, .. } => format!("{prefix}…"),
    }
}

fn general(details: &Details, capture_start: Option<f64>, rows: &mut Vec<Row>) {
    rows.push(Row::Section {
        section: Section::General,
        count: None,
        body: None,
    });
    if !details.url.is_empty() {
        rows.push(field_row("URL", details.url.clone()));
    }
    field(rows, "Method", details.method.clone());
    let status = match details.status {
        Some(0) => "0 (no response)".to_owned(),
        Some(code) if details.status_text.is_empty() => code.to_string(),
        Some(code) => format!("{code} {}", details.status_text),
        None => "No response".to_owned(),
    };
    field(rows, "Status", status);
    field(rows, "Error", details.error.clone());
    let version = if details.response_version.is_empty() {
        &details.request_version
    } else {
        &details.response_version
    };
    field(rows, "Protocol", version.clone());
    let address = match (details.server.as_str(), details.connection.as_str()) {
        ("", _) => String::new(),
        (ip, "") => ip.to_owned(),
        // Chrome stores the port as `connection`, Firefox the connection id.
        (ip, port) if port.bytes().all(|b| b.is_ascii_digit()) && !ip.contains(':') => {
            format!("{ip}:{port}")
        }
        (ip, _) => ip.to_owned(),
    };
    field(rows, "Remote address", address);
    field(rows, "Resource type", details.resource_type.clone());
    if let Some(body) = &details.response_body {
        field(rows, "MIME type", body.mime.clone());
        if let Some(size) = body.size {
            field(rows, "Size", model::bytes(size));
        }
    }
    let mut started = details.started.clone();
    if let (Some(start), Some(first)) = (har::parse_time(&details.started), capture_start) {
        started.push_str(&format!(" (+{})", model::millis((start - first).max(0.))));
    }
    field(rows, "Started", started);
    if let Some(time) = details.time {
        field(rows, "Time", model::millis(time));
    }
    field(rows, "Redirect", details.redirect.clone());
}

fn timing(details: &Details, rows: &mut Vec<Row>) {
    if details.timings.is_empty() {
        return;
    }
    rows.push(Row::Section {
        section: Section::Timing,
        count: None,
        body: None,
    });
    // Unknown (non-finite) phases neither draw nor move the bar.
    let known = |ms: f64| if ms.is_finite() { ms.max(0.) } else { 0. };
    let phases: Vec<_> = details
        .timings
        .iter()
        // Chrome's `_blocked_*` and similar custom phases overlap the
        // standard ones; they are listed but do not move the bar.
        .map(|(name, ms)| (name, *ms, !name.starts_with('_')))
        .collect();
    let sum = phases
        .iter()
        .filter(|p| p.2)
        .fold(0f64, |sum, p| sum + known(p.1));
    let time = details.time.map_or(0., known);
    let total = time.max(sum);
    let total = if total.is_finite() && total > 0. {
        total
    } else {
        f64::MAX
    };
    let mut at = 0f64;
    for (name, ms, sequential) in phases {
        let start = fraction(at / total);
        rows.push(Row::Timing {
            name: phase_label(name).into(),
            ms: known(ms),
            start,
            width: fraction(known(ms) / total).min(1. - start),
        });
        if sequential {
            at += known(ms);
        }
    }
}

/// A bar position in 0..=1; anything not finite is 0.
fn fraction(value: f64) -> f32 {
    if value.is_finite() {
        value.clamp(0., 1.) as f32
    } else {
        0.
    }
}

fn phase_label(name: &str) -> String {
    match name {
        "blocked" => "Blocked".into(),
        "dns" => "DNS".into(),
        "connect" => "Connect".into(),
        "ssl" => "TLS".into(),
        "send" => "Send".into(),
        "wait" => "Waiting (TTFB)".into(),
        "receive" => "Receive".into(),
        other => other.to_owned(),
    }
}

fn section(rows: &mut Vec<Row>, section: Section, pairs: &[Pair]) {
    if pairs.is_empty() {
        return;
    }
    rows.push(Row::Section {
        section,
        count: Some(pairs.len()),
        body: None,
    });
    for pair in pairs {
        rows.push(field_row(&pair.name, pair.value.clone()));
    }
}

fn cookies(rows: &mut Vec<Row>, section: Section, cookies: &[Cookie]) {
    if cookies.is_empty() {
        return;
    }
    rows.push(Row::Section {
        section,
        count: Some(cookies.len()),
        body: None,
    });
    for cookie in cookies {
        let mut value = shown(&cookie.value);
        for attribute in &cookie.attributes {
            value.push_str("; ");
            value.push_str(attribute);
        }
        // The row shows the attributes; Copy value copies the cookie value.
        rows.push(Row::Field {
            name: bounded(&cookie.name, 200).into(),
            value: bounded(&value, VALUE).into(),
            full: Some(cookie.value.clone()),
        });
    }
}

/// What a body's bytes are, for display.
#[derive(Debug, PartialEq)]
pub(crate) enum Content {
    Text(String),
    Binary,
}

/// Text when the bytes are UTF-8 (a cut final character is allowed when the
/// preview is partial) without NUL bytes; otherwise binary.
pub(crate) fn classify(bytes: &[u8], complete: bool) -> Content {
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) if !complete && error.error_len().is_none() => {
            // Safe: valid_up_to is a character boundary by definition.
            std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap_or_default()
        }
        Err(_) => return Content::Binary,
    };
    if text.contains('\0') {
        return Content::Binary;
    }
    Content::Text(text.to_owned())
}

/// JSON is shown indented when the whole body is valid JSON. Keys, numbers
/// and escapes stay exactly as written; only whitespace between tokens
/// changes. Nesting deeper than [`PRETTY_DEPTH`], output larger than
/// [`PRETTY_BYTES`] or cancellation give `None` (the text is shown as written).
pub(crate) fn pretty(
    text: &str,
    mime: &str,
    complete: bool,
    cancel: &AtomicBool,
) -> Option<String> {
    let start = text.trim_start().as_bytes().first().copied();
    let json = mime.to_ascii_lowercase().contains("json") || matches!(start, Some(b'{' | b'['));
    if !(json && complete) {
        return None;
    }
    let pretty = indent_json(text, cancel)?;
    (pretty != text).then_some(pretty)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Want {
    Value,
    Key,
    Colon,
    CommaOrClose,
    End,
}

/// Validates JSON (RFC 8259 grammar) while re-indenting it with two spaces
/// per level, in one pass with bounded depth and output.
fn indent_json(text: &str, cancel: &AtomicBool) -> Option<String> {
    let b = text.as_bytes();
    let mut out = String::with_capacity((b.len() + b.len() / 4).min(PRETTY_BYTES));
    let mut stack: Vec<u8> = Vec::new();
    let mut want = Want::Value;
    let mut i = 0;
    let mut steps = 0usize;
    let newline = |out: &mut String, depth: usize| {
        out.push('\n');
        for _ in 0..depth {
            out.push_str("  ");
        }
    };
    let done = |stack: &[u8]| {
        if stack.is_empty() {
            Want::End
        } else {
            Want::CommaOrClose
        }
    };
    while i < b.len() {
        steps += 1;
        if steps.is_multiple_of(4096) && cancel.load(Ordering::Relaxed) {
            return None;
        }
        if out.len() > PRETTY_BYTES {
            return None;
        }
        let c = b[i];
        if matches!(c, b' ' | b'\t' | b'\n' | b'\r') {
            i += 1;
            continue;
        }
        match c {
            b'"' if matches!(want, Want::Value | Want::Key) => {
                let start = i;
                i += 1;
                loop {
                    match *b.get(i)? {
                        b'"' => break,
                        b'\\' => match *b.get(i + 1)? {
                            b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => i += 2,
                            b'u' if b.get(i + 2..i + 6)?.iter().all(u8::is_ascii_hexdigit) => {
                                i += 6
                            }
                            _ => return None,
                        },
                        control if control < 0x20 => return None,
                        _ => i += 1,
                    }
                }
                i += 1;
                out.push_str(&text[start..i]);
                want = if want == Want::Key {
                    Want::Colon
                } else {
                    done(&stack)
                };
            }
            b'{' | b'[' if want == Want::Value => {
                if stack.len() >= PRETTY_DEPTH {
                    return None;
                }
                out.push(c as char);
                i += 1;
                while matches!(b.get(i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
                    i += 1;
                }
                let close = if c == b'{' { b'}' } else { b']' };
                if b.get(i) == Some(&close) {
                    out.push(close as char);
                    i += 1;
                    want = done(&stack);
                } else {
                    stack.push(c);
                    newline(&mut out, stack.len());
                    want = if c == b'{' { Want::Key } else { Want::Value };
                }
            }
            b'}' | b']' if want == Want::CommaOrClose => {
                let open = if c == b'}' { b'{' } else { b'[' };
                if stack.pop() != Some(open) {
                    return None;
                }
                newline(&mut out, stack.len());
                out.push(c as char);
                i += 1;
                want = done(&stack);
            }
            b',' if want == Want::CommaOrClose => {
                out.push(',');
                newline(&mut out, stack.len());
                i += 1;
                want = if stack.last() == Some(&b'{') {
                    Want::Key
                } else {
                    Want::Value
                };
            }
            b':' if want == Want::Colon => {
                out.push_str(": ");
                i += 1;
                want = Want::Value;
            }
            _ if want == Want::Value => {
                let start = i;
                while b
                    .get(i)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'+' | b'.'))
                {
                    i += 1;
                }
                let token = &b[start..i];
                if !(matches!(token, b"true" | b"false" | b"null") || har::json_number(token)) {
                    return None;
                }
                out.push_str(&text[start..i]);
                want = done(&stack);
            }
            _ => return None,
        }
    }
    (want == Want::End && out.len() <= PRETTY_BYTES).then_some(out)
}

fn body(
    path: &Path,
    section: Section,
    body: Option<&Body>,
    revision: Option<Revision>,
    cancel: &AtomicBool,
    rows: &mut Vec<Row>,
) -> Result<(), DetailError> {
    let Some(body) = body else {
        return Ok(());
    };
    let empty_text = body.text.as_ref().is_none_or(Text::is_empty);
    if empty_text && body.params.is_empty() {
        if section == Section::ResponseBody && body.size.is_some_and(|size| size > 0) {
            rows.push(Row::Section {
                section,
                count: None,
                body: None,
            });
            rows.push(Row::Note(
                format!(
                    "The capture does not include this body ({}).",
                    model::bytes(body.size.unwrap_or(0))
                )
                .into(),
            ));
        }
        return Ok(());
    }
    let section_row = rows.len();
    rows.push(Row::Section {
        section,
        count: None,
        body: None,
    });
    for pair in &body.params {
        rows.push(field_row(&pair.name, pair.value.clone()));
    }
    let Some(text) = body.text.as_ref().filter(|text| !text.is_empty()) else {
        return Ok(());
    };
    let decoded = match har::body_bytes(path, text, body.base64(), BODY_PREVIEW, revision, cancel) {
        Ok(decoded) => decoded,
        Err(DetailError::Failed(message)) => {
            rows.push(Row::Note(message.into()));
            return Ok(());
        }
        Err(stop) => return Err(stop),
    };
    let size = body
        .size
        .filter(|size| *size >= 0)
        .map(model::bytes)
        .unwrap_or_else(|| "unknown size".into());
    if decoded.invalid_base64 {
        rows.push(Row::Note(
            "Marked as base64 but not valid base64; shown as written.".into(),
        ));
    }
    match classify(&decoded.bytes, decoded.complete) {
        Content::Binary => {
            let mime = if body.mime.is_empty() {
                "unknown type"
            } else {
                body.mime.as_str()
            };
            rows.push(Row::Note(
                format!("Binary content · {size} · {mime}").into(),
            ));
        }
        Content::Text(text) => {
            let pretty = pretty(&text, &body.mime, decoded.complete, cancel);
            if cancel.load(Ordering::Relaxed) {
                return Err(DetailError::Cancelled);
            }
            let shown = pretty.as_deref().unwrap_or(&text);
            text_rows(shown, rows);
            // Copy gives the decoded body as in the file, not the indentation.
            if let Some(Row::Section { body: slot, .. }) = rows.get_mut(section_row) {
                *slot = Some(Arc::from(text.as_str()));
            }
            if !decoded.complete {
                rows.push(Row::Note(
                    format!(
                        "Showing the first {} of {size}.",
                        model::bytes(BODY_PREVIEW as i64)
                    )
                    .into(),
                ));
            }
        }
    }
    Ok(())
}

/// Splits text into display rows: one per line, long lines in parts.
pub(crate) fn text_rows(text: &str, rows: &mut Vec<Row>) {
    let mut added = 0;
    for line in text.split_inclusive('\n') {
        let line = line.trim_end_matches(['\n', '\r']);
        let mut rest = line;
        loop {
            if added == BODY_ROWS {
                rows.push(Row::Note(
                    format!("Only the first {} rows are shown.", model::count(BODY_ROWS)).into(),
                ));
                return;
            }
            let cut = rest.char_indices().nth(LINE).map_or(rest.len(), |(i, _)| i);
            rows.push(Row::Text(rest[..cut].to_owned().into()));
            added += 1;
            rest = &rest[cut..];
            if rest.is_empty() {
                break;
            }
        }
    }
}

/// What a copy command takes from a detail row (#2888).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CopyKind {
    /// The value in full: a field's whole value, a body's decoded text, a
    /// timing's duration, a note's text; a section title copies the section.
    Value,
    Name,
    /// `name: value`.
    Pair,
    /// Every row of the row's section.
    Section,
}

/// A piece of copied text; `File` parts are read from the capture.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Part {
    Literal(String),
    File(Text),
}

fn value_part(row: &Row) -> Option<Part> {
    Some(match row {
        Row::Field { value, full, .. } => match full {
            Some(Text::Inline(text)) => Part::Literal(text.clone()),
            Some(long @ Text::Long { .. }) => Part::File(long.clone()),
            None => Part::Literal(value.to_string()),
        },
        Row::Timing { ms, .. } => Part::Literal(model::millis(*ms)),
        Row::Note(text) => Part::Literal(text.to_string()),
        Row::Section { .. } | Row::Text(_) => return None,
    })
}

/// The section title row that `index` belongs to.
pub(crate) fn section_of(rows: &[Row], index: usize) -> Option<usize> {
    (0..=index.min(rows.len().saturating_sub(1)))
        .rev()
        .find(|&i| matches!(rows[i], Row::Section { .. }))
}

/// The parts `kind` copies from row `index`; `None` when it copies nothing.
pub(crate) fn clip(rows: &[Row], index: usize, kind: CopyKind) -> Option<Vec<Part>> {
    let row = rows.get(index)?;
    let section = || -> Option<Vec<Part>> {
        let start = section_of(rows, index)?;
        let Row::Section { body, .. } = &rows[start] else {
            return None;
        };
        if let Some(body) = body {
            return Some(vec![Part::Literal(body.to_string())]);
        }
        let mut parts = Vec::new();
        for row in rows[start + 1..]
            .iter()
            .take_while(|row| !matches!(row, Row::Section { .. }))
        {
            let name = match row {
                Row::Field { name, .. } | Row::Timing { name, .. } => name.to_string(),
                _ => continue,
            };
            if !parts.is_empty() {
                parts.push(Part::Literal("\n".into()));
            }
            parts.push(Part::Literal(format!("{name}: ")));
            parts.extend(value_part(row));
        }
        (!parts.is_empty()).then_some(parts)
    };
    match (row, kind) {
        (Row::Section { .. } | Row::Text(_), CopyKind::Name | CopyKind::Pair) => None,
        (Row::Section { .. } | Row::Text(_), _) | (_, CopyKind::Section) => section(),
        (Row::Field { name, .. } | Row::Timing { name, .. }, CopyKind::Name) => {
            Some(vec![Part::Literal(name.to_string())])
        }
        (Row::Field { name, .. } | Row::Timing { name, .. }, CopyKind::Pair) => {
            Some(vec![Part::Literal(format!("{name}: ")), value_part(row)?])
        }
        (_, CopyKind::Name | CopyKind::Pair) => None,
        (_, CopyKind::Value) => Some(vec![value_part(row)?]),
    }
}

/// Largest value a copy reads from the capture.
pub(crate) const COPY_BYTES: usize = 64 << 20;

/// Joins `parts`, reading `File` parts from `path` (the indexed `revision`).
pub(crate) fn resolve(
    path: &Path,
    parts: Vec<Part>,
    revision: Option<Revision>,
    cancel: &AtomicBool,
) -> Result<String, DetailError> {
    let mut out = String::new();
    for part in parts {
        match part {
            Part::Literal(text) => out.push_str(&text),
            Part::File(Text::Inline(text)) => out.push_str(&text),
            Part::File(long) => {
                let bytes = har::body_bytes(path, &long, false, COPY_BYTES, revision, cancel)?;
                out.push_str(&String::from_utf8_lossy(&bytes.bytes));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn classify_text_binary_and_cut_characters() {
        assert_eq!(classify(b"hi", true), Content::Text("hi".into()));
        assert_eq!(classify(b"a\0b", true), Content::Binary);
        assert_eq!(classify(&[0x89, b'P', b'N', b'G'], true), Content::Binary);
        // A partial preview may end inside a character.
        assert_eq!(
            classify("é".as_bytes()[..1].as_ref(), false),
            Content::Text(String::new())
        );
        assert_eq!(
            classify("é".as_bytes()[..1].as_ref(), true),
            Content::Binary
        );
    }

    #[test]
    fn json_is_indented_only_when_complete_and_valid() {
        assert_eq!(
            pretty(
                r#"{"a":[1]}"#,
                "application/json",
                true,
                &AtomicBool::new(false)
            )
            .as_deref(),
            Some("{\n  \"a\": [\n    1\n  ]\n}")
        );
        assert_eq!(
            pretty(
                r#"{"a":[1]"#,
                "application/json",
                true,
                &AtomicBool::new(false)
            ),
            None
        );
        assert_eq!(
            pretty(
                r#"{"a":1}"#,
                "application/json",
                false,
                &AtomicBool::new(false)
            ),
            None
        );
        assert_eq!(
            pretty(
                "<html>{}</html>",
                "text/html",
                true,
                &AtomicBool::new(false)
            ),
            None
        );
        // Order, numbers and escapes are kept as written.
        assert_eq!(
            pretty(
                r#"{"z":1.50,"a":{},"s":"x\"]{,:","e":[ ]}"#,
                "",
                true,
                &AtomicBool::new(false)
            )
            .as_deref(),
            Some("{\n  \"z\": 1.50,\n  \"a\": {},\n  \"s\": \"x\\\"]{,:\",\n  \"e\": []\n}")
        );
    }

    #[test]
    fn long_lines_split_and_rows_are_bounded() {
        let mut rows = Vec::new();
        text_rows(&("é".repeat(LINE + 5) + "\r\nnext"), &mut rows);
        assert_eq!(rows.len(), 3);
        assert!(matches!(&rows[0], Row::Text(t) if t.chars().count() == LINE));
        assert!(matches!(&rows[1], Row::Text(t) if t.chars().count() == 5));
        assert_eq!(rows[2], Row::Text("next".into()));
        let mut rows = Vec::new();
        text_rows(&"x\n".repeat(BODY_ROWS + 10), &mut rows);
        assert_eq!(rows.len(), BODY_ROWS + 1);
        assert!(matches!(rows.last(), Some(Row::Note(_))));
    }

    #[test]
    fn bounded_cuts_at_character_boundaries() {
        assert_eq!(bounded("ééé", 2), "éé…");
        assert_eq!(bounded("ab", 2), "ab");
    }

    #[test]
    fn collapsed_sections_keep_their_titles() {
        let rows = DetailRows {
            rows: vec![
                Row::Section {
                    section: Section::General,
                    count: None,
                    body: None,
                },
                Row::Note("a".into()),
                Row::Section {
                    section: Section::Timing,
                    count: None,
                    body: None,
                },
                Row::Note("b".into()),
            ],
        };
        assert_eq!(rows.visible(&|_| false), [0, 1, 2, 3]);
        assert_eq!(rows.visible(&|s| s == Section::General), [0, 2, 3]);
    }

    #[test]
    fn timing_bars_follow_phases_and_ignore_overlapping_custom_ones() {
        let details = Details {
            time: Some(100.),
            timings: vec![
                ("blocked".into(), 10.),
                ("_blocked_queueing".into(), 5.),
                ("wait".into(), 60.),
                ("receive".into(), 30.),
            ],
            ..Default::default()
        };
        let mut rows = Vec::new();
        timing(&details, &mut rows);
        let bars: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Timing { start, width, .. } => Some((*start, *width)),
                _ => None,
            })
            .collect();
        assert_eq!(bars, [(0., 0.1), (0.1, 0.05), (0.1, 0.6), (0.7, 0.3)]);
    }

    // --- Review of PR 2878: each case fails on head 9482eed4. ---

    #[test]
    fn review2878_deep_json_is_not_indented_and_never_grows_without_bound() {
        let open = AtomicBool::new(false);
        // 100,000 nested arrays around 0 (~200 KB): raw text, no indentation.
        let deep = format!("{}0{}", "[".repeat(100_000), "]".repeat(100_000));
        assert_eq!(pretty(&deep, "application/json", true, &open), None);
        // The depth bound is exact; one level less still indents.
        let limit = format!("{}0{}", "[".repeat(PRETTY_DEPTH), "]".repeat(PRETTY_DEPTH));
        assert!(pretty(&limit, "application/json", true, &open).is_some());
        let over = format!("[{limit}]");
        assert_eq!(pretty(&over, "application/json", true, &open), None);
        // Wide input whose indentation would exceed the output cap stays raw.
        let wide = format!("[{}0]", "0,".repeat(PRETTY_BYTES / 4));
        assert!(wide.len() < BODY_PREVIEW * 3);
        assert_eq!(pretty(&wide, "application/json", true, &open), None);
        // Grammar: raw control bytes, Rust-only numbers and trailing commas are not JSON.
        for bad in [
            "{\"a\":\"x\ty\"}",
            "[01]",
            "[1.]",
            "[1,]",
            "{\"a\" 1}",
            "[1] 2",
        ] {
            assert_eq!(pretty(bad, "application/json", true, &open), None, "{bad}");
        }
    }

    #[test]
    fn review2878_formatting_stops_when_cancelled() {
        let json = format!("[{}0]", "{\"k\":[1,2,3]},".repeat(5_000));
        // Control: uncancelled it indents.
        assert!(pretty(&json, "application/json", true, &AtomicBool::new(false)).is_some());
        assert_eq!(
            pretty(&json, "application/json", true, &AtomicBool::new(true)),
            None
        );
    }

    #[test]
    fn review2878_non_finite_timings_never_panic() {
        for (time, timings) in [
            (
                Some(f64::INFINITY),
                vec![("send".to_owned(), f64::INFINITY), ("wait".to_owned(), 1.)],
            ),
            (
                None,
                vec![("send".to_owned(), f64::NAN), ("wait".to_owned(), 1.)],
            ),
            (
                Some(f64::MAX),
                vec![("send".to_owned(), f64::MAX), ("wait".to_owned(), f64::MAX)],
            ),
        ] {
            let details = Details {
                time,
                timings,
                ..Default::default()
            };
            let mut rows = Vec::new();
            timing(&details, &mut rows);
            for row in rows {
                if let Row::Timing {
                    start, width, ms, ..
                } = row
                {
                    assert!((0. ..=1.).contains(&start) && (0. ..=1.).contains(&width));
                    assert!(start + width <= 1.0001 && ms.is_finite());
                }
            }
        }
    }

    #[test]
    fn review2878_build_stops_when_cancelled() {
        let details = Details {
            response_body: Some(Body {
                mime: "application/json".into(),
                text: Some(Text::Inline("[1,2]".into())),
                ..Default::default()
            }),
            ..Default::default()
        };
        let path = std::path::Path::new("/nonexistent");
        // Control: uncancelled, the body is shown.
        let rows = build(path, &details, None, None, &AtomicBool::new(false)).unwrap();
        assert!(rows.rows.contains(&Row::Text("  1,".into())));
        assert!(matches!(
            build(path, &details, None, None, &AtomicBool::new(true)),
            Err(DetailError::Cancelled)
        ));
    }

    #[test]
    fn fix2888_copies_take_whole_values_bodies_and_sections() {
        let section = |section, body: Option<&str>| Row::Section {
            section,
            count: None,
            body: body.map(Arc::from),
        };
        let rows = vec![
            section(Section::RequestHeaders, None),
            field_row("a", Text::Inline("1".into())),
            field_row("b", Text::Inline("v".repeat(VALUE + 10))),
            section(Section::ResponseBody, Some("{\"ok\":true}")),
            Row::Text("{".into()),
        ];
        let literal = |parts: Option<Vec<Part>>| {
            parts.map(|parts| {
                parts
                    .into_iter()
                    .map(|part| match part {
                        Part::Literal(text) => text,
                        Part::File(text) => text.shown().to_owned(),
                    })
                    .collect::<String>()
            })
        };
        assert_eq!(
            literal(clip(&rows, 2, CopyKind::Value)),
            Some("v".repeat(VALUE + 10))
        );
        assert_eq!(
            literal(clip(&rows, 1, CopyKind::Pair)).as_deref(),
            Some("a: 1")
        );
        assert_eq!(
            literal(clip(&rows, 1, CopyKind::Section)),
            Some(format!("a: 1\nb: {}", "v".repeat(VALUE + 10)))
        );
        // A body line copies the decoded body as written, not one display line.
        assert_eq!(
            literal(clip(&rows, 4, CopyKind::Value)).as_deref(),
            Some("{\"ok\":true}")
        );
        assert_eq!(
            literal(clip(&rows, 3, CopyKind::Value)).as_deref(),
            Some("{\"ok\":true}")
        );
        assert_eq!(clip(&rows, 4, CopyKind::Name), None);
    }
}
