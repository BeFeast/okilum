//! HTTP Archive (HAR 1.2) files for the Reader's request viewer (#2836).
//!
//! Captures are routinely 10–200 MB and carry cookies, tokens, passwords and
//! whole response bodies. The index pass streams the file once through a
//! bounded buffer and keeps only what the request list shows, plus each
//! entry's byte range. Details are parsed again from that range when a request
//! is selected; strings above [`INLINE_TEXT`] (bodies, huge cookies) stay on
//! disk as byte ranges and are decoded on demand within a caller's cap.
//!
//! Nothing in this module logs, records or returns file content anywhere but
//! to its caller. Errors carry byte offsets and fixed messages, never text
//! from the file.
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// Files above this are not indexed; the request list says so and the source
/// view keeps its own (smaller) limit.
pub const MAX_FILE_BYTES: u64 = 1 << 30;
/// The list keeps at most this many requests; the rest of the file is not read.
pub const MAX_ENTRIES: usize = 200_000;
/// Bytes of a URL, method or type the index keeps per request.
pub const LIST_TEXT: usize = 2048;
/// Detail strings longer than this stay on disk as a byte range.
pub const INLINE_TEXT: usize = 64 * 1024;
/// Headers, cookies, parameters or timings kept per detail list.
pub const MAX_ITEMS: usize = 5_000;
/// Values one detail parse may build, and decoded string bytes it may keep;
/// the rest of the request is skipped (the details say so).
pub const MAX_DETAIL_NODES: usize = 100_000;
pub const MAX_DETAIL_BYTES: usize = 8 << 20;
/// Durations above this (about 31 years) are not plausible and are unknown.
pub const MAX_MS: f64 = 1e12;
/// Bound for the iterative skipper; deeper input is reported as damaged.
const MAX_DEPTH: usize = 4096;
const BUFFER: usize = 256 * 1024;
/// A batch goes to the caller after this many requests or this much time.
const BATCH: usize = 2048;
const BATCH_TIME: Duration = Duration::from_millis(60);

/// One request in the list. Strings are bounded by [`LIST_TEXT`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    /// The entry object, `{` through `}`, in the file.
    pub span: Range<u64>,
    pub method: Box<str>,
    pub url: Box<str>,
    /// The URL was longer than [`LIST_TEXT`]; details hold the whole value.
    pub url_cut: bool,
    /// `None` when the response has no status; `Some(0)` is a failed request.
    pub status: Option<i64>,
    pub mime: Box<str>,
    /// Chrome's `_resourceType`, when present.
    pub resource_type: Box<str>,
    /// Decoded response size (`content.size`), when known.
    pub size: Option<i64>,
    /// Bytes on the wire: `_transferSize`, or headers + body sizes; unknown
    /// when neither `_transferSize` nor the body size is known.
    pub transferred: Option<i64>,
    /// Total time in milliseconds (`entry.time`).
    pub time: Option<f64>,
    pub started: Box<str>,
    /// `startedDateTime` as Unix milliseconds.
    pub started_ms: Option<f64>,
    /// A browser's own failure text (`_error`, `response._error`).
    pub error: Box<str>,
    /// Hash of the entry's bytes in the file, checked before details are
    /// shown: a replaced file with the same size and time cannot pass.
    pub hash: u64,
}

impl Entry {
    /// No response, status 0, an HTTP error status or a browser error.
    pub fn failed(&self) -> bool {
        !self.error.is_empty()
            || self
                .status
                .is_none_or(|status| status == 0 || status >= 400)
    }

    pub fn host(&self) -> &str {
        split_url(&self.url).0
    }

    /// Path and query, or the whole value when it is not an absolute URL.
    pub fn path(&self) -> &str {
        split_url(&self.url).1
    }

    pub fn kind(&self) -> Kind {
        Kind::of(&self.resource_type, &self.mime)
    }

    /// Short type label for the list: the browser's resource type, else the
    /// MIME subtype.
    pub fn type_label(&self) -> &str {
        if !self.resource_type.is_empty() {
            return &self.resource_type;
        }
        let mime = self.mime.split(';').next().unwrap_or("").trim();
        let subtype = mime.rsplit('/').next().unwrap_or(mime);
        subtype.rsplit('+').next().unwrap_or(subtype)
    }
}

/// `https://host:port/path?q` → (`host:port`, `/path?q`).
pub fn split_url(url: &str) -> (&str, &str) {
    let Some(scheme) = url.find("://") else {
        return ("", url);
    };
    let rest = &url[scheme + 3..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..end];
    // Credentials in the authority are not part of the host shown in a list.
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    let path = &rest[end..];
    (host, if path.is_empty() { "/" } else { path })
}

/// Request categories for filters, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Document,
    Fetch,
    Script,
    Stylesheet,
    Image,
    Font,
    Media,
    WebSocket,
    Other,
}

impl Kind {
    pub const ALL: [Kind; 9] = [
        Kind::Document,
        Kind::Fetch,
        Kind::Script,
        Kind::Stylesheet,
        Kind::Image,
        Kind::Font,
        Kind::Media,
        Kind::WebSocket,
        Kind::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Document => "Document",
            Kind::Fetch => "Fetch/XHR",
            Kind::Script => "Script",
            Kind::Stylesheet => "Stylesheet",
            Kind::Image => "Image",
            Kind::Font => "Font",
            Kind::Media => "Media",
            Kind::WebSocket => "WebSocket",
            Kind::Other => "Other",
        }
    }

    pub fn of(resource_type: &str, mime: &str) -> Kind {
        match resource_type.to_ascii_lowercase().as_str() {
            "document" => return Kind::Document,
            "xhr" | "fetch" | "eventsource" => return Kind::Fetch,
            "script" => return Kind::Script,
            "stylesheet" => return Kind::Stylesheet,
            "image" => return Kind::Image,
            "font" => return Kind::Font,
            "media" => return Kind::Media,
            "websocket" => return Kind::WebSocket,
            "" => {}
            _ => return Kind::Other,
        }
        let mime = mime
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if mime == "text/html" || mime == "application/xhtml+xml" {
            Kind::Document
        } else if mime.contains("javascript") || mime.contains("ecmascript") {
            Kind::Script
        } else if mime == "text/css" {
            Kind::Stylesheet
        } else if mime.starts_with("image/") {
            Kind::Image
        } else if mime.starts_with("font/") || mime.contains("font-") || mime.contains("woff") {
            Kind::Font
        } else if mime.starts_with("audio/") || mime.starts_with("video/") {
            Kind::Media
        } else if mime.contains("json") || mime.contains("xml") || mime == "text/plain" {
            Kind::Fetch
        } else {
            Kind::Other
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    pub offset: u64,
    pub message: &'static str,
}

impl std::fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at byte {}", self.message, self.offset)
    }
}

/// Why the index stopped before a complete, well-formed file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// Valid JSON (so far) that has no `log.entries` list.
    NotHar(&'static str),
    /// Invalid JSON; requests read before `offset` are kept.
    Damaged(SyntaxError),
    Unreadable(String),
    TooLarge(u64),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub entries: usize,
    /// [`MAX_ENTRIES`] were listed; the rest of the file was not read.
    pub limited: bool,
    pub creator: Option<String>,
    pub problem: Option<Problem>,
    /// The file as it was opened; details refuse a different file.
    pub revision: Option<Revision>,
}

