//! Edit drawing (#478): the Reader opens one drawing in an offline Excalidraw
//! page that Okilum itself serves on loopback, and the page hands edits back
//! through an explicit Apply. Displaying a drawing never starts anything here.
//!
//! One [`Bridge`] per drawing being edited:
//! - it owns the drawing's [`DrawingEdit`], so its editor lock, recovery draft
//!   and revision check are the ones notes use;
//! - it listens on `127.0.0.1` only, on a port the OS picks;
//! - every URL lives under a random 256-bit capability (`/<token>/…`); another
//!   path gets 404 and reveals nothing;
//! - the `Host` header must be the listener's own address (no DNS rebinding)
//!   and a change needs the page's exact `Origin` and a JSON body (no
//!   cross-site form posts);
//! - nothing is sent to a remote service or the clipboard; the page's
//!   Content-Security-Policy allows only its own origin.
//!
//! The session ends when the page says Done, after two idle hours, or when
//! Okilum quits. Closing the tab without Apply leaves the drawing as it was.
use crate::prelude::*;
use okilum_core::excalidraw::edit::{Applied, DrawingEdit, DrawingStore, Undone};
use okilum_core::excalidraw::MAX_SOURCE_BYTES;
use sha2::Digest as _;
use std::collections::{BTreeMap, HashMap};
use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The editor page: (served path, media type, stored gzip-compressed, bytes).
/// Built and checked by `web/excalidraw-editor/build.mjs`.
static ASSETS: &[(&str, &str, bool, &[u8])] =
    include!("../../okilum-shell/assets/excalidraw-editor/files.rs");

const HEAD_LIMIT: usize = 16 * 1024;
/// The page sends the whole scene; vault images it was shown come back too.
const BODY_LIMIT: usize = 2 * MAX_SOURCE_BYTES;
const IO_TIMEOUT: Duration = Duration::from_secs(15);
const IDLE_LIMIT: Duration = Duration::from_secs(2 * 60 * 60);
const CONNECTIONS: usize = 32;
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
font-src 'self' data:; img-src 'self' data: blob:; connect-src 'self' data: blob:; \
worker-src 'self' blob:; manifest-src 'none'; frame-src 'none'; object-src 'none'; \
base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// What the Reader hears from a session: the vault changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Saved,
    Restored,
    Copied(PathBuf),
}

