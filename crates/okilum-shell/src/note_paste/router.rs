//! Smart paste (#1611): which representation a paste into a note takes and
//! what text it inserts. Platform-neutral; the platform supplies the
//! clipboard through [`Pasteboard`].
//!
//! Cmd+V, first match wins:
//!
//! 0. The caret is inside a fenced code block: the plain text as it is.
//! 1. File URLs: each file linked (inside the vault) or copied into the
//!    attachment folder in the background, one embed per line.
//! 2. Image data: saved as a pasted image, `![[…]]` (an HTML flavor that is
//!    more than one `<img>` goes to step 5 instead).
//! 3. Code from an IDE: a fenced block with its language (VS Code's
//!    `vscode-editor-data`, or rich text that is all monospace).
//! 4. A one-line selection and a single URL: `[selection](url)`.
//! 5. HTML / RTF: Markdown by the `okilum-paste` converter; `data:` and
//!    `file://` images saved first, remote images downloaded afterwards.
//! 6. A TSV grid: a Markdown table.
//! 7. Plain text as it is (CRLF to LF).
//!
//! Only the type list is read up front, then only the bytes of the flavor a
//! step needs: a 50 MB TIFF is never read when a file URL or text wins.
//! Nothing here touches the network; a title fetch (Paste as Titled Link)
//! and the remote images of rich text are returned as a [`FollowUp`].

use super::anchor::{Anchor, Region};
use super::attachments;
use super::context::{code_block_prefix, continue_lines, fit, fit_markdown, in_fenced_code};
use super::download;
use super::files::{self, FilePlan};
use super::offer::{
    self, ImageKind, OfferError, PasteOffer, Pasteboard, PastedImage, CHROMIUM_SOURCE_URL,
    PUBLIC_URL, RTF, RTFD, URL_NAME, VSCODE_EDITOR_DATA,
};
use super::title;
use okilum_paste::{AssetSource, Converted, HtmlInput, VsCodeHint};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Reads repeated when the clipboard changes while it is being read.
const ATTEMPTS: usize = 3;
/// A TIFF larger than this is converted to PNG off the UI thread.
const BACKGROUND_TIFF_BYTES: usize = 4 * 1024 * 1024;
/// HTML larger than this is converted off the UI thread.
const BACKGROUND_HTML_BYTES: usize = 512 * 1024;
/// Tab-separated text larger than this is tried as a table off the UI thread.
const BACKGROUND_TSV_BYTES: usize = 256 * 1024;
/// A `file://` image in pasted HTML larger than this is left out.
const MAX_LOCAL_IMAGE_BYTES: u64 = 50 * 1024 * 1024;

/// The paste command.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Smart,
    /// Paste as Plain Text: the plain flavor (or rich text as plain), CRLF to
    /// LF, nothing else.
    Plain,
    /// Paste as Titled Link: a single URL becomes `[title](url)`.
    TitledLink,
}

/// Where the paste lands.
#[derive(Clone, Debug)]
pub struct RouteContext<'a> {
    pub image_options: super::image_import::Options,
    pub mode: Mode,
    /// The editor's whole source.
    pub source: &'a str,
    /// The selection the paste replaces (byte range; a caret is empty).
    pub selection: Range<usize>,
    pub vault_root: &'a Path,
    /// The note (vault-relative); `None` for a plain file (CSV, code), which
    /// takes text only.
    pub note_rel: Option<&'a str>,
    /// `YYYYMMDDHHMMSS` for pasted image names.
    pub stamp: &'a str,
    /// Ask before copying a file larger than this (`None`: never ask).
    pub ask_above: Option<u64>,
}

/// The path a paste took, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// Step 0: plain text inside a fenced code block.
    CodeBlockText,
    Files,
    Image,
    IdeCode,
    SelectionLink,
    RichText,
    Table,
    Text,
    /// Paste as Plain Text.
    PlainText,
    /// Paste as Titled Link.
    TitledLink,
    /// A plain file takes the clipboard's text only.
    FileText,
    Nothing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    /// Something failed (red toast); otherwise information.
    pub error: bool,
}

/// Work a paste leaves for the background executor; its text is inserted at
/// `anchor` when done, and only if the anchor can still be found.
#[derive(Debug)]
pub enum FollowUp {
    /// Copy files into the attachment folder (asking first when `plan.ask`),
    /// then insert their links, one per line.
    Files { plan: FilePlan, anchor: Anchor },
    /// Fetch the page title for the bare URL inserted at `anchor` and replace
    /// it with `[title](url)` if it is still intact.
    Title { url: String, anchor: Anchor },
    /// Convert and save a large image, then insert its embed.
    Image {
        image: PastedImage,
        vault_root: PathBuf,
        note_rel: String,
        stamp: String,
        image_options: super::image_import::Options,
        anchor: Anchor,
    },
    /// Download the remote images of pasted rich text into `folder`, then
    /// replace each one's `![alt](url)` inside the pasted text (`region`)
    /// with its embed, if that text is still there.
    RemoteImages {
        images: Vec<RemoteImage>,
        region: Region,
        folder: PathBuf,
        /// The vault, for links that stay unambiguous.
        vault_root: PathBuf,
        stamp: String,
        image_options: super::image_import::Options,
    },
    /// Convert HTML too large for the UI thread, then insert it at `anchor`
    /// and download its remote images.
    Rich { job: RichJob, anchor: Anchor },
}

/// A remote image in pasted rich text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteImage {
    pub url: String,
    /// The `![alt](url)` text that stands for it in the note.
    pub placeholder: String,
}

/// Rich text converted in the background.
#[derive(Debug)]
pub struct RichJob {
    pub image_options: super::image_import::Options,
    /// Empty: only the plain flavor, tried as a TSV table.
    pub html: String,
    pub base_url: Option<String>,
    /// The plain flavor, for HTML without formatting.
    pub plain: Option<String>,
    pub vault_root: PathBuf,
    pub note_rel: String,
    pub stamp: String,
}

#[derive(Debug)]
pub struct Routed {
    pub route: Route,
    /// The text that replaces the selection now, already fitted to the caret.
    pub insert: Option<String>,
    pub follow_up: Option<FollowUp>,
    pub notice: Option<Notice>,
    /// Complete locally saved files, published to inventory before their embeds.
    pub saved_files: Vec<PathBuf>,
    /// The clipboard's type names (never contents), for the log.
    pub types: Vec<String>,
    /// Bytes taken from the clipboard, for the log.
    pub bytes: usize,
    /// More for the log line (conversion time, images); never contents.
    pub detail: String,
    /// An image to save once the read is known to be consistent.
    image: Option<PastedImage>,
}

impl Routed {
    fn new(route: Route, types: &[String]) -> Self {
        Self {
            route,
            insert: None,
            follow_up: None,
            notice: None,
            saved_files: Vec::new(),
            types: types.to_vec(),
            bytes: 0,
            detail: String::new(),
            image: None,
        }
    }

    fn text(route: Route, types: &[String], text: String) -> Self {
        Self {
            bytes: text.len(),
            insert: Some(text),
            ..Self::new(route, types)
        }
    }

    fn notice(mut self, text: impl Into<String>, error: bool) -> Self {
        self.notice = Some(Notice {
            text: text.into(),
            error,
        });
        self
    }
}

/// Route one paste. A clipboard that changes while it is read is read again;
/// the image of a consistent read is saved before its embed is returned.
pub fn route(board: &impl Pasteboard, cx: &RouteContext) -> Result<Routed, OfferError> {
    for _ in 0..ATTEMPTS {
        let before = board.change_count();
        let routed = route_once(board, cx);
        if board.change_count() == before {
            return routed.map(|routed| clear_selection(finish(routed, cx), cx));
        }
    }
    Err(OfferError::Changed)
}

