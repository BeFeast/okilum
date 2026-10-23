//! What a paste offers, chosen from the clipboard's representations (#1602).
//!
//! Platform-neutral: a platform exposes its clipboard through [`Pasteboard`]
//! (types, bytes, its own string decoding and rich-text conversion), and this
//! module picks text or an image the way a native text view would.

use std::path::PathBuf;

/// Largest text a paste accepts (the editor's exact-read limit).
pub const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;
/// Largest image a paste saves into the vault.
pub const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
/// Reads repeated when the clipboard changes while it is being read.
const ATTEMPTS: usize = 3;

pub const UTF8_TEXT: &str = "public.utf8-plain-text";
/// Other plain-text representations, decoded by the platform.
pub const PLAIN_TEXT: [&str; 4] = [
    "public.plain-text",
    "NSStringPboardType",
    "public.utf16-plain-text",
    "public.utf16-external-plain-text",
];
pub const FILE_URL: &str = "public.file-url";
pub const RTF: [&str; 2] = ["public.rtf", "NeXT Rich Text Format v1.0 pasteboard type"];
pub const RTFD: [&str; 2] = ["com.apple.flat-rtfd", "NeXT RTFD pasteboard type"];
pub const HTML: [&str; 2] = ["public.html", "Apple HTML pasteboard type"];

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
    /// The platform's own decoding of a text representation.
    fn string(&self, kind: &str) -> Option<String>;
    /// Plain text of RTF (`rtfd == false`) or flattened RTFD data.
    fn rich_text(&self, data: &[u8], rtfd: bool) -> Option<String>;
}

/// The offer on the board. A change *before* the read is simply the newest
/// contents (a clipboard manager writes and pastes at once); a change *during*
/// the read is read again instead of failing the paste.
pub fn read_offer(board: &impl Pasteboard) -> Result<PasteOffer, OfferError> {
    for _ in 0..ATTEMPTS {
        let before = board.change_count();
        let offer = read_once(board);
        if board.change_count() == before {
            return offer;
        }
    }
    Err(OfferError::Changed)
}

fn read_once(board: &impl Pasteboard) -> Result<PasteOffer, OfferError> {
    let types = board.types();
    let has = |kind: &str| types.iter().any(|t| t == kind);
    // A copied image file (Finder) also offers its name as text; the image wins.
    if has(FILE_URL) {
        if let Some(image) = file_image(board)? {
            return Ok(PasteOffer::Image(image));
        }
    }
    let mut text_error = None;
    let mut empty_text = false;
    if has(UTF8_TEXT) {
        match board.data(UTF8_TEXT) {
            Some(bytes) if bytes.len() > MAX_TEXT_BYTES => return Err(OfferError::TooLarge),
            Some(bytes) => match String::from_utf8(bytes) {
                Ok(text) if !text.is_empty() => return Ok(PasteOffer::Text(text)),
                Ok(_) => empty_text = true,
                Err(_) => text_error = Some(OfferError::InvalidUtf8),
            },
            None => text_error = Some(OfferError::Io),
        }
    }
    for kind in PLAIN_TEXT.into_iter().filter(|kind| has(kind)) {
        match board.string(kind) {
            Some(text) if text.len() > MAX_TEXT_BYTES => return Err(OfferError::TooLarge),
            Some(text) if !text.is_empty() => return Ok(PasteOffer::Text(text)),
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
    if empty_text {
        return Ok(PasteOffer::Text(String::new()));
    }
    match text_error {
        Some(error) => Err(error),
        None => Ok(PasteOffer::Nothing { types }),
    }
}

/// The image a copied file URL points at, if it is one.
fn file_image(board: &impl Pasteboard) -> Result<Option<PastedImage>, OfferError> {
    let Some(raw) = board
        .data(FILE_URL)
        .map(|d| String::from_utf8_lossy(&d).into_owned())
        .or_else(|| board.string(FILE_URL))
    else {
        return Ok(None);
    };
    let Some(path) = url::Url::parse(raw.trim_end_matches('\0').trim())
        .ok()
        .and_then(|url| url.to_file_path().ok())
    else {
        return Ok(None);
    };
    let Some(kind) = path
        .extension()
        .and_then(|e| e.to_str())
        .and_then(ImageKind::from_extension)
    else {
        return Ok(None);
    };
    let size = std::fs::metadata(&path).map_err(|_| OfferError::Io)?.len();
    if size > MAX_IMAGE_BYTES as u64 {
        return Err(OfferError::TooLarge);
    }
    let bytes = std::fs::read(&path).map_err(|_| OfferError::Io)?;
    Ok(Some(PastedImage {
        bytes,
        kind,
        source: Some(path),
    }))
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

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest[..rest.len().min(12)].find(';') else {
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

    #[test]
    fn copied_image_file_is_an_image_not_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.PNG");
        std::fs::write(&path, b"\x89PNG data").unwrap();
        let url = url::Url::from_file_path(&path).unwrap().to_string();
        let board = Board::with(&[(FILE_URL, url.as_bytes()), (UTF8_TEXT, b"shot.PNG")]);
        match read_offer(&board) {
            Ok(PasteOffer::Image(image)) => {
                assert_eq!(image.kind, ImageKind::Png);
                assert_eq!(image.bytes, b"\x89PNG data");
            }
            other => panic!("{other:?}"),
        }
        // A copied document is still pasted as its name.
        let doc = dir.path().join("a.pdf");
        std::fs::write(&doc, b"%PDF").unwrap();
        let url = url::Url::from_file_path(&doc).unwrap().to_string();
        let board = Board::with(&[(FILE_URL, url.as_bytes()), (UTF8_TEXT, b"a.pdf")]);
        assert_eq!(text(read_offer(&board)), "a.pdf");
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