pub struct Request {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

#[derive(Debug)]
pub struct Response {
    status: u16,
    mime: &'static str,
    gzip: bool,
    body: Vec<u8>,
}

impl Response {
    fn json(status: u16, value: serde_json::Value) -> Self {
        Self {
            status,
            mime: "application/json",
            gzip: false,
            body: serde_json::to_vec(&value).expect("JSON value"),
        }
    }
    fn error(status: u16, error: &str) -> Self {
        Self::json(status, serde_json::json!({ "error": error }))
    }
    fn not_found() -> Self {
        Self::error(404, "Not found")
    }
}

fn digest(text: &str) -> String {
    use std::fmt::Write as _;
    sha2::Sha256::digest(text.as_bytes())
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// One drawing's session behind the HTTP surface. No I/O of its own besides
/// the drawing store, so the routing and every guard are tested directly.
pub struct Session<S: DrawingStore> {
    drawing: DrawingEdit<S>,
    name: String,
    /// `127.0.0.1:<port>`, the only accepted `Host`.
    authority: String,
    origin: String,
    prefix: String,
    /// A page reported the scene it loaded at the current revision.
    loaded: bool,
    closed: bool,
    events: Vec<Event>,
}

impl<S: DrawingStore> Session<S> {
    pub fn new(drawing: DrawingEdit<S>, authority: String, token: &str) -> Self {
        let name = drawing
            .path()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = name
            .strip_suffix(".excalidraw.md")
            .or_else(|| name.strip_suffix(".excalidraw"))
            .unwrap_or(&name)
            .to_owned();
        Self {
            drawing,
            name,
            origin: format!("http://{authority}"),
            authority,
            prefix: format!("/{token}/"),
            loaded: false,
            closed: false,
            events: Vec::new(),
        }
    }

    pub fn closed(&self) -> bool {
        self.closed
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// The part of `path` under this session's capability, or `None`.
    fn route<'a>(&self, path: &'a str) -> Option<&'a str> {
        let rest = path.strip_prefix(&self.prefix)?;
        Some(rest.split(['?', '#']).next().unwrap_or(""))
    }

    pub fn handle(&mut self, request: &Request) -> Response {
        if request.headers.get("host") != Some(&self.authority) {
            return Response::error(421, "Wrong host");
        }
        let Some(route) = self.route(&request.path) else {
            return Response::not_found();
        };
        let Some(action) = route.strip_prefix("api/") else {
            if request.method != "GET" {
                return Response::error(405, "Method not allowed");
            }
            return asset(if route.is_empty() {
                "index.html"
            } else {
                route
            });
        };
        if self.closed {
            return Response::error(
                410,
                "This editing session has ended. Open the drawing from Okilum again.",
            );
        }
        if request.method == "GET" {
            return if action == "session" {
                self.scene_reply("session", self.loaded)
            } else {
                Response::not_found()
            };
        }
        if request.method != "POST" {
            return Response::error(405, "Method not allowed");
        }
        if request.headers.get("origin") != Some(&self.origin) {
            return Response::error(403, "Changes need the editor page's origin");
        }
        let json = request
            .headers
            .get("content-type")
            .and_then(|value| value.split(';').next())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"));
        if !json {
            return Response::error(415, "Changes need a JSON body");
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Call {
            revision: u64,
            scene: Option<serde_json::Value>,
            reviewed: Option<String>,
        }
        let call: Call = match serde_json::from_slice(&request.body) {
            Ok(call) => call,
            Err(_) => return Response::error(400, "Unreadable request"),
        };
        let result = match action {
            "loaded" => {
                let Some(scene) = call.scene else {
                    return Response::error(400, "The loaded scene is missing");
                };
                self.drawing.loaded(call.revision, scene).map(|()| {
                    self.loaded = true;
                    self.reply(200, "ready")
                })
            }
            "apply" => {
                let Some(scene) = call.scene else {
                    return Response::error(400, "The edited scene is missing");
                };
                self.drawing
                    .apply(call.revision, &scene)
                    .map(|applied| self.applied(applied))
            }
            "keep-mine" => self.keep_mine(call.revision, call.reviewed.as_deref()),
            "reload" => self.drawing.reload(call.revision).map(|_| {
                self.loaded = false;
                self.scene_reply("reloaded", false)
            }),
            "save-copy" => self.drawing.save_as_copy(call.revision).map(|copy| {
                self.events.push(Event::Copied(copy.clone()));
                let mut reply = self.reply_body("copied");
                reply["copy"] = copy.to_string_lossy().into_owned().into();
                Response::json(200, reply)
            }),
            "undo" => self.drawing.undo(call.revision).map(|undone| match undone {
                Undone::Restored { .. } => {
                    self.loaded = false;
                    self.events.push(Event::Restored);
                    self.scene_reply("restored", false)
                }
                Undone::Blocked { .. } => self.reply(409, "blocked"),
            }),
            // Done with an unresolved conflict would leave the edit as a
            // recovery draft that blocks the next Edit; choose first.
            "close" if self.drawing.conflict().is_some() => Ok(Response::error(
                422,
                "Choose Reload, Keep mine or Save as copy first.",
            )),
            "close" if call.revision == self.drawing.revision() => {
                self.closed = true;
                Ok(self.reply(200, "closed"))
            }
            "close" => Ok(Response::error(
                422,
                "The drawing changed since the editor loaded it. Load it again.",
            )),
            _ => return Response::not_found(),
        };
        result.unwrap_or_else(|error| Response::error(422, &format!("{error:#}")))
    }

    fn keep_mine(&mut self, revision: u64, reviewed: Option<&str>) -> anyhow::Result<Response> {
        // Consent covers the outside version the page was shown, named by its
        // digest; a newer outside change is reported, never overwritten.
        let disk = self.drawing.conflict().map(str::to_owned);
        match disk {
            Some(disk) if reviewed == Some(digest(&disk).as_str()) => self
                .drawing
                .keep_mine(revision, &disk)
                .map(|applied| self.applied(applied)),
            Some(_) => Ok(self.reply(409, "conflict")),
            None => anyhow::bail!("There is no conflict to resolve"),
        }
    }

    fn applied(&mut self, applied: Applied) -> Response {
        match applied {
            Applied::Saved { .. } => {
                self.events.push(Event::Saved);
                self.reply(200, "saved")
            }
            Applied::Unchanged { .. } => self.reply(200, "unchanged"),
            Applied::Conflict { .. } => self.reply(409, "conflict"),
        }
    }

    fn reply_body(&self, status: &str) -> serde_json::Value {
        serde_json::json!({
            "status": status,
            "revision": self.drawing.revision(),
            "canUndo": self.drawing.can_undo(),
            "conflict": self.drawing.conflict().map(digest),
            "name": self.name,
        })
    }

    fn reply(&self, code: u16, status: &str) -> Response {
        Response::json(code, self.reply_body(status))
    }

    fn scene_reply(&self, status: &str, loaded: bool) -> Response {
        match self.drawing.editor_scene() {
            Ok(scene) => {
                let mut body = self.reply_body(status);
                body["scene"] = scene;
                body["loaded"] = loaded.into();
                Response::json(200, body)
            }
            Err(error) => Response::error(422, &format!("{error:#}")),
        }
    }
}

fn asset(path: &str) -> Response {
    match ASSETS.iter().find(|(name, ..)| *name == path) {
        Some((_, mime, gzip, bytes)) => Response {
            status: 200,
            mime,
            gzip: *gzip,
            body: bytes.to_vec(),
        },
        None => Response::not_found(),
    }
}

/// A deliberately small HTTP/1.1 subset: one bounded request per connection,
/// `Content-Length` framing only, no repeated headers, no pipelining.
pub fn parse_head(head: &[u8]) -> anyhow::Result<(Request, usize)> {
    use anyhow::ensure;
    ensure!(head.len() <= HEAD_LIMIT, "Request headers are too large");
    let text = std::str::from_utf8(head)?;
    let mut lines = text.split("\r\n");
    let mut line = lines.next().unwrap_or("").split(' ');
    let (method, path) = (line.next().unwrap_or(""), line.next().unwrap_or(""));
    ensure!(
        matches!(method, "GET" | "POST")
            && path.starts_with('/')
            && !path.starts_with("//")
            && matches!(line.next(), Some("HTTP/1.1" | "HTTP/1.0"))
            && line.next().is_none(),
        "Invalid request line"
    );
    let mut headers = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("Invalid header"))?;
        ensure!(
            !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "Invalid header name"
        );
        ensure!(
            headers
                .insert(key.to_ascii_lowercase(), value.trim().to_owned())
                .is_none(),
            "Repeated header"
        );
    }
    ensure!(
        !headers.contains_key("transfer-encoding"),
        "Unsupported framing"
    );
    let length = match headers.get("content-length") {
        Some(value) => {
            ensure!(
                !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()),
                "Invalid content length"
            );
            value.parse::<usize>()?
        }
        None => 0,
    };
    ensure!(length <= BODY_LIMIT, "Request body is too large");
    ensure!(method == "POST" || length == 0, "GET with a body");
    let request = Request {
        method: method.to_owned(),
        path: path.to_owned(),
        headers,
        body: Vec::new(),
    };
    Ok((request, length))
}