/// Identifies the file the byte ranges belong to: size and modification time
/// of the opened file. Details and bodies are read only from that revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Revision {
    pub len: u64,
    pub modified: Option<SystemTime>,
}

impl Revision {
    pub fn of(file: &std::fs::File) -> std::io::Result<Self> {
        let meta = file.metadata()?;
        Ok(Self {
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Cancelled;

/// Requests read since the previous batch, and how far the file was read.
pub struct Batch {
    pub entries: Vec<Entry>,
    pub offset: u64,
    pub revision: Option<Revision>,
}

/// Indexes the HAR file at `path`, sending requests in batches to `sink`.
pub fn index_file(
    path: &Path,
    cancel: &AtomicBool,
    sink: &mut dyn FnMut(Batch),
) -> Result<Outcome, Cancelled> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) => {
            return Ok(Outcome {
                problem: Some(Problem::Unreadable(error.kind().to_string())),
                ..Default::default()
            })
        }
    };
    let revision = match Revision::of(&file) {
        Ok(revision) => revision,
        Err(error) => {
            return Ok(Outcome {
                problem: Some(Problem::Unreadable(error.kind().to_string())),
                ..Default::default()
            })
        }
    };
    if revision.len > MAX_FILE_BYTES {
        return Ok(Outcome {
            problem: Some(Problem::TooLarge(revision.len)),
            revision: Some(revision),
            ..Default::default()
        });
    }
    // A file that grows while it is read is indexed up to its size at open.
    let mut outcome = index(file.take(revision.len), cancel, &mut |mut batch| {
        batch.revision = Some(revision);
        sink(batch)
    })?;
    outcome.revision = Some(revision);
    Ok(outcome)
}

/// Indexes HAR text from `reader`; see [`index_file`].
pub fn index(
    reader: impl Read,
    cancel: &AtomicBool,
    sink: &mut dyn FnMut(Batch),
) -> Result<Outcome, Cancelled> {
    let mut scanner = Scanner::new(reader, cancel, 0);
    let mut indexer = Indexer {
        pending: Vec::new(),
        flushed: Instant::now(),
        outcome: Outcome::default(),
        sink,
        found_entries: false,
    };
    let result = indexer.root(&mut scanner);
    indexer.flush(scanner.offset());
    let mut outcome = indexer.outcome;
    match result {
        Ok(()) => {
            if !indexer.found_entries && outcome.problem.is_none() {
                outcome.problem = Some(Problem::NotHar("There is no log.entries list"));
            }
        }
        Err(Stop::Cancelled) => return Err(Cancelled),
        Err(Stop::Limit) => outcome.limited = true,
        Err(Stop::Io(error)) => outcome.problem = Some(Problem::Unreadable(error.to_string())),
        Err(Stop::Syntax(error)) => outcome.problem = Some(Problem::Damaged(error)),
        Err(Stop::NotHar(reason)) => outcome.problem = Some(Problem::NotHar(reason)),
    }
    Ok(outcome)
}

enum Stop {
    Cancelled,
    Limit,
    NotHar(&'static str),
    Io(std::io::Error),
    Syntax(SyntaxError),
}

type Step<T = ()> = Result<T, Stop>;

struct Indexer<'s> {
    pending: Vec<Entry>,
    flushed: Instant,
    outcome: Outcome,
    sink: &'s mut dyn FnMut(Batch),
    found_entries: bool,
}

impl Indexer<'_> {
    fn flush(&mut self, offset: u64) {
        if !self.pending.is_empty() {
            (self.sink)(Batch {
                entries: std::mem::take(&mut self.pending),
                offset,
                revision: None,
            });
        }
        self.flushed = Instant::now();
    }

    fn root<R: Read>(&mut self, s: &mut Scanner<R>) -> Step {
        s.skip_bom()?;
        match s.peek()? {
            Some(b'{') => s.pos += 1,
            Some(_) => return Err(Stop::NotHar("The file is not a JSON object")),
            None => return Err(Stop::NotHar("The file is empty")),
        }
        s.object(|s, key| {
            if key == b"log" && s.peek()? == Some(b'{') {
                s.pos += 1;
                s.object(|s, key| match key {
                    b"entries" if s.peek()? == Some(b'[') => {
                        s.pos += 1;
                        self.found_entries = true;
                        s.array(|s| {
                            if s.peek()? != Some(b'{') {
                                return s.skip_value();
                            }
                            let start = s.offset();
                            s.begin_hash();
                            s.pos += 1;
                            let mut entry = Entry::default();
                            index_entry(s, &mut entry)?;
                            entry.span = start..s.offset();
                            entry.hash = s.end_hash();
                            self.pending.push(entry);
                            self.outcome.entries += 1;
                            if self.pending.len() >= BATCH || self.flushed.elapsed() >= BATCH_TIME {
                                self.flush(s.offset());
                            }
                            if self.outcome.entries >= MAX_ENTRIES {
                                return Err(Stop::Limit);
                            }
                            Ok(())
                        })
                    }
                    b"creator" if s.peek()? == Some(b'{') => {
                        s.pos += 1;
                        let (mut name, mut version) = (String::new(), String::new());
                        s.object(|s, key| match key {
                            b"name" => s.text_into(&mut name, 128),
                            b"version" => s.text_into(&mut version, 64),
                            _ => s.skip_value(),
                        })?;
                        if !name.is_empty() {
                            self.outcome.creator = Some(if version.is_empty() {
                                name
                            } else {
                                format!("{name} {version}")
                            });
                        }
                        Ok(())
                    }
                    _ => s.skip_value(),
                })
            } else {
                s.skip_value()
            }
        })?;
        if s.peek()?.is_some() {
            return Err(s.error("Unexpected text after the end of the JSON value"));
        }
        Ok(())
    }
}

