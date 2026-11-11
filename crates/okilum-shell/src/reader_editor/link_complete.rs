//! Shared inline completion in the note editor (#1921, #1638). Notes, headings,
//! emoji, fence languages and slash blocks reuse one popup and ranker.
//! Typing `[[` and the start of
//! a name opens a list of matching notes under the caret; Up and Down move
//! through it, Enter or Tab writes the link, Escape closes it for this link,
//! and a click chooses a row. Typing on without choosing keeps the text as
//! typed: a link to a note that does not exist yet stays possible.
//!
//! The written target is the shortest name that resolves to the chosen note
//! and no other (`[[Plan]]`, or `[[Work/Plan]]` when two notes are called
//! Plan): Okilum never writes a link it would itself report as ambiguous. An
//! alias or heading already typed after the name (`|…`, `#…`) is kept; a
//! closing `]]` is added only when the link has none. The completion is one
//! source edit and one Undo step. Nothing opens in a fenced code block, the
//! front matter or an inline code span, while an input method composes, or
//! in a code or text file.
use super::ime_guard::Trigger;
#[path = "inline_complete.rs"]
mod providers;
use super::lists::{self, Edit};
use super::*;
use gpui_component::input::projection::SourceStamp;
use gpui_component::input::{Enter, Escape, IndentInline, MoveDown, MoveUp, RopeExt as _};
use okilum_core::vault::warm::SourceRevision;
use providers::Kind;
use std::ops::Range;

/// Rows the list shows at most.
pub(super) const MAX_ROWS: usize = 8;
/// A longer name is no longer being typed as a link.
#[cfg(test)]
const MAX_QUERY_CHARS: usize = 120;
/// Characters that end a wikilink name.
const NAME_END: [char; 5] = ['[', ']', '|', '#', '^'];

/// An open `[[` before the caret on the caret's line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LinkQuery {
    /// Just after the `[[`.
    pub start: usize,
    /// The caret.
    pub caret: usize,
    /// The name typed so far.
    pub query: String,
    pub kind: Kind,
}

/// The link being typed at `caret`, if any.
pub(super) fn link_query(text: &str, caret: usize) -> Option<LinkQuery> {
    if caret > text.len() || !text.is_char_boundary(caret) {
        return None;
    }
    let line_start = text[..caret].rfind('\n').map_or(0, |at| at + 1);
    let before = &text[line_start..caret];
    let query = line_query(before, line_start, caret)?;
    if matches!(query.kind, Kind::Note | Kind::Heading(_))
        && text[..query.start - 2]
            .chars()
            .rev()
            .take_while(|c| *c == '\\')
            .count()
            % 2
            == 1
    {
        return None;
    }
    if before.starts_with("    ") || before.starts_with('\t') {
        return None;
    }
    if lists::in_code(text, line_start) {
        return None;
    }
    if !matches!(query.kind, Kind::Language) {
        // Inline code may cross a soft line break, but not a blank paragraph.
        let delimiter = if matches!(query.kind, Kind::Note | Kind::Heading(_)) {
            query.start - 2
        } else {
            query.start
        };
        let mut paragraph_start = line_start;
        for raw in text[..line_start].split_inclusive('\n').rev() {
            let body = raw.trim();
            // Blank lines end inline spans. Fence delimiters are block syntax,
            // including closing runs longer than their opening run.
            if body.is_empty() || body.starts_with("```") || body.starts_with("~~~") {
                break;
            }
            paragraph_start -= raw.len();
        }
        let paragraph = &text[paragraph_start..delimiter];
        if providers::in_inline_code(paragraph) {
            return None;
        }
    }
    Some(query)
}

/// The UI only copies the bounded tail of the caret's line. Full Markdown
/// context is validated on the worker before any rows can be published.
fn line_query(before: &str, line_start: usize, caret: usize) -> Option<LinkQuery> {
    let (kind, offset, query) = providers::trigger(before, true)?;
    let start = line_start + offset;
    Some(LinkQuery {
        start,
        caret,
        query,
        kind,
    })
}

/// A note the list offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Candidate {
    /// Vault-relative path, with `.md`.
    pub path: String,
    /// The note's name, as the sidebar shows it.
    pub title: String,
    /// Its folder, for telling namesakes apart; empty at the vault root.
    pub folder: String,
    /// What goes between `[[` and `]]`.
    pub target: String,
    pub badge: Option<String>,
}

/// The vault's notes, lowercased once for matching as the user types.
pub(super) struct Index {
    vault: Arc<Vault>,
    show_hidden: bool,
    resolution: (u64, u64),
    /// (lowercase title, lowercase path without `.md`, title length, vault index)
    notes: std::sync::OnceLock<Vec<(String, String, usize, usize)>>,
}

impl Index {
    pub(super) fn new(vault: Arc<Vault>, show_hidden: bool) -> Self {
        Self {
            resolution: vault.resolution_identity(),
            vault,
            show_hidden,
            notes: Default::default(),
        }
    }

    fn current(&self, vault: &Arc<Vault>, show_hidden: bool) -> bool {
        Arc::ptr_eq(&self.vault, vault)
            && self.resolution == vault.resolution_identity()
            && self.show_hidden == show_hidden
    }

    /// The best matches for `query`, written from the note at `from`.
    pub(super) fn search(&self, query: &str, from: &str) -> Vec<Candidate> {
        let query = query.trim().to_lowercase();
        let from_folder = folder_of(from);
        let matcher = providers::Matcher::new(&query);
        let notes = self.notes.get_or_init(|| {
            self.vault
                .notes
                .iter()
                .enumerate()
                .filter(|(_, note)| self.show_hidden || !reader_tree::hidden(&note.path))
                .filter(|(_, note)| !note.path.contains(NAME_END))
                .map(|(at, note)| {
                    let title = note.title.to_lowercase();
                    let length = title.chars().count();
                    (title, strip_md(&note.path).to_lowercase(), length, at)
                })
                .collect()
        });
        let mut ranked: Vec<(u8, usize, &str, usize)> = notes
            .iter()
            .filter(|(_, _, _, at)| self.vault.notes[*at].path != from)
            .filter_map(|(title, path, length, at)| {
                let score = if query.is_empty() {
                    // Nothing typed yet: the note's neighbours first.
                    u8::from(folder_of(&self.vault.notes[*at].path) != from_folder)
                } else {
                    matcher.score(title, path)?
                };
                Some((score, *length, path.as_str(), *at))
            })
            .collect();
        ranked.sort_unstable();
        ranked
            .into_iter()
            .map(|(_, _, _, at)| {
                let note = &self.vault.notes[at];
                Candidate {
                    path: note.path.clone(),
                    title: note.title.clone(),
                    folder: folder_of(&note.path).to_owned(),
                    target: target_for(&self.vault, &note.path, from),
                    badge: None,
                }
            })
            .collect()
    }
}

fn strip_md(path: &str) -> &str {
    path.strip_suffix(".md")
        .or_else(|| path.strip_suffix(".MD"))
        .or_else(|| path.strip_suffix(".Md"))
        .or_else(|| path.strip_suffix(".mD"))
        .unwrap_or(path)
}

fn folder_of(path: &str) -> &str {
    path.rfind('/').map_or("", |at| &path[..at])
}

/// The shortest trailing part of the note's path that resolves to it alone
/// from `from`; the whole path when none does.
pub(super) fn target_for(vault: &Vault, path: &str, from: &str) -> String {
    let name = strip_md(path);
    let mut at = name.len();
    loop {
        let start = name[..at].rfind('/').map_or(0, |slash| slash + 1);
        let suffix = &name[start..];
        if matches!(
            vault.resolve_from(suffix, from),
            okilum_core::vault::Resolution::Resolved { path: found } if found == path
        ) {
            return suffix.to_owned();
        }
        if start == 0 {
            return name.to_owned();
        }
        at = start - 1;
    }
}

/// The edit that writes `target` for the link at `query`, and the caret
/// after it. The name around the caret is replaced up to the link's `]]`,
/// `|` or `#`; with none on the line, `]]` closes the link.
pub(super) fn completion(text: &str, query: &LinkQuery, target: &str) -> (Vec<Edit>, usize) {
    let (range, replacement, caret) =
        providers::replacement(text, query.start, query.caret, &query.kind, target);
    (vec![(range, replacement)], caret)
}

/// Reuse the same rows, key handling and acceptance path for every provider.
/// A source edit can temporarily empty an async provider's rows. Remember the
/// user's choice by identity across that gap, never by its old numeric position.
fn publish(state: &mut State, query: LinkQuery, items: Vec<Candidate>) {
    let preferred = state
        .open
        .as_ref()
        .filter(|open| open.query.kind == query.kind && open.query.start == query.start)
        .and_then(|open| open.items.get(open.selected))
        .map(|item| {
            (
                query.kind.clone(),
                query.start,
                item.path.clone(),
                item.target.clone(),
            )
        })
        .or_else(|| state.preferred.clone())
        .filter(|(kind, start, _, _)| *kind == query.kind && *start == query.start);
    let selected = preferred
        .as_ref()
        .and_then(|(_, _, path, target)| {
            items
                .iter()
                .position(|item| item.path == *path && item.target == *target)
        })
        .unwrap_or(0);
    state.preferred = items
        .get(selected)
        .map(|item| {
            (
                query.kind.clone(),
                query.start,
                item.path.clone(),
                item.target.clone(),
            )
        })
        .or(preferred);
    state.open = (!items.is_empty()).then_some(Open {
        query,
        items,
        selected,
    });
}

/// A click belongs to the row that was painted, including its source revision.
/// A source edit or asynchronous reorder must never retarget an old click.
struct RowIntent {
    query: LinkQuery,
    source: SourceStamp,
    path: String,
    target: String,
}

impl RowIntent {
    fn choice(&self, open: &Open, source: SourceStamp) -> Option<usize> {
        if self.source != source || self.query != open.query {
            return None;
        }
        open.items
            .iter()
            .position(|item| item.path == self.path && item.target == self.target)
    }
}

fn row(title: &str, target: &str, folder: &str) -> Candidate {
    Candidate {
        path: String::new(),
        title: title.into(),
        folder: folder.into(),
        target: target.into(),
        badge: None,
    }
}

fn catalog(query: &LinkQuery) -> Vec<Candidate> {
    let items = match query.kind {
        Kind::Language => providers::LANGUAGES
            .iter()
            .map(|name| row(name, name, "Language"))
            .chain(
                okilum_core::code_language::ALIASES
                    .iter()
                    .map(|(alias, name)| row(alias, alias, name)),
            )
            .collect(),
        Kind::Emoji => providers::EMOJI
            .iter()
            .map(|(name, emoji)| row(name, emoji, emoji))
            .collect(),
        Kind::Block => {
            let mut rows: Vec<_> = providers::BLOCKS
                .iter()
                .map(|(name, source)| row(name, source, "Markdown"))
                .collect();
            rows.push(row(
                "date",
                &chrono::Local::now().format("%Y-%m-%d").to_string(),
                "Today",
            ));
            rows
        }
        _ => Vec::new(),
    };
    rank_rows(&query.query, items)
}

fn rank_rows(query: &str, items: Vec<Candidate>) -> Vec<Candidate> {
    let query = query.to_lowercase();
    let matcher = providers::Matcher::new(&query);
    let mut ranked: Vec<_> = items
        .into_iter()
        .filter_map(|item| {
            let title = item.title.to_lowercase();
            let score = matcher.score(&title, &title)?;
            Some((score, item))
        })
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.title.cmp(&b.1.title)));
    ranked.into_iter().map(|(_, row)| row).collect()
}