fn read_request(stream: &mut TcpStream) -> anyhow::Result<Request> {
    let mut bytes = Vec::new();
    let split = loop {
        anyhow::ensure!(bytes.len() <= HEAD_LIMIT, "Request headers are too large");
        let mut chunk = [0; 2048];
        let count = stream.read(&mut chunk)?;
        anyhow::ensure!(count != 0, "Truncated request");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(at) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let (mut request, length) = parse_head(&bytes[..split])?;
    let mut body = bytes.split_off(split);
    anyhow::ensure!(body.len() <= length, "Pipelined requests are not supported");
    let have = body.len();
    body.resize(length, 0);
    stream.read_exact(&mut body[have..])?;
    request.body = body;
    Ok(request)
}

fn write_response(stream: &mut TcpStream, response: &Response) -> std::io::Result<()> {
    let reason = match response.status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        410 => "Gone",
        415 => "Unsupported Media Type",
        421 => "Misdirected Request",
        _ => "Error",
    };
    let encoding = if response.gzip {
        "Content-Encoding: gzip\r\n"
    } else {
        ""
    };
    let head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\n{encoding}Content-Length: {}\r\n\
Connection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\
Referrer-Policy: no-referrer\r\nCross-Origin-Resource-Policy: same-origin\r\n\
Cross-Origin-Opener-Policy: same-origin\r\nContent-Security-Policy: {CSP}\r\n\r\n",
        response.status,
        response.mime,
        response.body.len(),
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()
}