/// Save a routed image: here when small, in the background when large.
fn finish(mut routed: Routed, cx: &RouteContext) -> Routed {
    let Some(image) = routed.image.take() else {
        return routed;
    };
    let Some(note_rel) = cx.note_rel else {
        return routed.notice("Images can be pasted into notes, not into this file.", true);
    };
    routed.bytes = image.bytes.len();
    let at = selection(cx).start;
    if cx.image_options.enabled
        || (image.kind == offer::ImageKind::Tiff && image.bytes.len() > BACKGROUND_TIFF_BYTES)
    {
        routed.follow_up = Some(FollowUp::Image {
            image,
            vault_root: cx.vault_root.to_path_buf(),
            note_rel: note_rel.to_string(),
            stamp: cx.stamp.to_string(),
            image_options: cx.image_options,
            anchor: Anchor::at(cx.source, at),
        });
        return routed;
    }
    match attachments::save_pasted_image(cx.vault_root, note_rel, &image, cx.stamp) {
        Ok(saved) => {
            routed.saved_files.push(saved.path);
            routed.insert = Some(fit(&saved.embed, false, cx.source, selection(cx)));
            routed
        }
        Err(error) => routed.notice(error.user_text(), true),
    }
}

/// A follow-up that inserts later still replaces the selection: the
/// selection goes now (an empty insert, one undo step) and the result goes
/// where it was.
fn clear_selection(mut routed: Routed, cx: &RouteContext) -> Routed {
    let sel = selection(cx);
    if sel.is_empty() || routed.insert.is_some() {
        return routed;
    }
    let anchor = match &mut routed.follow_up {
        Some(
            FollowUp::Files { anchor, .. }
            | FollowUp::Image { anchor, .. }
            | FollowUp::Rich { anchor, .. },
        ) => anchor,
        _ => return routed,
    };
    let after = format!("{}{}", &cx.source[..sel.start], &cx.source[sel.end..]);
    *anchor = Anchor::at(&after, sel.start);
    routed.insert = Some(String::new());
    routed
}

fn selection(cx: &RouteContext) -> Range<usize> {
    let len = cx.source.len();
    let start = cx.selection.start.min(cx.selection.end).min(len);
    let end = cx.selection.end.max(cx.selection.start).min(len);
    start..end
}

fn route_once(board: &impl Pasteboard, cx: &RouteContext) -> Result<Routed, OfferError> {
    let types = board.types();
    let has = |kind: &str| types.iter().any(|t| t == kind);
    match cx.mode {
        Mode::Plain => return plain_command(board, &types),
        Mode::TitledLink => {
            if let Some(routed) = titled_link(board, &types, cx)? {
                return Ok(routed);
            }
        }
        Mode::Smart => {}
    }
    let Some(note_rel) = cx.note_rel else {
        return file_text(board, &types);
    };
    let sel = selection(cx);
    let fitted = |text: &str, block: bool| fit(text, block, cx.source, sel.clone());

    // 0. Inside a fenced code block: the plain flavor, untouched, except
    //    that a fence inside a blockquote, callout or list item continues its
    //    `> ` markers or indentation on every pasted line.
    if in_fenced_code(cx.source, sel.start) {
        if let Some(text) = offer::plain_text(board, &types)? {
            let text = match code_block_prefix(cx.source, sel.start) {
                Some(prefix) if text.contains('\n') => continue_lines(&text, &prefix),
                _ => text,
            };
            return Ok(Routed::text(Route::CodeBlockText, &types, text));
        }
    }

    // 1. Files (Finder, Raycast's history, any app's file copy), including a
    //    Raycast history image offered only as its cache path (#1602).
    let paths = offer::clipboard_files(board, &types);
    if !paths.is_empty() {
        return Ok(file_paste(&paths, note_rel, &types, cx));
    }

    // 2. Image data, unless rich HTML or real text comes with it.
    let mut plain: Option<Option<String>> = None;
    // Rich HTML that outranked image data, for the plain-text fallback.
    let mut rich_over_image: Option<String> = None;
    if offer::has_image_data(&types) {
        let html = offer::html(board, &types)?;
        let only_image = html.as_deref().is_none_or(html_is_only_an_image);
        let text = offer::plain_text(board, &types)?;
        let incidental = text.as_deref().is_none_or(text_is_incidental);
        plain = Some(text);
        if !only_image {
            rich_over_image = html;
        } else if incidental {
            if let Some(image) = offer::image_data(board, &types)? {
                let mut routed = Routed::new(Route::Image, &types);
                routed.image = Some(image);
                return Ok(routed);
            }
        }
    }
    let plain = match plain {
        Some(plain) => plain,
        None => offer::plain_text(board, &types)?,
    };

    // 3. Code copied from VS Code, Cursor or Windsurf: their editor data
    //    names the language. Markdown, plain text or one line from the editor
    //    pastes as its text, never as its colored HTML.
    let mut skip_rich = false;
    if let Some(text) = plain.as_deref() {
        let hint = if has(VSCODE_EDITOR_DATA) {
            board
                .vscode_editor_data()
                .and_then(|data| okilum_paste::vscode_language(&data))
        } else {
            None
        };
        if let Some(hint) = hint {
            match ide_code_block(&hint, text) {
                Some(block) => {
                    let text = fit_markdown(&block, true, cx.source, sel.clone());
                    return Ok(Routed::text(Route::IdeCode, &types, text));
                }
                None => skip_rich = true,
            }
        }
    }

    // 4. A one-line selection and a single URL: a link around the selection.
    //    (Checked before the rich flavor is read: a one-line URL is never the
    //    multi-line code of step 3.)
    if !sel.is_empty() {
        let selected = &cx.source[sel.clone()];
        if !selected.contains('\n') && !selected.trim().is_empty() {
            if let Some(url) = plain.as_deref().and_then(title::single_url) {
                let link = title::selection_link(selected, &url);
                return Ok(Routed::text(Route::SelectionLink, &types, link));
            }
        }
    }

    // Work too large for the UI thread: converted, its images saved, then
    // inserted at the paste position.
    let background = |html: String, base_url: Option<String>, plain: Option<String>| {
        let mut routed = Routed::new(Route::RichText, &types);
        routed.bytes = html.len().max(plain.as_ref().map_or(0, String::len));
        routed.detail = "background=true".into();
        routed.follow_up = Some(FollowUp::Rich {
            job: RichJob {
                html,
                base_url,
                plain,
                vault_root: cx.vault_root.to_path_buf(),
                note_rel: note_rel.to_string(),
                stamp: cx.stamp.to_string(),
                image_options: cx.image_options,
            },
            anchor: Anchor::at(cx.source, sel.start),
        });
        routed
    };

    // 3 (rich text) and 5. HTML, or RTF/RTFD as HTML: code when it is all
    // monospace, otherwise Markdown.
    let mut from_rtf = false;
    let html = if skip_rich {
        None
    } else {
        match rich_over_image.take() {
            Some(html) => Some(html),
            None => match offer::html(board, &types)? {
                Some(html) => Some(html),
                None if RTF.iter().chain(RTFD.iter()).any(|kind| has(kind)) => {
                    from_rtf = true;
                    board.rich_html()
                }
                None => None,
            },
        }
    };
    // RTF has no tables (Numbers): a TSV plain flavor is the table (step 6).
    let rtf_table = from_rtf
        && plain.as_deref().is_some_and(|text| {
            text.len() <= BACKGROUND_TSV_BYTES && okilum_paste::tsv_to_table(text).is_some()
        });
    if let Some(html) = html.filter(|html| !rtf_table && !html.trim().is_empty()) {
        let base_url = if has(CHROMIUM_SOURCE_URL) {
            board.source_url()
        } else {
            None
        };
        if html.len() > BACKGROUND_HTML_BYTES {
            return Ok(background(html, base_url, plain.clone()));
        }
        let started = Instant::now();
        let converted = okilum_paste::html_to_markdown(&HtmlInput {
            html: &html,
            base_url: base_url.as_deref(),
        });
        let convert_ms = started.elapsed().as_secs_f64() * 1000.0;
        // `data:` and `file://` images are read and saved off the UI thread,
        // and only for the read that is kept.
        let local_images = converted
            .assets
            .iter()
            .any(|asset| !matches!(asset.source, AssetSource::Remote(_)));
        if local_images {
            return Ok(background(html, base_url, plain.clone()));
        }
        if let Some(rich) = rich_text(
            converted,
            plain.as_deref(),
            cx.vault_root,
            note_rel,
            cx.stamp,
        ) {
            return Ok(rich_routed(rich, html.len(), convert_ms, &types, cx));
        }
        // No plain flavor (rich text only): its text.
        if plain.is_none() {
            let text = offer::html_to_text(&html);
            if !text.trim().is_empty() {
                return Ok(Routed::text(Route::Text, &types, fitted(&text, false)));
            }
        }
    }

    if let Some(text) = plain {
        // 6. A TSV grid: a table (a large one is tried off the UI thread).
        let tabbed = text.lines().next().is_some_and(|line| line.contains('\t'));
        if tabbed && text.len() > BACKGROUND_TSV_BYTES {
            return Ok(background(String::new(), None, Some(text)));
        }
        if let Some(table) = okilum_paste::tsv_to_table(&text) {
            let table = fit_markdown(&table, true, cx.source, sel.clone());
            return Ok(Routed::text(Route::Table, &types, table));
        }
        // 7. Plain text.
        if !text.is_empty() {
            let text = lf(&text);
            return Ok(Routed::text(Route::Text, &types, fitted(&text, false)));
        }
    }

    match offer::read_offer(board)? {
        PasteOffer::Text(text) => {
            let text = lf(&text);
            Ok(Routed::text(Route::Text, &types, fitted(&text, false)))
        }
        PasteOffer::Image(image) => {
            let mut routed = Routed::new(Route::Image, &types);
            routed.image = Some(image);
            Ok(routed)
        }
        PasteOffer::Files(paths) => Ok(file_paste(&paths, note_rel, &types, cx)),
        PasteOffer::Nothing { .. } => Ok(Routed::new(Route::Nothing, &types)),
    }
}