/// Duplicate keys: the last occurrence wins, as in [`details`]; a repeated
/// `request`, `response` or `content` object replaces the earlier one whole.
fn index_entry<R: Read>(s: &mut Scanner<R>, entry: &mut Entry) -> Step {
    let mut started = String::new();
    let mut entry_error: Box<str> = Box::default();
    let mut response_error: Box<str> = Box::default();
    s.object(|s, key| match key {
        b"startedDateTime" => s.text_into(&mut started, 64),
        b"time" => {
            entry.time = s.number()?.and_then(duration);
            Ok(())
        }
        b"_resourceType" => s.boxed(&mut entry.resource_type, 32),
        b"_error" => s.boxed(&mut entry_error, 256),
        b"request" => {
            entry.method = Box::default();
            entry.url = Box::default();
            entry.url_cut = false;
            s.with_object(|s, key| match key {
                b"method" => s.boxed(&mut entry.method, 32),
                b"url" => {
                    let mut url = String::new();
                    entry.url_cut = !s.text_into_full(&mut url, LIST_TEXT)?;
                    entry.url = url.into();
                    Ok(())
                }
                _ => s.skip_value(),
            })
        }
        b"response" => {
            entry.status = None;
            entry.size = None;
            entry.mime = Box::default();
            response_error = Box::default();
            let (mut headers, mut body, mut transfer) = (None, None, None);
            s.with_object(|s, key| match key {
                b"status" => {
                    entry.status = s.number()?.and_then(whole);
                    Ok(())
                }
                b"headersSize" => {
                    headers = s.number()?.and_then(size);
                    Ok(())
                }
                b"bodySize" => {
                    body = s.number()?.and_then(size);
                    Ok(())
                }
                b"_transferSize" => {
                    transfer = s.number()?.and_then(size);
                    Ok(())
                }
                b"_error" => s.boxed(&mut response_error, 256),
                b"content" => {
                    entry.size = None;
                    entry.mime = Box::default();
                    s.with_object(|s, key| match key {
                        b"size" => {
                            entry.size = s.number()?.and_then(size);
                            Ok(())
                        }
                        b"mimeType" => s.boxed(&mut entry.mime, 128),
                        _ => s.skip_value(),
                    })
                }
                _ => s.skip_value(),
            })?;
            // An unknown body (`bodySize: -1`) leaves the wire size unknown:
            // the headers alone are not what was transferred (#2932).
            entry.transferred = transfer.or(match (headers, body) {
                (_, None) => None,
                (h, Some(b)) => h.unwrap_or(0).checked_add(b),
            });
            Ok(())
        }
        _ => s.skip_value(),
    })?;
    entry.error = if entry_error.is_empty() {
        response_error
    } else {
        entry_error
    };
    entry.started_ms = parse_time(&started);
    entry.started = started.into();
    Ok(())
}

/// Largest integer an f64 holds exactly; larger counts are not plausible.
const EXACT: f64 = 9_007_199_254_740_992.;

/// An integer field (status); non-finite or out of range is unknown.
pub fn whole(n: f64) -> Option<i64> {
    (n.is_finite() && n.abs() <= EXACT).then_some(n as i64)
}

/// A byte count; negative (HAR's "unknown" -1), non-finite or out of range is unknown.
pub fn size(n: f64) -> Option<i64> {
    (n.is_finite() && (0. ..=EXACT).contains(&n)).then_some(n as i64)
}

/// A duration in milliseconds; negative, non-finite or implausible is unknown.
pub fn duration(ms: f64) -> Option<f64> {
    (ms.is_finite() && (0. ..=MAX_MS).contains(&ms)).then_some(ms)
}

/// JSON number grammar (RFC 8259): no leading zeros, digits after `.` and
/// in the exponent, no `+` sign, no `inf`/`nan`.
pub fn json_number(text: &[u8]) -> bool {
    let digits = |i: &mut usize| {
        let start = *i;
        while text.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        *i > start
    };
    let mut i = 0;
    if text.first() == Some(&b'-') {
        i += 1;
    }
    match text.get(i) {
        Some(b'0') => i += 1,
        Some(b'1'..=b'9') => {
            digits(&mut i);
        }
        _ => return false,
    }
    if text.get(i) == Some(&b'.') {
        i += 1;
        if !digits(&mut i) {
            return false;
        }
    }
    if matches!(text.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(text.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if !digits(&mut i) {
            return false;
        }
    }
    i == text.len()
}

/// `startedDateTime` (ISO 8601 / RFC 3339) as Unix milliseconds.
pub fn parse_time(text: &str) -> Option<f64> {
    let time =
        time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).ok()?;
    Some(time.unix_timestamp_nanos() as f64 / 1e6)
}

// ---------------------------------------------------------------------------
// Details

/// A detail string: short values are decoded, long ones stay in the file.
#[derive(Clone, Debug, PartialEq)]
pub enum Text {
    Inline(String),
    /// `prefix` is the first [`INLINE_TEXT`] bytes; `span` is the JSON string
    /// in the file including its quotes.
    Long {
        prefix: String,
        span: Range<u64>,
    },
}

impl Text {
    pub fn shown(&self) -> &str {
        match self {
            Text::Inline(text) => text,
            Text::Long { prefix, .. } => prefix,
        }
    }
    pub fn is_empty(&self) -> bool {
        matches!(self, Text::Inline(text) if text.is_empty())
    }
}

