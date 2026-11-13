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
        /// A body section's decoded text for copying (#2888), and whether
        /// it is the whole body (else the decoded preview's first part).
        body: Option<(Arc<str>, bool)>,
    },
    Field {
        name: SharedString,
        value: SharedString,
        /// The whole value when `value` shows only part of it (#2888).
        full: Option<Vec<Part>>,
        /// The whole name when `name` shows only part of it.
        name_full: Option<Text>,
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
    /// A small raster image body, decoded on the worker (never SVG).
    Image(Preview),
    Note(SharedString),
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DetailRows {
    pub rows: Vec<Row>,
    /// Body sections that can be saved: (section row, source).
    pub sources: Vec<(usize, BodySource)>,
}

/// A body that Save as… writes whole from the capture.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BodySource {
    pub text: Text,
    pub base64: bool,
    /// A file name suggestion from the URL and MIME type.
    pub name: String,
}

/// A decoded preview image; compared by identity.
#[derive(Clone)]
pub(crate) struct Preview {
    pub image: Arc<gpui::RenderImage>,
    pub width: u32,
    pub height: u32,
}

impl PartialEq for Preview {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.image, &other.image)
    }
}

impl std::fmt::Debug for Preview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Preview({}x{})", self.width, self.height)
    }
}

/// Image bodies up to this decoded size are previewed.
pub(crate) const IMAGE_BYTES: usize = 8 << 20;

/// Largest decoded image (all frames' pixel buffer) a preview may allocate.
pub(crate) const IMAGE_ALLOC: u64 = 64 * 1024 * 1024;

/// Why an image body is not previewed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoPreview {
    /// Not PNG, JPEG, GIF, WebP or BMP (SVG and markup are never rendered).
    Format,
    /// Wider or taller than 8192 px, or a decoded buffer above
    /// [`IMAGE_ALLOC`]: refused from the header, before any allocation.
    TooLarge,
    Damaged,
}

/// A thumbnail of a raster image body (PNG, JPEG, GIF, WebP, BMP). The
/// header's dimensions and decoded size are checked before the pixel buffer
/// is allocated; anything else is not rendered.
pub(crate) fn preview(bytes: &[u8]) -> Option<Preview> {
    preview_checked(bytes).ok()
}

pub(crate) fn preview_checked(bytes: &[u8]) -> Result<Preview, NoPreview> {
    use image::ImageDecoder as _;
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| NoPreview::Damaged)?;
    if !matches!(
        reader.format(),
        Some(
            image::ImageFormat::Png
                | image::ImageFormat::Jpeg
                | image::ImageFormat::Gif
                | image::ImageFormat::WebP
                | image::ImageFormat::Bmp
        )
    ) {
        return Err(NoPreview::Format);
    }
    let mut reader = reader;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(IMAGE_ALLOC);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(|error| match error {
        image::ImageError::Limits(_) => NoPreview::TooLarge,
        _ => NoPreview::Damaged,
    })?;
    let (width, height) = decoder.dimensions();
    // `DynamicImage::from_decoder` allocates `total_bytes` without
    // consulting `max_alloc`: refuse oversized buffers from the header.
    if width > 8192 || height > 8192 || decoder.total_bytes() > IMAGE_ALLOC {
        return Err(NoPreview::TooLarge);
    }
    let decoded = image::DynamicImage::from_decoder(decoder).map_err(|_| NoPreview::Damaged)?;
    let mut thumbnail = decoded.thumbnail(480, 360).into_rgba8();
    // GPUI RenderImage frames are BGRA.
    for pixel in thumbnail.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Ok(Preview {
        image: Arc::new(gpui::RenderImage::new(vec![image::Frame::new(thumbnail)])),
        width,
        height,
    })
}

