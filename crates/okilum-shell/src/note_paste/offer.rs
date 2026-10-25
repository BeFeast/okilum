//! What a paste offers, chosen from the clipboard's representations (#1602).
//!
//! Platform-neutral: a platform exposes its clipboard through [`Pasteboard`]
//! (types, bytes, its own string decoding and rich-text conversion), and this
//! module picks text or an image the way a native text view would.

use std::path::PathBuf;

/// Largest text a paste accepts (the editor's exact-read limit).
pub const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;
/// RTF larger than this is not parsed into rich text on the UI thread when
/// a plain flavor comes with it (#1611): the plain flavor is the paste.
pub const UI_RTF_BYTES: usize = 256 * 1024;

/// Whether the board has a plain text flavor.
pub fn has_plain_text(types: &[String]) -> bool {
    types
        .iter()
        .any(|t| t == UTF8_TEXT || PLAIN_TEXT.contains(&t.as_str()))
}
/// Largest image a paste saves into the vault.
pub const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
/// Reads repeated when the clipboard changes while it is being read.
const ATTEMPTS: usize = 3;

pub const UTF8_TEXT: &str = "public.utf8-plain-text";
/// Other plain-text representations, decoded by the platform.
/// Clipboard managers (Raycast, Alfred, Maccy, Paste) and older apps write
/// any of these instead of, or next to, `public.utf8-plain-text`.
pub const PLAIN_TEXT: [&str; 8] = [
    "public.plain-text",
    "NSStringPboardType",
    "public.utf16-plain-text",
    "public.utf16-external-plain-text",
    "public.text",
    "com.apple.traditional-mac-plain-text",
    "public.utf8-tab-separated-values-text",
    "public.utf16-tab-separated-values-text",
];
pub const FILE_URL: &str = "public.file-url";
/// Legacy list of copied file paths (a property list of strings).
pub const FILENAMES: &str = "NSFilenamesPboardType";
/// A web or file URL; a web URL pastes as its text.
pub const URL: &str = "public.url";
pub const RTF: [&str; 2] = ["public.rtf", "NeXT Rich Text Format v1.0 pasteboard type"];
pub const RTFD: [&str; 2] = ["com.apple.flat-rtfd", "NeXT RTFD pasteboard type"];
pub const HTML: [&str; 2] = ["public.html", "Apple HTML pasteboard type"];
/// VS Code, Cursor and Windsurf: JSON whose `mode` is the language id (#1611).
pub const VSCODE_EDITOR_DATA: &str = "vscode-editor-data";
/// The page a Chromium browser copied from.
pub const CHROMIUM_SOURCE_URL: &str = "org.chromium.source-url";
pub const PUBLIC_URL: &str = URL;
/// A copied link's title (Safari, Chrome, Finder bookmarks).
pub const URL_NAME: &str = "public.url-name";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    Gif,
    Webp,
    Tiff,
    Bmp,
    Heic,
}