impl Default for Text {
    fn default() -> Self {
        Text::Inline(String::new())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pair {
    pub name: String,
    pub value: Text,
    /// The whole name when it is longer than [`INLINE_TEXT`].
    pub name_long: Option<Text>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cookie {
    pub name: String,
    pub value: Text,
    /// The whole name when it is longer than [`INLINE_TEXT`].
    pub name_long: Option<Text>,
    /// Remaining attributes, as `name=value` text.
    pub attributes: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Body {
    pub mime: String,
    pub size: Option<i64>,
    /// `encoding`, e.g. `base64`.
    pub encoding: String,
    pub text: Option<Text>,
    /// Form fields (`postData.params`).
    pub params: Vec<Pair>,
    pub comment: String,
}

impl Body {
    pub fn base64(&self) -> bool {
        self.encoding.eq_ignore_ascii_case("base64")
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Details {
    pub started: String,
    pub time: Option<f64>,
    pub method: String,
    pub url: Text,
    pub request_version: String,
    pub status: Option<i64>,
    pub status_text: String,
    pub response_version: String,
    pub redirect: String,
    pub server: String,
    pub connection: String,
    pub resource_type: String,
    pub error: String,
    pub request_headers: Vec<Pair>,
    pub response_headers: Vec<Pair>,
    pub query: Vec<Pair>,
    pub request_cookies: Vec<Cookie>,
    pub response_cookies: Vec<Cookie>,
    pub request_body: Option<Body>,
    pub response_body: Option<Body>,
    /// HAR `timings` in file order: blocked, dns, connect, ssl, send, wait,
    /// receive, plus any custom phase. `-1` (not applicable) is omitted.
    pub timings: Vec<(String, f64)>,
    /// Lists cut at [`MAX_ITEMS`], with their full lengths.
    pub cut_lists: Vec<(&'static str, usize)>,
    /// The parse reached [`MAX_DETAIL_NODES`] or [`MAX_DETAIL_BYTES`] and
    /// skipped the rest of the request.
    pub incomplete: bool,
    /// Values built by the parse (bounded by [`MAX_DETAIL_NODES`]).
    pub nodes: usize,
    /// Scalar fields longer than [`INLINE_TEXT`], by field name (`method`,
    /// `redirect`, …); the `String` fields above keep only their prefix.
    pub long: Vec<(&'static str, Text)>,
}

impl Details {
    /// The whole value of a scalar field kept in [`Details::long`].
    pub fn long(&self, field: &str) -> Option<&Text> {
        self.long
            .iter()
            .find(|(name, _)| *name == field)
            .map(|(_, text)| text)
    }
}

/// Why details or a body could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DetailError {
    Cancelled,
    /// The file is not the revision the request list was read from.
    Changed,
    Failed(String),
}

impl std::fmt::Display for DetailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DetailError::Cancelled => f.write_str("Cancelled."),
            DetailError::Changed => f.write_str("The file changed on disk."),
            DetailError::Failed(message) => f.write_str(message),
        }
    }
}

/// Opens `path` at `span.start`, refusing a file other than `revision`.
fn open_range(
    path: &Path,
    span: &Range<u64>,
    revision: Option<Revision>,
) -> Result<std::io::Take<std::fs::File>, DetailError> {
    let failed = |e: std::io::Error| DetailError::Failed(e.kind().to_string());
    let mut file = std::fs::File::open(path).map_err(failed)?;
    // The opened handle is checked, so a later rename cannot swap the bytes.
    if revision.is_some_and(|expected| Revision::of(&file).ok() != Some(expected)) {
        return Err(DetailError::Changed);
    }
    file.seek(SeekFrom::Start(span.start)).map_err(failed)?;
    Ok(file.take(span.end.saturating_sub(span.start)))
}

fn detail_error(stop: Stop, what: &str) -> DetailError {
    match stop {
        Stop::Cancelled => DetailError::Cancelled,
        Stop::Syntax(error) => DetailError::Failed(format!("This {what} is damaged: {error}.")),
        Stop::Io(error) => DetailError::Failed(error.kind().to_string()),
        Stop::Limit | Stop::NotHar(_) => {
            DetailError::Failed(format!("This {what} could not be read."))
        }
    }
}

/// Reads one entry's details from `span` (an [`Entry::span`]) of the file
/// `revision` whose entry bytes hash to `hash` ([`Entry::hash`]); `None`
/// skips a check. Different bytes are [`DetailError::Changed`].
pub fn details(
    path: &Path,
    span: Range<u64>,
    revision: Option<Revision>,
    hash: Option<u64>,
    cancel: &AtomicBool,
) -> Result<Details, DetailError> {
    if let Some(expected) = hash {
        verify_entry(path, &span, revision, expected, cancel)?;
    }
    let reader = open_range(path, &span, revision)?;
    parse_details(reader, span.start, cancel)
}

/// Checks that `span` of `path` still holds the indexed entry: the file
/// `revision` and the entry's own bytes ([`Entry::hash`]).
pub fn verify_entry(
    path: &Path,
    span: &Range<u64>,
    revision: Option<Revision>,
    hash: u64,
    cancel: &AtomicBool,
) -> Result<(), DetailError> {
    let mut reader = open_range(path, span, revision)?;
    if hash_bytes(&mut reader, cancel)? == hash {
        Ok(())
    } else {
        Err(DetailError::Changed)
    }
}

/// The same streaming hash the index computes over an entry's bytes.
fn hash_bytes(reader: &mut impl Read, cancel: &AtomicBool) -> Result<u64, DetailError> {
    use std::hash::Hasher as _;
    let mut hasher = std::hash::DefaultHasher::new();
    let mut buf = vec![0; BUFFER];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(DetailError::Cancelled);
        }
        match reader.read(&mut buf) {
            Ok(0) => return Ok(hasher.finish()),
            Ok(n) => hasher.write(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(DetailError::Failed(e.kind().to_string())),
        }
    }
}

/// Parses one entry from `reader`, positioned at file offset `base`.
fn parse_details(
    reader: impl Read,
    base: u64,
    cancel: &AtomicBool,
) -> Result<Details, DetailError> {
    let mut s = Scanner::new(reader, cancel, base);
    let mut budget = Budget::default();
    let node = match s.peek() {
        Ok(Some(b'{')) => s.node(Shape::Entry, &mut budget),
        Ok(_) => {
            return Err(DetailError::Failed(
                "The request is not a JSON object.".into(),
            ))
        }
        Err(stop) => Err(stop),
    };
    let mut details = Details::from_node(node.map_err(|stop| detail_error(stop, "request"))?);
    details.incomplete = budget.exhausted;
    details.nodes = budget.nodes;
    Ok(details)
}

/// The aggregate allowance of one detail parse.
#[derive(Default)]
struct Budget {
    nodes: usize,
    bytes: usize,
    exhausted: bool,
}

/// The parts of an entry the details show. Anything else is skipped while
/// parsing and never built, so unknown fields cost no memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Entry,
    Request,
    Response,
    Content,
    PostData,
    Pairs,
    Pair,
    Cookies,
    Cookie,
    Timings,
    /// A scalar; a container here is skipped.
    Value,
}

impl Shape {
    fn field(self, key: &[u8]) -> Option<Shape> {
        use Shape::*;
        match (self, key) {
            (Entry, b"request") => Some(Request),
            (Entry, b"response") => Some(Response),
            (Entry, b"timings") => Some(Timings),
            (
                Entry,
                b"startedDateTime" | b"time" | b"serverIPAddress" | b"connection"
                | b"_resourceType" | b"_error",
            ) => Some(Value),
            (Request, b"headers" | b"queryString") => Some(Pairs),
            (Request, b"cookies") => Some(Cookies),
            (Request, b"postData") => Some(PostData),
            (Request, b"method" | b"url" | b"httpVersion" | b"bodySize") => Some(Value),
            (Response, b"headers") => Some(Pairs),
            (Response, b"cookies") => Some(Cookies),
            (Response, b"content") => Some(Content),
            (Response, b"status" | b"statusText" | b"httpVersion" | b"redirectURL" | b"_error") => {
                Some(Value)
            }
            (Content, b"mimeType" | b"size" | b"encoding" | b"text" | b"comment") => Some(Value),
            (PostData, b"params") => Some(Pairs),
            (PostData, b"mimeType" | b"encoding" | b"text" | b"comment") => Some(Value),
            (Pair, b"name" | b"value") => Some(Value),
            (Cookie | Timings, _) => Some(Value),
            _ => None,
        }
    }

    fn item(self) -> Option<Shape> {
        match self {
            Shape::Pairs => Some(Shape::Pair),
            Shape::Cookies => Some(Shape::Cookie),
            _ => None,
        }
    }

    fn object(self) -> bool {
        !matches!(self, Shape::Pairs | Shape::Cookies | Shape::Value)
    }
}

/// A parsed detail value; big strings are [`Text::Long`].
#[derive(Clone, Debug, PartialEq)]
enum Node {
    Null,
    Bool(bool),
    /// Always finite.
    Number(f64),
    Text(Text),
    Array(Vec<Node>, usize),
    Object(Vec<(String, Node)>),
    /// Not built: outside the shape or beyond the budget.
    Skipped,
}

impl Node {
    /// Duplicate keys: the last occurrence wins, as in the index.
    fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Object(fields) => fields.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn text(&self, key: &str) -> String {
        match self.get(key) {
            Some(Node::Text(text)) => text.shown().to_owned(),
            Some(Node::Number(n)) => format_number(*n),
            Some(Node::Bool(b)) => b.to_string(),
            _ => String::new(),
        }
    }
    fn full_text(&self, key: &str) -> Text {
        match self.get(key) {
            Some(Node::Text(text)) => text.clone(),
            Some(Node::Number(n)) => Text::Inline(format_number(*n)),
            Some(Node::Bool(b)) => Text::Inline(b.to_string()),
            _ => Text::default(),
        }
    }
    fn number(&self, key: &str) -> Option<f64> {
        match self.get(key) {
            Some(Node::Number(n)) => Some(*n),
            _ => None,
        }
    }
    fn items(&self, key: &str) -> (&[Node], usize) {
        match self.get(key) {
            Some(Node::Array(items, total)) => (items, *total),
            _ => (&[], 0),
        }
    }
}

/// A string field kept in the file because it is longer than [`INLINE_TEXT`].
fn long_text(node: &Node, key: &str) -> Option<Text> {
    match node.get(key) {
        Some(Node::Text(long @ Text::Long { .. })) => Some(long.clone()),
        _ => None,
    }
}

fn format_number(n: f64) -> String {
    if n.fract() == 0. && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

impl Details {
    fn from_node(entry: Node) -> Self {
        let mut details = Details {
            started: entry.text("startedDateTime"),
            time: entry.number("time").and_then(duration),
            server: entry.text("serverIPAddress"),
            connection: entry.text("connection"),
            resource_type: entry.text("_resourceType"),
            error: entry.text("_error"),
            ..Default::default()
        };
        let null = Node::Null;
        let request = entry.get("request").unwrap_or(&null);
        details.method = request.text("method");
        details.url = request.full_text("url");
        details.request_version = request.text("httpVersion");
        details.request_headers = details.pairs(request, "headers", "Request headers");
        details.query = details.pairs(request, "queryString", "Query string");
        details.request_cookies = details.cookies(request, "Request cookies");
        if let Some(post) = request
            .get("postData")
            .filter(|n| matches!(n, Node::Object(_)))
        {
            let params = details.pairs(post, "params", "Form fields");
            details.request_body = Some(Body {
                mime: post.text("mimeType"),
                size: request.number("bodySize").and_then(size),
                encoding: post.text("encoding"),
                text: match post.get("text") {
                    Some(Node::Text(text)) => Some(text.clone()),
                    _ => None,
                },
                params,
                comment: post.text("comment"),
            });
        }
        let response = entry.get("response").unwrap_or(&null);
        details.status = response.number("status").and_then(whole);
        details.status_text = response.text("statusText");
        details.response_version = response.text("httpVersion");
        details.redirect = response.text("redirectURL");
        if details.error.is_empty() {
            details.error = response.text("_error");
        }
        details.response_headers = details.pairs(response, "headers", "Response headers");
        details.response_cookies = details.cookies(response, "Response cookies");
        if let Some(content) = response
            .get("content")
            .filter(|n| matches!(n, Node::Object(_)))
        {
            details.response_body = Some(Body {
                mime: content.text("mimeType"),
                size: content.number("size").and_then(size),
                encoding: content.text("encoding"),
                text: match content.get("text") {
                    Some(Node::Text(text)) => Some(text.clone()),
                    _ => None,
                },
                params: Vec::new(),
                comment: content.text("comment"),
            });
        }
        if let Some(Node::Object(timings)) = entry.get("timings") {
            for (index, (name, value)) in timings.iter().enumerate() {
                // A repeated phase: the last occurrence wins.
                if timings[index + 1..].iter().any(|(later, _)| later == name) {
                    continue;
                }
                if let Some(ms) = match value {
                    Node::Number(ms) if name != "comment" => duration(*ms),
                    _ => None,
                } {
                    details.timings.push((name.clone(), ms));
                }
            }
        }
        let null = Node::Null;
        let request = entry.get("request").unwrap_or(&null);
        let response = entry.get("response").unwrap_or(&null);
        let content = response.get("content").unwrap_or(&null);
        for (field, node, key) in [
            ("started", &entry, "startedDateTime"),
            ("server", &entry, "serverIPAddress"),
            ("connection", &entry, "connection"),
            ("resource_type", &entry, "_resourceType"),
            ("error", &entry, "_error"),
            ("method", request, "method"),
            ("request_version", request, "httpVersion"),
            ("status_text", response, "statusText"),
            ("response_version", response, "httpVersion"),
            ("redirect", response, "redirectURL"),
            ("response_error", response, "_error"),
            ("mime", content, "mimeType"),
        ] {
            if let Some(long) = long_text(node, key) {
                details.long.push((field, long));
            }
        }
        details
    }

    fn pairs(&mut self, node: &Node, key: &str, label: &'static str) -> Vec<Pair> {
        let (items, total) = node.items(key);
        if total > items.len() {
            self.cut_lists.push((label, total));
        }
        items
            .iter()
            .map(|item| Pair {
                name: item.text("name"),
                value: item.full_text("value"),
                name_long: long_text(item, "name"),
            })
            .collect()
    }

    fn cookies(&mut self, node: &Node, label: &'static str) -> Vec<Cookie> {
        let (items, total) = node.items("cookies");
        if total > items.len() {
            self.cut_lists.push((label, total));
        }
        items
            .iter()
            .map(|item| {
                let mut attributes = Vec::new();
                if let Node::Object(fields) = item {
                    for (key, value) in fields {
                        if key == "name" || key == "value" {
                            continue;
                        }
                        let value = match value {
                            Node::Text(text) => text.shown().to_owned(),
                            Node::Number(n) => format_number(*n),
                            Node::Bool(b) => b.to_string(),
                            _ => continue,
                        };
                        attributes.push(format!("{key}={value}"));
                    }
                }
                Cookie {
                    name: item.text("name"),
                    value: item.full_text("value"),
                    name_long: long_text(item, "name"),
                    attributes,
                }
            })
            .collect()
    }
}

/// A decoded body prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyBytes {
    pub bytes: Vec<u8>,
    /// Every byte of the body is in `bytes`.
    pub complete: bool,
    /// `encoding: base64` was declared but the text is not valid base64;
    /// `bytes` then holds the text as written.
    pub invalid_base64: bool,
}

/// Decodes at most `cap` bytes of a body, reading a [`Text::Long`] from `path`.
pub fn body_bytes(
    path: &Path,
    text: &Text,
    base64: bool,
    cap: usize,
    revision: Option<Revision>,
    cancel: &AtomicBool,
) -> Result<BodyBytes, DetailError> {
    // base64 expands 3 bytes to 4 characters; read whole quanta.
    let text_cap = if base64 { cap.div_ceil(3) * 4 } else { cap };
    let (raw, complete) = match text {
        Text::Inline(text) => {
            let end = text.len().min(text_cap);
            (text.as_bytes()[..end].to_vec(), end == text.len())
        }
        Text::Long { span, .. } => read_string(path, span.clone(), text_cap, revision, cancel)?,
    };
    if !base64 {
        return Ok(BodyBytes {
            bytes: raw,
            complete,
            invalid_base64: false,
        });
    }
    use base64::Engine as _;
    let mut clean: Vec<u8> = raw
        .into_iter()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    if !complete {
        clean.truncate(clean.len() / 4 * 4);
    }
    match base64::engine::general_purpose::STANDARD.decode(&clean) {
        Ok(mut bytes) => {
            let complete = complete && bytes.len() <= cap;
            bytes.truncate(cap);
            Ok(BodyBytes {
                bytes,
                complete,
                invalid_base64: false,
            })
        }
        Err(_) => Ok(BodyBytes {
            bytes: clean,
            complete,
            invalid_base64: true,
        }),
    }
}

/// Writes a whole body to `out`, decoding JSON escapes (and base64 when
/// `base64`) as it streams; memory stays bounded by the read buffer. When
/// `entry` is given (span, revision, hash) the request's bytes are verified
/// first. Returns the bytes written.
pub fn write_body(
    path: &Path,
    text: &Text,
    base64: bool,
    entry: Option<(&Range<u64>, Option<Revision>, u64)>,
    out: &mut dyn std::io::Write,
    cancel: &AtomicBool,
) -> Result<u64, DetailError> {
    let failed = |e: std::io::Error| DetailError::Failed(e.to_string());
    let revision = entry.and_then(|(_, revision, _)| revision);
    if let Some((span, revision, hash)) = entry {
        verify_entry(path, span, revision, hash, cancel)?;
    }
    let mut decoder = Base64Stream::default();
    let mut written = 0u64;
    let mut emit = |bytes: &[u8], written: &mut u64| -> std::io::Result<()> {
        if base64 {
            decoder.feed(bytes, &mut |decoded| {
                *written += decoded.len() as u64;
                out.write_all(decoded)
            })
        } else {
            *written += bytes.len() as u64;
            out.write_all(bytes)
        }
    };
    match text {
        Text::Inline(inline) => emit(inline.as_bytes(), &mut written).map_err(failed)?,
        Text::Long { span, .. } => {
            let reader = open_range(path, span, revision)?;
            let mut s = Scanner::new(reader, cancel, span.start);
            let mut io_error = None;
            let result = (|| -> Step {
                if s.next()? != b'"' {
                    return Err(s.error("Expected a string"));
                }
                s.string_to(&mut |bytes| {
                    emit(bytes, &mut written).map_err(|error| {
                        let kind = error.kind();
                        io_error = Some(error);
                        Stop::Io(std::io::Error::from(kind))
                    })
                })
            })();
            if let Some(error) = io_error {
                return Err(failed(error));
            }
            result.map_err(|stop| detail_error(stop, "body"))?;
        }
    }
    if base64 {
        decoder
            .finish(&mut |decoded| {
                written += decoded.len() as u64;
                out.write_all(decoded)
            })
            .map_err(|_| DetailError::Failed("The body is not valid base64.".into()))?;
    }
    out.flush().map_err(failed)?;
    Ok(written)
}

/// Streaming base64: whitespace is ignored and every complete 4-character
/// group is decoded as soon as it arrives, so at most 3 characters are kept.
/// Padding may only end the text: a padded group ends it, anything after it
/// (or a misplaced `=`) is an error at once.
#[derive(Default)]
struct Base64Stream {
    carry: Vec<u8>,
    ended: bool,
}

impl Base64Stream {
    fn invalid() -> std::io::Error {
        std::io::Error::from(std::io::ErrorKind::InvalidData)
    }

    fn feed(
        &mut self,
        bytes: &[u8],
        out: &mut dyn FnMut(&[u8]) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        use base64::Engine as _;
        let mut groups = std::mem::take(&mut self.carry);
        for &byte in bytes.iter().filter(|b| !b.is_ascii_whitespace()) {
            if self.ended {
                return Err(Self::invalid());
            }
            groups.push(byte);
            // A group is complete: check where padding may appear.
            if groups.len().is_multiple_of(4) {
                let group = &groups[groups.len() - 4..];
                if let Some(first) = group.iter().position(|&b| b == b'=') {
                    if first < 2 || group[first..].iter().any(|&b| b != b'=') {
                        return Err(Self::invalid());
                    }
                    self.ended = true;
                }
            }
        }
        let whole = groups.len() / 4 * 4;
        if whole > 0 {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(&groups[..whole])
                .map_err(|_| Self::invalid())?;
            out(&decoded)?;
        }
        groups.drain(..whole);
        self.carry = groups;
        Ok(())
    }

    fn finish(&mut self, out: &mut dyn FnMut(&[u8]) -> std::io::Result<()>) -> std::io::Result<()> {
        // Unpadded text must end on a whole group.
        if !self.carry.is_empty() {
            return Err(Self::invalid());
        }
        out(&[])
    }
}

/// Decodes the JSON string at `span` (quotes included) up to `cap` bytes.
fn read_string(
    path: &Path,
    span: Range<u64>,
    cap: usize,
    revision: Option<Revision>,
    cancel: &AtomicBool,
) -> Result<(Vec<u8>, bool), DetailError> {
    let reader = open_range(path, &span, revision)?;
    let mut s = Scanner::new(reader, cancel, span.start);
    let mut out = Vec::new();
    let result = (|| -> Step<bool> {
        if s.next()? != b'"' {
            return Err(s.error("Expected a string"));
        }
        s.string(&mut out, cap, true)
    })();
    result
        .map(|complete| (out, complete))
        .map_err(|stop| detail_error(stop, "body"))
}

// ---------------------------------------------------------------------------
// Scanner: a bounded-buffer JSON reader with absolute offsets.

struct Scanner<'c, R> {
    reader: R,
    buf: Box<[u8]>,
    pos: usize,
    len: usize,
    /// File offset of `buf[0]`.
    base: u64,
    cancel: &'c AtomicBool,
    /// Hashing consumed bytes from this buffer position (one entry).
    hash: Option<(std::hash::DefaultHasher, usize)>,
}

impl<'c, R: Read> Scanner<'c, R> {
    fn new(reader: R, cancel: &'c AtomicBool, base: u64) -> Self {
        Self {
            reader,
            buf: vec![0; BUFFER].into_boxed_slice(),
            pos: 0,
            len: 0,
            base,
            cancel,
            hash: None,
        }
    }