/// A named heading's immutable source identity. Validation runs off the UI
/// thread and checks exact bounded bytes as well as the native file revision.
#[derive(Clone)]
struct HeadingSource {
    root: PathBuf,
    note: String,
    path: PathBuf,
    revision: SourceRevision,
    digest: [u8; 32],
}

impl HeadingSource {
    const MAX_BYTES: u64 = 8 * 1024 * 1024;

    fn read(root: &std::path::Path, note: &str) -> Option<(Self, String)> {
        use sha2::{Digest, Sha256};
        use std::io::Read as _;
        if okilum_core::vault::service_path(std::path::Path::new(note)) {
            return None;
        }
        let path = reader_files::checked_path(root, note).ok()?;
        let metadata = std::fs::symlink_metadata(&path).ok()?;
        if !metadata.is_file() || metadata.len() > Self::MAX_BYTES {
            return None;
        }
        #[cfg(target_os = "macos")]
        {
            use std::os::macos::fs::MetadataExt as _;
            if metadata.st_flags() & 0x4000_0000 != 0 {
                return None;
            }
        }
        let revision = SourceRevision::read(&path).ok()?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            options.custom_flags(
                windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
            );
        }
        let file = options.open(&path).ok()?;
        if !file.metadata().ok()?.is_file() {
            return None;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
            if file.metadata().ok()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return None;
            }
        }
        let mut bytes = Vec::new();
        file.take(Self::MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > Self::MAX_BYTES
            || SourceRevision::read(&path).ok()? != revision
            || reader_files::checked_path(root, note).ok()? != path
        {
            return None;
        }
        let digest = Sha256::digest(&bytes).into();
        let text = String::from_utf8(bytes).ok()?;
        Some((
            Self {
                root: root.into(),
                note: note.into(),
                path,
                revision,
                digest,
            },
            text,
        ))
    }

    fn current(&self) -> bool {
        Self::read(&self.root, &self.note).is_some_and(|(now, _)| {
            now.path == self.path && now.revision == self.revision && now.digest == self.digest
        })
    }
}

#[derive(Default)]
struct Enriched {
    rows: Vec<Candidate>,
    heading: Option<HeadingSource>,
}

/// Reads/parsing remain in the delivered provider's worker. Current-note
/// headings use the editor's unsaved source, never the disk copy. Named rows
/// bind the resolved note path and exact target source; one changed read is
/// retried, never published as if it belonged to a newer revision.
fn enrich_bound(
    vault: &Vault,
    from: &str,
    source: Option<&str>,
    query: &LinkQuery,
    items: Vec<Candidate>,
) -> Enriched {
    use okilum_core::{document_links::HeadingInventory, vault::Resolution};
    let Kind::Heading(target) = &query.kind else {
        return Enriched {
            rows: items,
            heading: None,
        };
    };
    let resolved;
    let path = if target.is_empty() {
        from
    } else {
        resolved = vault.resolve_from(target, from);
        match &resolved {
            Resolution::Resolved { path } => path.as_str(),
            _ => return Enriched::default(),
        }
    };
    for _ in 0..2 {
        let loaded;
        let (text, proof) = if path == from {
            (source.unwrap_or(""), None)
        } else {
            let Some((proof, text)) = HeadingSource::read(&vault.root, path) else {
                return Enriched::default();
            };
            loaded = text;
            (loaded.as_str(), Some(proof))
        };
        let inventory = HeadingInventory::new(text);
        let rows = inventory
            .entries
            .iter()
            .filter(|h| !h.text.contains(NAME_END) && inventory.locate(&h.text).is_ok())
            .map(|h| {
                let mut candidate = row(&h.text, &format!("{target}#{}", h.text), path);
                // Painted heading rows carry the resolved note identity too.
                candidate.path = path.into();
                candidate
            })
            .collect();
        if proof.as_ref().is_none_or(HeadingSource::current) {
            return Enriched {
                rows: rank_rows(&query.query, rows),
                heading: proof,
            };
        }
    }
    Enriched::default()
}

#[cfg(test)]
fn enrich(
    vault: &Vault,
    from: &str,
    source: Option<&str>,
    query: &LinkQuery,
    items: Vec<Candidate>,
) -> Vec<Candidate> {
    enrich_bound(vault, from, source, query, items).rows
}

#[cfg(test)]
type HeadingHold = (
    futures::channel::oneshot::Sender<()>,
    futures::channel::oneshot::Receiver<()>,
);
#[cfg(test)]
thread_local! {
    static NOTE_READ_HOLD: std::cell::RefCell<Option<HeadingHold>> = const { std::cell::RefCell::new(None) };
    static HEADING_READ_HOLD: std::cell::RefCell<Option<HeadingHold>> = const { std::cell::RefCell::new(None) };
    static HEADING_ACCEPT_HOLD: std::cell::RefCell<Option<HeadingHold>> = const { std::cell::RefCell::new(None) };
}

/// Disposable presentation metadata. Native identity/change time catches
/// replacements and restored-mtime rewrites; a new vault snapshot also clears
/// the cache (watcher invalidation, including coarse/network filesystems).
#[derive(Clone, Debug, PartialEq, Eq)]
struct MetadataRevision {
    len: u64,
    modified: std::time::SystemTime,
    created: Option<std::time::SystemTime>,
    #[cfg(unix)]
    native: (u64, u64, i64, i64),
}

impl MetadataRevision {
    fn read(path: &std::path::Path) -> std::io::Result<Self> {
        let meta = std::fs::symlink_metadata(path)?;
        if !meta.is_file() {
            return Err(std::io::Error::other("Not a regular note"));
        }
        Ok(Self {
            len: meta.len(),
            modified: meta.modified()?,
            created: meta.created().ok(),
            #[cfg(unix)]
            native: {
                use std::os::unix::fs::MetadataExt;
                (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec())
            },
        })
    }
}

/// One visible page, bound to the exact editor context. It is also the
/// coalescing key: navigation during a read queues only the latest page.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MetadataPage {
    seen: (SourceStamp, Range<usize>, bool),
    query: LinkQuery,
    root: std::path::PathBuf,
    from: String,
    show_hidden: bool,
    rows: Vec<(String, String)>,
}

impl MetadataPage {
    fn rows(open: &Open) -> Vec<(String, String)> {
        let visible = providers::visible_rows(open.items.len(), open.selected, MAX_ROWS);
        open.items[visible]
            .iter()
            .map(|item| (item.path.clone(), item.target.clone()))
            .collect()
    }
}

struct BadgeUpdate {
    path: String,
    target: String,
    badge: Option<String>,
}

/// Update only metadata, never rank/order/selection or canonical note source.
fn apply_metadata(
    open: &mut Open,
    page: &MetadataPage,
    current: &(SourceStamp, Range<usize>, bool),
    updates: Vec<BadgeUpdate>,
) -> bool {
    if current != &page.seen || open.query != page.query || MetadataPage::rows(open) != page.rows {
        return false;
    }
    let visible = providers::visible_rows(open.items.len(), open.selected, MAX_ROWS);
    for update in updates {
        if let Some(item) = open.items[visible.clone()]
            .iter_mut()
            .find(|item| item.path == update.path && item.target == update.target)
        {
            item.badge = update.badge;
        }
    }
    true
}

const MAX_METADATA_BYTES: u64 = 64 * 1024;

/// A derived badge must not hydrate an evicted file. This bounded prefix reader
/// uses the same macOS placeholder flag as Vault's ordinary source reader.
fn metadata_prefix(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read as _;
    let metadata = std::fs::symlink_metadata(path)?;
    #[allow(unused_mut)]
    let mut unavailable = !metadata.is_file()
        || path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("icloud"));
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt as _;
        unavailable |= metadata.st_flags() & 0x4000_0000 != 0;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        // OFFLINE, RECALL_ON_OPEN and RECALL_ON_DATA_ACCESS.
        unavailable |= metadata.file_attributes() & (0x1000 | 0x40000 | 0x400000) != 0;
    }
    if unavailable {
        return Err(std::io::Error::other(
            "Completion metadata source is unavailable",
        ));
    }
    #[cfg(test)]
    METADATA_READS.with(|reads| reads.set(reads.get() + 1));
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_METADATA_BYTES)
        .read_to_end(&mut bytes)?;
    // A prefix can end mid-codepoint after a complete frontmatter block.
    let end = std::str::from_utf8(&bytes).map_or_else(|error| error.valid_up_to(), |_| bytes.len());
    Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
}

fn type_badge(text: &str) -> Option<String> {
    use okilum_core::properties::{self, PropertyValue};
    properties::frontmatter_block(text)
        .and_then(|yaml| properties::parse(yaml).ok())
        .and_then(|props| props.into_iter().find(|p| p.key == "type"))
        .and_then(|p| match p.value {
            PropertyValue::Text(value) if value.len() <= 256 => Some(value),
            _ => None,
        })
}

/// Called only on the background executor. At most one page (eight bounded
/// prefixes) is read per flight; stable no-type values are cached as well.
fn load_metadata(
    page: &MetadataPage,
    cache: &std::sync::Mutex<providers::MetadataCache<MetadataRevision>>,
) -> Vec<BadgeUpdate> {
    page.rows
        .iter()
        .take(MAX_ROWS)
        .map(|(path, target)| BadgeUpdate {
            path: path.clone(),
            target: target.clone(),
            // Unknown/unavailable metadata clears a previously painted badge.
            badge: cached_type_badge(&page.root, path, cache),
        })
        .collect()
}

fn cached_type_badge(
    root: &std::path::Path,
    path: &str,
    cache: &std::sync::Mutex<providers::MetadataCache<MetadataRevision>>,
) -> Option<String> {
    if okilum_core::vault::service_path(std::path::Path::new(path)) {
        return None;
    }
    let full_path = root.join(path);
    let before = MetadataRevision::read(&full_path).ok()?;
    if let Some(badge) = cache.lock().ok()?.get(root, path, &before).cloned() {
        return badge;
    }
    let prefix = metadata_prefix(&full_path).ok()?;
    let badge = type_badge(&prefix);
    if MetadataRevision::read(&full_path).ok()? != before {
        return None;
    }
    cache.lock().ok()?.insert(root, path, before, badge.clone());
    badge
}

/// The list while it is open.
pub(super) struct Open {
    pub query: LinkQuery,
    pub items: Vec<Candidate>,
    pub selected: usize,
}

/// Per editor: what the list shows, and what it was computed for.
#[derive(Default)]
pub(super) struct State {
    seen: Option<(
        gpui_component::input::projection::SourceStamp,
        Range<usize>,
        bool,
    )>,
    pub open: Option<Open>,
    pending: Option<LinkQuery>,
    accept_pending: Option<Trigger>,
    pasted: Option<SourceStamp>,
    /// The `[[` whose list Escape closed (or a choice completed); it stays
    /// closed until the caret leaves that link.
    dismissed: Option<usize>,
    index: Option<Arc<Index>>,
    task: Option<Task<()>>,
    heading_source: Option<HeadingSource>,
    heading_accepting: bool,
    metadata_task: Option<Task<()>>,
    metadata_page: Option<MetadataPage>,
    metadata_cache: Arc<std::sync::Mutex<providers::MetadataCache<MetadataRevision>>>,
    from: String,
    composing: bool,
    editable: bool,
    markdown: bool,
    preferred: Option<(Kind, usize, String, String)>,
}

