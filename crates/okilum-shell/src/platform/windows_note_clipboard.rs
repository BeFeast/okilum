//! Lazy Windows smart paste. Native reads stay in deadline-bound helpers.
use crate::note_paste::{
    offer::{self, Pasteboard},
    router,
};
use gpui_component::input::clipboard::ClipboardReadError as Error;

pub(super) mod formats;

const MAX_BYTES: usize = offer::MAX_TEXT_BYTES;
/// Two text flavors plus one presence byte and one separator; no JSON expansion.
pub(super) const MAX_SNAPSHOT_BYTES: usize = MAX_BYTES * 2 + 64 * 1024;

#[derive(Clone, Default)]
pub(super) struct Snapshot {
    pub(super) plain: Option<String>,
    pub(super) html: Option<String>,
    pub(super) native: Option<NativeOffer>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct NativeOffer {
    pub sequence: u32,
    pub types: Vec<String>,
}

type ReadFlavor = std::sync::Arc<dyn Fn(&str) -> Result<Option<Vec<u8>>, Error> + Send + Sync>;
pub(super) const VERIFY: &str = "verify-sequence";
impl Snapshot {
    pub(super) fn encode(self) -> String {
        if let Some(native) = self.native {
            return format!(
                "N{}",
                serde_json::to_string(&native).expect("clipboard metadata")
            );
        }
        format!(
            "{}{}\0{}",
            if self.plain.is_some() { '1' } else { '0' },
            self.plain.unwrap_or_default(),
            self.html.unwrap_or_default()
        )
    }
    pub(super) fn decode(frame: &str) -> Result<Self, Error> {
        if frame.len() > MAX_SNAPSHOT_BYTES {
            return Err(Error::TooLarge);
        }
        if let Some(json) = frame.strip_prefix('N') {
            let native: NativeOffer = serde_json::from_str(json).map_err(|_| Error::Io)?;
            if native.sequence == 0 || native.types.len() > 32 {
                return Err(Error::Io);
            }
            return Ok(Self {
                native: Some(native),
                ..Default::default()
            });
        }
        let (plain, html) = frame
            .get(1..)
            .and_then(|s| s.split_once('\0'))
            .ok_or(Error::Io)?;
        if plain.len() > MAX_BYTES || html.len() > MAX_BYTES {
            return Err(Error::TooLarge);
        }
        if html.contains('\0') {
            return Err(Error::Io);
        }
        let plain = match frame.as_bytes().first() {
            Some(b'1') => Some(plain.to_owned()),
            Some(b'0') if plain.is_empty() => None,
            _ => return Err(Error::Io),
        };
        Ok(Self {
            plain,
            html: (!html.is_empty()).then(|| html.to_owned()),
            native: None,
        })
    }
}

/// CF_HTML offsets are byte offsets into the complete UTF-8 payload, not chars.
/// Invalid/absent HTML falls back to the same snapshot's declared plain flavor.
fn fragment(raw: &str) -> Option<(String, Option<String>)> {
    let field = |key: &str| {
        raw.lines()
            .take(32)
            .find_map(|line| line.strip_prefix(key).map(str::trim))
    };
    let start: usize = field("StartFragment:")?.parse().ok()?;
    let end: usize = field("EndFragment:")?.parse().ok()?;
    let html_start = field("StartHTML:")?.parse::<isize>().ok()?;
    let html_end = field("EndHTML:")?.parse::<isize>().ok()?;
    // CF_HTML permits -1 when no surrounding HTML context is supplied.
    if html_start < -1 || html_end < -1 {
        return None;
    }
    let html_start = if html_start == -1 {
        start
    } else {
        html_start as usize
    };
    let html_end = if html_end == -1 {
        raw.len()
    } else {
        html_end as usize
    };
    if !(html_start <= start && start <= end && end <= html_end && html_end <= raw.len()) {
        return None;
    }
    let header = raw.get(..html_start)?;
    // Offsets cannot point back into the CF_HTML header itself.
    if !header.contains("EndFragment:") {
        return None;
    }
    let html = raw.get(start..end)?;
    if html.contains('\0') {
        return None;
    }
    let base = header
        .lines()
        .find_map(|line| line.strip_prefix("SourceURL:"))
        .and_then(|s| url::Url::parse(s.trim()).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https" | "file"))
        .map(|u| u.to_string());
    Some((html.to_owned(), base))
}

struct Board {
    plain: Option<String>,
    html: Option<String>,
    base: Option<String>,
    native: Option<NativeOffer>,
    read: Option<ReadFlavor>,
    cache: std::cell::RefCell<std::collections::BTreeMap<String, Option<Vec<u8>>>>,
    error: std::cell::RefCell<Option<Error>>,
    failed_sequence: std::cell::Cell<i64>,
}
impl From<Snapshot> for Board {
    fn from(snapshot: Snapshot) -> Self {
        let (html, base) = snapshot
            .html
            .as_deref()
            .and_then(fragment)
            .map_or((None, None), |(html, base)| (Some(html), base));
        Self {
            plain: snapshot.plain,
            html,
            base,
            native: snapshot.native,
            read: None,
            cache: Default::default(),
            error: Default::default(),
            failed_sequence: Default::default(),
        }
    }
}
impl Board {
    fn native_data(&self, kind: &str) -> Option<Vec<u8>> {
        if self.error.borrow().is_some() {
            return None;
        }
        if let Some(data) = self.cache.borrow().get(kind) {
            return data.clone();
        }
        let read = self.read.as_ref()?;
        let data = match read(kind) {
            Ok(data) => data,
            Err(error) => {
                *self.error.borrow_mut() = Some(error);
                None
            }
        };
        self.cache
            .borrow_mut()
            .insert(kind.to_owned(), data.clone());
        data
    }
    fn native_html(&self) -> Option<(String, Option<String>)> {
        let raw = self.native_data(offer::HTML[0])?;
        if raw.len() > MAX_BYTES {
            return None;
        }
        fragment(std::str::from_utf8(&raw).ok()?)
    }
}
impl Pasteboard for Board {
    fn change_count(&self) -> i64 {
        let Some(native) = &self.native else {
            return 0;
        };
        if let Some(read) = &self.read {
            if let Err(error) = read(VERIFY) {
                *self.error.borrow_mut() = Some(error);
            }
            if self.error.borrow().is_some() {
                let sequence = self.failed_sequence.get() - 1;
                self.failed_sequence.set(sequence);
                return sequence;
            }
        }
        i64::from(native.sequence)
    }
    fn types(&self) -> Vec<String> {
        if let Some(native) = &self.native {
            return native.types.clone();
        }
        [
            (offer::UTF8_TEXT, self.plain.as_ref()),
            (offer::HTML[0], self.html.as_ref()),
            (offer::CHROMIUM_SOURCE_URL, self.base.as_ref()),
        ]
        .into_iter()
        .filter_map(|(name, value)| value.map(|_| name.to_owned()))
        .collect()
    }
    fn data(&self, kind: &str) -> Option<Vec<u8>> {
        if self.native.is_some() {
            if kind == offer::HTML[0] || kind == offer::CHROMIUM_SOURCE_URL {
                return self.string(kind).map(String::into_bytes);
            }
            return self.native_data(kind);
        }
        self.string(kind).map(String::into_bytes)
    }
    fn file_urls(&self) -> Vec<String> {
        self.native_data(offer::FILE_URL)
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }
    fn string(&self, kind: &str) -> Option<String> {
        if self.native.is_some() {
            return match kind {
                "public.html" => self.native_html().map(|(html, _)| html),
                offer::CHROMIUM_SOURCE_URL => self.native_html().and_then(|(_, base)| base),
                _ => self
                    .native_data(kind)
                    .and_then(|bytes| String::from_utf8(bytes).ok()),
            };
        }
        match kind {
            offer::UTF8_TEXT => self.plain.clone(),
            "public.html" => self.html.clone(),
            offer::CHROMIUM_SOURCE_URL => self.base.clone(),
            _ => None,
        }
    }
    fn rich_text(&self, _: &[u8], _: bool) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod board_tests;

fn route_snapshot(
    snapshot: Snapshot,
    context: &router::RouteContext,
) -> Result<Box<router::Routed>, Error> {
    route_snapshot_with(snapshot, context, None)
}

fn route_snapshot_with(
    snapshot: Snapshot,
    context: &router::RouteContext,
    read: Option<ReadFlavor>,
) -> Result<Box<router::Routed>, Error> {
    let mut board = Board::from(snapshot);
    board.read = read;
    let routed = router::route(&board, context);
    if let Some(error) = board.error.into_inner() {
        return Err(error);
    }
    routed
        .map(Box::new)
        .map_err(crate::note_paste::clipboard_error)
}

#[cfg(any(windows, test))]
mod provider {
    use super::*;
    use crate::note_paste::{self, PasteNotice, PasteOutbox};
    use gpui::{App, Task};
    use gpui_component::input::clipboard::{
        ClipboardReadRequest, ExactClipboardProvider, PasteContext,
    };
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex},
        time::Instant,
    };
    struct Job {
        context: PasteContext,
        options: note_paste::image_import::Options,
        ask_above: Option<u64>,
    }
    type ReadSnapshot =
        Arc<dyn Fn(&ClipboardReadRequest, Instant) -> Result<Snapshot, Error> + Send + Sync>;
    pub(crate) struct WindowsNoteClipboard {
        text: Arc<dyn ExactClipboardProvider>,
        read: ReadSnapshot,
        root: PathBuf,
        note: String,
        notice: PasteNotice,
        outbox: PasteOutbox,
        job: Mutex<Option<Job>>,
    }
    impl WindowsNoteClipboard {
        #[cfg(windows)]
        pub(crate) fn new(
            text: Arc<dyn ExactClipboardProvider>,
            root: PathBuf,
            note: String,
            notice: PasteNotice,
            outbox: PasteOutbox,
        ) -> Self {
            Self::with_reader(
                text,
                root,
                note,
                notice,
                outbox,
                Arc::new(super::super::exact_windows_clipboard::read_note),
            )
        }
        pub(super) fn with_reader(
            text: Arc<dyn ExactClipboardProvider>,
            root: PathBuf,
            note: String,
            notice: PasteNotice,
            outbox: PasteOutbox,
            read: ReadSnapshot,
        ) -> Self {
            Self {
                text,
                read,
                root,
                note,
                notice,
                outbox,
                job: Mutex::new(None),
            }
        }
    }
    impl ExactClipboardProvider for WindowsNoteClipboard {
        fn read_paste_now(
            &self,
            context: &PasteContext,
            cx: &App,
        ) -> Option<Result<Option<String>, Error>> {
            if let Ok(mut slot) = self.notice.lock() {
                *slot = None;
            }
            if let Ok(mut slot) = self.outbox.lock() {
                *slot = Default::default();
            }
            let Ok(mut job) = self.job.lock() else {
                return Some(Err(Error::Io));
            };
            *job = Some(Job {
                context: context.clone(),
                options: crate::reader_ui_state::image_import_options(cx),
                ask_above: note_paste::ask_above_bytes(cx),
            });
            // Retain the editor's asynchronous request/cancellation/selection guard.
            None
        }
        fn read_text(
            &self,
            request: ClipboardReadRequest,
            cx: &App,
        ) -> Task<Result<Option<String>, Error>> {
            let job = match self.job.lock() {
                Ok(mut job) => job.take(),
                Err(_) => return Task::ready(Err(Error::Io)),
            };
            let Some(job) = job else {
                let started = Instant::now();
                let task = self.text.read_text(request, cx);
                return cx.background_executor().spawn(async move {
                    let result = task.await;
                    diagnostic_error!("Paste: windows note route=PlainText types=[CF_UNICODETEXT] bytes={} ms={} ok={}",
                        result.as_ref().ok().and_then(|text| text.as_ref()).map_or(0, String::len),
                        started.elapsed().as_millis(), result.is_ok());
                    result
                });
            };
            let (root, note, notice, outbox) = (
                self.root.clone(),
                self.note.clone(),
                self.notice.clone(),
                self.outbox.clone(),
            );
            let read = self.read.clone();
            let deadline = Instant::now() + super::super::clipboard_process::TIMEOUT;
            cx.background_executor().spawn(async move {
                let started = Instant::now();
                let snapshot = read(&request, deadline).inspect_err(|error| {
                    diagnostic_error!("Paste: windows note route=ReadError types=[] bytes=0 ms={} error={error:?}",
                        started.elapsed().as_millis());
                })?;
                let types = snapshot.native.as_ref().map(|native| native.types.clone())
                    .unwrap_or_else(|| Board::from(snapshot.clone()).types());
                #[cfg(windows)]
                let flavor_reader: Option<ReadFlavor> = snapshot.native.as_ref().map(|native| {
                    let sequence = std::sync::atomic::AtomicU32::new(native.sequence);
                    let request = request.clone();
                    Arc::new(move |kind: &str| {
                        super::super::exact_windows_clipboard::read_note_flavor(
                            &request, deadline, &sequence, kind,
                        )
                    }) as ReadFlavor
                });
                #[cfg(not(windows))]
                let flavor_reader = None;
                if request.is_cancelled() {
                    diagnostic_error!("Paste: windows note route=Cancelled types={types:?} bytes=0 ms={}",
                        started.elapsed().as_millis());
                    return Err(Error::Cancelled);
                }
                let stamp = note_paste::attachments::stamp_now();
                let context = router::RouteContext {
                    image_options: job.options,
                    mode: note_paste::router_mode(job.context.mode),
                    source: &job.context.source,
                    selection: job.context.selection,
                    vault_root: &root,
                    note_rel: Some(&note),
                    stamp: &stamp,
                    ask_above: job.ask_above,
                };
                let mut routed = *route_snapshot_with(snapshot, &context, flavor_reader)
                    .inspect_err(|error| {
                        diagnostic_error!("Paste: windows note route=ReadError types={types:?} bytes=0 ms={} error={error:?}",
                            started.elapsed().as_millis());
                    })?;
                diagnostic_error!(
                    "Paste: windows note route={:?} bytes={} types={:?} ms={}",
                    routed.route,
                    routed.bytes,
                    routed.types,
                    started.elapsed().as_millis()
                );
                if request.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let failed = routed
                    .notice
                    .as_ref()
                    .is_some_and(|n| n.error && routed.insert.is_none());
                if failed {
                    if let (Ok(mut slot), Some(reason)) = (notice.lock(), routed.notice) {
                        *slot = Some(reason.text);
                    }
                    return Err(Error::Io);
                }
                // Deferred rich work is an accepted no-op now, then one guarded
                // background insertion. None would mean Unavailable to Input.
                if routed.insert.is_none() && routed.follow_up.is_some() {
                    routed.insert = Some(String::new());
                }
                if let Ok(mut slot) = outbox.lock() {
                    slot.request = Some(request.clone());
                    slot.notice = routed.notice;
                    slot.follow_up = routed.follow_up;
                    slot.saved_files = routed.saved_files;
                }
                Ok(routed.insert)
            })
        }
    }
}
#[cfg(any(windows, test))]
pub(crate) use provider::WindowsNoteClipboard;

#[cfg(test)]
mod tests;