    /// Starts hashing the bytes from the current position.
    fn begin_hash(&mut self) {
        self.hash = Some((std::hash::DefaultHasher::new(), self.pos));
    }

    /// The hash of the bytes consumed since [`Self::begin_hash`].
    fn end_hash(&mut self) -> u64 {
        use std::hash::Hasher as _;
        match self.hash.take() {
            Some((mut hasher, from)) => {
                hasher.write(&self.buf[from..self.pos]);
                hasher.finish()
            }
            None => 0,
        }
    }

    fn offset(&self) -> u64 {
        self.base + self.pos as u64
    }

    fn error(&self, message: &'static str) -> Stop {
        Stop::Syntax(SyntaxError {
            offset: self.offset(),
            message,
        })
    }

    /// Ensures `pos < len`; false at the end of input.
    fn fill(&mut self) -> Step<bool> {
        if self.pos < self.len {
            return Ok(true);
        }
        if self.cancel.load(Ordering::Relaxed) {
            return Err(Stop::Cancelled);
        }
        if let Some((hasher, from)) = &mut self.hash {
            use std::hash::Hasher as _;
            hasher.write(&self.buf[*from..self.len]);
            *from = 0;
        }
        self.base += self.len as u64;
        self.pos = 0;
        self.len = 0;
        loop {
            match self.reader.read(&mut self.buf) {
                Ok(0) => return Ok(false),
                Ok(n) => {
                    self.len = n;
                    return Ok(true);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Stop::Io(e)),
            }
        }
    }

