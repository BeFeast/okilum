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
use std::time::{Duration, Instant};

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
/// Containers nested deeper than this inside one entry are skipped, not built.
const MAX_DETAIL_DEPTH: usize = 48;
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
    /// Bytes on the wire: `_transferSize`, or headers + body sizes.
    pub transferred: Option<i64>,
    /// Total time in milliseconds (`entry.time`).
    pub time: Option<f64>,
    pub started: Box<str>,
    /// `startedDateTime` as Unix milliseconds.
    pub started_ms: Option<f64>,
    /// A browser's own failure text (`_error`, `response._error`).
    pub error: Box<str>,
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
}

#[derive(Debug, PartialEq, Eq)]
pub struct Cancelled;

/// Requests read since the previous batch, and how far the file was read.
pub struct Batch {
    pub entries: Vec<Entry>,
    pub offset: u64,
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
    let bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
    if bytes > MAX_FILE_BYTES {
        return Ok(Outcome {
            problem: Some(Problem::TooLarge(bytes)),
            ..Default::default()
        });
    }
    // A file that grows while it is read is indexed up to its size at open.
    index(file.take(MAX_FILE_BYTES), cancel, sink)
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
                            s.pos += 1;
                            let mut entry = Entry::default();
                            index_entry(s, &mut entry)?;
                            entry.span = start..s.offset();
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

fn index_entry<R: Read>(s: &mut Scanner<R>, entry: &mut Entry) -> Step {
    let mut started = String::new();
    s.object(|s, key| match key {
        b"startedDateTime" => s.text_into(&mut started, 64),
        b"time" => {
            entry.time = s.number()?;
            Ok(())
        }
        b"_resourceType" => s.boxed(&mut entry.resource_type, 32),
        b"_error" => s.boxed(&mut entry.error, 256),
        b"request" => s.with_object(|s, key| match key {
            b"method" => s.boxed(&mut entry.method, 32),
            b"url" => {
                let mut url = String::new();
                entry.url_cut = !s.text_into_full(&mut url, LIST_TEXT)?;
                entry.url = url.into();
                Ok(())
            }
            _ => s.skip_value(),
        }),
        b"response" => {
            let (mut headers, mut body) = (None, None);
            s.with_object(|s, key| match key {
                b"status" => {
                    entry.status = s.number()?.map(|n| n as i64);
                    Ok(())
                }
                b"headersSize" => {
                    headers = s.number()?;
                    Ok(())
                }
                b"bodySize" => {
                    body = s.number()?;
                    Ok(())
                }
                b"_transferSize" => {
                    entry.transferred = s.number()?.filter(|n| *n >= 0.).map(|n| n as i64);
                    Ok(())
                }
                b"_error" if entry.error.is_empty() => s.boxed(&mut entry.error, 256),
                b"content" => s.with_object(|s, key| match key {
                    b"size" => {
                        entry.size = s.number()?.filter(|n| *n >= 0.).map(|n| n as i64);
                        Ok(())
                    }
                    b"mimeType" => s.boxed(&mut entry.mime, 128),
                    _ => s.skip_value(),
                }),
                _ => s.skip_value(),
            })?;
            if entry.transferred.is_none() {
                let known = |n: Option<f64>| n.filter(|n| *n >= 0.).map(|n| n as i64);
                entry.transferred = match (known(headers), known(body)) {
                    (None, None) => None,
                    (h, b) => Some(h.unwrap_or(0) + b.unwrap_or(0)),
                };
            }
            Ok(())
        }
        _ => s.skip_value(),
    })?;
    entry.started_ms = parse_time(&started);
    entry.started = started.into();
    Ok(())
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
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cookie {
    pub name: String,
    pub value: Text,
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
}

/// Reads one entry's details from `span` (an [`Entry::span`]).
pub fn details(path: &Path, span: Range<u64>, cancel: &AtomicBool) -> Result<Details, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.kind().to_string())?;
    file.seek(SeekFrom::Start(span.start))
        .map_err(|e| e.kind().to_string())?;
    let length = span.end.saturating_sub(span.start);
    let mut s = Scanner::new(file.take(length), cancel, span.start);
    let node = match s.peek() {
        Ok(Some(b'{')) => s.node(0),
        Ok(_) => return Err("The request is not a JSON object.".into()),
        Err(stop) => Err(stop),
    };
    match node {
        Ok(node) => Ok(Details::from_node(node)),
        Err(Stop::Cancelled) => Err("Cancelled".into()),
        Err(Stop::Syntax(error)) => Err(format!("This request is damaged: {error}.")),
        Err(Stop::Io(error)) => Err(error.kind().to_string()),
        Err(Stop::Limit | Stop::NotHar(_)) => Err("This request could not be read.".into()),
    }
}