/// A running session: its listener thread, URL and event stream.
pub struct Bridge {
    pub url: String,
    pub events: async_channel::Receiver<Event>,
    address: SocketAddr,
    stop: Arc<AtomicBool>,
}

impl Bridge {
    pub fn start<S: DrawingStore + Send + 'static>(
        drawing: DrawingEdit<S>,
    ) -> anyhow::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let url = format!("http://{address}/{token}/");
        let session = Arc::new(Mutex::new(Session::new(
            drawing,
            address.to_string(),
            &token,
        )));
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, events) = async_channel::unbounded();
        let active = Arc::new(Mutex::new(Instant::now()));
        let open = Arc::new(AtomicUsize::new(0));
        {
            let (stop, active) = (stop.clone(), active.clone());
            std::thread::Builder::new()
                .name("drawing-edit-idle".into())
                .spawn(move || {
                    while !stop.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_secs(30));
                        let idle = active.lock().map(|at| at.elapsed()).unwrap_or_default();
                        if idle > IDLE_LIMIT {
                            wake(address, &stop);
                        }
                    }
                })?;
        }
        let accept_stop = stop.clone();
        std::thread::Builder::new()
            .name("drawing-edit".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if accept_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    if !stream.peer_addr().is_ok_and(|peer| peer.ip().is_loopback())
                        || open.load(Ordering::SeqCst) >= CONNECTIONS
                    {
                        continue;
                    }
                    if let Ok(mut at) = active.lock() {
                        *at = Instant::now();
                    }
                    open.fetch_add(1, Ordering::SeqCst);
                    let (session, sender, stop, slot) = (
                        session.clone(),
                        sender.clone(),
                        accept_stop.clone(),
                        open.clone(),
                    );
                    let spawned = std::thread::Builder::new()
                        .name("drawing-edit-request".into())
                        .spawn(move || {
                            serve(stream, &session, &sender, &stop, address);
                            slot.fetch_sub(1, Ordering::SeqCst);
                        });
                    if spawned.is_err() {
                        open.fetch_sub(1, Ordering::SeqCst);
                    }
                }
                // Dropping the last session reference releases the editor lock;
                // dropping the sender tells the Reader the session ended.
                accept_stop.store(true, Ordering::SeqCst);
            })?;
        Ok(Self {
            url,
            events,
            address,
            stop,
        })
    }

    pub fn running(&self) -> bool {
        !self.stop.load(Ordering::SeqCst)
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        wake(self.address, &self.stop);
    }
}