/// Step 1: files linked in place or copied in the background.
fn file_paste(paths: &[PathBuf], note_rel: &str, types: &[String], cx: &RouteContext) -> Routed {
    let mut plan = files::plan(paths, cx.vault_root, note_rel, cx.stamp, cx.ask_above);
    plan.image_options = cx.image_options;
    let mut routed = Routed::new(Route::Files, types);
    if let Some(skipped) = plan.skipped_text() {
        routed = routed.notice(skipped, false);
    }
    if plan.items.is_empty() {
        return routed;
    }
    let sel = selection(cx);
    if plan.copies() == 0 {
        let links = files::links(&plan, &[]);
        let block = links.len() > 1;
        let text = links.join("\n");
        routed.bytes = text.len();
        routed.insert = Some(fit(&text, block, cx.source, sel));
        return routed;
    }
    routed.follow_up = Some(FollowUp::Files {
        plan,
        anchor: Anchor::at(cx.source, sel.start),
    });
    routed
}

/// Paste as Plain Text (Option+Shift+Cmd+V): the plain flavor, rich text as plain, or file paths.
fn plain_command(board: &impl Pasteboard, types: &[String]) -> Result<Routed, OfferError> {
    let paths = offer::file_paths(board, types);
    if !paths.is_empty() {
        return Ok(Routed::text(Route::PlainText, types, path_lines(&paths)));
    }
    if let Some(text) = offer::plain_text(board, types)? {
        return Ok(Routed::text(Route::PlainText, types, lf(&text)));
    }
    Ok(match offer::read_offer(board)? {
        PasteOffer::Text(text) => Routed::text(Route::PlainText, types, lf(&text)),
        PasteOffer::Image(_) => Routed::new(Route::PlainText, types).notice(
            "The clipboard holds an image. Paste as Plain Text pastes text only.",
            false,
        ),
        PasteOffer::Files(paths) => Routed::text(Route::PlainText, types, path_lines(&paths)),
        PasteOffer::Nothing { .. } => Routed::new(Route::Nothing, types),
    })
}

/// Paths as text, one per line.
fn path_lines(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A plain file (CSV, code): the clipboard's text, exactly (#1602).
fn file_text(board: &impl Pasteboard, types: &[String]) -> Result<Routed, OfferError> {
    Ok(match offer::read_offer(board)? {
        PasteOffer::Text(text) => Routed::text(Route::FileText, types, text),
        PasteOffer::Image(_) => Routed::new(Route::FileText, types)
            .notice("Images can be pasted into notes, not into this file.", true),
        // A plain file takes copied files as their paths (#1602).
        PasteOffer::Files(paths) => Routed::text(Route::FileText, types, path_lines(&paths)),
        PasteOffer::Nothing { .. } => Routed::new(Route::Nothing, types),
    })
}

/// Cmd+Alt+V: `[title](url)` from the clipboard's own title (the link's name
/// or its HTML anchor text), else the bare URL now and the page title from
/// the network later. `None`: not a URL, so a normal paste.
fn titled_link(
    board: &impl Pasteboard,
    types: &[String],
    cx: &RouteContext,
) -> Result<Option<Routed>, OfferError> {
    let has = |kind: &str| types.iter().any(|t| t == kind);
    let plain = offer::plain_text(board, types)?;
    // A copied link (Safari, Chrome, Finder): its URL and title, no network.
    let (link_url, link_name) = match has(PUBLIC_URL).then(|| board.url_with_name()).flatten() {
        Some((url, name)) => (Some(url), name),
        None => (
            None,
            has(URL_NAME)
                .then(|| board.text_flavor(URL_NAME))
                .flatten()
                .map(|name| name.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|name| !name.is_empty()),
        ),
    };
    let url = plain.as_deref().and_then(title::single_url).or_else(|| {
        link_url
            .as_deref()
            .and_then(title::single_url)
            .filter(|url| !url.starts_with("file:"))
    });
    let Some(url) = url else {
        return Ok(None);
    };
    // The title belongs to this URL (no `public.url`, or the same one).
    let mut name = link_name
        .filter(|_| link_url.as_deref().is_none_or(|link| same_url(link, &url)))
        .filter(|name| *name != url);
    if name.is_none() {
        name = offer::html(board, types)?.and_then(|html| title::anchor_text(&html, &url));
    }
    if let Some(name) = name {
        let link = title::markdown_link(&name, &url);
        return Ok(Some(Routed::text(Route::TitledLink, types, link)));
    }
    let sel = selection(cx);
    let mut routed = Routed::text(Route::TitledLink, types, url.clone());
    if title::fetchable(&url) {
        let after = format!(
            "{}{}{}",
            &cx.source[..sel.start],
            url,
            &cx.source[sel.end..]
        );
        routed.follow_up = Some(FollowUp::Title {
            anchor: Anchor::at(&after, sel.start),
            url,
        });
    }
    Ok(Some(routed))
}

/// The same link, give or take a trailing slash.
fn same_url(a: &str, b: &str) -> bool {
    a.trim().trim_end_matches('/') == b.trim().trim_end_matches('/')
}

/// CRLF and lone CR to LF.
pub fn lf(text: &str) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text.to_string()
    }
}

/// An HTML flavor that only wraps one image (a browser's Copy Image): the
/// image data is the paste, no network needed.
pub(crate) fn html_is_only_an_image(html: &str) -> bool {
    let images = html
        .as_bytes()
        .windows(4)
        .filter(|w| w.eq_ignore_ascii_case(b"<img"))
        .take(2)
        .count();
    // Large HTML without an image is a page or a sheet, never just a picture;
    // it is not converted to text here on the UI thread.
    if images > 1 || (images == 0 && html.len() > 64 * 1024) {
        return false;
    }
    offer::html_to_text(html).trim().is_empty()
}

/// Text that comes along with image data without being the content: none,
/// whitespace, or one URL or path (Safari's Copy Image, a screenshot tool).
pub(crate) fn text_is_incidental(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return true;
    }
    if text.contains('\n') {
        return false;
    }
    title::single_url(text).is_some()
        || text.starts_with('/')
        || text.starts_with("~/")
        || (text.len() > 2
            && text.as_bytes()[1] == b':'
            && matches!(text.as_bytes()[2], b'\\' | b'/'))
}

// ---------------------------------------------------------------------------
// Steps 3, 5 and 6 with the converter crate (okilum-paste, #1611)
// ---------------------------------------------------------------------------