/// A parsed detail value; big strings are [`Text::Long`].
#[derive(Clone, Debug, PartialEq)]
enum Node {
    Null,
    Bool(bool),
    Number(f64),
    Text(Text),
    Array(Vec<Node>, usize),
    Object(Vec<(String, Node)>),
    Deep,
}

impl Node {
    fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
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
            time: entry.number("time"),
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
                size: request
                    .number("bodySize")
                    .filter(|n| *n >= 0.)
                    .map(|n| n as i64),
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
        details.status = response.number("status").map(|n| n as i64);
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
                size: content
                    .number("size")
                    .filter(|n| *n >= 0.)
                    .map(|n| n as i64),
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
            for (name, value) in timings.iter().take(MAX_ITEMS) {
                if let Node::Number(ms) = value {
                    if *ms >= 0. && name != "comment" {
                        details.timings.push((name.clone(), *ms));
                    }
                }
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
    cancel: &AtomicBool,
) -> Result<BodyBytes, String> {
    // base64 expands 3 bytes to 4 characters; read whole quanta.
    let text_cap = if base64 { cap.div_ceil(3) * 4 } else { cap };
    let (raw, complete) = match text {
        Text::Inline(text) => {
            let end = text.len().min(text_cap);
            (text.as_bytes()[..end].to_vec(), end == text.len())
        }
        Text::Long { span, .. } => read_string(path, span.clone(), text_cap, cancel)?,
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

/// Decodes the JSON string at `span` (quotes included) up to `cap` bytes.
fn read_string(
    path: &Path,
    span: Range<u64>,
    cap: usize,
    cancel: &AtomicBool,
) -> Result<(Vec<u8>, bool), String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.kind().to_string())?;
    file.seek(SeekFrom::Start(span.start))
        .map_err(|e| e.kind().to_string())?;
    let length = span.end.saturating_sub(span.start);
    let mut s = Scanner::new(file.take(length), cancel, span.start);
    let mut out = Vec::new();
    let result = (|| -> Step<bool> {
        if s.next()? != b'"' {
            return Err(s.error("Expected a string"));
        }
        s.string(&mut out, cap, true)
    })();
    match result {
        Ok(complete) => Ok((out, complete)),
        Err(Stop::Cancelled) => Err("Cancelled".into()),
        Err(Stop::Syntax(error)) => Err(format!("This body is damaged: {error}.")),
        Err(Stop::Io(error)) => Err(error.kind().to_string()),
        Err(_) => Err("This body could not be read.".into()),
    }
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

    /// A number or literal starting at the next byte.
    fn scalar(&mut self) -> Step<Node> {
        let start = self.offset();
        let mut text = [0u8; 64];
        let mut len = 0;
        while self.fill()? {
            let byte = self.buf[self.pos];
            if !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'+' | b'.')) {
                break;
            }
            if len == text.len() {
                return Err(self.error("A number is too long"));
            }
            text[len] = byte;
            len += 1;
            self.pos += 1;
        }
        let text = &text[..len];
        let error = |message| {
            Stop::Syntax(SyntaxError {
                offset: start,
                message,
            })
        };
        match text {
            b"true" => Ok(Node::Bool(true)),
            b"false" => Ok(Node::Bool(false)),
            b"null" => Ok(Node::Null),
            b"" => Err(error("Expected a value")),
            _ if matches!(text[0], b'-' | b'0'..=b'9') => std::str::from_utf8(text)
                .ok()
                .and_then(|t| t.parse::<f64>().ok())
                .map(Node::Number)
                .ok_or_else(|| error("Invalid number")),
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

    /// Builds a detail value at the next byte.
    fn node(&mut self, depth: usize) -> Step<Node> {
        if depth > MAX_DETAIL_DEPTH {
            self.skip_value()?;
            return Ok(Node::Deep);
        }
        match self.peek()? {
            Some(b'{') => {
                self.pos += 1;
                let mut fields = Vec::new();
                self.object(|s, key| {
                    if fields.len() >= MAX_ITEMS {
                        return s.skip_value();
                    }
                    let key = String::from_utf8_lossy(key).into_owned();
                    let value = s.node(depth + 1)?;
                    fields.push((key, value));
                    Ok(())
                })?;
                Ok(Node::Object(fields))
            }
            Some(b'[') => {
                self.pos += 1;
                let mut items = Vec::new();
                let mut total = 0usize;
                self.array(|s| {
                    total += 1;
                    if items.len() >= MAX_ITEMS {
                        return s.skip_value();
                    }
                    items.push(s.node(depth + 1)?);
                    Ok(())
                })?;
                Ok(Node::Array(items, total))
            }
            Some(b'"') => {
                let start = self.offset();
                self.pos += 1;
                let mut bytes = Vec::new();
                let complete = self.string(&mut bytes, INLINE_TEXT, false)?;
                let text = String::from_utf8(bytes)
                    .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned());
                Ok(Node::Text(if complete {
                    Text::Inline(text)
                } else {
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
