//! One request's details as display rows, built on a worker. Bodies are
//! decoded within [`BODY_PREVIEW`]; HTML and other markup is only ever text.
use super::model;
use gpui::SharedString;
use okilum_core::har::{self, Body, Cookie, Details, Pair, Text};
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// Decoded bytes of a body shown in details.
pub(crate) const BODY_PREVIEW: usize = 256 * 1024;
/// Characters per displayed text row; longer lines continue in further rows.
const LINE: usize = 2000;
/// Displayed characters of one header, cookie or parameter value.
const VALUE: usize = 4000;
/// Rows of one body.
const BODY_ROWS: usize = 20_000;

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
    },
    Field {
        name: SharedString,
        value: SharedString,
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

/// Builds the rows for `details`, reading bodies from `path` within the
/// preview bound. `capture_start` is the first request's start, for offsets.
pub(crate) fn build(
    path: &Path,
    details: &Details,
    capture_start: Option<f64>,
    cancel: &AtomicBool,
) -> DetailRows {
    let mut rows = Vec::new();
    general(details, capture_start, &mut rows);
    timing(details, &mut rows);
    section(&mut rows, Section::RequestHeaders, &details.request_headers);
    section(&mut rows, Section::Query, &details.query);
    cookies(&mut rows, Section::RequestCookies, &details.request_cookies);
    body(
        path,
        Section::RequestBody,
        details.request_body.as_ref(),
        cancel,
        &mut rows,
    );
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
    body(
        path,
        Section::ResponseBody,
        details.response_body.as_ref(),
        cancel,
        &mut rows,
    );
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
    DetailRows { rows }
}

fn field(rows: &mut Vec<Row>, name: &str, value: impl Into<String>) {
    let value = value.into();
    if !value.is_empty() {
        rows.push(Row::Field {
            name: name.to_owned().into(),
            value: bounded(&value, VALUE).into(),
        });
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
    });
    field(rows, "URL", shown(&details.url));
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
    });
    let phases: Vec<_> = details
        .timings
        .iter()
        // Chrome's `_blocked_*` and similar custom phases overlap the
        // standard ones; they are listed but do not move the bar.
        .map(|(name, ms)| (name, *ms, !name.starts_with('_')))
        .collect();
    let sum: f64 = phases.iter().filter(|p| p.2).map(|p| p.1).sum();
    let total = details.time.unwrap_or(0.).max(sum).max(f64::EPSILON);
    let mut at = 0.;
    for (name, ms, sequential) in phases {
        let start = (at / total) as f32;
        rows.push(Row::Timing {
            name: phase_label(name).into(),
            ms,
            start: start.clamp(0., 1.),
            width: ((ms / total) as f32).clamp(0., 1. - start.clamp(0., 1.)),
        });
        if sequential {
            at += ms;
        }
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
    });
    for pair in pairs {
        rows.push(Row::Field {
            name: bounded(&pair.name, 200).into(),
            value: bounded(&shown(&pair.value), VALUE).into(),
        });
    }
}

fn cookies(rows: &mut Vec<Row>, section: Section, cookies: &[Cookie]) {
    if cookies.is_empty() {
        return;
    }
    rows.push(Row::Section {
        section,
        count: Some(cookies.len()),
    });
    for cookie in cookies {
        let mut value = shown(&cookie.value);
        for attribute in &cookie.attributes {
            value.push_str("; ");
            value.push_str(attribute);
        }
        rows.push(Row::Field {
            name: bounded(&cookie.name, 200).into(),
            value: bounded(&value, VALUE).into(),
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
/// and escapes stay exactly as written; only whitespace between tokens changes.
pub(crate) fn pretty(text: &str, mime: &str, complete: bool) -> Option<String> {
    let start = text.trim_start().as_bytes().first().copied();
    let json = mime.to_ascii_lowercase().contains("json") || matches!(start, Some(b'{' | b'['));
    if !(json && complete) {
        return None;
    }
    serde_json::from_str::<serde::de::IgnoredAny>(text).ok()?;
    let pretty = indent_json(text);
    (pretty != text).then_some(pretty)
}

/// Re-indents valid JSON text with two spaces per level.
fn indent_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 4);
    let mut depth = 0usize;
    let (mut in_string, mut escaped) = (false, false);
    let mut chars = text.chars().peekable();
    let newline = |out: &mut String, depth: usize| {
        out.push('\n');
        for _ in 0..depth {
            out.push_str("  ");
        }
    };
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '{' | '[' => {
                out.push(c);
                while chars.next_if(|c| c.is_ascii_whitespace()).is_some() {}
                if matches!(chars.peek(), Some('}' | ']')) {
                    out.push(chars.next().unwrap_or_default());
                } else {
                    depth += 1;
                    newline(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                newline(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(c);
                newline(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c if c.is_ascii_whitespace() => {}
            c => out.push(c),
        }
    }
    out
}

fn body(
    path: &Path,
    section: Section,
    body: Option<&Body>,
    cancel: &AtomicBool,
    rows: &mut Vec<Row>,
) {
    let Some(body) = body else {
        return;
    };
    let empty_text = body.text.as_ref().is_none_or(Text::is_empty);
    if empty_text && body.params.is_empty() {
        if section == Section::ResponseBody && body.size.is_some_and(|size| size > 0) {
            rows.push(Row::Section {
                section,
                count: None,
            });
            rows.push(Row::Note(
                format!(
                    "The capture does not include this body ({}).",
                    model::bytes(body.size.unwrap_or(0))
                )
                .into(),
            ));
        }
        return;
    }
    rows.push(Row::Section {
        section,
        count: None,
    });
    for pair in &body.params {
        rows.push(Row::Field {
            name: bounded(&pair.name, 200).into(),
            value: bounded(&shown(&pair.value), VALUE).into(),
        });
    }
    let Some(text) = body.text.as_ref().filter(|text| !text.is_empty()) else {
        return;
    };
    let decoded = match har::body_bytes(path, text, body.base64(), BODY_PREVIEW, cancel) {
        Ok(decoded) => decoded,
        Err(error) => {
            rows.push(Row::Note(error.into()));
            return;
        }
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
            let pretty = pretty(&text, &body.mime, decoded.complete);
            let shown = pretty.as_deref().unwrap_or(&text);
            text_rows(shown, rows);
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
            pretty(r#"{"a":[1]}"#, "application/json", true).as_deref(),
            Some("{\n  \"a\": [\n    1\n  ]\n}")
        );
        assert_eq!(pretty(r#"{"a":[1]"#, "application/json", true), None);
        assert_eq!(pretty(r#"{"a":1}"#, "application/json", false), None);
        assert_eq!(pretty("<html>{}</html>", "text/html", true), None);
        // Order, numbers and escapes are kept as written.
        assert_eq!(
            pretty(r#"{"z":1.50,"a":{},"s":"x\"]{,:","e":[ ]}"#, "", true).as_deref(),
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
                },
                Row::Note("a".into()),
                Row::Section {
                    section: Section::Timing,
                    count: None,
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
}
