//! Note-only CF_HTML ingestion. No changes to ordinary Windows text fields.
use crate::note_paste::{
    offer::{self, Pasteboard},
    router,
};
use gpui_component::input::clipboard::ClipboardReadError as Error;

const MAX_BYTES: usize = offer::MAX_TEXT_BYTES;
/// Two text flavors plus one presence byte and one separator; no JSON expansion.
pub(super) const MAX_SNAPSHOT_BYTES: usize = MAX_BYTES * 2 + 2;

#[derive(Clone, Default)]
pub(super) struct Snapshot {
    pub(super) plain: Option<String>,
    pub(super) html: Option<String>,
}
impl Snapshot {
    pub(super) fn encode(self) -> String {
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
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .map(|u| u.to_string());
    Some((html.to_owned(), base))
}

struct Board {
    plain: Option<String>,
    html: Option<String>,
    base: Option<String>,
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
        }
    }
}
impl Pasteboard for Board {
    fn change_count(&self) -> i64 {
        0
    } // One immutable, owner/sequence-checked offer.
    fn types(&self) -> Vec<String> {
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
        self.string(kind).map(String::into_bytes)
    }
    fn string(&self, kind: &str) -> Option<String> {
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

enum Paste {
    Plain(Option<String>),
    Rich(Box<router::Routed>),
}

fn wants_rich(mode: router::Mode, source: &str, selection: usize) -> bool {
    mode == router::Mode::Smart && !crate::note_paste::context::in_fenced_code(source, selection)
}

fn route_snapshot(
    snapshot: Snapshot,
    context: &router::RouteContext,
) -> Result<Paste, offer::OfferError> {
    let board = Board::from(snapshot);
    // No valid CF_HTML: retain the exact declared text, including BOM/CRLF,
    // rather than enabling unrelated TSV/URL smart-paste routes on Windows.
    if board.html.is_none() || !wants_rich(context.mode, context.source, context.selection.start) {
        return Ok(Paste::Plain(board.plain));
    }
    router::route(&board, context).map(|routed| Paste::Rich(Box::new(routed)))
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
            *job = wants_rich(
                note_paste::router_mode(context.mode),
                &context.source,
                context.selection.start,
            )
            .then(|| Job {
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
                return self.text.read_text(request, cx);
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
                let snapshot = read(&request, deadline)?;
                if request.is_cancelled() {
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
                let routed = match route_snapshot(snapshot, &context)
                    .map_err(note_paste::clipboard_error)?
                {
                    Paste::Plain(text) => {
                        return if request.is_cancelled() {
                            Err(Error::Cancelled)
                        } else {
                            Ok(text)
                        };
                    }
                    Paste::Rich(routed) => *routed,
                };
                if request.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                eprintln!(
                    "Paste: windows note route={:?} bytes={} types={:?}",
                    routed.route, routed.bytes, routed.types
                );
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
                if let Ok(mut slot) = outbox.lock() {
                    slot.apply_only = true;
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