    fn skip_bom(&mut self) -> Step {
        if self.fill()? && self.buf[self.pos..self.len].starts_with(&[0xEF, 0xBB, 0xBF]) {
            self.pos += 3;
        }
        Ok(())
    }

    /// The next byte that is not whitespace, not consumed.
    fn peek(&mut self) -> Step<Option<u8>> {
        loop {
            if !self.fill()? {
                return Ok(None);
            }
            let byte = self.buf[self.pos];
            if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
                self.pos += 1;
            } else {
                return Ok(Some(byte));
            }
        }
    }

    /// The next byte that is not whitespace, consumed.
    fn next(&mut self) -> Step<u8> {
        match self.peek()? {
            Some(byte) => {
                self.pos += 1;
                Ok(byte)
            }
            None => Err(self.error("The file ends unexpectedly")),
        }
    }

    /// The next raw byte (inside a string), consumed.
    fn raw(&mut self) -> Step<u8> {
        if !self.fill()? {
            return Err(self.error("The file ends inside a string"));
        }
        let byte = self.buf[self.pos];
        self.pos += 1;
        Ok(byte)
    }

    fn expect(&mut self, byte: u8, message: &'static str) -> Step {
        if self.next()? == byte {
            Ok(())
        } else {
            self.pos = self.pos.saturating_sub(1);
            Err(self.error(message))
        }
    }