/// A file name for a saved body: the URL's last path segment, else
/// `response`, with an extension from the MIME type when it has none.
pub(crate) fn file_name(url: &str, mime: &str) -> String {
    let path = har::split_url(url).1;
    let path = path.split(['?', '#']).next().unwrap_or("");
    let last = path.rsplit('/').next().unwrap_or("");
    let mut name: String = last
        .chars()
        .filter(|c| !c.is_control() && !r#"\/:*?"<>|"#.contains(*c))
        .take(120)
        .collect();
    let name_trim = name.trim_matches(['.', ' ']).to_owned();
    name = if name_trim.is_empty() {
        "response".into()
    } else {
        name_trim
    };
    if !name.contains('.') {
        let mime = mime
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let extension = match mime.as_str() {
            m if m.contains("json") => "json",
            "text/html" => "html",
            "text/css" => "css",
            m if m.contains("javascript") => "js",
            m if m.contains("xml") => "xml",
            "text/plain" => "txt",
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            "image/svg+xml" => "svg",
            "application/pdf" => "pdf",
            _ => "bin",
        };
        name.push('.');
        name.push_str(extension);
    }
    name
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
    let mut sources = Vec::new();
    for (section, body) in [
        (Section::RequestBody, &details.request_body),
        (Section::ResponseBody, &details.response_body),
    ] {
        let Some(body) = body else { continue };
        let Some(text) = body.text.as_ref().filter(|text| !text.is_empty()) else {
            continue;
        };
        let row = rows
            .iter()
            .position(|row| matches!(row, Row::Section { section: s, .. } if *s == section));
        if let Some(row) = row {
            sources.push((
                row,
                BodySource {
                    text: text.clone(),
                    base64: body.base64(),
                    name: file_name(details.url.shown(), &body.mime),
                },
            ));
        }
    }
    Ok(DetailRows { rows, sources })
}

fn field(rows: &mut Vec<Row>, name: &str, value: impl Into<String>) {
    let value = value.into();
    if !value.is_empty() {
        rows.push(field_row(
            Text::Inline(name.to_owned()),
            Text::Inline(value),
        ));
    }
}

/// A field whose source may be longer than the details keep (`long`).
fn field_long(rows: &mut Vec<Row>, name: &str, value: String, long: Option<&Text>) {
    match long {
        Some(long) => rows.push(field_row(Text::Inline(name.to_owned()), long.clone())),
        None => field(rows, name, value),
    }
}

/// A field shown as `display` whose copy is `parts` when some are long.
fn field_parts(rows: &mut Vec<Row>, name: &str, display: String, parts: Vec<Part>) {
    if display.is_empty() {
        return;
    }
    let long = parts
        .iter()
        .any(|part| matches!(part, Part::File(Text::Long { .. })));
    rows.push(Row::Field {
        name: name.to_owned().into(),
        value: bounded(&display, VALUE).into(),
        full: long.then_some(parts),
        name_full: None,
    });
}

pub(crate) fn text_part(text: &Text) -> Part {
    match text {
        Text::Inline(text) => Part::Literal(text.clone()),
        long => Part::File(long.clone()),
    }
}

/// A name/value row; `full` and `name_full` keep what the row shows only
/// in part, so a copy is always the whole original (#2888).
fn field_row(name: Text, value: Text) -> Row {
    let display = bounded(&shown(&value), VALUE);
    let full = match &value {
        Text::Long { .. } => Some(vec![text_part(&value)]),
        Text::Inline(text) if display != *text => Some(vec![text_part(&value)]),
        Text::Inline(_) => None,
    };
    let display_name = bounded(&shown(&name), 200);
    let name_full = match &name {
        Text::Long { .. } => Some(name),
        Text::Inline(text) if display_name != *text => Some(name),
        Text::Inline(_) => None,
    };
    Row::Field {
        name: display_name.into(),
        value: display.into(),
        full,
        name_full,
    }
}

fn name_text(name: &str, long: &Option<Text>) -> Text {
    long.clone()
        .unwrap_or_else(|| Text::Inline(name.to_owned()))
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
    let long = |field: &str| details.long(field);
    if !details.url.is_empty() {
        rows.push(field_row(Text::Inline("URL".into()), details.url.clone()));
    }
    field_long(rows, "Method", details.method.clone(), long("method"));
    let (status, parts) = match details.status {
        Some(0) => ("0 (no response)".to_owned(), Vec::new()),
        Some(code) if details.status_text.is_empty() => (code.to_string(), Vec::new()),
        Some(code) => (
            format!("{code} {}", details.status_text),
            match long("status_text") {
                Some(text) => vec![Part::Literal(format!("{code} ")), text_part(text)],
                None => Vec::new(),
            },
        ),
        None => ("No response".to_owned(), Vec::new()),
    };
    field_parts(rows, "Status", status, parts);
    let error = long("error").or_else(|| long("response_error"));
    field_long(rows, "Error", details.error.clone(), error);
    let (version, version_long) = if details.response_version.is_empty() {
        (&details.request_version, long("request_version"))
    } else {
        (&details.response_version, long("response_version"))
    };
    field_long(rows, "Protocol", version.clone(), version_long);
    let server = long("server").map_or_else(|| Part::Literal(details.server.clone()), text_part);
    let (address, parts) = match (details.server.as_str(), details.connection.as_str()) {
        ("", _) => (String::new(), Vec::new()),
        (ip, "") => (ip.to_owned(), vec![server]),
        // Chrome stores the port as `connection`, Firefox the connection id.
        (ip, port) if port.bytes().all(|b| b.is_ascii_digit()) && !ip.contains(':') => (
            format!("{ip}:{port}"),
            vec![server, Part::Literal(format!(":{port}"))],
        ),
        (ip, _) => (ip.to_owned(), vec![server]),
    };
    field_parts(rows, "Remote address", address, parts);
    field_long(
        rows,
        "Resource type",
        details.resource_type.clone(),
        long("resource_type"),
    );
    if let Some(body) = &details.response_body {
        field_long(rows, "MIME type", body.mime.clone(), long("mime"));
        if let Some(size) = body.size {
            field(rows, "Size", model::bytes(size));
        }
    }
    let mut started = details.started.clone();
    let mut suffix = String::new();
    if let (Some(start), Some(first)) = (har::parse_time(&details.started), capture_start) {
        suffix = format!(" (+{})", model::millis((start - first).max(0.)));
    }
    started.push_str(&suffix);
    let parts = match long("started") {
        Some(text) => vec![text_part(text), Part::Literal(suffix)],
        None => Vec::new(),
    };
    field_parts(rows, "Started", started, parts);
    if let Some(time) = details.time {
        field(rows, "Time", model::millis(time));
    }
    field_long(rows, "Redirect", details.redirect.clone(), long("redirect"));
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
        rows.push(field_row(
            name_text(&pair.name, &pair.name_long),
            pair.value.clone(),
        ));
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
        let Row::Field {
            name, name_full, ..
        } = field_row(name_text(&cookie.name, &cookie.name_long), Text::default())
        else {
            continue;
        };
        rows.push(Row::Field {
            name,
            value: bounded(&value, VALUE).into(),
            full: Some(vec![text_part(&cookie.value)]),
            name_full,
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
        rows.push(field_row(
            name_text(&pair.name, &pair.name_long),
            pair.value.clone(),
        ));
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
            let raster = body.mime.to_ascii_lowercase().starts_with("image/")
                && !body.mime.to_ascii_lowercase().contains("svg");
            if raster {
                let whole = if decoded.complete {
                    Some(decoded.bytes)
                } else {
                    har::body_bytes(path, text, body.base64(), IMAGE_BYTES, revision, cancel)
                        .ok()
                        .filter(|bytes| bytes.complete)
                        .map(|bytes| bytes.bytes)
                };
                if cancel.load(Ordering::Relaxed) {
                    return Err(DetailError::Cancelled);
                }
                if let Some(preview) = whole.as_deref().and_then(preview) {
                    rows.push(Row::Image(preview));
                }
            }
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
                *slot = Some((Arc::from(text.as_str()), decoded.complete));
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
    /// Text known to be only the first part of the value (a body preview).
    Cut(String),
    File(Text),
}

fn value_parts(row: &Row) -> Option<Vec<Part>> {
    Some(match row {
        Row::Field { value, full, .. } => full
            .clone()
            .unwrap_or_else(|| vec![Part::Literal(value.to_string())]),
        Row::Timing { ms, .. } => vec![Part::Literal(model::millis(*ms))],
        Row::Note(text) => vec![Part::Literal(text.to_string())],
        Row::Section { .. } | Row::Text(_) | Row::Image(_) => return None,
    })
}

fn name_part(row: &Row) -> Option<Part> {
    match row {
        Row::Field {
            name_full: Some(name),
            ..
        } => Some(text_part(name)),
        Row::Field { name, .. } | Row::Timing { name, .. } => Some(Part::Literal(name.to_string())),
        _ => None,
    }
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
        if let Some((body, complete)) = body {
            return Some(vec![if *complete {
                Part::Literal(body.to_string())
            } else {
                Part::Cut(body.to_string())
            }]);
        }
        let mut parts = Vec::new();
        for row in rows[start + 1..]
            .iter()
            .take_while(|row| !matches!(row, Row::Section { .. }))
        {
            let Some(name) = name_part(row) else {
                continue;
            };
            if !parts.is_empty() {
                parts.push(Part::Literal("\n".into()));
            }
            parts.push(name);
            parts.push(Part::Literal(": ".into()));
            parts.extend(value_parts(row).unwrap_or_default());
        }
        (!parts.is_empty()).then_some(parts)
    };
    match (row, kind) {
        (Row::Section { .. } | Row::Text(_), CopyKind::Name | CopyKind::Pair) => None,
        (Row::Section { .. } | Row::Text(_), _) | (_, CopyKind::Section) => section(),
        (_, CopyKind::Name) => Some(vec![name_part(row)?]),
        (_, CopyKind::Pair) => {
            let mut parts = vec![name_part(row)?, Part::Literal(": ".into())];
            parts.extend(value_parts(row)?);
            Some(parts)
        }
        (_, CopyKind::Value) => value_parts(row),
    }
}

/// Largest value a copy reads from the capture.
pub(crate) const COPY_BYTES: usize = 64 << 20;

/// Where a request's long values live: its byte range in the indexed file
/// revision and the hash of its bytes ([`har::Entry::hash`]).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Origin {
    pub span: std::ops::Range<u64>,
    pub revision: Option<Revision>,
    pub hash: u64,
}

/// Copied text, and the limit it was cut at when it is not whole.
#[derive(Debug, PartialEq)]
pub(crate) struct Copied {
    pub text: String,
    pub cut_at: Option<usize>,
}

impl Copied {
    /// What the user is told: a cut copy says so.
    pub(crate) fn label(&self) -> String {
        match self.cut_at {
            None => "Copied".into(),
            Some(0) => "Copied without the binary request body".into(),
            Some(1) => "Copied without the values containing ! (unsafe in cmd; use bash or PowerShell for them)".into(),
            Some(limit) => format!(
                "Copied the first {}; the value is longer",
                model::bytes(limit as i64)
            ),
        }
    }
}

/// Joins `parts`, reading `File` parts (at most `cap` bytes each) from the
/// request at `origin`, which is verified first: a file whose request bytes
/// changed is [`DetailError::Changed`], never a copy of other bytes.
pub(crate) fn resolve(
    path: &Path,
    parts: Vec<Part>,
    origin: Option<&Origin>,
    cap: usize,
    cancel: &AtomicBool,
) -> Result<Copied, DetailError> {
    let reads = parts
        .iter()
        .any(|part| matches!(part, Part::File(Text::Long { .. })));
    let revision = origin.and_then(|origin| origin.revision);
    if let (true, Some(origin)) = (reads, origin) {
        har::verify_entry(path, &origin.span, revision, origin.hash, cancel)?;
    }
    let mut copied = Copied {
        text: String::new(),
        cut_at: None,
    };
    let cut = |limit: usize, copied: &mut Copied| {
        copied.cut_at = Some(copied.cut_at.map_or(limit, |at| at.min(limit)));
    };
    for part in parts {
        match part {
            Part::Literal(text) | Part::File(Text::Inline(text)) => copied.text.push_str(&text),
            Part::Cut(text) => {
                copied.text.push_str(&text);
                cut(BODY_PREVIEW, &mut copied);
            }
            Part::File(long) => {
                let bytes = har::body_bytes(path, &long, false, cap, revision, cancel)?;
                copied.text.push_str(&String::from_utf8_lossy(&bytes.bytes));
                if !bytes.complete {
                    cut(cap, &mut copied);
                }
            }
        }
    }
    Ok(copied)
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
            ..Default::default()
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
            body: body.map(|body| (Arc::from(body), true)),
        };
        let rows = vec![
            section(Section::RequestHeaders, None),
            field_row(Text::Inline("a".into()), Text::Inline("1".into())),
            field_row(
                Text::Inline("b".into()),
                Text::Inline("v".repeat(VALUE + 10)),
            ),
            section(Section::ResponseBody, Some("{\"ok\":true}")),
            Row::Text("{".into()),
        ];
        let literal = |parts: Option<Vec<Part>>| {
            parts.map(|parts| {
                parts
                    .into_iter()
                    .map(|part| match part {
                        Part::Literal(text) | Part::Cut(text) => text,
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

    /// Review of PR 2900 (P2-5): a copy cut at its cap says so.
    #[test]
    fn review2900_cut_copies_are_labelled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cut.har");
        let value = "z".repeat(1000);
        std::fs::write(&path, format!("\"{value}\"")).unwrap();
        let long = Text::Long {
            prefix: String::new(),
            span: 0..1002,
        };
        let open = AtomicBool::new(false);
        let cut = resolve(&path, vec![Part::File(long.clone())], None, 10, &open).unwrap();
        assert_eq!((cut.text.as_str(), cut.cut_at), ("zzzzzzzzzz", Some(10)));
        assert_eq!(cut.label(), "Copied the first 10 B; the value is longer");
        // Control: a cap above the value copies it whole and says Copied.
        let whole = resolve(&path, vec![Part::File(long)], None, 4096, &open).unwrap();
        assert_eq!((whole.text, whole.cut_at), (value, None));
        let body = resolve(&path, vec![Part::Cut("partial".into())], None, 4096, &open).unwrap();
        assert_eq!(body.cut_at, Some(BODY_PREVIEW));
        assert!(body.label().starts_with("Copied the first"));
    }

    /// A PNG header for `width`×`height` RGBA16 with a tiny zlib IDAT: the
    /// header alone claims the size; no pixel data is needed to test it.
    fn png_header(width: u32, height: u32) -> Vec<u8> {
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut crc = flate2::Crc::new();
            crc.update(kind);
            crc.update(data);
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            out.extend_from_slice(&crc.sum().to_be_bytes());
        }
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[16, 6, 0, 0, 0]);
        chunk(&mut png, b"IHDR", &ihdr);
        chunk(
            &mut png,
            b"IDAT",
            &[0x78, 0x01, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
        );
        chunk(&mut png, b"IEND", &[]);
        png
    }

    /// Review of PR 2904 (P1-2): an 8192×8192 RGBA16 PNG (512 MiB decoded,
    /// a few bytes of input) is refused from its header.
    #[test]
    fn review2904_huge_images_are_refused_before_allocation() {
        let huge = png_header(8192, 8192);
        assert!(huge.len() < 8 * 1024 * 1024);
        assert_eq!(preview_checked(&huge).err(), Some(NoPreview::TooLarge));
        // Control: a small image of the same kind decodes.
        let mut small = std::io::Cursor::new(Vec::new());
        image::ImageBuffer::<image::Rgba<u16>, _>::from_pixel(
            4,
            3,
            image::Rgba([0, 1000, 2000, 65535]),
        )
        .write_to(&mut small, image::ImageFormat::Png)
        .unwrap();
        let preview = preview_checked(small.get_ref()).unwrap();
        assert_eq!((preview.width, preview.height), (4, 3));
        assert_eq!(preview_checked(b"<svg/>").err(), Some(NoPreview::Format));
    }
}