/// Stop the accept loop: set the flag, then connect once so `accept` returns.
fn wake(address: SocketAddr, stop: &AtomicBool) {
    if !stop.swap(true, Ordering::SeqCst) {
        let _ = TcpStream::connect_timeout(&address, Duration::from_secs(1));
    }
}

fn serve<S: DrawingStore>(
    mut stream: TcpStream,
    session: &Mutex<Session<S>>,
    events: &async_channel::Sender<Event>,
    stop: &AtomicBool,
    address: SocketAddr,
) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let response = match read_request(&mut stream) {
        Ok(_) if stop.load(Ordering::SeqCst) => {
            Response::error(410, "This editing session has ended.")
        }
        Ok(request) => {
            let Ok(mut session) = session.lock() else {
                return;
            };
            let response = session.handle(&request);
            for event in session.take_events() {
                let _ = events.try_send(event);
            }
            if session.closed() {
                drop(session);
                let _ = write_response(&mut stream, &response);
                wake(address, stop);
                return;
            }
            response
        }
        Err(_) => Response::error(400, "Invalid or incomplete request"),
    };
    let _ = write_response(&mut stream, &response);
}

/// Live sessions by canonical drawing path; one per drawing.
#[derive(Default)]
struct Sessions(HashMap<PathBuf, Bridge>);
impl Global for Sessions {}

pub fn open_edit_drawing(
    vault_root: PathBuf,
    session_directory: Option<PathBuf>,
    path: PathBuf,
    window: &mut Window,
    cx: &mut App,
) {
    let key = path.canonicalize().unwrap_or_else(|_| path.clone());
    if let Some(bridge) = cx.default_global::<Sessions>().0.get(&key) {
        if bridge.running() {
            let url = bridge.url.clone();
            cx.open_url(&url);
            reader_toast::transient("The drawing is open in your browser", window, cx);
            return;
        }
    }
    let Some(state) = session_directory else {
        reader_toast::transient(
            "Editing a drawing needs Okilum's recovery folder",
            window,
            cx,
        );
        return;
    };
    let root = vault_root;
    let handle = window.window_handle();
    let open = cx.background_executor().spawn({
        let (root, path) = (root.clone(), path.clone());
        async move {
            let drawing = DrawingEdit::open(&root, &path, &state.join("editor-drafts"))?;
            Bridge::start(drawing)
        }
    });
    cx.spawn(async move |cx| {
        let started: anyhow::Result<Bridge> = open.await;
        let _ = cx.update_window(handle, |_, window, cx| match started {
            Ok(bridge) => {
                let (url, events) = (bridge.url.clone(), bridge.events.clone());
                cx.default_global::<Sessions>()
                    .0
                    .insert(key.clone(), bridge);
                cx.open_url(&url);
                reader_toast::transient("The drawing opened in your browser", window, cx);
                cx.spawn(async move |cx| {
                    while let Ok(event) = events.recv().await {
                        let _ = cx.update_window(handle, |_, window, cx| {
                            reader_drawing::invalidate(&root, cx);
                            let message = match &event {
                                Event::Saved => "Drawing saved".to_owned(),
                                Event::Restored => "Drawing restored".to_owned(),
                                Event::Copied(copy) => {
                                    format!("Saved a copy: {}", copy.display())
                                }
                            };
                            reader_toast::transient(message, window, cx);
                        });
                    }
                    cx.update(|cx| {
                        let sessions = &mut cx.default_global::<Sessions>().0;
                        if sessions.get(&key).is_some_and(|bridge| bridge.url == url) {
                            sessions.remove(&key);
                        }
                    });
                })
                .detach();
            }
            Err(error) => {
                reader_toast::transient(format!("Cannot edit the drawing: {error:#}"), window, cx)
            }
        });
    })
    .detach();
}

#[cfg(test)]
#[path = "reader_drawing_edit/tests.rs"]
mod tests;