    /// Reads a string whose opening quote was consumed, appending at most
    /// `keep` decoded bytes to `out`. With `stop`, returns as soon as `keep`
    /// bytes are kept. Returns whether the whole string was kept.
    fn string(&mut self, out: &mut Vec<u8>, keep: usize, stop: bool) -> Step<bool> {
        let mut complete = true;
        loop {
            if !self.fill()? {
                return Err(self.error("The file ends inside a string"));
            }
            let chunk = &self.buf[self.pos..self.len];
            let found = memchr::memchr2(b'"', b'\\', chunk);
            let plain = found.unwrap_or(chunk.len());
            // JSON strings never hold raw control characters.
            if let Some(control) = chunk[..plain].iter().position(|&b| b < 0x20) {
                self.pos += control;
                return Err(self.error("Unescaped control character in a string"));
            }
            let room = keep.saturating_sub(out.len());
            if plain > room {
                complete = false;
                // Keep whole UTF-8 characters only.
                let mut cut = room;
                while cut > 0 && cut < chunk.len() && (chunk[cut] & 0xC0) == 0x80 {
                    cut -= 1;
                }
                out.extend_from_slice(&chunk[..cut]);
                if stop {
                    self.pos += cut;
                    return Ok(false);
                }
            } else {
                out.extend_from_slice(&chunk[..plain]);
            }
            self.pos += plain;
            if found.is_none() {
                continue;
            }
            if self.raw()? == b'"' {
                return Ok(complete);
            }
            let start = out.len();
            self.escape(out)?;
            if out.len() > keep {
                out.truncate(start);
                complete = false;
                if stop {
                    return Ok(false);
                }
            }
        }
    }

    /// Streams a string whose opening quote was consumed to `sink`, decoded.
    fn string_to(&mut self, sink: &mut dyn FnMut(&[u8]) -> Step) -> Step {
        let mut escaped = Vec::with_capacity(8);
        loop {
            if !self.fill()? {
                return Err(self.error("The file ends inside a string"));
            }
            let chunk = &self.buf[self.pos..self.len];
            let found = memchr::memchr2(b'"', b'\\', chunk);
            let plain = found.unwrap_or(chunk.len());
            if let Some(control) = chunk[..plain].iter().position(|&b| b < 0x20) {
                self.pos += control;
                return Err(self.error("Unescaped control character in a string"));
            }
            sink(&self.buf[self.pos..self.pos + plain])?;
            self.pos += plain;
            if found.is_none() {
                continue;
            }
            if self.raw()? == b'"' {
                return Ok(());
            }
            escaped.clear();
            self.escape(&mut escaped)?;
            sink(&escaped)?;
        }
    }