/// Step 3: code copied from VS Code (or Cursor, Windsurf) as a fenced block
/// with its language, or `None` to paste it as text: the editor's mode was
/// Markdown or plain text, or the code is one line. An unknown language is
/// guessed only when the code leaves no doubt.
pub fn ide_code_block(hint: &VsCodeHint, plain: &str) -> Option<String> {
    if hint.is_text {
        return None;
    }
    let code = lf(plain);
    let code = code.trim_end_matches('\n');
    if !code.contains('\n') {
        return None;
    }
    let language = hint
        .language
        .clone()
        .or_else(|| okilum_paste::guess_language(code).map(str::to_owned));
    Some(okilum_paste::fence_code(code, language.as_deref()))
}

/// Converted rich text ready to insert (not yet fitted to the caret).
#[derive(Debug, Default)]
pub struct RichText {
    /// [`Route::IdeCode`] for all-monospace code, else [`Route::RichText`].
    pub route: Option<Route>,
    pub markdown: String,
    pub block: bool,
    /// Remote images left as `![alt](url)`, to download afterwards.
    pub remote: Vec<RemoteImage>,
    /// `data:` and `file://` images saved into the attachment folder.
    pub saved: usize,
    /// Images that could not be saved; their alt text was kept.
    pub failed: usize,
    /// The files saved, removed again if the text is never inserted.
    pub files: Vec<PathBuf>,
}

/// Steps 3 and 5 for converted HTML (or RTF as HTML). `None`: the plain
/// flavor is the paste (steps 6 and 7): the HTML had no formatting beyond
/// paragraphs and line breaks, or it is one monospace line.
///
/// All-monospace text of two lines or more (JetBrains, Xcode, a page's code
/// block) is a fenced block of the plain flavor with the converter's
/// language. Otherwise the Markdown, with every `data:` and `file://` image
/// saved into the attachment folder before it is returned (a failed one keeps
/// its alt text) and remote images listed for download.
pub fn rich_text(
    converted: Converted,
    plain: Option<&str>,
    vault_root: &Path,
    note_rel: &str,
    stamp: &str,
) -> Option<RichText> {
    rich_text_with(
        converted,
        plain,
        vault_root,
        note_rel,
        stamp,
        super::image_import::Options::disabled(),
    )
}

fn rich_text_with(
    converted: Converted,
    plain: Option<&str>,
    vault_root: &Path,
    note_rel: &str,
    stamp: &str,
    options: super::image_import::Options,
) -> Option<RichText> {
    let plain = plain.filter(|text| !text.trim().is_empty());
    // One line of code (a JetBrains `<pre>`, a page's one-command block):
    // the plain flavor, never a fence in the middle of a sentence.
    let one_line = plain.is_some_and(|text| !lf(text).trim_end_matches('\n').contains('\n'));
    if (converted.monospace || converted.code_language.is_some()) && one_line {
        return None;
    }
    if converted.monospace {
        if let Some(language) = converted.code_language.as_deref() {
            let markdown = match plain {
                Some(code) => {
                    let code = lf(code);
                    let code = code.trim_end_matches('\n');
                    let language = Some(language)
                        .filter(|language| !language.is_empty())
                        .or_else(|| okilum_paste::guess_language(code));
                    okilum_paste::fence_code(code, language)
                }
                None => converted.markdown,
            };
            return Some(RichText {
                route: Some(Route::IdeCode),
                markdown,
                block: true,
                ..RichText::default()
            });
        }
    }
    if converted.plain && plain.is_some() {
        return None;
    }
    if converted.markdown.trim().is_empty() {
        return None;
    }
    let mut rich = RichText {
        route: Some(Route::RichText),
        block: converted.block,
        ..RichText::default()
    };
    let mut markdown = converted.markdown;
    for asset in converted.assets {
        match asset.source {
            AssetSource::Remote(url) => {
                let known = rich
                    .remote
                    .iter()
                    .any(|image| image.placeholder == asset.placeholder);
                if !known && rich.remote.len() < download::MAX_IMAGES {
                    rich.remote.push(RemoteImage {
                        url,
                        placeholder: asset.placeholder,
                    });
                }
            }
            AssetSource::Data { mime, bytes } => {
                let saved = save_data_image(&mime, bytes, vault_root, note_rel, stamp, options);
                let text = rich.account(saved, &asset.alt);
                markdown = markdown.replacen(&asset.placeholder, &text, 1);
            }
            AssetSource::LocalFile(path) => {
                let saved = save_local_image(&path, vault_root, note_rel, stamp, options);
                let text = rich.account(saved, &asset.alt);
                markdown = markdown.replacen(&asset.placeholder, &text, 1);
            }
        }
    }
    rich.markdown = markdown;
    Some(rich)
}

impl RichText {
    /// The text for one saved (or failed) image.
    fn account(&mut self, saved: Option<(String, Vec<PathBuf>)>, alt: &str) -> String {
        match saved {
            Some((embed, path)) => {
                self.saved += 1;
                self.files.extend(path);
                embed
            }
            None => {
                self.failed += 1;
                alt.replace(['[', ']', '\n'], " ").trim().to_string()
            }
        }
    }
}

/// The toast for images of rich text that could not be saved.
pub fn failed_images_text(failed: usize) -> String {
    if failed == 1 {
        "An image in the pasted text couldn’t be saved; its description was kept.".into()
    } else {
        format!(
            "{failed} images in the pasted text couldn’t be saved; their descriptions were kept."
        )
    }
}

/// The image kind a `data:` URI's MIME type names.
fn kind_of_mime(mime: &str) -> Option<ImageKind> {
    Some(match mime.trim().to_ascii_lowercase().as_str() {
        "image/png" | "image/apng" => ImageKind::Png,
        "image/jpeg" | "image/jpg" | "image/pjpeg" => ImageKind::Jpeg,
        "image/gif" => ImageKind::Gif,
        "image/webp" => ImageKind::Webp,
        "image/tiff" => ImageKind::Tiff,
        "image/bmp" | "image/x-ms-bmp" => ImageKind::Bmp,
        "image/heic" | "image/heif" => ImageKind::Heic,
        _ => return None,
    })
}

/// Save a `data:` image as a pasted image; its embed and path.
fn save_data_image(
    mime: &str,
    bytes: Vec<u8>,
    vault_root: &Path,
    note_rel: &str,
    stamp: &str,
    options: super::image_import::Options,
) -> Option<(String, Vec<PathBuf>)> {
    if bytes.is_empty() {
        return None;
    }
    let extension = if mime.trim().eq_ignore_ascii_case("image/svg+xml") {
        "svg"
    } else {
        attachments::sniff_image(&bytes)
            .or(kind_of_mime(mime))?
            .extension()
    };
    let folder = attachments::attachment_folder(vault_root, note_rel);
    let wanted = attachments::pasted_name(stamp, 0, extension);
    super::image_import::save(&folder, &wanted, &bytes, options)
        .ok()
        .map(|saved| {
            let target = if options.enabled {
                folder
                    .join(&saved.name)
                    .strip_prefix(vault_root)
                    .map(files::slash_path)
                    .unwrap_or_else(|_| saved.name.clone())
            } else {
                saved.name.clone()
            };
            (files::wikilink(&target), saved.paths)
        })
}

fn save_local_image(
    path: &Path,
    vault_root: &Path,
    note_rel: &str,
    stamp: &str,
    options: super::image_import::Options,
) -> Option<(String, Vec<PathBuf>)> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_LOCAL_IMAGE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    let kind = attachments::sniff_image(&bytes)
        .or_else(|| extension.as_deref().and_then(ImageKind::from_extension));
    if kind.is_none()
        && !extension
            .as_deref()
            .is_some_and(attachments::is_image_extension)
    {
        return None;
    }
    let name = if attachments::is_clipboard_cache(path) {
        attachments::pasted_name(
            stamp,
            0,
            kind.map(|k| k.extension()).or(extension.as_deref())?,
        )
    } else {
        files::sanitize_name(&path.file_name()?.to_string_lossy())
    };
    let folder = attachments::attachment_folder(vault_root, note_rel);
    super::image_import::save(&folder, &name, &bytes, options)
        .ok()
        .map(|saved| {
            let target = if options.enabled {
                folder
                    .join(&saved.name)
                    .strip_prefix(vault_root)
                    .map(files::slash_path)
                    .unwrap_or_else(|_| saved.name.clone())
            } else {
                saved.name.clone()
            };
            (files::wikilink(&target), saved.paths)
        })
}

