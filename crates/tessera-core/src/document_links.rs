//! Document link syntax and navigation intent shared by all reader surfaces.
//! Comrak defines syntax; byte ranges refer to the untouched authored source.
mod paths;
pub mod prepared;
pub(crate) use paths::fallback_links;
pub use paths::{markdown_path, parse_in_vault};

use crate::{render, Resolution, Vault};
use comrak::{nodes::NodeValue, parse_document, Arena};
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedLink {
    pub range: Range<usize>,
    pub label: String,
    pub title: String,
    pub target: String,
    pub wiki: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Destination {
    Note {
        path: String,
        heading: Option<String>,
    },
    External(String),
    Unsupported(&'static str),
}

/// Decode exactly once, after splitting the authored fragment delimiter.
pub fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(a), Some(b)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                out.push((a * 16 + b) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| value.to_owned())
}

/// Escape transport delimiters too: a literal `%23` filename must round-trip.
pub fn encode(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || "/-._~".contains(ch) || !ch.is_ascii() {
            out.push(ch);
        } else {
            for byte in ch.to_string().bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
}

/// Image types supported by Reader and managed opaque-byte preview.
pub fn image_media_type(path: &str) -> Option<&'static str> {
    match std::path::Path::new(path)
        .extension()?
        .to_str()?
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "svg" => Some("image/svg+xml"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

/// Protocols the desktop Reader can hand to the system URL handler.
pub fn is_external_url(target: &str) -> bool {
    target.split_once(':').is_some_and(|(scheme, _)| {
        ["http", "https", "mailto", "tel", "ftp"]
            .iter()
            .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
    })
}

pub fn destination(target: &str, wiki: bool) -> Destination {
    if !wiki && is_external_url(target) {
        return Destination::External(target.into());
    }
    let (path, fragment) = target
        .split_once('#')
        .map_or((target, None), |(p, f)| (p, Some(f)));
    let path = if wiki {
        path.trim().to_owned()
    } else {
        decode(path)
    };
    let heading = fragment.map(|f| {
        if wiki {
            f.trim().to_owned()
        } else {
            decode(f).trim().to_owned()
        }
    });
    // `note#^id` lands on a block (#651); a `^` in the path is not Obsidian's.
    if path.contains('^') {
        return Destination::Unsupported("Block references are not supported.");
    }
    if heading.as_ref().is_some_and(String::is_empty) {
        return Destination::Unsupported("The heading target is empty.");
    }
    if heading.as_deref() == Some("^") {
        return Destination::Unsupported("The block target is empty.");
    }
    if !wiki && (path.contains(':') || path.contains('\\') || path.starts_with('!')) {
        return Destination::Unsupported("This URI or filesystem link is not supported.");
    }
    if !wiki && !path.is_empty() && !path.to_lowercase().ends_with(".md") {
        return Destination::Unsupported("Only local .md document links are supported; attachment and extensionless actions are not available.");
    }
    if path.is_empty() && heading.is_none() {
        return Destination::Unsupported("This link has no destination.");
    }
    Destination::Note { path, heading }
}

/// Obsidian permits the alias delimiter to be raw or table-escaped.
/// Only the backslash immediately protecting that delimiter is removed.
pub(crate) fn wiki_parts(inner: &str) -> (&str, Option<&str>) {
    match inner.split_once('|') {
        Some((target, alias)) => (
            target.strip_suffix('\\').unwrap_or(target).trim(),
            Some(alias.trim()),
        ),
        None => (inner.trim(), None),
    }
}

/// Comrak splits table cells before parsing wiki links. Hide alias delimiters
/// from that pass without moving a single byte offset: link ranges still refer
/// to canonical source, including Unicode and escaped delimiters. The original
/// target/label are recovered from the authored span below, never from the mask.
pub(crate) fn wiki_parse_source(source: &str) -> String {
    let mut bytes = source.as_bytes().to_vec();
    static WIKI: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\[\[([^\[\]\r\n]+)\]\]").unwrap());
    for prose in crate::prose::prose_spans(source) {
        for found in WIKI.find_iter(&source[prose.clone()]) {
            let start = prose.start + found.start();
            let end = prose.start + found.end();
            for at in start..end {
                if bytes[at] == b'|' {
                    bytes[at] = b'_';
                    if at > start && bytes[at - 1] == b'\\' {
                        bytes[at - 1] = b'_';
                    }
                }
            }
        }
    }
    String::from_utf8(bytes).expect("only ASCII separators replaced")
}

pub fn parse(source: &str) -> Vec<ParsedLink> {
    let arena = Arena::new();
    let parse_source = wiki_parse_source(source);
    let root = parse_document(&arena, &parse_source, &render::comrak_options());
    let lines: Vec<_> = std::iter::once(0)
        .chain(source.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    root.descendants()
        .filter_map(|node| {
            let data = node.data.borrow();
            let (target, title, wiki) = match &data.value {
                NodeValue::Link(link) => (link.url.clone(), link.title.clone(), false),
                NodeValue::WikiLink(link) => (link.url.clone(), String::new(), true),
                _ => return None,
            };
            if node
                .ancestors()
                .skip(1)
                .any(|n| matches!(n.data.borrow().value, NodeValue::Image(_)))
            {
                return None;
            }
            let pos = data.sourcepos;
            let start = lines
                .get(pos.start.line.checked_sub(1)?)?
                .checked_add(pos.start.column.checked_sub(1)?)?;
            let end = lines
                .get(pos.end.line.checked_sub(1)?)?
                .checked_add(pos.end.column)?;
            let authored = source.get(start..end)?;
            // A normal Markdown URL/title can itself contain wiki-looking bytes.
            // Parse that isolated link without table splitting to retain its exact
            // normal Markdown semantics rather than leaking the mask into a URL.
            let (target, title) = if !wiki && authored.contains("[[") && authored.contains('|') {
                let link_arena = Arena::new();
                let original = parse_document(&link_arena, authored, &render::comrak_options());
                let original_link =
                    original
                        .descendants()
                        .find_map(|node| match &node.data.borrow().value {
                            NodeValue::Link(link) => Some((link.url.clone(), link.title.clone())),
                            _ => None,
                        });
                original_link.unwrap_or((target, title))
            } else {
                (target, title)
            };
            // Wiki paths are literal authored bytes (the existing vault grammar),
            // unlike CommonMark destinations which decode entities/escapes.
            let wiki_inner = if wiki {
                Some(authored.strip_prefix("[[")?.strip_suffix("]]")?)
            } else {
                None
            };
            let target = wiki_inner.map_or(target, |inner| wiki_parts(inner).0.to_owned());
            // Preserve inline formatting in labels. Wiki aliases come from the
            // authored span because table parsing uses a length-preserving mask.
            let label = if let Some(inner) = wiki_inner {
                wiki_parts(inner).1.unwrap_or(&target).to_owned()
            } else if authored.starts_with("[]") {
                String::new()
            } else if authored.starts_with('[') {
                let first = node.first_child()?;
                let last = node.last_child()?;
                let a = first.data.borrow().sourcepos.start;
                let b = last.data.borrow().sourcepos.end;
                let a = lines.get(a.line.checked_sub(1)?)? + a.column.checked_sub(1)?;
                let b = lines.get(b.line.checked_sub(1)?)? + b.column;
                source.get(a..b)?.to_owned()
            } else {
                target.clone()
            };
            Some(ParsedLink {
                range: start..end,
                label,
                title,
                target,
                wiki,
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedLink {
    pub url: String,
    pub status: &'static str,
    pub candidates: Vec<String>,
    pub heading: Option<String>,
    pub reason: Option<&'static str>,
}

/// Local file links open an in-app preview, never an external process.
fn attachment(target: &str, wiki: bool, vault: &Vault, from: &str) -> Option<ResolvedLink> {
    if target.contains(':') || target.contains('\\') {
        return None;
    }
    let raw = target.split('#').next().unwrap_or(target);
    let path = if wiki {
        raw.trim().to_owned()
    } else {
        decode(raw)
    };
    let path = if wiki {
        std::borrow::Cow::Borrowed(path.as_str())
    } else {
        markdown_path(vault, &path)?
    };
    let ext = std::path::Path::new(path.as_ref()).extension()?.to_str()?;
    if ext.eq_ignore_ascii_case("md") {
        return None;
    }
    if (!vault.inventory_complete && !vault.single_file) || vault.graph_root.is_some() {
        return Some(ResolvedLink {
            url: format!("{}{}", render::UNRESOLVED_SCHEME, encode(target)),
            status: "unresolved",
            candidates: vec![],
            heading: None,
            reason: Some("File identity is pending background inventory verification."),
        });
    }
    let root = vault.root.canonicalize().ok()?;
    let mut candidates = Vec::new();
    let explicit_relative = path.starts_with("./") || path.starts_with("../");
    let from_dir = std::path::Path::new(from)
        .parent()
        .unwrap_or(std::path::Path::new(""));
    let exact = root.join(path.trim_start_matches('/'));
    let relative = root.join(from_dir).join(path.as_ref());
    let lookups = if explicit_relative {
        vec![relative]
    } else if wiki || path.starts_with('/') {
        vec![exact]
    } else {
        vec![relative, exact]
    };
    let mut existing = false;
    for candidate in lookups {
        // Only lexical absence permits redirecting to a root/suffix namesake.
        // Dangling symlinks, denied paths and non-directory ancestors count as occupied.
        match std::fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            _ => existing = true,
        }
        if let Ok(candidate) = candidate.canonicalize() {
            if candidate.is_file() && candidate.starts_with(&root) {
                candidates.push(
                    candidate
                        .strip_prefix(&root)
                        .ok()?
                        .to_string_lossy()
                        .replace(std::path::MAIN_SEPARATOR, "/"),
                );
            }
        }
        break;
    }
    if !existing && candidates.is_empty() && !explicit_relative && !path.starts_with('/') {
        candidates = vault
            .entries
            .iter()
            .filter(|e| {
                e.kind == crate::vault::EntryKind::Attachment
                    && (e.path == path.as_ref() || e.path.ends_with(&format!("/{path}")))
                    && root
                        .join(&e.path)
                        .canonicalize()
                        .is_ok_and(|p| p.is_file() && p.starts_with(&root))
            })
            .map(|e| e.path.clone())
            .collect();
        candidates.sort();
        candidates.dedup();
    }
    let (status, url) = match candidates.as_slice() {
        [path] => (
            "attachment",
            format!("tessera://attachment/{}", encode(path)),
        ),
        [] => (
            "unresolved",
            format!("{}{}", render::UNRESOLVED_SCHEME, encode(target)),
        ),
        _ => (
            "ambiguous",
            format!("{}{}", render::AMBIGUOUS_SCHEME, encode(target)),
        ),
    };
    Some(ResolvedLink {
        url,
        status,
        candidates,
        heading: None,
        reason: None,
    })
}

pub fn resolve(target: &str, wiki: bool, vault: &Vault, from: &str) -> ResolvedLink {
    if !wiki {
        let path = decode(target.split('#').next().unwrap_or(target));
        if vault.inventory_complete
            && vault.graph_root.is_none()
            && markdown_path(vault, &path).is_none()
            && std::path::Path::new(&path).is_file()
        {
            return ResolvedLink {
                url: format!("tessera://outside-file/{}", encode(&path)),
                status: "outside_file",
                candidates: vec![],
                heading: None,
                reason: Some("This file is outside the vault. Choose Reveal to locate it."),
            };
        }
    }
    if let Some(file) = attachment(target, wiki, vault, from) {
        return file;
    }
    let (path, heading) = match destination(target, wiki) {
        Destination::External(url) => {
            return ResolvedLink {
                url,
                status: "external",
                candidates: vec![],
                heading: None,
                reason: None,
            }
        }
        Destination::Unsupported(reason) => {
            return ResolvedLink {
                url: format!(
                    "tessera://unsupported/{}",
                    encode(&format!("{reason} Target: {target}"))
                ),
                status: "unsupported",
                candidates: vec![],
                heading: None,
                reason: Some(reason),
            }
        }
        Destination::Note { path, heading } => (path, heading),
    };
    let resolution = if path.is_empty() {
        Resolution::Resolved { path: from.into() }
    } else if wiki {
        vault.resolve_from(&path, from)
    } else {
        vault.resolve_markdown(&path, from)
    };
    let (status, url, candidates) = match resolution {
        Resolution::Resolved { path } => {
            let suffix = heading
                .as_ref()
                .map(|h| format!("#{}", encode(h)))
                .unwrap_or_default();
            (
                "resolved",
                format!("{}{}{suffix}", render::WIKI_SCHEME, encode(&path)),
                vec![path],
            )
        }
        Resolution::Ambiguous { candidates } => (
            "ambiguous",
            format!(
                "{}{}",
                if wiki {
                    render::AMBIGUOUS_SCHEME
                } else {
                    "tessera://ambiguous-markdown/"
                },
                encode(target)
            ),
            candidates,
        ),
        Resolution::Unresolved => (
            "unresolved",
            format!("{}{}", render::UNRESOLVED_SCHEME, encode(target)),
            vec![],
        ),
    };
    ResolvedLink {
        url,
        status,
        candidates,
        heading,
        reason: None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadingTarget {
    pub block: usize,
    pub offset: usize,
    pub setext: bool,
}

/// A heading in the exact supplied source, including unsupported containers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadingEntry {
    pub text: String,
    pub level: u8,
    pub target: HeadingTarget,
    pub supported_container: bool,
}

/// A block ID (`^id`) and the top-level block a link to it lands on (#651).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockEntry {
    pub id: String,
    pub target: HeadingTarget,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeadingInventory {
    pub entries: Vec<HeadingEntry>,
    /// Block IDs, from the markers [`crate::obsidian::rewrite_block_ids`]
    /// leaves in the Reader source. Empty for a source without them.
    pub blocks: Vec<BlockEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeadingFailure {
    Missing,
    Ambiguous,
    Unsupported,
    MissingBlock,
    AmbiguousBlock,
}

impl HeadingFailure {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Missing => "No matching heading exists in the current document.",
            Self::Ambiguous => "Multiple headings have that name. Use a unique heading.",
            Self::Unsupported => "This heading is inside an unsupported container.",
            Self::MissingBlock => "No block with that ID exists in the current document.",
            Self::AmbiguousBlock => "Multiple blocks have that ID. Use a unique block ID.",
        }
    }
}

impl HeadingInventory {
    /// Parse once per target revision, then reuse for links and the TOC.
    pub fn new(source: &str) -> Self {
        let arena = Arena::new();
        let root = parse_document(&arena, source, &render::comrak_options());
        let offsets: Vec<_> = std::iter::once(0)
            .chain(source.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        let mut entries = Vec::new();
        let mut blocks = Vec::new();
        let mut previous_offset = 0;
        for (block, top) in root.children().enumerate() {
            let top_offset = offsets[top.data.borrow().sourcepos.start.line.saturating_sub(1)];
            for node in top.descendants() {
                let data = node.data.borrow();
                let marker = match &data.value {
                    NodeValue::HtmlBlock(h) => crate::obsidian::marker_id(&h.literal),
                    NodeValue::HtmlInline(h) => crate::obsidian::marker_id(h),
                    _ => None,
                };
                if let Some(id) = marker {
                    // An ID alone on its line names the block before it.
                    let alone = std::ptr::eq(node, top) && block > 0;
                    blocks.push(BlockEntry {
                        id: id.to_owned(),
                        target: HeadingTarget {
                            block: if alone { block - 1 } else { block },
                            offset: if alone { previous_offset } else { top_offset },
                            setext: false,
                        },
                    });
                    continue;
                }
                let NodeValue::Heading(h) = &data.value else {
                    continue;
                };
                let mut text = String::new();
                for child in node.descendants().skip(1) {
                    match &child.data.borrow().value {
                        NodeValue::Text(t) => text.push_str(t),
                        NodeValue::Code(c) => text.push_str(&c.literal),
                        NodeValue::SoftBreak | NodeValue::LineBreak => text.push(' '),
                        _ => {}
                    }
                }
                entries.push(HeadingEntry {
                    text,
                    level: h.level,
                    target: HeadingTarget {
                        block,
                        offset: offsets[data.sourcepos.start.line.saturating_sub(1)],
                        setext: h.setext,
                    },
                    supported_container: std::ptr::eq(node, top),
                });
            }
            previous_offset = top_offset;
        }
        Self { entries, blocks }
    }

    pub fn locate(&self, target: &str) -> Result<HeadingTarget, HeadingFailure> {
        if let Some(id) = target.strip_prefix('^') {
            let mut matches = self
                .blocks
                .iter()
                .filter(|b| b.id.eq_ignore_ascii_case(id.trim()));
            let first = matches.next().ok_or(HeadingFailure::MissingBlock)?;
            if matches.next().is_some() {
                return Err(HeadingFailure::AmbiguousBlock);
            }
            return Ok(first.target.clone());
        }
        let mut matches = self
            .entries
            .iter()
            .filter(|h| render::heading_matches(&h.text, target));
        let first = matches.next().ok_or(HeadingFailure::Missing)?;
        if matches.next().is_some() {
            return Err(HeadingFailure::Ambiguous);
        }
        if !first.supported_container {
            return Err(HeadingFailure::Unsupported);
        }
        Ok(first.target.clone())
    }
}

/// Inventory all real headings before selecting a surface-supported landing.
/// Duplicates, including headings in unsupported containers, never pick a winner.
pub fn heading(source: &str, target: &str) -> Result<HeadingTarget, &'static str> {
    HeadingInventory::new(source)
        .locate(target)
        .map_err(HeadingFailure::reason)
}

#[cfg(test)]
mod attachment_tests {
    use super::*;

    #[test]
    fn attachments_preserve_root_relative_and_ambiguous_intent() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("vault");
        std::fs::create_dir(&dir).unwrap();
        for sub in ["notes", "a", "b"] {
            std::fs::create_dir(dir.join(sub)).unwrap();
        }
        for file in [
            "note.md",
            "notes/start.md",
            "diagram.svg",
            "notes/diagram.svg",
            "a/shared.pdf",
            "b/shared.pdf",
            "a/Схема 1.svg",
        ] {
            std::fs::write(dir.join(file), "fixture").unwrap();
        }
        let vault = Vault::scan(&dir).unwrap();
        assert_eq!(
            resolve("diagram.svg", true, &vault, "notes/start.md").candidates,
            ["diagram.svg"]
        );
        assert_eq!(
            resolve("diagram.svg", false, &vault, "notes/start.md").candidates,
            ["notes/diagram.svg"]
        );
        assert_eq!(
            resolve("/diagram.svg", false, &vault, "notes/start.md").candidates,
            ["diagram.svg"]
        );
        assert_eq!(
            resolve("shared.pdf", true, &vault, "notes/start.md").status,
            "ambiguous"
        );
        assert_eq!(
            resolve("../a/Схема%201.svg", false, &vault, "notes/start.md").status,
            "attachment"
        );
        assert_eq!(
            resolve("./shared.pdf", true, &vault, "notes/start.md").status,
            "unresolved"
        );
        assert_eq!(
            resolve("https://example.com/x.pdf", false, &vault, "note.md").status,
            "external"
        );
        assert_eq!(
            crate::render::preprocess("![[diagram.svg]]"),
            "![](diagram.svg)"
        );
        assert_eq!(
            crate::render::preprocess("![](diagram.svg)"),
            "![](diagram.svg)"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("diagram.svg")).unwrap(),
            "fixture"
        );
    }
}