    /// Decodes one escape after its backslash.
    fn escape(&mut self, out: &mut Vec<u8>) -> Step {
        let byte = self.raw()?;
        let simple = match byte {
            b'"' => b'"',
            b'\\' => b'\\',
            b'/' => b'/',
            b'b' => 0x08,
            b'f' => 0x0C,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'u' => {
                let unit = self.hex4()?;
                let c = if (0xD800..0xDC00).contains(&unit) {
                    // A high surrogate needs a following low one.
                    if self.fill()? && self.buf[self.pos] == b'\\' {
                        self.pos += 1;
                        if self.fill()? && self.buf[self.pos] == b'u' {
                            self.pos += 1;
                            let low = self.hex4()?;
                            if (0xDC00..0xE000).contains(&low) {
                                char::from_u32(0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00))
                            } else {
                                push_char(out, char::REPLACEMENT_CHARACTER);
                                char::from_u32(low)
                            }
                        } else {
                            push_char(out, char::REPLACEMENT_CHARACTER);
                            return self.escape(out);
                        }
                    } else {
                        None
                    }
                } else {
                    char::from_u32(unit)
                };
                push_char(out, c.unwrap_or(char::REPLACEMENT_CHARACTER));
                return Ok(());
            }
            _ => return Err(self.error("Invalid escape in a string")),
        };
        out.push(simple);
        Ok(())
    }

    fn hex4(&mut self) -> Step<u32> {
        let mut value = 0;
        for _ in 0..4 {
            let digit = match self.raw()? {
                b @ b'0'..=b'9' => b - b'0',
                b @ b'a'..=b'f' => b - b'a' + 10,
                b @ b'A'..=b'F' => b - b'A' + 10,
                _ => return Err(self.error("Invalid \\u escape")),
            };
            value = value * 16 + digit as u32;
        }
        Ok(value)
    }

    /// A number or literal starting at the next byte, by JSON grammar. A
    /// number outside the f64 range is valid JSON but an unknown value.
    fn scalar(&mut self) -> Step<Node> {
        let start = self.offset();
        let mut text = Vec::new();
        while self.fill()? {
            let byte = self.buf[self.pos];
            if !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'+' | b'.')) {
                break;
            }
            if text.len() == 4096 {
                return Err(self.error("A number is too long"));
            }
            text.push(byte);
            self.pos += 1;
        }
        let error = |message| {
            Stop::Syntax(SyntaxError {
                offset: start,
                message,
            })
        };
        match text.as_slice() {
            b"true" => Ok(Node::Bool(true)),
            b"false" => Ok(Node::Bool(false)),
            b"null" => Ok(Node::Null),
            b"" => Err(error("Expected a value")),
            number if json_number(number) => Ok(std::str::from_utf8(number)
                .ok()
                .and_then(|t| t.parse::<f64>().ok())
                .filter(|n| n.is_finite())
                .map_or(Node::Null, Node::Number)),
            [b'-' | b'0'..=b'9', ..] => Err(error("Invalid number")),
            _ => Err(error("Expected a value")),
        }
    }

    /// Skips one value of any kind without keeping it.
    fn skip_value(&mut self) -> Step {
        let mut stack: Vec<u8> = Vec::new();
        let mut scratch = Vec::new();
        loop {
            // A value starts here.
            match self.next()? {
                b'{' => {
                    if self.peek()? == Some(b'}') {
                        self.pos += 1;
                    } else {
                        stack.push(b'{');
                        self.key(&mut scratch)?;
                        if stack.len() > MAX_DEPTH {
                            return Err(self.error("The JSON is nested too deeply"));
                        }
                        continue;
                    }
                }
                b'[' => {
                    if self.peek()? == Some(b']') {
                        self.pos += 1;
                    } else {
                        stack.push(b'[');
                        if stack.len() > MAX_DEPTH {
                            return Err(self.error("The JSON is nested too deeply"));
                        }
                        continue;
                    }
                }
                b'"' => {
                    scratch.clear();
                    self.string(&mut scratch, 0, false)?;
                }
                _ => {
                    self.pos -= 1;
                    self.scalar()?;
                }
            }
            // A value ended: close containers until a comma asks for more.
            loop {
                let Some(&top) = stack.last() else {
                    return Ok(());
                };
                match (top, self.next()?) {
                    (b'{', b',') => {
                        self.key(&mut scratch)?;
                        break;
                    }
                    (b'[', b',') => break,
                    (b'{', b'}') | (b'[', b']') => {
                        stack.pop();
                    }
                    _ => {
                        self.pos -= 1;
                        return Err(self.error("Expected a comma or a closing bracket"));
                    }
                }
            }
        }
    }

    /// Reads `"key":` into `key` (bounded; longer keys match nothing).
    fn key(&mut self, key: &mut Vec<u8>) -> Step {
        self.expect(b'"', "Expected a property name")?;
        key.clear();
        self.string(key, 256, false)?;
        self.expect(b':', "Expected a colon after a property name")
    }

    /// Visits the properties of an object whose `{` was consumed. `visit`
    /// must consume each value.
    fn object(&mut self, mut visit: impl FnMut(&mut Self, &[u8]) -> Step) -> Step {
        if self.peek()? == Some(b'}') {
            self.pos += 1;
            return Ok(());
        }
        let mut key = Vec::new();
        loop {
            self.key(&mut key)?;
            visit(self, &key)?;
            match self.next()? {
                b',' => continue,
                b'}' => return Ok(()),
                _ => {
                    self.pos -= 1;
                    return Err(self.error("Expected a comma or }"));
                }
            }
        }
    }

    /// Visits the items of an array whose `[` was consumed.
    fn array(&mut self, mut visit: impl FnMut(&mut Self) -> Step) -> Step {
        if self.peek()? == Some(b']') {
            self.pos += 1;
            return Ok(());
        }
        loop {
            visit(self)?;
            match self.next()? {
                b',' => continue,
                b']' => return Ok(()),
                _ => {
                    self.pos -= 1;
                    return Err(self.error("Expected a comma or ]"));
                }
            }
        }
    }

    /// Visits an object value, or skips a value of another type.
    fn with_object(&mut self, visit: impl FnMut(&mut Self, &[u8]) -> Step) -> Step {
        if self.peek()? == Some(b'{') {
            self.pos += 1;
            self.object(visit)
        } else {
            self.skip_value()
        }
    }

    /// A string value into `out`, at most `keep` bytes; other types are skipped.
    fn text_into(&mut self, out: &mut String, keep: usize) -> Step {
        self.text_into_full(out, keep).map(|_| ())
    }

    /// As [`Self::text_into`]; false when the value was cut.
    fn text_into_full(&mut self, out: &mut String, keep: usize) -> Step<bool> {
        if self.peek()? != Some(b'"') {
            // Last wins: a later non-string value replaces an earlier string.
            out.clear();
            self.skip_value()?;
            return Ok(true);
        }
        self.pos += 1;
        let mut bytes = Vec::new();
        let complete = self.string(&mut bytes, keep, false)?;
        *out = String::from_utf8(bytes)
            .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned());
        Ok(complete)
    }

    fn boxed(&mut self, out: &mut Box<str>, keep: usize) -> Step {
        let mut text = String::new();
        self.text_into(&mut text, keep)?;
        *out = text.into();
        Ok(())
    }

    /// A number value; other types are skipped.
    fn number(&mut self) -> Step<Option<f64>> {
        match self.peek()? {
            Some(b'-' | b'0'..=b'9') => match self.scalar()? {
                Node::Number(n) => Ok(Some(n)),
                _ => Ok(None),
            },
            _ => self.skip_value().map(|_| None),
        }
    }

    /// Builds the detail value of `shape` at the next byte within `budget`.
    /// Values outside the shape, and everything after the budget runs out,
    /// are skipped without being built. The shape bounds the recursion.
    fn node(&mut self, shape: Shape, budget: &mut Budget) -> Step<Node> {
        if budget.exhausted {
            self.skip_value()?;
            return Ok(Node::Skipped);
        }
        budget.nodes += 1;
        if budget.nodes >= MAX_DETAIL_NODES {
            budget.exhausted = true;
        }
        match self.peek()? {
            Some(b'{') if shape.object() => {
                self.pos += 1;
                let mut fields: Vec<(String, Node)> = Vec::new();
                self.object(|s, key| {
                    let Some(child) = shape.field(key) else {
                        return s.skip_value();
                    };
                    let key = String::from_utf8_lossy(key);
                    // Duplicate keys: last wins all the way, as in the list; a
                    // repeat replaces its field instead of taking a new one.
                    if let Some(slot) = fields.iter().position(|(k, _)| *k == key) {
                        fields[slot].1 = s.node(child, budget)?;
                    } else if fields.len() < MAX_ITEMS {
                        let value = s.node(child, budget)?;
                        fields.push((key.into_owned(), value));
                    } else {
                        // More distinct fields than details keep: say so.
                        budget.exhausted = true;
                        s.skip_value()?;
                    }
                    Ok(())
                })?;
                Ok(Node::Object(fields))
            }
            Some(b'[') if shape.item().is_some() => {
                self.pos += 1;
                let item = shape.item().unwrap_or(Shape::Value);
                let mut items = Vec::new();
                let mut total = 0usize;
                self.array(|s| {
                    total += 1;
                    if items.len() >= MAX_ITEMS {
                        return s.skip_value();
                    }
                    items.push(s.node(item, budget)?);
                    Ok(())
                })?;
                Ok(Node::Array(items, total))
            }
            Some(b'{' | b'[') => {
                self.skip_value()?;
                Ok(Node::Skipped)
            }
            Some(b'"') => {
                let start = self.offset();
                self.pos += 1;
                let keep = INLINE_TEXT.min(MAX_DETAIL_BYTES.saturating_sub(budget.bytes));
                let mut bytes = Vec::new();
                let complete = self.string(&mut bytes, keep, false)?;
                budget.bytes += bytes.len();
                let text = String::from_utf8(bytes)
                    .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned());
                Ok(Node::Text(if complete {
                    Text::Inline(text)
                } else {
                    if keep < INLINE_TEXT {
                        budget.exhausted = true;
                    }
                    Text::Long {
                        prefix: text,
                        span: start..self.offset(),
                    }
                }))
            }
            Some(_) => self.scalar(),
            None => Err(self.error("The file ends unexpectedly")),
        }
    }
}

fn push_char(out: &mut Vec<u8>, c: char) {
    let mut buf = [0; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

#[cfg(test)]
#[path = "har_tests.rs"]
mod tests;