/// The routed result of converted rich text, fitted to the caret, with its
/// remote images as a follow-up.
fn rich_routed(
    rich: RichText,
    html_bytes: usize,
    convert_ms: f64,
    types: &[String],
    cx: &RouteContext,
) -> Routed {
    let sel = selection(cx);
    let route = rich.route.unwrap_or(Route::RichText);
    let text = fit_markdown(&rich.markdown, rich.block, cx.source, sel.clone());
    let mut routed = Routed::text(route, types, text);
    routed.bytes = html_bytes;
    routed.detail = format!(
        "convert_ms={convert_ms:.1} images_saved={} images_failed={} images_remote={}",
        rich.saved,
        rich.failed,
        rich.remote.len()
    );
    if rich.failed > 0 {
        routed = routed.notice(failed_images_text(rich.failed), false);
    }
    if let (Some(note_rel), false) = (cx.note_rel, rich.remote.is_empty()) {
        let inserted = routed.insert.as_deref().unwrap_or_default();
        let after = format!(
            "{}{}{}",
            &cx.source[..sel.start],
            inserted,
            &cx.source[sel.end..]
        );
        routed.follow_up = Some(FollowUp::RemoteImages {
            region: Region::at(&after, sel.start, inserted.len()),
            images: rich.remote,
            folder: attachments::attachment_folder(cx.vault_root, note_rel),
            vault_root: cx.vault_root.to_path_buf(),
            stamp: cx.stamp.to_string(),
            image_options: cx.image_options,
        });
    }
    routed
}

/// What a background conversion inserts.
#[derive(Debug, Default)]
pub struct RichJobDone {
    pub text: String,
    pub block: bool,
    /// Fit as Markdown ([`fit_markdown`]) rather than as plain text.
    pub markdown: bool,
    pub remote: Vec<RemoteImage>,
    pub saved: usize,
    pub failed: usize,
    /// The images saved, removed again if the text is never inserted.
    pub files: Vec<PathBuf>,
    pub convert_ms: f64,
}