#[cfg(test)]
thread_local! {
    static REFRESH_TIMES: std::cell::RefCell<Vec<f64>> = const { std::cell::RefCell::new(Vec::new()) };
    static METADATA_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
#[cfg(test)]
struct RefreshTiming(std::time::Instant);
#[cfg(test)]
impl Drop for RefreshTiming {
    fn drop(&mut self) {
        REFRESH_TIMES.with(|samples| {
            samples
                .borrow_mut()
                .push(self.0.elapsed().as_secs_f64() * 1000.0)
        });
    }
}

impl Reader {
    pub(super) fn suppress_pasted_completion(&mut self, cx: &mut Context<Self>) {
        if let Some(editing) = self.editing.as_mut() {
            let stamp = editing.input.read(cx).source_stamp();
            let state = &mut editing.link_complete;
            state.pasted = Some(stamp);
            state.pending = None;
            state.accept_pending = None;
            state.open = None;
            state.task = None;
            cx.notify();
        }
    }

    /// Recompute the list after the text or the caret changed.
    pub(crate) fn refresh_link_complete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(test)]
        let _timing = RefreshTiming(std::time::Instant::now());
        let emoji = reader_ui_state::emoji_completion(cx);
        let show_hidden = self.sidebar.show_hidden;
        let vault = self.vault.clone();
        let from = self.current_rel.clone();
        let Some(editing) = self.editing.as_mut() else {
            return;
        };
        let input = editing.input.read(cx);
        let seen = (input.source_stamp(), input.selected_range(), emoji);
        if editing.link_complete.seen.as_ref() == Some(&seen)
            && editing.link_complete.composing == input.ime_composing()
            && editing.link_complete.editable == input.is_editable()
            && editing.link_complete.markdown == editing.markdown
            && editing.link_complete.from == from
            && editing
                .link_complete
                .index
                .as_ref()
                .is_some_and(|index| index.current(&vault, show_hidden))
        {
            return;
        }
        let was_open = editing.link_complete.open.is_some();
        // A different document, vault snapshot or visibility policy starts a
        // new completion session even if the source stamp/query looks equal.
        if editing.link_complete.from != from
            || !editing
                .link_complete
                .index
                .as_ref()
                .is_some_and(|index| index.current(&vault, show_hidden))
        {
            editing.link_complete.open = None;
            editing.link_complete.preferred = None;
            editing.link_complete.dismissed = None;
            editing.link_complete.metadata_page = None;
            editing.link_complete.metadata_cache = Default::default();
        }
        let selection = seen.1.clone();
        let expected_seen = seen.clone();
        editing.link_complete.seen = Some(seen);
        editing.link_complete.composing = input.ime_composing();
        editing.link_complete.editable = input.is_editable();
        editing.link_complete.markdown = editing.markdown;
        editing.link_complete.from = from.clone();
        let query = (editing.markdown
            && selection.is_empty()
            && input.is_editable()
            && !input.ime_composing()
            && editing.link_complete.pasted != Some(input.source_stamp()))
        .then(|| {
            let row = input.cursor_position().line as usize;
            let start = input.text().line_start_offset(row);
            // Only the bounded lookbehind is copied, even beyond column 1024.
            // Move a byte cut to a UTF-8 boundary; the worker owns full context.
            let mut window_start = selection.start.saturating_sub(1024).max(start);
            while !input.text().is_char_boundary(window_start) {
                window_start += 1;
            }
            let line = input
                .text()
                .slice(window_start..selection.start)
                .to_string();
            line_query(&line, window_start, selection.start)
                .filter(|query| emoji || !matches!(query.kind, Kind::Emoji))
        })
        .flatten();
        let source = input.text().clone();
        let input_entity = editing.input.clone();
        let state = &mut editing.link_complete;
        state.task = None;
        state.pending = None;
        state.accept_pending = None;
        state.heading_source = None;
        state.heading_accepting = false;
        let Some(query) = query else {
            state.dismissed = None;
            state.preferred = None;
            state.open = None;
            if was_open {
                cx.notify();
            }
            return;
        };
        if state.dismissed.is_some_and(|start| start != query.start) {
            state.dismissed = None;
        }
        if state.dismissed.is_some() {
            state.open = None;
            if was_open {
                cx.notify();
            }
            return;
        }
        if !state
            .index
            .as_ref()
            .is_some_and(|index| index.current(&vault, show_hidden))
        {
            state.index = Some(Arc::new(Index::new(vault, show_hidden)));
        }
        state.pending = Some(query.clone());
        publish(state, query.clone(), Vec::new());
        {
            let expected_vault = self.vault.clone();
            let expected_publication = self.source_publication_epoch;
            let vault = expected_vault.clone();
            let from_expected = from.clone();
            let index = state.index.as_ref().unwrap().clone();
            #[cfg(test)]
            let hold = if matches!(query.kind, Kind::Heading(_)) {
                HEADING_READ_HOLD.with(|hold| hold.borrow_mut().take())
            } else {
                NOTE_READ_HOLD.with(|hold| hold.borrow_mut().take())
            };
            state.task = Some(cx.spawn_in(window, async move |this, cx| {
                let worker_query = query.clone();
                let enriched = cx
                    .background_executor()
                    .spawn(async move {
                        let source = source.to_string();
                        if link_query(&source, worker_query.caret).as_ref() != Some(&worker_query) {
                            return Enriched::default();
                        }
                        let items = match worker_query.kind {
                            Kind::Note => index.search(&worker_query.query, &from),
                            Kind::Heading(_) => Vec::new(),
                            _ => catalog(&worker_query),
                        };
                        let mut enriched =
                            enrich_bound(&vault, &from, Some(&source), &worker_query, items);
                        #[cfg(test)]
                        if let Some((ready, held)) = hold {
                            let _ = ready.send(());
                            let _ = held.await;
                        }
                        if enriched
                            .heading
                            .as_ref()
                            .is_some_and(|source| !source.current())
                        {
                            enriched = enrich_bound(
                                &vault,
                                &from,
                                Some(&source),
                                &worker_query,
                                Vec::new(),
                            );
                        }
                        enriched
                    })
                    .await;
                let _ = this.update_in_own_window(cx, |this, window, cx| {
                    if !Arc::ptr_eq(&this.vault, &expected_vault)
                        || this.source_publication_epoch != expected_publication
                        || this.current_rel != from_expected
                    {
                        return;
                    }
                    let Some(editing) = this.editing.as_mut() else {
                        return;
                    };
                    if editing.input != input_entity {
                        return;
                    }
                    let input = editing.input.read(cx);
                    let current = (
                        input.source_stamp(),
                        input.selected_range(),
                        reader_ui_state::emoji_completion(cx),
                    );
                    if current != expected_seen
                        || editing.link_complete.seen.as_ref() != Some(&expected_seen)
                        || this.sidebar.show_hidden != show_hidden
                        || !editing.markdown
                        || input.ime_composing()
                        || !input.is_editable()
                        || editing.link_complete.dismissed == Some(query.start)
                    {
                        return;
                    }
                    editing.link_complete.pending = None;
                    let accept = std::mem::take(&mut editing.link_complete.accept_pending);
                    editing.link_complete.heading_source = enriched.heading;
                    publish(&mut editing.link_complete, query, enriched.rows);
                    if let Some(trigger) = accept {
                        if this
                            .editing
                            .as_ref()
                            .is_some_and(|editing| editing.link_complete.open.is_some())
                        {
                            this.link_complete_accept(None, trigger, window, cx);
                        } else if this.ime_allows(trigger, cx)
                            && input_entity.read(cx).focus_handle(cx).is_focused(window)
                        {
                            // An empty result (including code context) must not
                            // eat the user's ordinary Enter or Tab.
                            match trigger {
                                Trigger::Enter => window.dispatch_action(
                                    Box::new(Enter {
                                        secondary: false,
                                        shift: false,
                                    }),
                                    cx,
                                ),
                                Trigger::Tab => window.dispatch_action(Box::new(IndentInline), cx),
                                _ => {}
                            }
                        }
                    }
                    this.refresh_link_complete_metadata(cx);
                    cx.notify();
                });
            }));
        }
        cx.notify();
    }

    /// Internal hook for initial rows and keyboard paging; slot32's editor
    /// observer/actions/overlay hooks remain unchanged.
    fn refresh_link_complete_metadata(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.as_mut() else {
            return;
        };
        let Some(open) = editing.link_complete.open.as_ref() else {
            return;
        };
        if !matches!(open.query.kind, Kind::Note) || editing.link_complete.metadata_task.is_some() {
            return;
        }
        let input = editing.input.read(cx);
        let seen = (
            input.source_stamp(),
            input.selected_range(),
            reader_ui_state::emoji_completion(cx),
        );
        if editing.link_complete.seen.as_ref() != Some(&seen)
            || !editing.markdown
            || !input.is_editable()
            || input.ime_composing()
            || !seen.1.is_empty()
        {
            return;
        }
        let page = MetadataPage {
            seen,
            query: open.query.clone(),
            root: self.vault.root.clone(),
            from: self.current_rel.clone(),
            show_hidden: self.sidebar.show_hidden,
            rows: MetadataPage::rows(open),
        };
        if editing.link_complete.metadata_page.as_ref() == Some(&page) {
            return;
        }
        editing.link_complete.metadata_page = Some(page.clone());
        let input_entity = editing.input.clone();
        let expected_vault = self.vault.clone();
        let expected_publication = self.source_publication_epoch;
        let cache = editing.link_complete.metadata_cache.clone();
        editing.link_complete.metadata_task = Some(cx.spawn(async move |this, cx| {
            let request = page.clone();
            let updates = cx
                .background_executor()
                .spawn(async move { load_metadata(&request, &cache) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(editing) = this.editing.as_mut() else {
                    return;
                };
                if editing.input != input_entity {
                    return;
                }
                editing.link_complete.metadata_task = None;
                let input = editing.input.read(cx);
                let current = (
                    input.source_stamp(),
                    input.selected_range(),
                    reader_ui_state::emoji_completion(cx),
                );
                if Arc::ptr_eq(&this.vault, &expected_vault)
                    && this.source_publication_epoch == expected_publication
                    && this.current_rel == page.from
                    && this.sidebar.show_hidden == page.show_hidden
                    && current == page.seen
                    && editing.link_complete.seen.as_ref() == Some(&page.seen)
                    && editing.markdown
                    && input.is_editable()
                    && !input.ime_composing()
                    && editing.link_complete.dismissed != Some(page.query.start)
                {
                    if let Some(open) = editing.link_complete.open.as_mut() {
                        if apply_metadata(open, &page, &current, updates) {
                            cx.notify();
                        }
                    }
                }
                // Coalesce rapid navigation/source edits to the latest visible
                // page after this worker finishes; never overlap page workers.
                this.refresh_link_complete_metadata(cx);
            });
        }));
    }

    /// The editor while its link list is open and may take keys.
    fn link_complete_input(&self, window: &Window, cx: &App) -> Option<Entity<EditorState>> {
        let editing = self.editing.as_ref()?;
        if editing.link_complete.open.is_none()
            && !editing
                .link_complete
                .pending
                .as_ref()
                .is_some_and(|query| matches!(query.kind, Kind::Note))
        {
            return None;
        }
        let input = editing.input.read(cx);
        let current = (
            input.source_stamp(),
            input.selected_range(),
            reader_ui_state::emoji_completion(cx),
        );
        if !editing.markdown
            || editing.link_complete.seen.as_ref() != Some(&current)
            || editing.link_complete.from != self.current_rel
            || !editing
                .link_complete
                .index
                .as_ref()
                .is_some_and(|index| index.current(&self.vault, self.sidebar.show_hidden))
        {
            return None;
        }
        if editing
            .link_complete
            .open
            .as_ref()
            .is_some_and(|open| matches!(open.query.kind, Kind::Emoji))
            && !reader_ui_state::emoji_completion(cx)
        {
            return None;
        }
        (input.focus_handle(cx).is_focused(window)
            && input.is_editable()
            && input.selected_range().is_empty()
            && !input.ime_composing())
        .then(|| editing.input.clone())
    }

    fn link_complete_step(&mut self, delta: isize, window: &Window, cx: &mut Context<Self>) {
        if self.link_complete_input(window, cx).is_none() {
            return;
        }
        let Some(open) = self
            .editing
            .as_mut()
            .and_then(|editing| editing.link_complete.open.as_mut())
        else {
            return;
        };
        let count = open.items.len() as isize;
        if count == 0 {
            return;
        }
        open.selected = (open.selected as isize + delta).rem_euclid(count) as usize;
        self.refresh_link_complete_metadata(cx);
        cx.stop_propagation();
        cx.notify();
    }

    fn link_complete_dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_link_complete(window, cx);
        if let Some(editing) = self
            .editing
            .as_mut()
            .filter(|editing| editing.link_complete.heading_accepting)
        {
            editing.link_complete.task = None;
            editing.link_complete.heading_accepting = false;
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.link_complete_input(window, cx).is_none() {
            return;
        }
        if let Some(editing) = self.editing.as_mut() {
            let state = &mut editing.link_complete;
            state.dismissed = state
                .open
                .take()
                .map(|open| open.query.start)
                .or_else(|| state.pending.take().map(|query| query.start));
            state.pending = None;
            state.accept_pending = None;
            state.task = None;
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn link_complete_accept_row(
        &mut self,
        intent: &RowIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(choice) = self.editing.as_ref().and_then(|editing| {
            intent.choice(
                editing.link_complete.open.as_ref()?,
                editing.input.read(cx).source_stamp(),
            )
        }) else {
            return;
        };
        self.link_complete_accept(Some(choice), Trigger::Text, window, cx);
    }

    /// Write the chosen note's link: row `choice`, or the highlighted one.
    pub(super) fn link_complete_accept(
        &mut self,
        choice: Option<usize>,
        trigger: Trigger,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ime_allows(trigger, cx) {
            return;
        }
        self.refresh_link_complete(window, cx);
        let Some(input) = self.link_complete_input(window, cx) else {
            return;
        };
        let Some(editing) = self.editing.as_mut() else {
            return;
        };
        let Some(open) = editing.link_complete.open.take() else {
            if choice.is_none()
                && editing
                    .link_complete
                    .pending
                    .as_ref()
                    .is_some_and(|query| matches!(query.kind, Kind::Note))
            {
                editing.link_complete.accept_pending = Some(trigger);
                cx.stop_propagation();
            }
            return;
        };
        let Some(item) = open.items.get(choice.unwrap_or(open.selected)) else {
            return;
        };
        editing.link_complete.dismissed = Some(open.query.start);
        cx.stop_propagation();
        let text = input.read(cx).value();
        // The list was computed for this text and caret; anything else is stale.
        if link_query(&text, input.read(cx).selected_range().start).as_ref() != Some(&open.query) {
            cx.notify();
            return;
        }
        if let Some(source) = editing.link_complete.heading_source.take() {
            let expected = editing.link_complete.seen.clone().unwrap();
            let query = open.query.clone();
            let target = item.target.clone();
            let vault = self.vault.clone();
            let expected_publication = self.source_publication_epoch;
            let from = self.current_rel.clone();
            let show_hidden = self.sidebar.show_hidden;
            let input_entity = input.clone();
            #[cfg(test)]
            let hold = HEADING_ACCEPT_HOLD.with(|hold| hold.borrow_mut().take());
            editing.link_complete.heading_accepting = true;
            editing.link_complete.task = Some(cx.spawn_in(window, async move |this, cx| {
                let valid = cx
                    .background_executor()
                    .spawn(async move {
                        #[cfg(test)]
                        if let Some((ready, held)) = hold {
                            let _ = ready.send(());
                            let _ = held.await;
                        }
                        source.current()
                    })
                    .await;
                let _ = this.update_in_own_window(cx, |reader, window, cx| {
                    if !Arc::ptr_eq(&reader.vault, &vault)
                        || reader.source_publication_epoch != expected_publication
                        || reader.current_rel != from
                        || reader.sidebar.show_hidden != show_hidden
                    {
                        return;
                    }
                    let Some(editing) = reader.editing.as_mut() else {
                        return;
                    };
                    if editing.input != input_entity || !editing.link_complete.heading_accepting {
                        return;
                    }
                    editing.link_complete.heading_accepting = false;
                    let state = input_entity.read(cx);
                    let current = (
                        state.source_stamp(),
                        state.selected_range(),
                        reader_ui_state::emoji_completion(cx),
                    );
                    if current != expected
                        || !editing.markdown
                        || !state.is_editable()
                        || state.ime_composing()
                        || !state.focus_handle(cx).is_focused(window)
                        || link_query(&state.value(), state.selected_range().start).as_ref()
                            != Some(&query)
                    {
                        return;
                    }
                    if !valid {
                        // The chosen target changed while the popup was open.
                        // Keep the draft and recompute this same query once.
                        editing.link_complete.dismissed = None;
                        editing.link_complete.seen = None;
                        reader.refresh_link_complete(window, cx);
                        cx.notify();
                        return;
                    }
                    let (edits, caret) = completion(&state.value(), &query, &target);
                    input_entity.update(cx, |state, cx| {
                        state.replace_source_edits(&edits, caret..caret, window, cx);
                    });
                    cx.notify();
                });
            }));
            cx.notify();
            return;
        }
        let (edits, caret) = completion(&text, &open.query, &item.target);
        input.update(cx, |state, cx| {
            state.replace_source_edits(&edits, caret..caret, window, cx)
        });
        cx.notify();
    }

    /// Up, Down, Enter, Tab and Escape go to the list while it is open,
    /// before the list keys, code comforts and the input see them.
    pub(super) fn link_complete_actions(&self, column: Div, cx: &mut Context<Self>) -> Div {
        column
            .capture_action(
                cx.listener(|this, _: &MoveUp, window, cx| this.link_complete_step(-1, window, cx)),
            )
            .capture_action(
                cx.listener(|this, _: &MoveDown, window, cx| {
                    this.link_complete_step(1, window, cx)
                }),
            )
            .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                if !action.secondary && !action.shift {
                    this.link_complete_accept(None, Trigger::Enter, window, cx)
                }
            }))
            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                this.link_complete_accept(None, Trigger::Tab, window, cx)
            }))
            .capture_action(
                cx.listener(|this, _: &Escape, window, cx| this.link_complete_dismiss(window, cx)),
            )
    }

    /// The list under the caret, while the editor has focus.
    pub(super) fn render_link_complete(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let input = self.link_complete_input(window, cx)?;
        let open = self.editing.as_ref()?.link_complete.open.as_ref()?;
        let state = input.read(cx);
        let caret = state.range_to_bounds(&(open.query.caret..open.query.caret))?;
        let p = brand::palette(cx);
        let visible = providers::visible_rows(open.items.len(), open.selected, MAX_ROWS);
        let rows = open
            .items
            .iter()
            .enumerate()
            .skip(visible.start)
            .take(visible.len())
            .map(|(i, item)| {
                let selected = i == open.selected;
                let intent = RowIntent {
                    query: open.query.clone(),
                    source: state.source_stamp(),
                    path: item.path.clone(),
                    target: item.target.clone(),
                };
                h_flex()
                    .id(("link-complete-row", i))
                    .debug_selector(move || format!("link-complete-{i}"))
                    .h(px(28.))
                    .px_2()
                    .gap_2()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |row| row.bg(p.selected))
                    .hover(move |row| row.bg(p.selected))
                    .child(
                        Icon::new(IconName::FileText)
                            .small()
                            .text_color(p.text_muted),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .max_w(px(220.))
                            .text_ellipsis()
                            .child(item.title.clone()),
                    )
                    .when(!item.folder.is_empty(), |row| {
                        row.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_ellipsis()
                                .text_color(p.text_muted)
                                .child(item.folder.replace('/', " › ")),
                        )
                    })
                    .children(item.badge.as_ref().map(|kind| {
                        let tones = [p.info, p.success, p.warning, p.accent];
                        let tone =
                            tones[kind.bytes().fold(0usize, |n, b| n.wrapping_add(b as usize))
                                % tones.len()];
                        div()
                            .px_1()
                            .rounded_sm()
                            .text_xs()
                            .bg(tone.opacity(0.10))
                            .text_color(tone)
                            .child(kind.clone())
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.link_complete_accept_row(&intent, window, cx);
                        }),
                    )
            });
        Some(
            deferred(
                anchored()
                    .position(point(caret.left(), caret.bottom() + px(4.)))
                    .snap_to_window_with_margin(px(12.))
                    .child(
                        v_flex()
                            .id("link-complete")
                            .debug_selector(|| "link-complete".into())
                            .occlude()
                            .on_prepaint(gpui_base::GlobalState::record_native_overlay)
                            .w(px(360.))
                            .p_1()
                            .rounded_lg()
                            .shadow_lg()
                            .bg(p.surface)
                            .text_color(p.text)
                            .text_size(px(13.))
                            .children(rows),
                    ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui_component::input::Undo;

    fn metadata_test_page(open: &Open, root: &std::path::Path) -> MetadataPage {
        MetadataPage {
            seen: (
                SourceStamp {
                    document: 1,
                    generation: 1,
                },
                4..4,
                true,
            ),
            query: open.query.clone(),
            root: root.into(),
            from: "note.md".into(),
            show_hidden: false,
            rows: MetadataPage::rows(open),
        }
    }

    #[test]
    fn tail_metadata_enriches_only_the_visible_page_without_moving_selection() {
        let directory = tempfile::tempdir().unwrap();
        let query = LinkQuery {
            start: 2,
            caret: 4,
            query: "go".into(),
            kind: Kind::Note,
        };
        let mut items: Vec<_> = (0..20)
            .map(|n| {
                let mut item = row(&format!("note{n}"), &format!("note{n}"), "");
                item.path = format!("note{n}.md");
                item
            })
            .collect();
        // A badge from an older observation must clear if the source vanishes.
        items[15].badge = Some("obsolete".into());
        let mut open = Open {
            query,
            items,
            selected: 15,
        };
        let page = metadata_test_page(&open, directory.path());
        assert_eq!(page.rows.len(), MAX_ROWS);
        for (path, _) in &page.rows {
            std::fs::write(
                directory.path().join(path),
                "---\ntype: project\n---\n# Body\n",
            )
            .unwrap();
        }
        let cache = std::sync::Mutex::new(providers::MetadataCache::default());
        let updates = load_metadata(&page, &cache);
        assert_eq!(updates.len(), MAX_ROWS);
        assert!(apply_metadata(&mut open, &page, &page.seen, updates));
        assert_eq!(open.selected, 15);
        let mut stale = page.seen.clone();
        stale.0.generation += 1;
        assert!(
            !apply_metadata(&mut open, &page, &stale, load_metadata(&page, &cache)),
            "an unchanged query cannot authorize old-source metadata"
        );
        for (n, item) in open.items.iter().enumerate() {
            assert_eq!(
                item.badge.as_deref(),
                (8..16).contains(&n).then_some("project")
            );
            assert_eq!(item.path, format!("note{n}.md"), "ranking stays fixed");
        }
        // A different page rejects late enrichment entirely.
        open.selected = 16;
        assert!(!apply_metadata(
            &mut open,
            &page,
            &page.seen,
            load_metadata(&page, &cache)
        ));
        assert_eq!(open.selected, 16);
        open.selected = 15;
        std::fs::remove_file(directory.path().join("note15.md")).unwrap();
        assert!(apply_metadata(
            &mut open,
            &page,
            &page.seen,
            load_metadata(&page, &cache)
        ));
        assert_eq!(
            open.items[15].badge, None,
            "missing source cannot reuse a badge"
        );
    }

    #[test]
    fn metadata_cache_skips_bytes_until_native_revision_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Plan.md");
        std::fs::write(&path, "---\ntype: project\n---\n").unwrap();
        let cache = std::sync::Mutex::new(providers::MetadataCache::default());
        METADATA_READS.with(|reads| reads.set(0));
        assert_eq!(
            cached_type_badge(directory.path(), "Plan.md", &cache).as_deref(),
            Some("project")
        );
        assert_eq!(
            METADATA_READS.with(|reads| reads.get()),
            1,
            "positive control: real prefix read"
        );
        for _ in 0..30 {
            assert_eq!(
                cached_type_badge(directory.path(), "Plan.md", &cache).as_deref(),
                Some("project")
            );
        }
        assert_eq!(
            METADATA_READS.with(|reads| reads.get()),
            1,
            "typing does not reopen unchanged notes"
        );
        std::fs::write(&path, "---\ntype: contact\n---\n").unwrap();
        assert_eq!(
            cached_type_badge(directory.path(), "Plan.md", &cache).as_deref(),
            Some("contact")
        );
        assert_eq!(
            METADATA_READS.with(|reads| reads.get()),
            2,
            "changed note read again"
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(cached_type_badge(directory.path(), "Plan.md", &cache), None);
    }

    #[test]
    fn metadata_cache_refreshes_same_size_same_mtime_rewrites() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Plan.md");
        std::fs::write(&path, "---\ntype: project\n---\n").unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let cache = std::sync::Mutex::new(providers::MetadataCache::default());
        assert_eq!(
            cached_type_badge(directory.path(), "Plan.md", &cache).as_deref(),
            Some("project")
        );
        std::fs::write(&path, "---\ntype: contact\n---\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert_eq!(
            cached_type_badge(directory.path(), "Plan.md", &cache).as_deref(),
            Some("contact")
        );
    }

    #[test]
    fn metadata_prefix_bounds_reads_and_handles_a_partial_final_codepoint() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.md");
        let mut source = "---\ntype: project\n---\n".to_owned();
        source.push_str(&"x".repeat(MAX_METADATA_BYTES as usize - source.len() - 1));
        source.push_str("🙂tail");
        std::fs::write(&path, source).unwrap();
        let prefix = metadata_prefix(&path).unwrap();
        assert!(prefix.len() <= MAX_METADATA_BYTES as usize);
        assert_eq!(type_badge(&prefix).as_deref(), Some("project"));
    }

    #[test]
    fn source_refresh_keeps_identity_across_pending_rows_and_reorders() {
        let query = LinkQuery {
            start: 2,
            caret: 4,
            query: "go".into(),
            kind: Kind::Note,
        };
        let a = row("Goals", "Goals", "");
        let b = row("Good", "Good", "");
        let mut state = State::default();
        publish(&mut state, query.clone(), vec![a.clone(), b.clone()]);
        state.open.as_mut().unwrap().selected = 1;
        publish(&mut state, query.clone(), Vec::new());
        assert!(
            state.open.is_none(),
            "pending rows cannot accept stale source"
        );
        // Multiple source edits may supersede workers before any rows return.
        publish(&mut state, query.clone(), Vec::new());
        publish(&mut state, query.clone(), vec![a.clone(), b.clone()]);
        assert_eq!(
            state.open.as_ref().unwrap().selected,
            1,
            "pending source refresh preserves the selected second row"
        );
        publish(&mut state, query.clone(), Vec::new());
        publish(&mut state, query.clone(), vec![b.clone(), a.clone()]);
        assert_eq!(state.open.as_ref().unwrap().selected, 0);
        assert_eq!(state.open.as_ref().unwrap().items[0].target, "Good");
        // If that choice disappears, select the first remaining current row.
        publish(&mut state, query.clone(), Vec::new());
        publish(&mut state, query.clone(), vec![a.clone()]);
        assert_eq!(state.open.as_ref().unwrap().items[0].target, "Goals");
        // A new trigger does not inherit the previous session's choice.
        let next = LinkQuery {
            start: 12,
            caret: 14,
            ..query
        };
        publish(&mut state, next, vec![b, a]);
        assert_eq!(state.open.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn painted_click_cannot_retarget_after_source_edit_or_async_reorder() {
        let query = LinkQuery {
            start: 2,
            caret: 4,
            query: "go".into(),
            kind: Kind::Note,
        };
        let source = SourceStamp {
            document: 7,
            generation: 1,
        };
        let intent = RowIntent {
            query: query.clone(),
            source,
            path: String::new(),
            target: "Good".into(),
        };
        let mut open = Open {
            query,
            items: vec![row("Goals", "Goals", ""), row("Good", "Good", "")],
            selected: 0,
        };
        assert_eq!(intent.choice(&open, source), Some(1), "positive control");
        open.items.swap(0, 1);
        assert_eq!(
            intent.choice(&open, source),
            Some(0),
            "identity follows a reorder"
        );
        assert_eq!(
            intent.choice(
                &open,
                SourceStamp {
                    generation: 2,
                    ..source
                }
            ),
            None
        );
        assert_eq!(
            intent.choice(
                &open,
                SourceStamp {
                    document: 8,
                    ..source
                }
            ),
            None
        );
        open.query.query.push('d');
        assert_eq!(intent.choice(&open, source), None, "another query");
        open.query = intent.query.clone();
        open.items.remove(0);
        assert_eq!(intent.choice(&open, source), None, "removed candidate");
    }

    /// `|` is the caret.
    fn at(text: &str) -> (String, usize) {
        let caret = text.find('|').unwrap();
        (text.replacen('|', "", 1), caret)
    }

    /// The caret is the last `|`, so an alias bar can come before it.
    fn query(text: &str) -> Option<String> {
        let caret = text.rfind('|').unwrap();
        let text = format!("{}{}", &text[..caret], &text[caret + 1..]);
        link_query(&text, caret).map(|q| q.query)
    }

    #[test]
    fn a_name_after_double_brackets_is_a_query() {
        assert_eq!(query("See [[|").as_deref(), Some(""));
        assert_eq!(query("See [[Pla|").as_deref(), Some("Pla"));
        assert_eq!(query("See [[Pla|]]").as_deref(), Some("Pla"));
        assert_eq!(query("See [[Work/Pl|an]] x").as_deref(), Some("Work/Pl"));
        assert_eq!(query("![[Diag|").as_deref(), Some("Diag"), "an embed too");
        assert_eq!(query("[[a]] and [[ש|").as_deref(), Some("ש"));
        // Positive controls for every refusal below.
        assert_eq!(query("[[Plan]] |"), None, "after a closed link");
        assert_eq!(query("[[Plan|alias|"), None, "in the alias");
        assert_eq!(
            query("[[Plan#Head|").as_deref(),
            Some("Head"),
            "in the heading"
        );
        assert_eq!(query("[[\nPlan|"), None, "another line");
        assert_eq!(query("Plan|"), None, "no brackets");
        assert_eq!(query("[Plan|"), None, "one bracket");
        assert_eq!(query("`[[Pla|`"), None, "inline code");
        assert_eq!(query(r"\[[Pla|"), None, "escaped opener");
        assert_eq!(
            query("``x\nx`` [[Pla|").as_deref(),
            Some("Pla"),
            "after multiline inline code"
        );
        assert_eq!(query("``` \n[[Pla|\n```"), None, "fenced code");
        assert_eq!(query("---\na: [[Pla|\n---\n"), None, "front matter");
        assert_eq!(
            query("`x` [[Pla|").as_deref(),
            Some("Pla"),
            "after a code span"
        );
        assert_eq!(
            query("```\nx\n```\n[[Pla|").as_deref(),
            Some("Pla"),
            "after a fence"
        );
        let long = format!("[[{}|", "a".repeat(MAX_QUERY_CHARS + 1));
        assert_eq!(query(&long), None);
    }

    fn vault(paths: &[&str]) -> Arc<Vault> {
        Arc::new(Vault::from_note_paths(paths.iter().map(|p| p.to_string())))
    }

    fn titles(found: &[Candidate]) -> Vec<&str> {
        found.iter().map(|c| c.path.as_str()).collect()
    }

    #[test]
    fn notes_rank_by_how_their_name_matches() {
        let index = Index::new(
            vault(&[
                "Archive/Old roadmap.md",
                "Road.md",
                "Roadmap 2027.md",
                "Projects/Railroad.md",
                "Projects/roadcanary/notes.md",
                "Unrelated.md",
                "Here.md",
                ".hidden/Road trip.md",
            ]),
            false,
        );
        assert_eq!(
            titles(&index.search("road", "Here.md")),
            [
                "Road.md",
                "Roadmap 2027.md",
                "Archive/Old roadmap.md",
                "Projects/Railroad.md",
                "Projects/roadcanary/notes.md",
            ]
        );
        assert_eq!(
            titles(&index.search("ROADM", "Here.md")).len(),
            2,
            "any case"
        );
        assert!(index.search("zzz", "Here.md").is_empty());
        // The note being edited is not offered; hidden folders stay hidden.
        assert!(titles(&index.search("here", "Here.md")).is_empty());
        assert_eq!(titles(&index.search("here", "Road.md")), ["Here.md"]);
        let shown = Index::new(index.vault.clone(), true);
        assert!(titles(&shown.search("trip", "Here.md")).contains(&".hidden/Road trip.md"));
        // Nothing typed: the note's own folder first, at most a page.
        let many: Vec<String> = (0..20).map(|n| format!("Other/n{n:02}.md")).collect();
        let mut paths: Vec<&str> = many.iter().map(String::as_str).collect();
        paths.extend(["Mine/Here.md", "Mine/Sibling.md"]);
        let found = Index::new(vault(&paths), false).search("", "Mine/Here.md");
        assert_eq!(found.len(), 21);
        assert_eq!(found[0].path, "Mine/Sibling.md");
        assert_eq!(found[0].folder, "Mine");
    }

    #[test]
    fn the_target_is_the_shortest_unambiguous_name() {
        let v = vault(&[
            "Work/Plan.md",
            "Home/Plan.md",
            "Work/Solo.md",
            "a/b/Deep.md",
        ]);
        assert_eq!(target_for(&v, "Work/Solo.md", ""), "Solo");
        assert_eq!(target_for(&v, "Work/Plan.md", ""), "Work/Plan");
        assert_eq!(target_for(&v, "Home/Plan.md", ""), "Home/Plan");
        assert_eq!(target_for(&v, "a/b/Deep.md", ""), "Deep");
        let found = Index::new(v.clone(), false).search("plan", "Work/Solo.md");
        assert_eq!(
            found.iter().map(|c| c.target.as_str()).collect::<Vec<_>>(),
            ["Home/Plan", "Work/Plan"]
        );
        // Every written target resolves back to the chosen note (positive control).
        for c in &found {
            assert!(matches!(
                v.resolve_from(&c.target, "Work/Solo.md"),
                okilum_core::vault::Resolution::Resolved { path: p } if p == c.path
            ));
        }
    }

    fn complete(text: &str, target: &str) -> String {
        let (text, caret) = at(text);
        let query = link_query(&text, caret).unwrap();
        let (edits, caret) = completion(&text, &query, target);
        let mut out = lists::apply(&text, &edits);
        out.insert(caret, '|');
        out
    }

    #[test]
    fn completion_writes_the_name_and_closes_the_link_once() {
        assert_eq!(complete("See [[Pl|", "Plan"), "See [[Plan]]|");
        assert_eq!(complete("See [[Pl|]] more", "Plan"), "See [[Plan]]| more");
        assert_eq!(complete("[[Pl|xx]]", "Work/Plan"), "[[Work/Plan]]|");
        assert_eq!(complete("[[Pl|an|alias]]", "Plan"), "[[Plan||alias]]");
        assert_eq!(complete("[[Pl|#Goals]]", "Plan"), "[[Plan|#Goals]]");
        assert_eq!(
            complete("[[Pl| rest of line", "Plan"),
            "[[Plan]]| rest of line"
        );
        assert_eq!(complete("[[Pl|\r\nnext", "Plan"), "[[Plan]]|\r\nnext");
        assert_eq!(complete("[[ש|", "שלום"), "[[שלום]]|");
    }

    #[test]
    fn headings_use_unsaved_source_and_never_choose_ambiguous_targets() {
        let v = vault(&["note.md", "a/Plan.md", "b/Plan.md"]);
        let (text, caret) = at("# Goals\n\n[[#Go|");
        let q = link_query(&text, caret).unwrap();
        let rows = enrich(&v, "note.md", Some(&text), &q, Vec::new());
        assert_eq!(
            rows.iter().map(|r| r.target.as_str()).collect::<Vec<_>>(),
            ["#Goals"]
        );
        let (edits, _) = completion(&text, &q, &rows[0].target);
        assert_eq!(lists::apply(&text, &edits), "# Goals\n\n[[#Goals]]");
        let ambiguous = link_query("[[Plan#Go", 9).unwrap();
        assert!(enrich(&v, "note.md", Some(&text), &ambiguous, Vec::new()).is_empty());
        let duplicate = "# Goals\n# Goals\n[[#Go";
        let q = link_query(duplicate, duplicate.len()).unwrap();
        assert!(enrich(&v, "note.md", Some(duplicate), &q, Vec::new()).is_empty());
        for marked in ["` :smi|`", "```\n/ta|\n```", "---\ntype: :smi|\n---"] {
            let (text, caret) = at(marked);
            assert!(link_query(&text, caret).is_none(), "{marked}");
        }
    }

    #[test]
    fn note_rows_read_type_metadata_and_heading_rows_read_the_resolved_note() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("Plan.md"),
            "---\ntype: project\n---\n# Goals\n",
        )
        .unwrap();
        let mut v = Vault::from_note_paths(["Plan.md".into(), "note.md".into()]);
        v.root = directory.path().into();
        let v = Arc::new(v);
        let q = link_query("[[Pla", 5).unwrap();
        let candidates = Index::new(v.clone(), false).search("Pla", "note.md");
        let mut open = Open {
            query: q.clone(),
            items: candidates,
            selected: 0,
        };
        let page = MetadataPage {
            seen: (
                SourceStamp {
                    document: 1,
                    generation: 1,
                },
                5..5,
                true,
            ),
            query: q,
            root: v.root.clone(),
            from: "note.md".into(),
            show_hidden: false,
            rows: MetadataPage::rows(&open),
        };
        let cache = std::sync::Mutex::new(providers::MetadataCache::default());
        assert!(apply_metadata(
            &mut open,
            &page,
            &page.seen,
            load_metadata(&page, &cache)
        ));
        assert_eq!(open.items.len(), 1);
        assert_eq!(open.items[0].badge.as_deref(), Some("project"));
        let q = link_query("[[Plan#Go", 9).unwrap();
        let rows = enrich(&v, "note.md", None, &q, Vec::new());
        assert_eq!(rows[0].target, "Plan#Goals");
        let q = link_query("/dat", 4).unwrap();
        let rows = catalog(&q);
        assert_eq!(rows[0].title, "date");
        assert!(chrono::NaiveDate::parse_from_str(&rows[0].target, "%Y-%m-%d").is_ok());
    }

    #[test]
    fn named_heading_source_rejects_changed_missing_and_unsupported_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Plan.md");
        std::fs::write(&path, "# Goals\r\n").unwrap();
        let (proof, text) = HeadingSource::read(directory.path(), "Plan.md").unwrap();
        assert_eq!(
            text, "# Goals\r\n",
            "exact resident source positive control"
        );
        assert!(proof.current());
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, "# Other\r\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert!(
            !proof.current(),
            "same size/mtime does not reuse old headings"
        );
        let (replacement, _) = HeadingSource::read(directory.path(), "Plan.md").unwrap();
        assert!(replacement.current(), "new revision remains readable");
        std::fs::remove_file(&path).unwrap();
        assert!(!replacement.current());
        std::fs::write(&path, [0xff, b'\n']).unwrap();
        assert!(HeadingSource::read(directory.path(), "Plan.md").is_none());
        std::fs::File::create(&path)
            .unwrap()
            .set_len(HeadingSource::MAX_BYTES + 1)
            .unwrap();
        assert!(HeadingSource::read(directory.path(), "Plan.md").is_none());
        assert!(HeadingSource::read(directory.path(), ".").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn named_heading_source_cannot_follow_an_outside_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let path = root.join("Plan.md");
        std::fs::write(&path, "# Owned\n").unwrap();
        let (proof, _) = HeadingSource::read(&root, "Plan.md").unwrap();
        assert!(proof.current(), "contained regular file positive control");
        let outside = directory.path().join("outside.md");
        std::fs::write(&outside, "# Foreign\n").unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert!(!proof.current());
        assert!(HeadingSource::read(&root, "Plan.md").is_none());
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "# Foreign\n");
    }

    #[test]
    fn painted_heading_row_keeps_the_resolved_note_identity() {
        let query = link_query("[[Plan#Go", 9).unwrap();
        let source = SourceStamp {
            document: 7,
            generation: 1,
        };
        let mut candidate = row("Goals", "Plan#Goals", "");
        candidate.path = "a/Plan.md".into();
        let intent = RowIntent {
            query: query.clone(),
            source,
            path: candidate.path.clone(),
            target: candidate.target.clone(),
        };
        let mut open = Open {
            query,
            items: vec![candidate],
            selected: 0,
        };
        assert_eq!(intent.choice(&open, source), Some(0));
        open.items[0].path = "b/Plan.md".into();
        assert_eq!(
            intent.choice(&open, source),
            None,
            "same display text cannot retarget a painted heading to another note"
        );
    }

    struct Note {
        reader: Entity<Reader>,
        input: Entity<EditorState>,
        _directory: tempfile::TempDir,
    }

    fn open(cx: &mut TestAppContext) -> (Note, &mut VisualTestContext) {
        open_fixture(cx, &[], "\n")
    }

    fn open_fixture<'a>(
        cx: &'a mut TestAppContext,
        corpus: &[String],
        source: &str,
    ) -> (Note, &'a mut VisualTestContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        for folder in ["Projects", "Archive"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        std::fs::write(root.join("note.md"), source).unwrap();
        let defaults =
            ["Recipes.md", "Projects/Roadmap.md", "Archive/Roadmap.md"].map(String::from);
        for note in if corpus.is_empty() {
            &defaults[..]
        } else {
            corpus
        } {
            let path = root.join(note);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "# x\n").unwrap();
        }
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("note.md")),
                        index_dir: Some(directory.path().join("index")),
                        session_directory: Some(directory.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        let input = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone());
        (
            Note {
                reader,
                input,
                _directory: directory,
            },
            visual,
        )
    }

    fn set(note: &Note, visual: &mut VisualTestContext, text: &str) {
        let (text, caret) = at(text);
        note.input
            .update_in(visual, |i, window, cx| i.set_value(text, window, cx));
        note.input
            .update(visual, |i, cx| i.set_selected_range(caret..caret, cx));
        visual.run_until_parked();
    }

    fn shown(note: &Note, visual: &mut VisualTestContext) -> String {
        note.input.read_with(visual, |i, _| {
            let mut text = i.value().to_string();
            text.insert(i.selected_range().start, '|');
            text
        })
    }

    fn typed(visual: &mut VisualTestContext, text: &str) {
        for ch in text.chars() {
            visual.simulate_input(&ch.to_string());
            visual.run_until_parked();
        }
    }

    fn keys(visual: &mut VisualTestContext, keys: &str) {
        visual.simulate_keystrokes(keys);
        visual.run_until_parked();
    }

    /// The rows on offer, by path; `None` while the list is closed.
    fn offered(note: &Note, visual: &mut VisualTestContext) -> Option<Vec<String>> {
        note.reader.read_with(visual, |r, _| {
            let open = r.editing.as_ref()?.link_complete.open.as_ref()?;
            Some(open.items.iter().map(|c| c.path.clone()).collect())
        })
    }

    fn undo(visual: &mut VisualTestContext) {
        visual.dispatch_action(Undo);
        visual.run_until_parked();
    }

    fn hold_heading(
        accept: bool,
    ) -> (
        futures::channel::oneshot::Receiver<()>,
        futures::channel::oneshot::Sender<()>,
    ) {
        let (ready, reached) = futures::channel::oneshot::channel();
        let (release, held) = futures::channel::oneshot::channel();
        let hold = if accept {
            &HEADING_ACCEPT_HOLD
        } else {
            &HEADING_READ_HOLD
        };
        hold.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some((ready, held));
        });
        (reached, release)
    }

    fn heading_targets(note: &Note, visual: &mut VisualTestContext) -> Vec<String> {
        note.reader.read_with(visual, |reader, _| {
            reader
                .editing
                .as_ref()
                .unwrap()
                .link_complete
                .open
                .as_ref()
                .map(|open| open.items.iter().map(|row| row.target.clone()).collect())
                .unwrap_or_default()
        })
    }

    fn named_path(note: &Note) -> PathBuf {
        note._directory.path().join("vault/Recipes.md")
    }

    #[gpui::test]
    fn named_heading_read_recomputes_a_target_changed_before_publication(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        std::fs::write(named_path(&note), "# Before\n").unwrap();
        let (mut reached, release) = hold_heading(false);
        set(&note, visual, "[[Recipes#|");
        assert_eq!(
            reached.try_recv(),
            Ok(Some(())),
            "actual named-source worker reached hold"
        );
        assert!(
            offered(&note, visual).is_none(),
            "no unvalidated rows published"
        );
        std::fs::write(named_path(&note), "# After\n").unwrap();
        release.send(()).unwrap();
        visual.run_until_parked();
        assert_eq!(heading_targets(&note, visual), ["Recipes#After"]);
        keys(visual, "tab");
        assert_eq!(shown(&note, visual), "[[Recipes#After]]|");
        undo(visual);
        assert_eq!(
            shown(&note, visual),
            "[[Recipes#|",
            "one acceptance, one Undo"
        );
    }

    #[gpui::test]
    fn named_heading_accept_rejects_changed_or_deleted_target_and_keeps_draft(
        cx: &mut TestAppContext,
    ) {
        let (note, visual) = open(cx);
        std::fs::write(named_path(&note), "# Before\n").unwrap();
        set(&note, visual, "[[Recipes#|");
        assert_eq!(
            heading_targets(&note, visual),
            ["Recipes#Before"],
            "positive control"
        );
        std::fs::write(named_path(&note), "# After\n").unwrap();
        keys(visual, "tab");
        assert_eq!(
            shown(&note, visual),
            "[[Recipes#|",
            "stale row does not edit the draft"
        );
        assert_eq!(
            heading_targets(&note, visual),
            ["Recipes#After"],
            "fresh target recomputed"
        );
        keys(visual, "tab");
        assert_eq!(shown(&note, visual), "[[Recipes#After]]|");
        undo(visual);
        assert_eq!(shown(&note, visual), "[[Recipes#|");
        // A new input session offers the current target again before deletion.
        set(&note, visual, "next [[Recipes#|");
        assert_eq!(heading_targets(&note, visual), ["Recipes#After"]);
        std::fs::remove_file(named_path(&note)).unwrap();
        keys(visual, "enter");
        assert_eq!(shown(&note, visual), "next [[Recipes#|");
        assert!(offered(&note, visual).is_none());
    }

    #[gpui::test]
    fn named_heading_click_and_unsaved_heading_share_undo_in_live_preview(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        note.reader.update_in(visual, |reader, window, cx| {
            reader.set_live_preview(true, window, cx)
        });
        visual.run_until_parked();
        std::fs::write(named_path(&note), "# Goals\r\n").unwrap();
        set(&note, visual, "See [[Recipes#Go|");
        assert_eq!(heading_targets(&note, visual), ["Recipes#Goals"]);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let row = visual
            .debug_bounds("link-complete-0")
            .expect("named heading row is painted");
        visual.simulate_click(row.center(), Modifiers::none());
        visual.run_until_parked();
        assert_eq!(shown(&note, visual), "See [[Recipes#Goals]]|");
        undo(visual);
        assert_eq!(shown(&note, visual), "See [[Recipes#Go|");
        set(&note, visual, "# Unsaved\n\n[[#Un|");
        assert_eq!(heading_targets(&note, visual), ["#Unsaved"]);
        keys(visual, "tab");
        assert_eq!(shown(&note, visual), "# Unsaved\n\n[[#Unsaved]]|");
        undo(visual);
        assert_eq!(shown(&note, visual), "# Unsaved\n\n[[#Un|");
        assert!(note.reader.read_with(visual, |reader, _| {
            reader.editing.as_ref().unwrap().live_preview.enabled
        }));
    }

    #[gpui::test]
    fn named_heading_pending_acceptance_cannot_outlive_escape_source_or_caret(
        cx: &mut TestAppContext,
    ) {
        let (note, visual) = open(cx);
        std::fs::write(named_path(&note), "# Goals\n").unwrap();
        for action in ["escape", "source", "caret"] {
            let original = format!("{action} [[Recipes#|");
            set(&note, visual, &original);
            assert_eq!(
                heading_targets(&note, visual),
                ["Recipes#Goals"],
                "{action} positive control"
            );
            let (mut reached, release) = hold_heading(true);
            keys(visual, "tab");
            assert_eq!(
                reached.try_recv(),
                Ok(Some(())),
                "accept validation actually started: {action}"
            );
            assert_eq!(shown(&note, visual), original, "no premature edit");
            match action {
                "escape" => keys(visual, "escape"),
                "source" => set(&note, visual, "replacement draft|"),
                "caret" => keys(visual, "left"),
                _ => unreachable!(),
            }
            let retained = shown(&note, visual);
            // Cancellation may already have dropped the receiver; either outcome
            // must leave this exact draft and caret unchanged after worker wake.
            let _ = release.send(());
            visual.run_until_parked();
            assert_eq!(
                shown(&note, visual),
                retained,
                "late {action} result must not edit"
            );
        }
        set(&note, visual, "healthy [[Recipes#|");
        keys(visual, "enter");
        assert_eq!(
            shown(&note, visual),
            "healthy [[Recipes#Goals]]|",
            "uncancelled control"
        );
        undo(visual);
        assert_eq!(shown(&note, visual), "healthy [[Recipes#|");
    }

    #[gpui::test]
    fn source_changes_with_the_same_query_cannot_accept_old_headings(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        set(&note, visual, "# Old goals\n\n[[#|");
        assert!(
            offered(&note, visual).is_some(),
            "positive control: headings loaded"
        );
        note.input.update_in(visual, |input, window, cx| {
            let text = "# New goals\n\n[[#";
            input.set_value(text, window, cx);
            input.set_selected_range(text.len()..text.len(), cx);
        });
        // Before queued refresh/worker publication, the query and caret are
        // unchanged but the candidate belongs to a different source revision.
        note.reader.update_in(visual, |reader, window, cx| {
            assert!(reader.link_complete_input(window, cx).is_none());
            reader.link_complete_accept(None, Trigger::Tab, window, cx);
        });
        assert_eq!(shown(&note, visual), "# New goals\n\n[[#|");
        visual.run_until_parked();
        keys(visual, "tab");
        assert_eq!(shown(&note, visual), "# New goals\n\n[[#New goals]]|");
    }

    #[gpui::test]
    fn keyboard_reaches_catalog_tail_in_the_same_popup(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        for source in [":|", "```r|", "/|"] {
            set(&note, visual, source);
            let target = note.reader.read_with(visual, |reader, _| {
                let open = reader
                    .editing
                    .as_ref()
                    .unwrap()
                    .link_complete
                    .open
                    .as_ref()
                    .unwrap();
                open.items.last().unwrap().target.clone()
            });
            keys(visual, "up");
            visual.update(|window, cx| window.draw(cx).clear(cx));
            let last = note.reader.read_with(visual, |reader, _| {
                let open = reader
                    .editing
                    .as_ref()
                    .unwrap()
                    .link_complete
                    .open
                    .as_ref()
                    .unwrap();
                assert_eq!(open.selected, open.items.len() - 1);
                open.selected
            });
            assert!(visual
                .debug_bounds(format!("link-complete-{last}").leak())
                .is_some());
            keys(visual, "tab");
            assert!(shown(&note, visual).contains(&target));
            undo(visual);
            assert_eq!(shown(&note, visual), source);
        }
    }

    fn hold_notes() -> (
        futures::channel::oneshot::Receiver<()>,
        futures::channel::oneshot::Sender<()>,
    ) {
        let (ready, reached) = futures::channel::oneshot::channel();
        let (release, held) = futures::channel::oneshot::channel();
        NOTE_READ_HOLD.with(|slot| *slot.borrow_mut() = Some((ready, held)));
        (reached, release)
    }

    #[gpui::test]
    fn enter_while_changed_note_query_is_pending_accepts_fresh_best_once(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        set(&note, visual, "[[rec|");
        assert_eq!(
            offered(&note, visual).unwrap()[0],
            "Recipes.md",
            "positive control: previous best"
        );
        let (mut reached, release) = hold_notes();
        set(&note, visual, "[[road|");
        assert_eq!(reached.try_recv(), Ok(Some(())), "actual note worker held");
        keys(visual, "enter");
        assert_eq!(
            shown(&note, visual),
            "[[road|",
            "Enter waits without a newline"
        );
        assert!(note.reader.read_with(visual, |r, _| r
            .editing
            .as_ref()
            .unwrap()
            .link_complete
            .accept_pending
            .is_some()));
        release.send(()).unwrap();
        visual.run_until_parked();
        assert_eq!(shown(&note, visual), "[[Archive/Roadmap]]|");
        undo(visual);
        assert_eq!(shown(&note, visual), "[[road|", "one-step Undo");
    }

    #[gpui::test]
    fn pending_enter_with_no_match_returns_to_the_editor(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        let (mut reached, release) = hold_notes();
        set(&note, visual, "[[zzzz|");
        assert_eq!(
            reached.try_recv(),
            Ok(Some(())),
            "positive control: empty search actually ran"
        );
        keys(visual, "enter");
        assert_eq!(shown(&note, visual), "[[zzzz|");
        release.send(()).unwrap();
        visual.run_until_parked();
        assert!(
            shown(&note, visual).contains('\n'),
            "no match leaves ordinary Enter available"
        );
        assert!(offered(&note, visual).is_none());
    }

    #[gpui::test]
    fn pending_enter_cannot_outlive_escape_caret_source_or_ime(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        for cancel in ["escape", "caret", "source", "ime"] {
            set(&note, visual, "plain|");
            let (mut reached, release) = hold_notes();
            set(&note, visual, "[[road|");
            assert_eq!(
                reached.try_recv(),
                Ok(Some(())),
                "positive control: {cancel} worker held"
            );
            keys(visual, "enter");
            match cancel {
                "escape" => keys(visual, "escape"),
                "caret" => {
                    note.input
                        .update(visual, |i, cx| i.set_selected_range(0..0, cx));
                    visual.run_until_parked();
                }
                "source" => set(&note, visual, "plain|"),
                "ime" => {
                    use gpui::EntityInputHandler as _;
                    visual.update(|window, cx| {
                        note.input.update(cx, |i, cx| {
                            i.replace_and_mark_text_in_range(None, "に", Some(1..1), window, cx)
                        })
                    });
                    visual.run_until_parked();
                }
                _ => unreachable!(),
            }
            let draft = shown(&note, visual);
            let _ = release.send(());
            visual.run_until_parked();
            assert_eq!(
                shown(&note, visual),
                draft,
                "{cancel} cancels queued acceptance"
            );
            if cancel == "escape" {
                assert!(offered(&note, visual).is_none());
            }
        }
    }

    #[gpui::test]
    fn popup_waits_during_preedit_and_commit_enter_settle(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        set(&note, visual, "|");
        visual.update(|window, cx| {
            note.input.update(cx, |i, cx| {
                i.replace_and_mark_text_in_range(None, "[[rec", None, window, cx);
            })
        });
        visual.run_until_parked();
        assert!(
            note.input.read_with(visual, |i, _| i.ime_composing()),
            "positive control: preedit is active"
        );
        assert!(offered(&note, visual).is_none());
        visual.update(|window, cx| {
            note.input.update(cx, |i, cx| {
                let marked = i.marked_text_range(window, cx);
                assert!(marked.is_some());
                i.replace_text_in_range(marked, "[[rec", window, cx);
            })
        });
        visual.run_until_parked();
        assert!(
            offered(&note, visual).is_some(),
            "committed trigger is typing"
        );
        note.reader.update_in(visual, |r, window, cx| {
            r.link_complete_accept(None, Trigger::Enter, window, cx)
        });
        assert_eq!(
            shown(&note, visual),
            "[[rec|",
            "commit Enter does not accept"
        );
        visual
            .executor()
            .advance_clock(std::time::Duration::from_millis(600));
        visual.run_until_parked();
        keys(visual, "enter");
        assert_eq!(
            shown(&note, visual),
            "[[Recipes]]|",
            "positive control: settled Enter accepts"
        );
        undo(visual);
        assert_eq!(shown(&note, visual), "[[rec|", "one acceptance Undo");
    }

    #[gpui::test]
    fn paste_ending_in_a_trigger_stays_closed_but_typing_resumes(cx: &mut TestAppContext) {
        use gpui_component::input::{
            clipboard::{ClipboardReadError, ClipboardReadRequest, ExactClipboardProvider},
            Paste,
        };
        struct Exact(String);
        impl ExactClipboardProvider for Exact {
            fn read_text_now(&self, _: &App) -> Option<Result<Option<String>, ClipboardReadError>> {
                Some(Ok(Some(self.0.clone())))
            }
            fn read_text(
                &self,
                _: ClipboardReadRequest,
                _: &App,
            ) -> Task<Result<Option<String>, ClipboardReadError>> {
                Task::ready(Ok(Some(self.0.clone())))
            }
        }
        let (note, visual) = open(cx);
        for text in ["[[", "[[rec", "/", ":smi", "```ru"] {
            set(&note, visual, "|");
            note.input.update_in(visual, |input, window, cx| {
                input.set_exact_clipboard_provider(Some(Arc::new(Exact(text.into()))), cx);
                input.focus_handle(cx).focus(window, cx);
                window.activate_window();
            });
            visual.dispatch_action(Paste);
            visual.run_until_parked();
            assert_eq!(
                shown(&note, visual),
                format!("{text}|"),
                "actual paste inserted"
            );
            assert!(
                offered(&note, visual).is_none(),
                "paste {text} must not open popup"
            );
        }
        typed(visual, "s");
        assert!(
            offered(&note, visual).is_some(),
            "positive control: typing after pasted trigger opens popup"
        );
        set(&note, visual, "|");
        typed(visual, "[[rec");
        assert!(
            offered(&note, visual).is_some(),
            "positive control: typing opens popup"
        );
    }

    #[gpui::test]
    fn actual_popup_suppresses_code_and_reopens_in_prose(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        for source in [
            "`[[rec|`",
            "``[[rec|``",
            "``line\n[[rec|``",
            "```rust\n[[rec|\n```",
            "~~~\n[[rec|\n~~~",
            "    [[rec|",
            "\t[[rec|",
        ] {
            set(&note, visual, source);
            assert!(offered(&note, visual).is_none(), "code: {source}");
        }
        set(&note, visual, "```rust\n[[rec\n````\n[[rec|");
        assert_eq!(
            offered(&note, visual).unwrap()[0],
            "Recipes.md",
            "positive control: after code"
        );
    }

    /// Exercise the real popup, worker, resolver and Undo with an on-disk
    /// synthetic inventory. Rank is measured for named intents, not inferred
    /// from a scorer-only fixture or claimed for every possible note title.
    #[gpui::test]
    fn intended_notes_in_5576_are_selectable_with_three_or_four_characters(
        cx: &mut TestAppContext,
    ) {
        let intents = [
            ("rec", "Kitchen/Recipes.md"),
            ("road", "Projects/Roadmap.md"),
            ("road", "Archive/Roadmap.md"),
            ("mac", "Hardware/Macbook.md"),
            ("quan", "Science/Quantum.md"),
            ("éla", "Writing/Élan.md"),
            ("slee", "Health/Sleep.md"),
            ("w/no", "Work/Notes.md"),
            ("guit", "Music/Guitar.md"),
            ("rus", "Code/Rust.md"),
            ("tok", "Travel/Tokyo.md"),
            ("budg", "Finance/Budget.md"),
        ];
        let mut corpus: Vec<String> = intents.iter().map(|(_, path)| (*path).into()).collect();
        let topics = [
            "biology",
            "calculus",
            "history",
            "design",
            "gardening",
            "photography",
        ];
        while corpus.len() < 5575 {
            let n = corpus.len();
            corpus.push(format!(
                "Library/Group {}/Observation {n:04} about {}.md",
                n % 73,
                topics[n % topics.len()]
            ));
        }
        let (note, visual) = open_fixture(cx, &corpus, "\n");
        assert_eq!(
            note.reader.read_with(visual, |r, _| r.vault.notes.len()),
            5576
        );
        let mut measurements = Vec::new();
        REFRESH_TIMES.with(|times| times.borrow_mut().clear());
        for (query, path) in intents {
            set(&note, visual, "See [[|");
            let start = std::time::Instant::now();
            typed(visual, query);
            let offered = offered(&note, visual).expect("actual worker published candidates");
            let rank = offered
                .iter()
                .position(|p| p == path)
                .expect("intended note found");
            assert!(rank < MAX_ROWS, "{query}: intended {path} at rank {rank}");
            for _ in 0..rank {
                keys(visual, "down");
            }
            visual.update(|window, cx| window.draw(cx).clear(cx));
            assert!(
                visual
                    .debug_bounds(
                        [
                            "link-complete-0",
                            "link-complete-1",
                            "link-complete-2",
                            "link-complete-3",
                            "link-complete-4",
                            "link-complete-5",
                            "link-complete-6",
                            "link-complete-7"
                        ][rank]
                    )
                    .is_some(),
                "intended row painted"
            );
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            let target = note
                .reader
                .read_with(visual, |r, _| target_for(&r.vault, path, "note.md"));
            keys(visual, "enter");
            assert_eq!(shown(&note, visual), format!("See [[{target}]]|"));
            undo(visual);
            assert_eq!(
                shown(&note, visual),
                format!("See [[{query}|"),
                "single acceptance Undo"
            );
            measurements.push(serde_json::json!({"query": query, "characters": query.chars().count(),
                "intended": path, "rank_one_based": rank + 1, "candidates": offered.len(), "query_to_draw_ms": elapsed}));
        }
        let mut refresh_ms = REFRESH_TIMES.with(|times| times.borrow().clone());
        assert!(
            refresh_ms.len() >= intents.len() * 3,
            "positive control: actual UI refresh callbacks sampled"
        );
        refresh_ms.sort_by(f64::total_cmp);
        let max_ms = *refresh_ms.last().unwrap();
        assert!(
            max_ms < 16.0,
            "UI completion refresh exceeds one 60 Hz frame: {max_ms} ms"
        );
        diagnostic_println!(
            "COMPLETION_UI_REFRESH {}",
            serde_json::json!({"notes": 5576, "samples": refresh_ms.len(),
                "p50_ms": refresh_ms[refresh_ms.len()/2], "p95_ms": refresh_ms[refresh_ms.len()*95/100],
                "max_ms": max_ms, "budget_ms": 16,
                "boundary": "synchronous refresh_link_complete; ranking/context validation on background executor"})
        );
        diagnostic_println!(
            "COMPLETION_5576 {}",
            serde_json::json!({"corpus": "synthetic-topic-and-folder-inventory",
            "notes": 5576, "boundary": "GPUI-test-worker-to-popup-draw", "measurements": measurements})
        );
    }

    #[gpui::test]
    fn large_source_pending_worker_keeps_ui_refresh_bounded(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        let mut source = "paragraph\n\n".repeat(((1 << 20) - 8192 - 6) / 11);
        source.push_str(&"x".repeat((1 << 20) - 8192 - 6 - source.len()));
        source.push_str(&"🙂".repeat(2048));
        source.push_str(" [[rec|");
        assert_eq!(source.len() - 1, 1 << 20);
        // The 1 KiB lookbehind starts inside an emoji, far past column 1024.
        assert!(!source.is_char_boundary((1 << 20) - 1024));
        let (mut reached, release) = hold_notes();
        REFRESH_TIMES.with(|times| times.borrow_mut().clear());
        set(&note, visual, &source);
        assert_eq!(
            reached.try_recv(),
            Ok(Some(())),
            "positive control: 1 MiB source worker reached hold"
        );
        let samples = REFRESH_TIMES.with(|times| times.borrow().clone());
        assert!(!samples.is_empty());
        let max = samples.iter().copied().fold(0.0, f64::max);
        assert!(max < 16.0, "bounded UI query: {max} ms");
        release.send(()).unwrap();
        visual.run_until_parked();
        assert_eq!(
            offered(&note, visual).unwrap()[0],
            "Recipes.md",
            "positive control: query actually published"
        );
        diagnostic_println!(
            "COMPLETION_LARGE_SOURCE_UI {}",
            serde_json::json!({"bytes": source.len()-1, "max_refresh_ms": max, "samples": samples.len(), "boundary": "completion refresh only; Source mode"})
        );
    }

    #[gpui::test]
    fn providers_share_keyboard_acceptance_and_one_step_undo(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        for (source, expected) in [
            ("Hello :smi|", "Hello 😄|"),
            ("```rus|\n\n```", "```rust|\n\n```"),
            ("/tas|", "- [ ] |"),
            ("# Goals\n\n[[#Go|", "# Goals\n\n[[#Goals]]|"),
        ] {
            set(&note, visual, source);
            assert!(offered(&note, visual).is_some(), "{source}");
            keys(visual, "tab");
            assert_eq!(shown(&note, visual), expected);
            undo(visual);
            assert_eq!(shown(&note, visual), source, "one Undo step");
        }
        set(&note, visual, "/tas|");
        keys(visual, "escape");
        assert!(offered(&note, visual).is_none());
        typed(visual, "k");
        assert!(
            offered(&note, visual).is_none(),
            "dismissed until leaving this command"
        );
    }

    #[gpui::test]
    fn typing_a_link_name_offers_notes_and_enter_writes_the_link(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        set(&note, visual, "See |");
        assert_eq!(
            offered(&note, visual),
            None,
            "positive control: no `[[` yet"
        );
        typed(visual, "[[");
        // Nothing typed yet: every other note is offered, not this one.
        let all = offered(&note, visual).expect("`[[` opens the list");
        assert_eq!(all.len(), 3);
        assert!(!all.contains(&"note.md".to_owned()));
        typed(visual, "rec");
        assert_eq!(
            offered(&note, visual),
            Some(vec!["Recipes.md".into(), "Projects/Roadmap.md".into()]),
            // The shared fuzzy ranker also matches r/e/c in Projects; the
            // direct title match remains first and is the accepted row.
            "title prefix ranks before a fuzzy folder match"
        );
        // The list is painted under the caret.
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let list = visual
            .debug_bounds("link-complete")
            .expect("the list is painted");
        let caret = note.input.read_with(visual, |i, _| {
            let c = i.cursor();
            i.range_to_bounds(&(c..c)).unwrap()
        });
        assert!(list.top() >= caret.bottom(), "{list:?} below {caret:?}");
        keys(visual, "enter");
        // Brackets may have closed themselves; either way one `]]` closes it.
        assert_eq!(shown(&note, visual), "See [[Recipes]]|");
        assert_eq!(offered(&note, visual), None);
        undo(visual);
        let undone = shown(&note, visual);
        assert!(undone.starts_with("See [[rec|"), "one Undo step: {undone}");

        // Namesakes: the folder tells them apart and is written into the link.
        set(&note, visual, "|");
        typed(visual, "[[road");
        assert_eq!(
            offered(&note, visual),
            Some(vec![
                "Archive/Roadmap.md".into(),
                "Projects/Roadmap.md".into()
            ])
        );
        keys(visual, "down");
        assert_eq!(
            shown(&note, visual).matches('|').count(),
            1,
            "the caret stays"
        );
        keys(visual, "tab");
        assert_eq!(shown(&note, visual), "[[Projects/Roadmap]]|");

        // A click on a row writes that row's link.
        set(&note, visual, "|");
        typed(visual, "[[road");
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let row = visual
            .debug_bounds("link-complete-1")
            .expect("the second row is painted");
        visual.simulate_click(row.center(), Modifiers::none());
        visual.run_until_parked();
        assert_eq!(shown(&note, visual), "[[Projects/Roadmap]]|");

        // Escape closes the list for this link: Enter is a new line again.
        set(&note, visual, "|");
        typed(visual, "[[rec");
        assert!(offered(&note, visual).is_some());
        keys(visual, "escape");
        assert_eq!(offered(&note, visual), None);
        typed(visual, "i");
        assert_eq!(offered(&note, visual), None, "stays closed for this link");
        keys(visual, "enter");
        assert!(
            shown(&note, visual).contains("\n"),
            "Enter typed a new line"
        );

        // No match: nothing opens, and typing a new name stays plain.
        set(&note, visual, "|");
        typed(visual, "[[Brand new");
        assert_eq!(offered(&note, visual), None);

        // Up and Down outside the list still move the caret (positive control).
        set(&note, visual, "one\ntw|o");
        keys(visual, "up");
        assert_eq!(shown(&note, visual), "on|e\ntwo");
        assert!(std::fs::read_to_string(note._directory.path().join("vault/note.md")).is_ok());
    }
}

#[cfg(test)]
mod at_prefix_tests {
    use super::*;
    #[test]
    fn completion_obeys_at_hiding_without_hiding_inventory() {
        let vault = Arc::new(Vault::from_note_paths(
            ["Area/@index.md", "@Private/y.md", "Visible.md"]
                .into_iter()
                .map(str::to_owned),
        ));
        assert!(Index::new(vault.clone(), false)
            .search("y", "Visible.md")
            .is_empty());
        assert_eq!(
            Index::new(vault, true).search("y", "Visible.md")[0].path,
            "@Private/y.md"
        );
    }
}