impl ImageKind {
    /// Clipboard image types, most faithful first.
    pub const TYPES: [(&'static str, ImageKind); 8] = [
        ("public.png", ImageKind::Png),
        ("public.jpeg", ImageKind::Jpeg),
        ("public.tiff", ImageKind::Tiff),
        ("NeXT TIFF v4.0 pasteboard type", ImageKind::Tiff),
        ("com.compuserve.gif", ImageKind::Gif),
        ("org.webmproject.webp", ImageKind::Webp),
        ("public.heic", ImageKind::Heic),
        ("com.microsoft.bmp", ImageKind::Bmp),
    ];

    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
            Self::Webp => "webp",
            Self::Tiff => "tiff",
            Self::Bmp => "bmp",
            Self::Heic => "heic",
        }
    }

    /// The kind a file extension names (any case); `None` for other files.
    pub fn from_extension(extension: &str) -> Option<Self> {
        Some(match extension.to_ascii_lowercase().as_str() {
            "png" => Self::Png,
            "jpg" | "jpeg" => Self::Jpeg,
            "gif" => Self::Gif,
            "webp" => Self::Webp,
            "tif" | "tiff" => Self::Tiff,
            "bmp" => Self::Bmp,
            "heic" | "heif" => Self::Heic,
            _ => return None,
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct PastedImage {
    pub bytes: Vec<u8>,
    pub kind: ImageKind,
    /// The image file the clipboard pointed at, when it was a copied file.
    pub source: Option<PathBuf>,
}

impl std::fmt::Debug for PastedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PastedImage")
            .field("bytes", &self.bytes.len())
            .field("kind", &self.kind)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PasteOffer {
    Text(String),
    Image(PastedImage),
    /// Copied files (Finder, or a clipboard manager's cached image/file):
    /// existing local paths, in clipboard order.
    Files(Vec<PathBuf>),
    /// No usable representation; the type names (never contents) for the log.
    Nothing {
        types: Vec<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfferError {
    TooLarge,
    InvalidUtf8,
    /// The clipboard kept changing while it was read.
    Changed,
    Io,
}

/// One platform clipboard, read synchronously.
pub trait Pasteboard {
    /// Increments whenever another writer replaces the contents.
    fn change_count(&self) -> i64;
    fn types(&self) -> Vec<String>;
    fn data(&self, kind: &str) -> Option<Vec<u8>>;
    /// Every item's file URL (several copied files are several items). The
    /// default reads the first item only.
    fn file_urls(&self) -> Vec<String> {
        self.data(FILE_URL)
            .map(|d| String::from_utf8_lossy(&d).into_owned())
            .or_else(|| self.string(FILE_URL))
            .into_iter()
            .collect()
    }
    /// The platform's own decoding of a text representation.
    fn string(&self, kind: &str) -> Option<String>;
    /// Plain text of RTF (`rtfd == false`) or flattened RTFD data.
    fn rich_text(&self, data: &[u8], rtfd: bool) -> Option<String>;
    /// RTF or RTFD on the board as simple HTML (`b`, `i`, `s`, `code`, `a`,
    /// `p`, `br`, lists) for the Markdown converter (#1611). Reads the rich
    /// data only when called. `None`: no rich text, or no platform reader.
    fn rich_html(&self) -> Option<String> {
        None
    }
    /// One flavor as text: the platform's string, else its bytes as UTF-8.
    /// `None` when the board does not have it.
    fn text_flavor(&self, kind: &str) -> Option<String> {
        self.string(kind).or_else(|| {
            self.data(kind).map(|d| {
                String::from_utf8_lossy(&d)
                    .trim_end_matches('\0')
                    .to_owned()
            })
        })
    }
    /// VS Code, Cursor and Windsurf's `vscode-editor-data` JSON (its `mode`
    /// is the language id), read only when called (#1611).
    fn vscode_editor_data(&self) -> Option<String> {
        self.text_flavor(VSCODE_EDITOR_DATA)
    }
    /// The page a Chromium browser copied from (`org.chromium.source-url`).
    fn source_url(&self) -> Option<String> {
        self.text_flavor(CHROMIUM_SOURCE_URL)
            .map(|url| url.trim().to_owned())
            .filter(|url| !url.is_empty())
    }
    /// A copied link: `public.url` and its title `public.url-name`, if any.
    fn url_with_name(&self) -> Option<(String, Option<String>)> {
        let url = self
            .text_flavor(URL)
            .map(|url| url.trim().to_owned())
            .filter(|url| !url.is_empty())?;
        let name = self
            .text_flavor(URL_NAME)
            .map(|name| name.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|name| !name.is_empty());
        Some((url, name))
    }
    /// The HTML flavor (`public.html`, then the legacy Apple type) as text.
    fn html_string(&self) -> Option<String> {
        HTML.into_iter().find_map(|kind| self.text_flavor(kind))
    }
}

/// The board's plain text flavor only (`public.utf8-plain-text`, then the
/// legacy plain types), never converted from rich text or HTML. `Ok(None)`
/// when there is none.
pub fn plain_text(
    board: &(impl Pasteboard + ?Sized),
    types: &[String],
) -> Result<Option<String>, OfferError> {
    let has = |kind: &str| types.iter().any(|t| t == kind);
    // Empty text in one type falls through to the next (#1602: clipboard
    // managers write several); empty everywhere is empty text.
    let mut empty = false;
    if has(UTF8_TEXT) {
        if let Some(bytes) = board.data(UTF8_TEXT) {
            if bytes.len() > MAX_TEXT_BYTES {
                return Err(OfferError::TooLarge);
            }
            match String::from_utf8(bytes) {
                Ok(text) if !text.is_empty() => return Ok(Some(text)),
                Ok(_) => empty = true,
                Err(_) => {}
            }
        }
    }
    for kind in PLAIN_TEXT.into_iter().filter(|kind| has(kind)) {
        match board.string(kind) {
            Some(text) if text.len() > MAX_TEXT_BYTES => return Err(OfferError::TooLarge),
            Some(text) if !text.is_empty() => return Ok(Some(text)),
            Some(_) => empty = true,
            None => {}
        }
    }
    Ok(empty.then(String::new))
}

/// The board's HTML flavor, if any (read only when called).
pub fn html(
    board: &(impl Pasteboard + ?Sized),
    types: &[String],
) -> Result<Option<String>, OfferError> {
    for kind in HTML
        .into_iter()
        .filter(|kind| types.iter().any(|t| t == kind))
    {
        let html = board.string(kind).or_else(|| {
            board
                .data(kind)
                .map(|d| String::from_utf8_lossy(&d).into_owned())
        });
        if let Some(html) = html {
            if html.len() > MAX_TEXT_BYTES * 4 {
                return Err(OfferError::TooLarge);
            }
            return Ok(Some(html));
        }
    }
    Ok(None)
}

/// Whether the board offers image data (not a file).
pub fn has_image_data(types: &[String]) -> bool {
    ImageKind::TYPES
        .iter()
        .any(|(kind, _)| types.iter().any(|t| t == kind))
}

/// The most faithful image data on the board.
pub fn image_data(
    board: &(impl Pasteboard + ?Sized),
    types: &[String],
) -> Result<Option<PastedImage>, OfferError> {
    for (kind, image_kind) in ImageKind::TYPES {
        if !types.iter().any(|t| t == kind) {
            continue;
        }
        match board.data(kind) {
            Some(bytes) if bytes.len() > MAX_IMAGE_BYTES => return Err(OfferError::TooLarge),
            Some(bytes) if !bytes.is_empty() => {
                return Ok(Some(PastedImage {
                    bytes,
                    kind: image_kind,
                    source: None,
                }))
            }
            _ => {}
        }
    }
    Ok(None)
}

/// Video files a feedback report takes (#1611, #1609).
pub const VIDEO_EXTENSIONS: [&str; 4] = ["mp4", "mov", "m4v", "webm"];

/// What the Feedback window attaches from the clipboard instead of pasting a
/// path (#1609): copied image or video files, or image data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipAttachment {
    Files(Vec<PathBuf>),
    Image(PastedImage),
}

/// The media on the board, if any. Files win over image data: Raycast's
/// clipboard history offers its cached file (a file URL, or only its path as
/// text), and its text is that path (#1609).
pub fn read_attachment(board: &(impl Pasteboard + ?Sized)) -> Option<ClipAttachment> {
    let types = board.types();
    let media: Vec<PathBuf> = clipboard_files(board, &types)
        .into_iter()
        .filter(|path| is_media_file(path))
        .collect();
    if !media.is_empty() {
        return Some(ClipAttachment::Files(media));
    }
    if !has_image_data(&types) {
        return None;
    }
    // The router's step 2 rule: image data that comes with real text or rich
    // HTML (Excel and Numbers add a picture of copied cells) is not the
    // paste; the text is.
    let text = plain_text(board, &types).ok().flatten();
    if text
        .as_deref()
        .is_some_and(|text| !super::router::text_is_incidental(text))
    {
        return None;
    }
    let rich = html(board, &types).ok().flatten();
    if rich
        .as_deref()
        .is_some_and(|html| !super::router::html_is_only_an_image(html))
    {
        return None;
    }
    image_data(board, &types)
        .ok()
        .flatten()
        .map(ClipAttachment::Image)
}

/// An image or video file by its extension, or an image by its first bytes
/// when it has none (Raycast's cache names some files `file-…`).
pub fn is_media_file(path: &std::path::Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(extension) => {
            super::attachments::is_image_extension(extension)
                || VIDEO_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
        }
        None => {
            let mut head = [0u8; 16];
            let read = std::fs::File::open(path)
                .and_then(|mut file| std::io::Read::read(&mut file, &mut head))
                .unwrap_or(0);
            super::attachments::sniff_image(&head[..read]).is_some()
        }
    }
}

/// The local files a paste of this board means, in clipboard order: every
/// item's file URL, the legacy filename list, a file `public.url`, or, with
/// none of those, a plain text that is just the path of a file in Raycast's
/// clipboard cache (#1602). Reads only the type list and small flavors.
pub fn clipboard_files(board: &(impl Pasteboard + ?Sized), types: &[String]) -> Vec<PathBuf> {
    let paths = file_paths(board, types);
    if !paths.is_empty() {
        return paths;
    }
    plain_text(board, types)
        .ok()
        .flatten()
        .and_then(|text| cached_clipboard_file(&text))
        .into_iter()
        .collect()
}

/// The offer on the board. A change *before* the read is simply the newest
/// contents (a clipboard manager writes and pastes at once); a change *during*
/// the read is read again instead of failing the paste.
pub fn read_offer(board: &impl Pasteboard) -> Result<PasteOffer, OfferError> {
    read_offer_with(board, true)
}

/// Like [`read_offer`]; `files == false` for a text-only field, where a copied
/// file pastes as its name (the text next to it) rather than as a file.
pub fn read_offer_with(board: &impl Pasteboard, files: bool) -> Result<PasteOffer, OfferError> {
    for _ in 0..ATTEMPTS {
        let before = board.change_count();
        let offer = read_once(board, files);
        if board.change_count() == before {
            return offer;
        }
    }
    Err(OfferError::Changed)
}

fn read_once(board: &impl Pasteboard, files: bool) -> Result<PasteOffer, OfferError> {
    let types = board.types();
    let has = |kind: &str| types.iter().any(|t| t == kind);
    // A copied file (Finder) also offers its name as text; the file wins.
    // Raycast's clipboard history offers an image or file *only* as a file URL
    // into its cache, so without this a Raycast paste was "nothing" (#1602).
    if files {
        let paths = file_paths(board, &types);
        if !paths.is_empty() {
            return Ok(PasteOffer::Files(paths));
        }
    }
    let mut text_error = None;
    let mut empty_text = false;
    if has(UTF8_TEXT) {
        match board.data(UTF8_TEXT) {
            Some(bytes) if bytes.len() > MAX_TEXT_BYTES => return Err(OfferError::TooLarge),
            Some(bytes) => match String::from_utf8(bytes) {
                Ok(text) if !text.is_empty() => return Ok(text_offer(text, files)),
                Ok(_) => empty_text = true,
                Err(_) => text_error = Some(OfferError::InvalidUtf8),
            },
            None => text_error = Some(OfferError::Io),
        }
    }
    for kind in PLAIN_TEXT.into_iter().filter(|kind| has(kind)) {
        match board.string(kind) {
            Some(text) if text.len() > MAX_TEXT_BYTES => return Err(OfferError::TooLarge),
            Some(text) if !text.is_empty() => return Ok(text_offer(text, files)),
            Some(_) => empty_text = true,
            None => {}
        }
    }
    // Only rich text and an image: a browser's Copy Image. Take the image.
    for (kind, image_kind) in ImageKind::TYPES {
        if !has(kind) {
            continue;
        }
        match board.data(kind) {
            Some(bytes) if bytes.len() > MAX_IMAGE_BYTES => return Err(OfferError::TooLarge),
            Some(bytes) if !bytes.is_empty() => {
                return Ok(PasteOffer::Image(PastedImage {
                    bytes,
                    kind: image_kind,
                    source: None,
                }))
            }
            _ => {}
        }
    }
    let rich = RTF
        .into_iter()
        .map(|kind| (kind, false))
        .chain(RTFD.into_iter().map(|kind| (kind, true)));
    for (kind, rtfd) in rich.filter(|(kind, _)| has(kind)) {
        let Some(data) = board.data(kind) else {
            continue;
        };
        if data.len() > MAX_TEXT_BYTES * 4 {
            return Err(OfferError::TooLarge);
        }
        if let Some(text) = board.rich_text(&data, rtfd).filter(|t| !t.is_empty()) {
            return Ok(PasteOffer::Text(text));
        }
    }
    for kind in HTML.into_iter().filter(|kind| has(kind)) {
        let html = board.string(kind).or_else(|| {
            board
                .data(kind)
                .map(|d| String::from_utf8_lossy(&d).into_owned())
        });
        let Some(html) = html else {
            continue;
        };
        if html.len() > MAX_TEXT_BYTES * 4 {
            return Err(OfferError::TooLarge);
        }
        let text = html_to_text(&html);
        if !text.is_empty() {
            return Ok(PasteOffer::Text(text));
        }
    }
    // A web link alone pastes as its address.
    if has(URL) {
        let url = board.string(URL).or_else(|| {
            board
                .data(URL)
                .map(|d| String::from_utf8_lossy(&d).into_owned())
        });
        if let Some(url) = url.map(|u| u.trim_end_matches('\0').trim().to_owned()) {
            if !url.is_empty() && !url.starts_with("file:") && url.len() <= MAX_TEXT_BYTES {
                return Ok(PasteOffer::Text(url));
            }
        }
    }
    if empty_text {
        return Ok(PasteOffer::Text(String::new()));
    }
    match text_error {
        Some(error) => Err(error),
        None => Ok(PasteOffer::Nothing { types }),
    }
}

/// Text, unless it is just the path of a file in Raycast's clipboard cache:
/// Raycast can offer a history image as that path (#1609), which is the image.
fn text_offer(text: String, files: bool) -> PasteOffer {
    if files {
        if let Some(path) = cached_clipboard_file(&text) {
            return PasteOffer::Files(vec![path]);
        }
    }
    PasteOffer::Text(text)
}

/// The existing file a single-line Raycast clipboard-cache path names.
pub fn cached_clipboard_file(text: &str) -> Option<PathBuf> {
    let line = text.trim();
    if line.is_empty() || line.len() > 4096 || line.contains('\n') {
        return None;
    }
    let path = local_path(line)?;
    let normalized = path.to_string_lossy().replace('\\', "/");
    (normalized.contains("/com.raycast.macos/clipboard/") && path.is_file()).then_some(path)
}

/// Local files the clipboard points at that exist, without duplicates:
/// every item's `public.file-url`, the legacy filename list, a file `public.url`.
pub fn file_paths(board: &(impl Pasteboard + ?Sized), types: &[String]) -> Vec<PathBuf> {
    let has = |kind: &str| types.iter().any(|t| t == kind);
    let mut raw: Vec<String> = Vec::new();
    if has(FILE_URL) {
        raw.extend(board.file_urls());
    }
    if has(FILENAMES) {
        if let Some(list) = board
            .data(FILENAMES)
            .map(|d| String::from_utf8_lossy(&d).into_owned())
            .or_else(|| board.string(FILENAMES))
        {
            raw.extend(plist_strings(&list));
        }
    }
    if raw.is_empty() && has(URL) {
        raw.extend(board.string(URL).or_else(|| {
            board
                .data(URL)
                .map(|d| String::from_utf8_lossy(&d).into_owned())
        }));
    }
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in raw {
        let Some(path) = local_path(&entry) else {
            continue;
        };
        if path.exists() && !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

/// A `file://` URL or an absolute path as a local path.
pub fn local_path(entry: &str) -> Option<PathBuf> {
    let entry = entry.trim_end_matches('\0').trim();
    if entry.starts_with('/') {
        return Some(PathBuf::from(entry));
    }
    let url = url::Url::parse(entry).ok()?;
    if url.scheme() != "file" {
        return None;
    }
    url.to_file_path().ok()
}

/// The `<string>` values of an XML property list (the legacy filename list).
fn plist_strings(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<string>") {
        rest = &rest[start + "<string>".len()..];
        let Some(end) = rest.find("</string>") else {
            break;
        };
        out.push(decode_entities(&rest[..end]));
        rest = &rest[end..];
    }
    out
}

/// Plain text of an HTML fragment: tags dropped, blocks on their own lines,
/// entities decoded, whitespace collapsed outside `<pre>`.
pub fn html_to_text(html: &str) -> String {
    const BLOCKS: [&str; 22] = [
        "p",
        "div",
        "br",
        "li",
        "tr",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "blockquote",
        "pre",
        "ul",
        "ol",
        "table",
        "section",
        "article",
        "header",
        "footer",
        "hr",
        "dd",
    ];
    let mut out = String::new();
    let mut rest = html;
    let mut pre = 0usize;
    let mut skip: Option<&str> = None;
    while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix("<!--") {
            rest = stripped.find("-->").map_or("", |end| &stripped[end + 3..]);
            continue;
        }
        if rest.starts_with('<') {
            let Some(end) = rest.find('>') else {
                break;
            };
            let tag = &rest[1..end];
            rest = &rest[end + 1..];
            let closing = tag.starts_with('/');
            let name: String = tag
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            if let Some(open) = skip {
                if closing && name == open {
                    skip = None;
                }
                continue;
            }
            match name.as_str() {
                "script" | "style" | "head" | "title" if !closing => {
                    skip = Some(match name.as_str() {
                        "script" => "script",
                        "style" => "style",
                        "head" => "head",
                        _ => "title",
                    })
                }
                "pre" if closing => pre = pre.saturating_sub(1),
                "pre" => pre += 1,
                _ => {}
            }
            if name == "br" {
                out.push('\n');
            } else if name == "td" || name == "th" {
                if closing {
                    out.push('\t');
                }
            } else if BLOCKS.contains(&name.as_str()) && !out.ends_with('\n') && !out.is_empty() {
                out.push('\n');
            }
            continue;
        }
        let end = rest.find('<').unwrap_or(rest.len());
        let chunk = decode_entities(&rest[..end]);
        rest = &rest[end..];
        if skip.is_some() {
            continue;
        }
        if pre > 0 {
            out.push_str(&chunk);
        } else {
            for c in chunk.chars() {
                if c.is_whitespace() && c != '\u{a0}' {
                    if !out.is_empty() && !out.ends_with([' ', '\n', '\t']) {
                        out.push(' ');
                    }
                } else {
                    out.push(if c == '\u{a0}' { ' ' } else { c });
                }
            }
        }
    }
    let lines: Vec<&str> = out.lines().map(|line| line.trim_end()).collect();
    let mut text = String::new();
    let mut blank = 0;
    for line in lines {
        if line.is_empty() {
            blank += 1;
            if blank > 1 || text.is_empty() {
                continue;
            }
        } else {
            blank = 0;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(line);
    }
    text.trim_end().to_string()
}

pub(crate) fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        // Search bytes: a 12-byte cut can split a character, ';' cannot.
        let window = &rest.as_bytes()[..rest.len().min(12)];
        let Some(end) = window.iter().position(|&b| b == b';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|n| n.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::BTreeMap;

    /// A clipboard in memory; `writes` replaces the contents on the given reads
    /// of `change_count`, like another app writing mid-paste.
    #[derive(Default)]
    struct Board {
        items: BTreeMap<String, Vec<u8>>,
        count: Cell<i64>,
        reads: Cell<usize>,
        writes: Vec<(usize, BTreeMap<String, Vec<u8>>)>,
        current: std::cell::RefCell<Option<BTreeMap<String, Vec<u8>>>>,
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
        fn items(&self) -> BTreeMap<String, Vec<u8>> {
            self.current
                .borrow()
                .clone()
                .unwrap_or_else(|| self.items.clone())
        }
    }
    impl Pasteboard for Board {
        fn change_count(&self) -> i64 {
            let read = self.reads.get();
            self.reads.set(read + 1);
            if let Some((_, items)) = self.writes.iter().find(|(at, _)| *at == read) {
                *self.current.borrow_mut() = Some(items.clone());
                self.count.set(self.count.get() + 1);
            }
            self.count.get()
        }
        fn types(&self) -> Vec<String> {
            self.items().keys().cloned().collect()
        }
        fn data(&self, kind: &str) -> Option<Vec<u8>> {
            self.items().get(kind).cloned()
        }
        fn string(&self, kind: &str) -> Option<String> {
            self.data(kind)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        }
        fn rich_text(&self, data: &[u8], _: bool) -> Option<String> {
            // Stand-in for NSAttributedString: words that are not control words.
            let text = String::from_utf8_lossy(data).replace(['{', '}'], " ");
            let words: Vec<&str> = text
                .split_whitespace()
                .filter(|word| !word.starts_with('\\'))
                .collect();
            Some(words.join(" "))
        }
    }

    fn text(offer: Result<PasteOffer, OfferError>) -> String {
        match offer {
            Ok(PasteOffer::Text(text)) => text,
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn utf8_text_is_exact_bytes() {
        let board = Board::with(&[(UTF8_TEXT, "a\r\nб 🧠".as_bytes())]);
        assert_eq!(text(read_offer(&board)), "a\r\nб 🧠");
    }

    #[test]
    fn legacy_plain_text_types_are_accepted() {
        for kind in PLAIN_TEXT {
            let board = Board::with(&[(kind, b"legacy")]);
            assert_eq!(text(read_offer(&board)), "legacy", "{kind}");
        }
    }

    #[test]
    fn rich_text_only_becomes_plain_text() {
        let board = Board::with(&[("public.rtf", br"{\rtf1\ansi {\b bold} plain}")]);
        assert_eq!(text(read_offer(&board)), "bold plain");
        let html = "<html><head><style>p{}</style></head><body><!--StartFragment-->\
                    <p>One &amp; <b>two</b></p><ul><li>three&nbsp;x</li><li>&#x1F9E0;</li></ul>\
                    <pre>  keep\n  this</pre></body></html>";
        let board = Board::with(&[("public.html", html.as_bytes())]);
        assert_eq!(
            text(read_offer(&board)),
            "One & two\nthree x\n🧠\n  keep\n  this"
        );
    }

    #[test]
    fn non_ascii_after_an_ampersand_is_kept() {
        assert_eq!(
            decode_entities("A & 日本語のテキスト"),
            "A & 日本語のテキスト"
        );
        assert_eq!(decode_entities("&日本語;&amp;"), "&日本語;&");
        assert_eq!(html_to_text("<p>Tom &Jerry — été</p>"), "Tom &Jerry — été");
    }

    #[test]
    fn plain_text_wins_over_rich_text_and_images() {
        let board = Board::with(&[
            (UTF8_TEXT, b"plain"),
            ("public.html", b"<b>rich</b>"),
            ("public.png", b"\x89PNG"),
        ]);
        assert_eq!(text(read_offer(&board)), "plain");
    }

    #[test]
    fn image_wins_over_rich_text_only() {
        let board = Board::with(&[
            ("public.html", b"<img src=\"x.png\">"),
            ("public.tiff", b"II*\0"),
        ]);
        match read_offer(&board) {
            Ok(PasteOffer::Image(image)) => assert_eq!(image.kind, ImageKind::Tiff),
            other => panic!("{other:?}"),
        }
        let board = Board::with(&[("public.png", b"\x89PNG"), ("public.tiff", b"II*\0")]);
        match read_offer(&board) {
            Ok(PasteOffer::Image(image)) => assert_eq!(image.kind, ImageKind::Png),
            other => panic!("{other:?}"),
        }
    }

    /// #1602: a copied file (Finder) or a Raycast clipboard-history image or
    /// file (a file URL into its cache, nothing else) is offered as the file.
    #[test]
    fn copied_files_are_offered_as_files_not_their_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.PNG");
        std::fs::write(&path, b"\x89PNG data").unwrap();
        let url = url::Url::from_file_path(&path).unwrap().to_string();
        let board = Board::with(&[(FILE_URL, url.as_bytes()), (UTF8_TEXT, b"shot.PNG")]);
        assert_eq!(
            read_offer(&board),
            Ok(PasteOffer::Files(vec![path.clone()]))
        );
        // Raycast: the file URL alone.
        let board = Board::with(&[(FILE_URL, url.as_bytes())]);
        assert_eq!(
            read_offer(&board),
            Ok(PasteOffer::Files(vec![path.clone()]))
        );
        let doc = dir.path().join("a b.pdf");
        std::fs::write(&doc, b"%PDF").unwrap();
        let doc_url = url::Url::from_file_path(&doc).unwrap().to_string();
        let board = Board::with(&[(FILE_URL, doc_url.as_bytes()), (UTF8_TEXT, b"a b.pdf")]);
        assert_eq!(read_offer(&board), Ok(PasteOffer::Files(vec![doc.clone()])));
        // A text-only field still takes the name.
        assert_eq!(text(read_offer_with(&board, false)), "a b.pdf");
        // The legacy filename list, several files, duplicates once.
        let list = format!(
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><array>\
             <string>{}</string><string>{}</string><string>{}</string></array></plist>",
            path.display(),
            doc.display(),
            path.display()
        );
        let board = Board::with(&[(FILENAMES, list.as_bytes())]);
        assert_eq!(
            read_offer(&board),
            Ok(PasteOffer::Files(vec![path.clone(), doc]))
        );
        // A file URL whose file is gone falls back to the text next to it.
        let gone = url::Url::from_file_path(dir.path().join("gone.png"))
            .unwrap()
            .to_string();
        let board = Board::with(&[(FILE_URL, gone.as_bytes()), (UTF8_TEXT, b"gone.png")]);
        assert_eq!(text(read_offer(&board)), "gone.png");
        // Raycast's history image offered as its cache path is the image.
        let cache = dir.path().join("com.raycast.macos/clipboard");
        std::fs::create_dir_all(&cache).unwrap();
        let cached = cache.join("file-1.png");
        std::fs::write(&cached, b"\x89PNG").unwrap();
        let board = Board::with(&[(UTF8_TEXT, cached.to_string_lossy().as_bytes())]);
        assert_eq!(
            read_offer(&board),
            Ok(PasteOffer::Files(vec![cached.clone()]))
        );
        assert_eq!(
            text(read_offer_with(&board, false)),
            cached.to_string_lossy()
        );
        // Any other path is just text.
        let board = Board::with(&[(UTF8_TEXT, path.to_string_lossy().as_bytes())]);
        assert_eq!(text(read_offer(&board)), path.to_string_lossy());
        // A web URL alone pastes as its address.
        let board = Board::with(&[(URL, b"https://example.com/a")]);
        assert_eq!(text(read_offer(&board)), "https://example.com/a");
    }

    #[cfg(unix)]
    #[test]
    fn local_paths_from_urls_and_paths() {
        assert_eq!(
            local_path("file:///Users/o/Library/Application%20Support/com.raycast.macos/clipboard/file-1.png\0"),
            Some(PathBuf::from(
                "/Users/o/Library/Application Support/com.raycast.macos/clipboard/file-1.png"
            ))
        );
        assert_eq!(local_path("/tmp/x.png"), Some(PathBuf::from("/tmp/x.png")));
        assert_eq!(local_path("https://example.com/x.png"), None);
        assert_eq!(local_path("x.png"), None);
    }

    /// Raycast and other clipboard managers write these text types.
    #[test]
    fn every_plain_text_type_is_text() {
        for kind in PLAIN_TEXT {
            let board = Board::with(&[
                (kind, "Привет".as_bytes()),
                ("org.nspasteboard.source", b"com.raycast.macos"),
            ]);
            assert_eq!(text(read_offer(&board)), "Привет", "{kind}");
        }
    }

    #[test]
    fn nothing_usable_reports_the_types() {
        let board = Board::with(&[("com.example.private", b"x")]);
        assert_eq!(
            read_offer(&board),
            Ok(PasteOffer::Nothing {
                types: vec!["com.example.private".into()]
            })
        );
        assert_eq!(
            read_offer(&Board::default()),
            Ok(PasteOffer::Nothing { types: vec![] })
        );
        let board = Board::with(&[(UTF8_TEXT, b"")]);
        assert_eq!(read_offer(&board), Ok(PasteOffer::Text(String::new())));
        let board = Board::with(&[(UTF8_TEXT, &[0xff])]);
        assert_eq!(read_offer(&board), Err(OfferError::InvalidUtf8));
        let big = vec![b'x'; MAX_TEXT_BYTES + 1];
        let board = Board::with(&[(UTF8_TEXT, &big)]);
        assert_eq!(read_offer(&board), Err(OfferError::TooLarge));
    }

    /// Raycast writes the clipboard and posts Cmd+V at once: the change before
    /// the read is the content; a change during the read is read again.
    #[test]
    fn change_count_race_is_tolerated() {
        let mut board = Board::with(&[(UTF8_TEXT, b"old")]);
        board.count.set(41);
        // Changed just before the paste: the newest contents, no error.
        board.writes = vec![(
            0,
            [(UTF8_TEXT.to_string(), b"from raycast".to_vec())].into(),
        )];
        assert_eq!(text(read_offer(&board)), "from raycast");
        // Changed during the first read (after its first change_count): read again.
        let mut board = Board::with(&[(UTF8_TEXT, b"old")]);
        board.writes = vec![(1, [(UTF8_TEXT.to_string(), b"new".to_vec())].into())];
        assert_eq!(text(read_offer(&board)), "new");
        // A clipboard that never settles fails after a bounded number of reads.
        let mut board = Board::with(&[(UTF8_TEXT, b"old")]);
        board.writes = (0..10)
            .filter(|n| n % 2 == 1)
            .map(|n| (n, [(UTF8_TEXT.to_string(), b"x".to_vec())].into()))
            .collect();
        assert_eq!(read_offer(&board), Err(OfferError::Changed));
    }
}