/// Convert large rich text off the UI thread: steps 3, 5, 6 and 7 for it,
/// with its `data:` and `file://` images saved.
pub fn run_rich_job(job: RichJob) -> RichJobDone {
    let started = Instant::now();
    let rich = if job.html.is_empty() {
        None
    } else {
        let converted = okilum_paste::html_to_markdown(&HtmlInput {
            html: &job.html,
            base_url: job.base_url.as_deref(),
        });
        rich_text_with(
            converted,
            job.plain.as_deref(),
            &job.vault_root,
            &job.note_rel,
            &job.stamp,
            job.image_options,
        )
    };
    let convert_ms = started.elapsed().as_secs_f64() * 1000.0;
    if let Some(rich) = rich {
        return RichJobDone {
            text: rich.markdown,
            block: rich.block,
            markdown: true,
            remote: rich.remote,
            saved: rich.saved,
            failed: rich.failed,
            files: rich.files,
            convert_ms,
        };
    }
    let plain = job.plain.unwrap_or_else(|| offer::html_to_text(&job.html));
    match okilum_paste::tsv_to_table(&plain) {
        Some(table) => RichJobDone {
            text: table,
            block: true,
            markdown: true,
            convert_ms,
            ..RichJobDone::default()
        },
        None => RichJobDone {
            text: lf(&plain),
            convert_ms,
            ..RichJobDone::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note_paste::files::FileAction;

    #[test]
    fn webp_policy_is_captured_for_raw_file_and_rich_image_jobs() {
        use super::super::image_import::{fixture_png, Options};
        use base64::Engine as _;
        let note = Note::new();
        let options = Options {
            quality: 60,
            keep_original: true,
            ..Default::default()
        };
        let png = fixture_png();
        let board = Board::with(&[("public.png", &png)]);
        let mut context = note.cx(Mode::Smart, "", 0..0);
        context.image_options = options;
        let routed = route(&board, &context).unwrap();
        assert!(
            routed.insert.is_none(),
            "encoding never runs on the UI thread"
        );
        let Some(FollowUp::Image { image_options, .. }) = routed.follow_up else {
            panic!("raw image job")
        };
        assert_eq!(image_options, options);
        assert!(!note.vault.path().join("attachments").exists());

        let external = tempfile::tempdir().unwrap();
        let source = external.path().join("image.png");
        std::fs::write(&source, &png).unwrap();
        let routed = route(
            &crate::note_paste::DroppedFiles(vec![source.clone()]),
            &context,
        )
        .unwrap();
        let Some(FollowUp::Files { plan, .. }) = routed.follow_up else {
            panic!("import job")
        };
        assert_eq!(plan.image_options, options);
        // A file already inside the vault is still linked without conversion.
        let existing = note.vault.path().join("old.png");
        std::fs::write(&existing, &png).unwrap();
        let before = std::fs::metadata(&existing).unwrap().modified().unwrap();
        let routed = route(
            &crate::note_paste::DroppedFiles(vec![existing.clone()]),
            &context,
        )
        .unwrap();
        assert!(routed.follow_up.is_none());
        assert_eq!(routed.insert.as_deref(), Some("![[old.png]]"));
        assert_eq!(std::fs::read(&existing).unwrap(), png);
        assert_eq!(
            std::fs::metadata(&existing).unwrap().modified().unwrap(),
            before
        );

        let data = base64::engine::general_purpose::STANDARD.encode(&png);
        let html = format!(
            "<p>Data<img src=\"data:image/png;base64,{data}\"> Local<img src=\"{}\"></p>",
            url::Url::from_file_path(&source).unwrap()
        );
        let board = Board::with(&[("public.html", html.as_bytes())]);
        let routed = route(&board, &context).unwrap();
        let Some(FollowUp::Rich { job, .. }) = routed.follow_up else {
            panic!("rich image job")
        };
        assert_eq!(job.image_options, options);
        assert!(!note.vault.path().join("attachments").exists());
        let done = run_rich_job(job);
        assert_eq!((done.saved, done.failed, done.files.len()), (2, 0, 4));
        assert_eq!(done.text.matches(".webp]]").count(), 2);
        for path in &done.files {
            let bytes = std::fs::read(path).unwrap();
            if path.extension().unwrap() == "webp" {
                assert_eq!(
                    image::guess_format(&bytes).unwrap(),
                    image::ImageFormat::WebP
                );
            } else {
                assert_eq!(bytes, png);
            }
        }
        assert_eq!(std::fs::read(source).unwrap(), png);
    }
    use crate::note_paste::offer::FILE_URL;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Board {
        items: BTreeMap<String, Vec<u8>>,
        files: Vec<PathBuf>,
        reads: std::cell::RefCell<Vec<String>>,
    }
    impl Board {
        fn with(items: &[(&str, &[u8])]) -> Self {
            Self {
                items: items
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_vec()))
                    .collect(),
                ..Default::default()
            }
        }
    }
    impl Pasteboard for Board {
        fn change_count(&self) -> i64 {
            0
        }
        fn types(&self) -> Vec<String> {
            self.items.keys().cloned().collect()
        }
        fn data(&self, kind: &str) -> Option<Vec<u8>> {
            self.reads.borrow_mut().push(kind.to_string());
            self.items.get(kind).cloned()
        }
        fn string(&self, kind: &str) -> Option<String> {
            self.data(kind)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        }
        fn rich_text(&self, data: &[u8], _: bool) -> Option<String> {
            Some(String::from_utf8_lossy(data).into_owned())
        }
        fn file_urls(&self) -> Vec<String> {
            self.files
                .iter()
                .map(|path| url::Url::from_file_path(path).unwrap().to_string())
                .collect()
        }
    }

    struct Note {
        vault: tempfile::TempDir,
    }
    impl Note {
        fn new() -> Self {
            Self {
                vault: tempfile::tempdir().unwrap(),
            }
        }
        fn cx<'a>(
            &'a self,
            mode: Mode,
            source: &'a str,
            selection: Range<usize>,
        ) -> RouteContext<'a> {
            RouteContext {
                image_options: super::super::image_import::Options::disabled(),
                mode,
                source,
                selection,
                vault_root: self.vault.path(),
                note_rel: Some("Notes/n.md"),
                stamp: "20261023120000",
                ask_above: Some(100 * 1024 * 1024),
            }
        }
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nimage";

    fn smart(board: &Board, note: &Note, source: &str, at: usize) -> Routed {
        route(board, &note.cx(Mode::Smart, source, at..at)).unwrap()
    }

    fn smart_selection(board: &Board, note: &Note, source: &str, sel: Range<usize>) -> Routed {
        route(board, &note.cx(Mode::Smart, source, sel)).unwrap()
    }

    #[test]
    fn plain_text_is_inserted_with_lf() {
        let note = Note::new();
        let board = Board::with(&[(offer::UTF8_TEXT, b"one\r\ntwo")]);
        let routed = smart(&board, &note, "", 0);
        assert_eq!(routed.route, Route::Text);
        assert_eq!(routed.insert.as_deref(), Some("one\ntwo"));
    }

    #[test]
    fn inside_a_code_fence_the_plain_text_is_untouched() {
        let note = Note::new();
        let source = "```\n\n```";
        let board = Board::with(&[
            (offer::UTF8_TEXT, b"a\r\n\tb"),
            ("public.png", PNG),
            ("public.html", b"<b>x</b>"),
        ]);
        let routed = smart(&board, &note, source, 4);
        assert_eq!(routed.route, Route::CodeBlockText);
        assert_eq!(routed.insert.as_deref(), Some("a\r\n\tb"));
        // The image was never read.
        assert!(!board.reads.borrow().iter().any(|k| k == "public.png"));
    }

    #[test]
    fn files_win_over_their_name_and_icon_and_large_images_are_not_read() {
        let note = Note::new();
        let outside = tempfile::tempdir().unwrap();
        let doc = outside.path().join("a.pdf");
        std::fs::write(&doc, b"%PDF").unwrap();
        let mut board = Board::with(&[
            (FILE_URL, b"file:///x"),
            (offer::UTF8_TEXT, b"a.pdf"),
            ("public.tiff", b"II*\0icon"),
        ]);
        board.files = vec![doc.clone()];
        let routed = smart(&board, &note, "text ", 5);
        assert_eq!(routed.route, Route::Files);
        assert_eq!(routed.insert, None);
        match routed.follow_up {
            Some(FollowUp::Files { plan, anchor }) => {
                assert_eq!(plan.copies(), 1);
                assert_eq!(anchor.offset, 5);
            }
            other => panic!("{other:?}"),
        }
        assert!(
            board.reads.borrow().is_empty(),
            "{:?}",
            board.reads.borrow()
        );
    }

    #[test]
    fn vault_files_are_linked_at_once_one_per_line() {
        let note = Note::new();
        let root = note.vault.path();
        std::fs::create_dir_all(root.join("attachments")).unwrap();
        std::fs::write(root.join("attachments/a.png"), PNG).unwrap();
        std::fs::write(root.join("attachments/b.zip"), b"zip").unwrap();
        let mut board = Board::with(&[(FILE_URL, b"")]);
        board.files = vec![
            root.join("attachments/a.png"),
            root.join("attachments/b.zip"),
        ];
        let routed = smart(&board, &note, "- ", 2);
        assert_eq!(
            routed.insert.as_deref(),
            Some("![[attachments/a.png]]\n  [[attachments/b.zip]]")
        );
        assert!(routed.follow_up.is_none());
    }

    /// #1602 Raycast: a history image offered only as its cache path is the
    /// file, named like a pasted image; Paste as Plain Text keeps the path.
    #[test]
    fn a_raycast_cache_path_is_the_file() {
        let note = Note::new();
        let outside = tempfile::tempdir().unwrap();
        let cache = outside.path().join("com.raycast.macos/clipboard");
        std::fs::create_dir_all(&cache).unwrap();
        let cached = cache.join("file-1.png");
        std::fs::write(&cached, PNG).unwrap();
        let path = cached.to_string_lossy().into_owned();
        let board = Board::with(&[(offer::UTF8_TEXT, path.as_bytes())]);
        let routed = smart(&board, &note, "", 0);
        assert_eq!(routed.route, Route::Files);
        match routed.follow_up {
            Some(FollowUp::Files { plan, .. }) => match &plan.items[0].action {
                FileAction::Copy { file_name } => {
                    assert_eq!(file_name, "Pasted image 20261023120000.png")
                }
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
        let routed = route(&board, &note.cx(Mode::Plain, "", 0..0)).unwrap();
        assert_eq!(routed.insert.as_deref(), Some(path.as_str()));
        // Any other path is text.
        let other = outside.path().join("x.png");
        std::fs::write(&other, PNG).unwrap();
        let other = other.to_string_lossy().into_owned();
        let board = Board::with(&[(offer::UTF8_TEXT, other.as_bytes())]);
        assert_eq!(smart(&board, &note, "", 0).route, Route::Text);
    }

    /// The decision order, first match wins: code fence, files, image data,
    /// IDE code, selection link, text.
    #[test]
    fn decision_order_first_match_wins() {
        let note = Note::new();
        let outside = tempfile::tempdir().unwrap();
        let doc = outside.path().join("a.pdf");
        std::fs::write(&doc, b"%PDF").unwrap();
        let editor = br#"{"mode":"rust"}"#;
        let mut board = Board::with(&[
            (FILE_URL, b""),
            ("public.png", PNG),
            (VSCODE_EDITOR_DATA, editor),
            (offer::UTF8_TEXT, b"fn a() {}\nfn b() {}"),
        ]);
        board.files = vec![doc];
        // 0 beats 1.
        assert_eq!(
            smart(&board, &note, "```\n\n```", 4).route,
            Route::CodeBlockText
        );
        // 1 beats 2 and 3.
        assert_eq!(smart(&board, &note, "", 0).route, Route::Files);
        board.files.clear();
        board.items.remove(FILE_URL);
        // Real text beats image data; 3 beats 4 and 7.
        let routed = route(&board, &note.cx(Mode::Smart, "word", 0..4)).unwrap();
        assert_eq!(routed.route, Route::IdeCode);
        board.items.remove(VSCODE_EDITOR_DATA);
        assert_eq!(smart(&board, &note, "", 0).route, Route::Text);
    }

    #[test]
    fn a_copied_folder_is_refused_with_a_notice() {
        let note = Note::new();
        let outside = tempfile::tempdir().unwrap();
        let mut board = Board::with(&[(FILE_URL, b"")]);
        board.files = vec![outside.path().to_path_buf()];
        let routed = smart(&board, &note, "", 0);
        assert_eq!(routed.insert, None);
        assert!(routed.follow_up.is_none());
        assert!(routed.notice.unwrap().text.contains("folder"));
    }

    #[test]
    fn image_data_is_saved_and_embedded() {
        let note = Note::new();
        let board = Board::with(&[
            ("public.png", PNG),
            (
                "public.html",
                b"<meta charset=utf-8><img src=\"https://x.org/a.png\">",
            ),
            (offer::UTF8_TEXT, b"https://x.org/a.png"),
        ]);
        let routed = smart(&board, &note, "", 0);
        assert_eq!(routed.route, Route::Image);
        assert_eq!(
            routed.insert.as_deref(),
            Some("![[Pasted image 20261023120000.png]]")
        );
        assert!(note
            .vault
            .path()
            .join("attachments/Pasted image 20261023120000.png")
            .is_file());
    }

    #[test]
    fn real_text_or_rich_html_beats_image_data() {
        let note = Note::new();
        // A spreadsheet: cells as text, and a picture of them.
        let board = Board::with(&[(offer::UTF8_TEXT, b"a\tb\n1\t2"), ("public.png", PNG)]);
        let routed = smart(&board, &note, "", 0);
        assert_eq!(routed.route, Route::Table);
        assert_eq!(
            routed.insert.as_deref(),
            Some("|   a |   b |\n| --: | --: |\n|   1 |   2 |")
        );
        // A web page with text and two images: not the image.
        let board = Board::with(&[
            ("public.html", b"<p>Hi</p><img src=a><img src=b>"),
            ("public.png", PNG),
        ]);
        let routed = smart(&board, &note, "", 0);
        assert_ne!(routed.route, Route::Image);
        assert!(!note.vault.path().join("attachments").exists());
    }

    #[test]
    fn a_selection_and_a_url_make_a_link() {
        let note = Note::new();
        let board = Board::with(&[(offer::UTF8_TEXT, b"https://okilum.app/docs")]);
        let source = "read the docs now";
        let routed = route(&board, &note.cx(Mode::Smart, source, 9..13)).unwrap();
        assert_eq!(routed.route, Route::SelectionLink);
        assert_eq!(
            routed.insert.as_deref(),
            Some("[docs](https://okilum.app/docs)")
        );
        // A caret, or a multi-line selection: the URL itself.
        let routed = smart(&board, &note, source, 9);
        assert_eq!(routed.insert.as_deref(), Some("https://okilum.app/docs"));
        let routed = route(&board, &note.cx(Mode::Smart, "a\nb", 0..3)).unwrap();
        assert_eq!(routed.route, Route::Text);
    }

    /// The URL's twelfth byte falls inside a Cyrillic codepoint.
    #[test]
    fn a_selection_and_a_unicode_url_make_a_link() {
        let note = Note::new();
        let board = Board::with(&[(offer::UTF8_TEXT, "http://пример.рф".as_bytes())]);
        let source = "read the docs now";
        let routed = route(&board, &note.cx(Mode::Smart, source, 9..13)).unwrap();
        assert_eq!(routed.route, Route::SelectionLink);
        assert_eq!(routed.insert.as_deref(), Some("[docs](http://пример.рф)"));
    }

    #[test]
    fn vscode_code_becomes_a_fenced_block() {
        let note = Note::new();
        let board = Board::with(&[
            (offer::UTF8_TEXT, b"const a = 1;\nconst b = 2;\n"),
            (
                VSCODE_EDITOR_DATA,
                br#"{"version":1,"isFromEmptySelection":false,"mode":"typescriptreact"}"#,
            ),
        ]);
        let routed = smart(&board, &note, "x ", 2);
        assert_eq!(routed.route, Route::IdeCode);
        assert_eq!(
            routed.insert.as_deref(),
            Some("\n\n```tsx\nconst a = 1;\nconst b = 2;\n```")
        );
        // One line, or Markdown: plain text.
        let board = Board::with(&[
            (offer::UTF8_TEXT, b"const a = 1;"),
            (VSCODE_EDITOR_DATA, br#"{"mode":"typescript"}"#),
        ]);
        assert_eq!(smart(&board, &note, "", 0).route, Route::Text);
        // Markdown from the editor: its text, not its colored HTML.
        let board = Board::with(&[
            (offer::UTF8_TEXT, b"# a\n- b"),
            (VSCODE_EDITOR_DATA, br#"{"mode":"markdown"}"#),
            (
                "public.html",
                b"<pre style=\"font-family: Menlo\"><b># a</b>\n- b</pre>",
            ),
        ]);
        let routed = smart(&board, &note, "", 0);
        assert_eq!(routed.route, Route::Text);
        assert_eq!(routed.insert.as_deref(), Some("# a\n- b"));
        // Backticks in the code lengthen the fence.
        let hint = okilum_paste::vscode_language(r#"{"mode":"markdown2"}"#).unwrap();
        assert_eq!(
            ide_code_block(&hint, "a\n```\nb").unwrap(),
            "````markdown2\na\n```\nb\n````"
        );
    }

    #[test]
    fn plain_command_takes_text_paths_or_says_why_not() {
        let note = Note::new();
        let board = Board::with(&[(offer::UTF8_TEXT, b"a\r\nb"), ("public.html", b"<b>a</b>")]);
        let routed = route(&board, &note.cx(Mode::Plain, "- ", 2..2)).unwrap();
        assert_eq!(routed.route, Route::PlainText);
        // No list continuation, no conversion: the text only.
        assert_eq!(routed.insert.as_deref(), Some("a\nb"));
        let outside = tempfile::tempdir().unwrap();
        let (a, b) = (outside.path().join("b.txt"), outside.path().join("c d.png"));
        std::fs::write(&a, b"t").unwrap();
        std::fs::write(&b, PNG).unwrap();
        let mut board = Board::with(&[(FILE_URL, b"")]);
        board.files = vec![a.clone(), b.clone()];
        let routed = route(&board, &note.cx(Mode::Plain, "", 0..0)).unwrap();
        assert_eq!(
            routed.insert,
            Some(format!("{}\n{}", a.display(), b.display()))
        );
        let board = Board::with(&[("public.png", PNG)]);
        let routed = route(&board, &note.cx(Mode::Plain, "", 0..0)).unwrap();
        assert_eq!(routed.insert, None);
        assert!(routed.notice.unwrap().text.contains("image"));
        assert!(!note.vault.path().join("attachments").exists());
    }

    #[test]
    fn titled_link_uses_the_clipboard_title_or_fetches_later() {
        let note = Note::new();
        let board = Board::with(&[
            (offer::UTF8_TEXT, b"https://x.org/p"),
            (URL_NAME, b"The Page"),
        ]);
        let routed = route(&board, &note.cx(Mode::TitledLink, "", 0..0)).unwrap();
        assert_eq!(
            routed.insert.as_deref(),
            Some("[The Page](https://x.org/p)")
        );
        assert!(routed.follow_up.is_none());
        let board = Board::with(&[
            (offer::UTF8_TEXT, b"https://x.org/p"),
            ("public.html", b"<a href=\"https://x.org/p\">Anchor</a>"),
        ]);
        let routed = route(&board, &note.cx(Mode::TitledLink, "", 0..0)).unwrap();
        assert_eq!(routed.insert.as_deref(), Some("[Anchor](https://x.org/p)"));
        // No title on the clipboard: the bare URL now, the title later.
        let board = Board::with(&[(offer::UTF8_TEXT, b"https://x.org/p")]);
        let source = "see  now";
        let routed = route(&board, &note.cx(Mode::TitledLink, source, 4..4)).unwrap();
        assert_eq!(routed.insert.as_deref(), Some("https://x.org/p"));
        match routed.follow_up {
            Some(FollowUp::Title { url, anchor }) => {
                assert_eq!(url, "https://x.org/p");
                assert_eq!(anchor.locate_text("see https://x.org/p now", &url), Some(4));
            }
            other => panic!("{other:?}"),
        }
        // Not a URL: a normal paste.
        let board = Board::with(&[(offer::UTF8_TEXT, b"words")]);
        let routed = route(&board, &note.cx(Mode::TitledLink, "", 0..0)).unwrap();
        assert_eq!(routed.route, Route::Text);
        // mailto: no network.
        let board = Board::with(&[(offer::UTF8_TEXT, b"mailto:a@b.c")]);
        let routed = route(&board, &note.cx(Mode::TitledLink, "", 0..0)).unwrap();
        assert!(routed.follow_up.is_none());
    }

    #[test]
    fn a_plain_file_takes_text_only() {
        let note = Note::new();
        let board = Board::with(&[("public.png", PNG)]);
        let cx = RouteContext {
            note_rel: None,
            ..note.cx(Mode::Smart, "", 0..0)
        };
        let routed = route(&board, &cx).unwrap();
        assert_eq!(routed.insert, None);
        assert!(routed.notice.unwrap().error);
        let board = Board::with(&[(offer::UTF8_TEXT, b"a,b\r\n")]);
        let routed = route(&board, &cx).unwrap();
        assert_eq!(routed.insert.as_deref(), Some("a,b\r\n"));
    }

    #[test]
    fn multi_line_text_continues_a_list_item() {
        let note = Note::new();
        let board = Board::with(&[(offer::UTF8_TEXT, b"one\ntwo")]);
        let routed = smart(&board, &note, "- item ", 7);
        assert_eq!(routed.insert.as_deref(), Some("one\n  two"));
        let routed = smart(&board, &note, "> q ", 4);
        assert_eq!(routed.insert.as_deref(), Some("one\n> two"));
    }

    #[test]
    fn rich_html_becomes_markdown_with_links_resolved() {
        let note = Note::new();
        let board = Board::with(&[
            (
                "public.html",
                b"<h2>Title</h2><p>Some <b>bold</b> and <a href=\"/x\">link</a>.</p>",
            ),
            (CHROMIUM_SOURCE_URL, b"https://ex.org/p/q"),
            (offer::UTF8_TEXT, b"Title\nSome bold and link."),
        ]);
        let routed = smart(&board, &note, "", 0);
        assert_eq!(routed.route, Route::RichText);
        assert_eq!(
            routed.insert.as_deref(),
            Some("## Title\n\nSome **bold** and [link](https://ex.org/x).")
        );
        assert!(routed.follow_up.is_none());
        // A block mid-line goes on its own lines.
        let routed = smart(&board, &note, "text tail", 5);
        assert!(routed
            .insert
            .as_deref()
            .is_some_and(|text| text.starts_with("\n\n## Title") && text.ends_with(".\n\n")));
    }

    #[test]
    fn html_without_formatting_pastes_the_plain_flavor() {
        let note = Note::new();
        let board = Board::with(&[
            (
                "public.html",
                b"<meta charset=utf-8><span>plain words</span><br><span>second line</span>",
            ),
            (offer::UTF8_TEXT, b"plain words\r\nsecond line"),
        ]);
        let routed = smart(&board, &note, "", 0);
        assert_eq!(routed.route, Route::Text);
        assert_eq!(routed.insert.as_deref(), Some("plain words\nsecond line"));
    }

    #[test]
    fn data_images_are_saved_before_the_text_is_inserted() {
        let note = Note::new();
        let board = Board::with(&[
            (
                "public.html",
                b"<p>One <img alt=\"dot\" src=\"data:image/png;base64,iVBORw0KGgo=\"> two</p>",
            ),
            (offer::UTF8_TEXT, b"One  two"),
        ]);
        let routed = smart(&board, &note, "", 0);
        assert_eq!(routed.route, Route::RichText);
        // Nothing is saved on the UI thread (or for a read that is retried).
        assert_eq!(routed.insert, None);
        let image = note
            .vault
            .path()
            .join("attachments/Pasted image 20261023120000.png");
        assert!(!image.exists());
        let Some(FollowUp::Rich { job, .. }) = routed.follow_up else {
            panic!("no background job");
        };
        let done = run_rich_job(job);
        assert_eq!(done.text, "One ![[Pasted image 20261023120000.png]] two");
        assert_eq!(done.files, std::slice::from_ref(&image));
        assert!(image.is_file());
    }

    #[test]
    fn a_one_line_pre_is_plain_text() {
        let note = Note::new();
        let board = Board::with(&[
            (
                "public.html",
                b"<pre style=\"font-family:'JetBrains Mono'\">println(\"hi\")</pre>",
            ),
            (offer::UTF8_TEXT, b"println(\"hi\")"),
        ]);
        let routed = smart(&board, &note, "say ", 4);
        assert_eq!(routed.route, Route::Text);
        assert_eq!(routed.insert.as_deref(), Some("println(\"hi\")"));
    }

    #[test]
    fn a_follow_up_still_replaces_the_selection() {
        let note = Note::new();
        let html = "<p>x</p>".repeat(70_000);
        let board = Board::with(&[("public.html", html.as_bytes())]);
        let routed = smart_selection(&board, &note, "a TODO b", 2..6);
        assert_eq!(routed.insert.as_deref(), Some(""));
        let Some(FollowUp::Rich { anchor, .. }) = routed.follow_up else {
            panic!("no background job");
        };
        assert_eq!(anchor.locate("a  b"), Some(2));
    }

    #[test]
    fn code_pasted_into_a_quoted_fence_keeps_the_quote() {
        let note = Note::new();
        let board = Board::with(&[(offer::UTF8_TEXT, b"a\nb\nc")]);
        let source = "> [!note]\n> ```\n> \n> ```\n";
        let routed = smart(&board, &note, source, 18);
        assert_eq!(routed.route, Route::CodeBlockText);
        assert_eq!(routed.insert.as_deref(), Some("a\n> b\n> c"));
    }

    #[test]
    fn large_tab_separated_text_is_tried_as_a_table_in_the_background() {
        let note = Note::new();
        let text = "a\tb\n".repeat(70_000);
        let board = Board::with(&[(offer::UTF8_TEXT, text.as_bytes())]);
        let routed = smart(&board, &note, "", 0);
        let Some(FollowUp::Rich { job, .. }) = routed.follow_up else {
            panic!("no background job");
        };
        assert!(job.html.is_empty());
        let done = run_rich_job(job);
        assert!(done.text.starts_with("| a"), "{:?}", &done.text[..20]);
    }

    #[test]
    fn remote_images_are_inserted_now_and_downloaded_later() {
        let note = Note::new();
        let board = Board::with(&[
            (
                "public.html",
                b"<p>Look</p><p><img alt=\"cat\" src=\"https://ex.org/cat.png\"></p><p>more</p>",
            ),
            (offer::UTF8_TEXT, b"Look\n\nmore"),
        ]);
        let routed = smart(&board, &note, "", 0);
        assert_eq!(routed.route, Route::RichText);
        let inserted = routed.insert.clone().unwrap();
        assert_eq!(inserted, "Look\n\n![cat](https://ex.org/cat.png)\n\nmore");
        match routed.follow_up {
            Some(FollowUp::RemoteImages {
                images,
                region,
                folder,
                ..
            }) => {
                assert_eq!(images.len(), 1);
                assert_eq!(images[0].url, "https://ex.org/cat.png");
                assert_eq!(
                    region.occurrences(&inserted, &images[0].placeholder).len(),
                    1
                );
                assert_eq!(folder, note.vault.path().join("attachments"));
            }
            other => panic!("{other:?}"),
        }
        // Nothing was written yet.
        assert!(!note.vault.path().join("attachments").exists());
    }

    #[test]
    fn monospace_rich_text_is_code_and_one_line_is_text() {
        let note = Note::new();
        let board = Board::with(&[
            (
                "public.html",
                b"<div style=\"font-family: Menlo\">fn a() {}</div>\
                  <div style=\"font-family: Menlo\">fn b() {}</div>",
            ),
            (offer::UTF8_TEXT, b"fn a() {}\nfn b() {}"),
        ]);
        let routed = smart(&board, &note, "", 0);
        assert_eq!(routed.route, Route::IdeCode);
        let inserted = routed.insert.unwrap();
        assert!(inserted.starts_with("```"), "{inserted:?}");
        assert!(
            inserted.ends_with("\nfn a() {}\nfn b() {}\n```"),
            "{inserted:?}"
        );
        let board = Board::with(&[
            (
                "public.html",
                b"<div style=\"font-family: Menlo\">let x = 1</div>",
            ),
            (offer::UTF8_TEXT, b"let x = 1"),
        ]);
        let routed = smart(&board, &note, "", 0);
        assert_eq!(routed.route, Route::Text);
        assert_eq!(routed.insert.as_deref(), Some("let x = 1"));
    }

    #[test]
    fn a_selection_link_wins_over_the_html_of_a_copied_link() {
        let note = Note::new();
        let board = Board::with(&[
            ("public.html", b"<a href=\"https://x.org/p\">The Page</a>"),
            (offer::UTF8_TEXT, b"https://x.org/p"),
        ]);
        let routed = route(&board, &note.cx(Mode::Smart, "see **this** now", 4..12)).unwrap();
        assert_eq!(routed.route, Route::SelectionLink);
        assert_eq!(
            routed.insert.as_deref(),
            Some("[**this**](https://x.org/p)")
        );
    }

    #[test]
    fn large_html_is_converted_in_the_background() {
        let note = Note::new();
        let html = "<p>x</p>".repeat(70_000);
        let board = Board::with(&[("public.html", html.as_bytes())]);
        let routed = smart(&board, &note, "", 0);
        assert_eq!(routed.route, Route::RichText);
        assert_eq!(routed.insert, None);
        let Some(FollowUp::Rich { job, anchor }) = routed.follow_up else {
            panic!("no background job");
        };
        assert_eq!(anchor.offset, 0);
        let done = run_rich_job(job);
        assert!(done.markdown);
        assert!(done.text.starts_with("x\n\nx"), "{:?}", &done.text[..10]);
        assert!(done.remote.is_empty());
    }
}
