//! External `okilum:` links (#1049, #1414, docs/deep-links.md): parsing,
//! building and read-only resolution. Internal Reader URLs (`okilum://open/…`
//! and friends) are rejected here so they can never navigate from outside.

/// Where a link points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Address {
    /// Vault by folder name and a vault-relative path with `/` separators.
    Vault {
        vault: String,
        path: String,
    },
    /// A vault by its portable identity (`v/<vault>/<path>?vault_id=…`,
    /// #1414). `vault` is only the folder name of the machine that copied it,
    /// for messages; it is never a fallback.
    VaultId {
        id: String,
        vault: String,
        path: String,
    },
    /// An absolute file path (`/…` or `C:/…`), as written.
    File(String),
    /// Reserved stable-id forms; Okilum answers «needs a newer Okilum».
    Note(String),
    Task(String),
    Project(String),
}

/// Where inside the target to land. `line` and `column` are 1-based;
/// `column` counts Unicode scalar values.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Position {
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub page: Option<u32>,
    pub heading: Option<String>,
    pub block: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub address: Address,
    pub position: Position,
}

/// Why a link was refused; each has a message for the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    NotOkilum,
    TooLong,
    Internal,
    UnknownKind,
    Malformed,
    UnsafePath,
}

impl Refused {
    pub fn message(self) -> &'static str {
        match self {
            Self::NotOkilum => "This is not an Okilum link.",
            Self::TooLong => "This link is too long.",
            Self::Internal => "This link only works inside a note.",
            Self::UnknownKind => "This link needs a newer Okilum.",
            Self::Malformed => "This Okilum link is incomplete.",
            Self::UnsafePath => "This link points outside its vault.",
        }
    }
}

pub const MAX_LEN: usize = 4096;

/// A link matched against the vaults this machine knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Open `rel` (vault-relative, `/`-separated) in `root`.
    Open {
        root: std::path::PathBuf,
        rel: String,
        position: Position,
    },
    /// Several known vaults have this name: the user picks one (never
    /// guessed). Each root carries the note's path there, or `None` when the
    /// note is not in that vault.
    Choose {
        vault: String,
        roots: Vec<(std::path::PathBuf, Option<String>)>,
        position: Position,
    },
    /// An existing file outside every known vault: it opens on its own as a
    /// single-file view after the folder's first-use consent. Its folder never
    /// becomes a vault (#1414, #1439).
    SingleFile {
        file: std::path::PathBuf,
        position: Position,
    },
    /// A clear, user-facing refusal.
    Unavailable(String),
}

/// Why a document's address cannot be copied as an `okilum:` link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotEncodable {
    /// The path is not Unicode, or not one this link form can carry: a
    /// replacement character would point at another file.
    Path,
    /// The link would be longer than [`MAX_LEN`].
    TooLong,
}

impl NotEncodable {
    pub fn message(&self) -> &'static str {
        match self {
            Self::Path => "This file\u{2019}s path cannot be written as an Okilum link, so nothing was copied.",
            Self::TooLong => "The link to this place would be too long, so nothing was copied.",
        }
    }
}

impl std::fmt::Display for NotEncodable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for NotEncodable {}

/// Match `link` against `known` vault roots (canonical paths). `exists`
/// reports whether a path is a file; injectable for tests.
pub fn resolve(
    link: &Link,
    known: &[std::path::PathBuf],
    exists: &dyn Fn(&std::path::Path) -> bool,
) -> Resolution {
    use std::path::{Path, PathBuf};
    let position = link.position.clone();
    let name_of = |root: &Path| {
        root.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let same_name = |a: &str, b: &str| {
        if cfg!(any(target_os = "macos", windows)) {
            a.to_lowercase() == b.to_lowercase()
        } else {
            a == b
        }
    };
    // `.md` may be omitted when the exact name is absent, also after a dot
    // (`v1.2`), but is never appended to a name already ending in `.md`.
    let pick = |root: &Path, rel: &str| -> Option<String> {
        if exists(&root.join(rel)) {
            return Some(rel.to_owned());
        }
        let has_md = rel.to_ascii_lowercase().ends_with(".md");
        let md = format!("{rel}.md");
        (!has_md && exists(&root.join(&md))).then_some(md)
    };
    match &link.address {
        Address::VaultId { id, vault, path } => {
            let mut roots: Vec<PathBuf> = Vec::new();
            for root in known {
                let here = crate::vault_link_identity::read(root).ok().flatten();
                if here.as_deref() == Some(id.as_str()) && !roots.contains(root) {
                    roots.push(root.clone());
                }
            }
            // Exactly one vault with this identity; a namesake without it is
            // never used (decision 26: refuse, select nothing, rewrite nothing).
            match roots.len() {
                1 => match pick(&roots[0], path) {
                    Some(rel) => Resolution::Open {
                        root: roots.remove(0),
                        rel,
                        position,
                    },
                    None => Resolution::Unavailable(format!(
                        "\u{201c}{path}\u{201d} is not in vault \u{201c}{}\u{201d}.",
                        name_of(&roots[0])
                    )),
                },
                0 if known.iter().any(|root| same_name(&name_of(root), vault)) => {
                    Resolution::Unavailable(format!(
                        "This link belongs to a vault \u{201c}{vault}\u{201d} with a different identity, so no vault was opened. Wait until the vault\u{2019}s .okilum folder has synced, then copy the link again."
                    ))
                }
                0 => Resolution::Unavailable(format!(
                    "Vault \u{201c}{vault}\u{201d} is not on this computer."
                )),
                _ => Resolution::Unavailable(
                    "Several vaults on this computer have this link\u{2019}s identity, so no vault was opened.".into(),
                ),
            }
        }
        Address::Vault { vault, path } => {
            let mut roots: Vec<PathBuf> = Vec::new();
            for root in known {
                if same_name(&name_of(root), vault) && !roots.contains(root) {
                    roots.push(root.clone());
                }
            }
            match roots.len() {
                0 => Resolution::Unavailable(format!(
                    "Vault \u{201c}{vault}\u{201d} is not on this computer."
                )),
                1 => match pick(&roots[0], path) {
                    Some(rel) => Resolution::Open {
                        root: roots.remove(0),
                        rel,
                        position,
                    },
                    None => Resolution::Unavailable(format!(
                        "\u{201c}{path}\u{201d} is not in vault \u{201c}{vault}\u{201d}."
                    )),
                },
                _ => Resolution::Choose {
                    vault: vault.clone(),
                    roots: roots
                        .into_iter()
                        .map(|root| {
                            let rel = pick(&root, path);
                            (root, rel)
                        })
                        .collect(),
                    position,
                },
            }
        }
        Address::File(file) => {
            // The canonical spelling, so a plain `C:/…` matches the verbatim
            // `\\?\C:\…` of a known root (#1391); a missing path stays as written.
            let written = Path::new(file);
            let file = crate::vault::canonical_root(written).unwrap_or_else(|_| written.into());
            let file = file.as_path();
            // The deepest known vault that contains the file.
            let root = known
                .iter()
                .filter(|root| file.starts_with(root) && file != root.as_path())
                .max_by_key(|root| root.components().count());
            match root {
                Some(root) if exists(file) => Resolution::Open {
                    root: root.clone(),
                    rel: file
                        .strip_prefix(root)
                        .map(|r| {
                            r.components()
                                .map(|c| c.as_os_str().to_string_lossy())
                                .collect::<Vec<_>>()
                                .join("/")
                        })
                        .unwrap_or_default(),
                    position,
                },
                Some(_) => Resolution::Unavailable("This file no longer exists.".into()),
                None if !file.is_absolute() => Resolution::Unavailable(
                    "This file\u{2019}s path is not a path on this computer.".into(),
                ),
                None if exists(file) => Resolution::SingleFile {
                    file: file.to_owned(),
                    position,
                },
                None => Resolution::Unavailable("This file no longer exists.".into()),
            }
        }
        Address::Note(_) | Address::Task(_) | Address::Project(_) => {
            Resolution::Unavailable(Refused::UnknownKind.message().into())
        }
    }
}

/// Hosts the Reader uses for links inside rendered notes.
const INTERNAL: &[&str] = &[
    "open",
    "attachment",
    "footnote",
    "footnote-back",
    "outside-file",
];

pub fn parse(link: &str) -> Result<Link, Refused> {
    let link = link.trim();
    if link.len() > MAX_LEN {
        return Err(Refused::TooLong);
    }
    if link.chars().any(|c| c.is_control()) {
        return Err(Refused::Malformed);
    }
    let Some(rest) = strip_scheme(link) else {
        return Err(Refused::NotOkilum);
    };
    let (rest, fragment) = match rest.split_once('#') {
        Some((rest, fragment)) => (rest, Some(fragment)),
        None => (rest, None),
    };
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (kind, target) = rest.split_once('/').unwrap_or((rest, ""));
    if INTERNAL.contains(&kind) {
        return Err(Refused::Internal);
    }
    let mut address = match kind {
        "v" => {
            let (vault, path) = target.split_once('/').ok_or(Refused::Malformed)?;
            let vault = decode_clean(vault)?;
            let path = segments(path)?;
            if vault.is_empty() || vault.contains('/') {
                return Err(Refused::Malformed);
            }
            Address::Vault { vault, path }
        }
        "file" => {
            // A Windows verbatim path (`\\?\C:\…`, `\\?\UNC\server\…`) is its
            // plain spelling (#1391); other verbatim forms stay and are refused.
            let path = without_verbatim(decode_clean(target)?);
            // `C:/…` or `C:\…`; drive-relative `C:x` and a bare `C:` are not
            // absolute and are refused on every platform.
            let bytes = path.as_bytes();
            let windows = bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'/' | b'\\');
            if !(path.starts_with('/') || windows) {
                return Err(Refused::Malformed);
            }
            if path.split(['/', '\\']).any(|s| s == "..") {
                return Err(Refused::UnsafePath);
            }
            Address::File(path)
        }
        "note" | "task" | "project" => {
            let id = decode_clean(target)?;
            if id.is_empty() || id.contains('/') {
                return Err(Refused::Malformed);
            }
            match kind {
                "note" => Address::Note(id),
                "task" => Address::Task(id),
                _ => Address::Project(id),
            }
        }
        "" => return Err(Refused::Malformed),
        _ => return Err(Refused::UnknownKind),
    };
    let mut position = Position::default();
    let mut identity = None;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let number = || decode(value).parse::<u32>().ok().filter(|n| *n > 0);
        match key {
            // Strict: an identity that is present but unreadable must never
            // degrade into a folder-name link.
            "vault_id" => {
                let id =
                    uuid::Uuid::parse_str(&decode_clean(value)?).map_err(|_| Refused::Malformed)?;
                if identity.replace(id.to_string()).is_some() {
                    return Err(Refused::Malformed);
                }
            }
            "line" => position.line = number(),
            "column" => position.column = number(),
            "page" => position.page = number(),
            // Forward compatible: unknown parameters are ignored.
            _ => {}
        }
    }
    if let Some(id) = identity {
        address = match address {
            Address::Vault { vault, path } => Address::VaultId { id, vault, path },
            _ => return Err(Refused::Malformed),
        };
    }
    if position.line.is_none() {
        position.column = None;
    }
    let fragment = fragment.map(decode_clean).transpose()?;
    match fragment.filter(|f| !f.is_empty()) {
        Some(block) if block.starts_with('^') => position.block = Some(block[1..].to_owned()),
        Some(heading) => position.heading = Some(heading),
        None => {}
    }
    Ok(Link { address, position })
}

/// `\\?\C:\…` → `C:\…`, `\\?\UNC\server\share` → `//server/share`.
fn without_verbatim(path: String) -> String {
    if let Some(share) = path.strip_prefix(r"\\?\UNC\") {
        return format!("//{}", share.replace('\\', "/"));
    }
    match path.strip_prefix(r"\\?\") {
        Some(rest) if matches!(rest.as_bytes(), [l, b':', b'\\' | b'/', ..] if l.is_ascii_alphabetic()) => {
            rest.to_owned()
        }
        _ => path,
    }
}

fn strip_scheme(link: &str) -> Option<&str> {
    let (scheme, rest) = link.split_once("://")?;
    scheme.eq_ignore_ascii_case("okilum").then_some(rest)
}

/// The canonical external link to `rel` in the vault folder named `vault`
/// («Copy Okilum link»): `okilum://v/<vault>/<path>`, every segment
/// percent-encoded as UTF-8 with `/` kept, then `line`/`column`/`page` and a
/// `#heading` or `#^block` fragment. `parse` reads it back exactly.
pub fn build(vault: &str, rel: &str, position: &Position) -> String {
    let path = rel.split('/').map(encode).collect::<Vec<_>>().join("/");
    with_position(
        format!("okilum://v/{}/{path}", encode(vault)),
        None,
        position,
    )
}

/// The address, then `vault_id`, `line`/`column`/`page` and the fragment.
fn with_position(mut link: String, id: Option<&str>, position: &Position) -> String {
    let mut query = Vec::new();
    if let Some(id) = id {
        query.push(format!("vault_id={}", encode(id)));
    }
    if let Some(line) = position.line {
        query.push(format!("line={line}"));
        if let Some(column) = position.column {
            query.push(format!("column={column}"));
        }
    }
    if let Some(page) = position.page {
        query.push(format!("page={page}"));
    }
    if !query.is_empty() {
        link.push('?');
        link.push_str(&query.join("&"));
    }
    if let Some(heading) = &position.heading {
        link.push('#');
        link.push_str(&encode(heading));
    } else if let Some(block) = &position.block {
        link.push_str("#^");
        link.push_str(&encode(block));
    }
    link
}

/// «Copy link» in a vault (#1414): the folder-name form plus the vault's
/// portable identity, refused unless [`parse`] reads back exactly this vault,
/// path and position. Older Okilum ignores `vault_id` and uses the folder name.
pub fn build_vault_copy(
    vault: &str,
    id: &str,
    rel: &str,
    position: &Position,
) -> Result<String, NotEncodable> {
    let path = rel.split('/').map(encode).collect::<Vec<_>>().join("/");
    let link = with_position(
        format!("okilum://v/{}/{path}", encode(vault)),
        Some(id),
        position,
    );
    let expected = Address::VaultId {
        id: id.to_owned(),
        vault: vault.to_owned(),
        path: rel.to_owned(),
    };
    exact(link, expected, position)
}

/// «Copy link» in a single-file view (#1439): `okilum://file/<absolute path>`,
/// so the link reopens this file and never names its folder as a vault. The
/// path is shown plain (no `\\?\`, #1391), `/`-separated; a path that is not
/// Unicode or does not read back exactly is refused rather than approximated.
pub fn build_file_copy(
    path: &std::path::Path,
    position: &Position,
) -> Result<String, NotEncodable> {
    let plain = crate::vault::plain_path(path);
    let text = plain.to_str().ok_or(NotEncodable::Path)?;
    if !plain.is_absolute() {
        return Err(NotEncodable::Path);
    }
    #[cfg(windows)]
    let text = &text.replace('\\', "/");
    let encoded = text.split('/').map(encode).collect::<Vec<_>>().join("/");
    let link = with_position(format!("okilum://file/{encoded}"), None, position);
    exact(link, Address::File(text.to_owned()), position)
}

/// `link`, if it parses back to exactly `address` and `position`.
fn exact(link: String, address: Address, position: &Position) -> Result<String, NotEncodable> {
    if link.len() > MAX_LEN {
        return Err(NotEncodable::TooLong);
    }
    let mut expected = position.clone();
    if expected.line.is_none() {
        expected.column = None;
    }
    if expected.heading.is_some() {
        expected.block = None;
    }
    match parse(&link) {
        Ok(back) if back.address == address && back.position == expected => Ok(link),
        _ => Err(NotEncodable::Path),
    }
}

/// RFC 3986 unreserved characters stay; everything else is `%XX` per UTF-8
/// byte, so spaces are `%20` and Hebrew or `#` never break the link.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Percent-decoding as UTF-8; unencoded non-ASCII (pasted from chat) passes
/// through. Invalid escapes are kept literally.
fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// [`decode`], refusing control characters (NUL, line breaks, ESC, DEL, C1)
/// that only appear after decoding: raw ones are refused up front.
fn decode_clean(value: &str) -> Result<String, Refused> {
    let value = decode(value);
    if value.chars().any(char::is_control) {
        return Err(Refused::Malformed);
    }
    Ok(value)
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// A vault-relative path: decoded per segment, never empty, never escaping.
fn segments(path: &str) -> Result<String, Refused> {
    let parts = path
        .split('/')
        .map(decode_clean)
        .collect::<Result<Vec<_>, _>>()?;
    if parts.iter().any(|p| p.is_empty()) {
        return Err(if path.is_empty() {
            Refused::Malformed
        } else {
            Refused::UnsafePath
        });
    }
    if parts
        .iter()
        .any(|p| p == "." || p == ".." || p.contains('/') || p.contains('\\'))
    {
        return Err(Refused::UnsafePath);
    }
    Ok(parts.join("/"))
}

/// The Reader block that shows file line `line` (1-based, frontmatter
/// counted). `rendered` is the Reader's rewrite of the body; the block
/// ordinal maps across only when both parses have the same top-level blocks,
/// as task landing does. `None` means the line cannot be placed in the
/// reading view: the caller says so instead of guessing. Lines in the
/// frontmatter land on the first block, blank lines on the next block, lines
/// past the end on the last one.
pub fn reader_block(source: &str, rendered: &str, line: u32) -> Option<usize> {
    use comrak::{parse_document, Arena};
    let body = crate::render::without_frontmatter(source);
    let prefix_lines = source[..source.len() - body.len()]
        .bytes()
        .filter(|b| *b == b'\n')
        .count();
    let body_line = (line as usize).saturating_sub(prefix_lines).max(1);
    let options = crate::render::comrak_options();
    let block_ends = |text: &str| {
        let arena = Arena::new();
        let root = parse_document(&arena, text, &options);
        root.children()
            .map(|top| top.data.borrow().sourcepos.end.line)
            .collect::<Vec<_>>()
    };
    let original = block_ends(body);
    if original.is_empty() || original.len() != block_ends(rendered).len() {
        return None;
    }
    Some(
        original
            .iter()
            .position(|end| *end >= body_line)
            .unwrap_or(original.len() - 1),
    )
}

/// The file line (1-based, frontmatter counted) where Reader block `block`
/// starts: the inverse of [`reader_block`], with the same rule that both
/// parses must have the same top-level blocks. Entering the editor opens it
/// at the Reader's place (#1296).
pub fn block_line(source: &str, rendered: &str, block: usize) -> Option<u32> {
    use comrak::{parse_document, Arena};
    let body = crate::render::without_frontmatter(source);
    let prefix_lines = source[..source.len() - body.len()]
        .bytes()
        .filter(|b| *b == b'\n')
        .count();
    let options = crate::render::comrak_options();
    let block_starts = |text: &str| {
        let arena = Arena::new();
        let root = parse_document(&arena, text, &options);
        root.children()
            .map(|top| top.data.borrow().sourcepos.start.line)
            .collect::<Vec<_>>()
    };
    let original = block_starts(body);
    if original.len() != block_starts(rendered).len() {
        return None;
    }
    let line = *original.get(block)? + prefix_lines;
    u32::try_from(line).ok()
}

/// The byte offsets in `source` where its top-level body blocks start: the
/// start of each block's first line, in order, never inside the frontmatter,
/// a code block or a paragraph. These are the only places a drop before a
/// rendered block may be relocated to (#1615).
pub fn block_starts(source: &str) -> Vec<usize> {
    use comrak::{parse_document, Arena};
    let body = crate::render::without_frontmatter(source);
    let prefix = source.len() - body.len();
    let lines: Vec<usize> = std::iter::once(0)
        .chain(body.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    let options = crate::render::comrak_options();
    let arena = Arena::new();
    let root = parse_document(&arena, body, &options);
    root.children()
        .filter_map(|top| {
            let line = top.data.borrow().sourcepos.start.line;
            lines.get(line.checked_sub(1)?).map(|at| prefix + at)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_starts_are_top_level_body_blocks_only() {
        let source = "---\ntags: [a] # label\n---\n# Title\n\n```\n# not a heading\n```\n\nPara # x\nstill para.\n";
        let starts = block_starts(source);
        assert_eq!(
            starts,
            vec![
                source.find("# Title").unwrap(),
                source.find("```").unwrap(),
                source.find("Para").unwrap(),
            ]
        );
        assert!(block_starts("").is_empty());
    }

    fn vault(link: &str) -> (String, String, Position) {
        match parse(link).unwrap() {
            Link {
                address: Address::Vault { vault, path },
                position,
            } => (vault, path, position),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn block_line_is_the_inverse_of_reader_block() {
        let source =
            "---\ntitle: Plan\n---\n# Plan\n\nFirst paragraph\nstill first.\n\n- one\n- two\n";
        let body = crate::render::without_frontmatter(source);
        for (block, line) in [(0, 4), (1, 6), (2, 9)] {
            assert_eq!(block_line(source, body, block), Some(line), "block {block}");
            assert_eq!(reader_block(source, body, line), Some(block));
        }
        assert_eq!(block_line(source, body, 3), None);
        assert_eq!(block_line(source, "# only one block", 1), None);
    }

    #[test]
    fn reader_block_follows_file_lines() {
        let source =
            "---\ntitle: Plan\n---\n# Plan\n\nFirst paragraph\nstill first.\n\n- one\n- two\n";
        let body = crate::render::without_frontmatter(source);
        // Lines 1-3 frontmatter, 4 heading, 5 blank, 6-7 paragraph, 9-10 list.
        for (line, block) in [(1, 0), (4, 0), (5, 1), (7, 1), (8, 2), (10, 2), (99, 2)] {
            assert_eq!(reader_block(source, body, line), Some(block), "line {line}");
        }
        // Positive control: a rewrite that changes the block count is refused.
        assert_eq!(reader_block(source, "# Plan\n", 6), None);
        assert_eq!(reader_block("", "", 1), None);
    }

    #[test]
    fn built_links_parse_back_exactly() {
        let at = |line, column, heading: Option<&str>, block: Option<&str>| Position {
            line,
            column,
            heading: heading.map(str::to_owned),
            block: block.map(str::to_owned),
            ..Default::default()
        };
        for (vault, rel, position) in [
            ("Notes", "Projects/Launch plan.md", Position::default()),
            (
                "My Brain",
                "תכנון/שבוע 1.md",
                at(Some(12), Some(4), None, None),
            ),
            ("Заметки", "a#b?c%d&e+f.md", at(Some(3), None, None, None)),
            (
                "Notes",
                "Plan.md",
                at(None, None, Some("Next steps & risks"), None),
            ),
            ("Notes", "Plan.md", at(None, None, None, Some("abc-1"))),
        ] {
            let link = build(vault, rel, &position);
            assert!(link.is_ascii(), "{link}");
            assert!(!link.contains(' ') && !link.contains('+'), "{link}");
            let parsed = parse(&link).unwrap();
            assert_eq!(
                parsed.address,
                Address::Vault {
                    vault: vault.into(),
                    path: rel.into()
                },
                "{link}"
            );
            assert_eq!(parsed.position, position, "{link}");
        }
        assert_eq!(
            build(
                "Notes",
                "Projects/Launch plan.md",
                &at(Some(12), Some(4), None, None)
            ),
            "okilum://v/Notes/Projects/Launch%20plan.md?line=12&column=4"
        );
    }

    #[test]
    fn canonical_vault_form_with_position_and_encoding() {
        let (v, p, pos) = vault("okilum://v/Notes/Projects/Launch%20plan.md?line=12&column=4");
        assert_eq!(
            (v.as_str(), p.as_str()),
            ("Notes", "Projects/Launch plan.md")
        );
        assert_eq!((pos.line, pos.column), (Some(12), Some(4)));
        // Hebrew and Cyrillic, encoded and as pasted.
        let (_, p, _) = vault(
            "okilum://v/%D0%97%D0%B0%D0%BC%D0%B5%D1%82%D0%BA%D0%B8/%D7%A9%D7%9C%D7%95%D7%9D.md",
        );
        assert_eq!(p, "שלום.md");
        let (v, p, _) = vault("okilum://v/Заметки/שלום עולם.md");
        assert_eq!((v.as_str(), p.as_str()), ("Заметки", "שלום עולם.md"));
        // `+` is not a space; an invalid escape stays literal.
        assert_eq!(vault("okilum://v/N/a+b%zz.md").1, "a+b%zz.md");
        // Scheme is case-insensitive.
        assert_eq!(vault("OKILUM://v/N/x.md").1, "x.md");
    }

    #[test]
    fn fragments_pages_and_ignored_parameters() {
        let (_, _, pos) = vault("okilum://v/N/a.md#Next%20steps");
        assert_eq!(pos.heading.as_deref(), Some("Next steps"));
        let (_, _, pos) = vault("okilum://v/N/a.md#^abc123");
        assert_eq!(pos.block.as_deref(), Some("abc123"));
        let (_, _, pos) = vault("okilum://v/N/doc.pdf?page=3&future=1");
        assert_eq!(pos.page, Some(3));
        // Column without a line, zero and junk numbers are dropped.
        let (_, _, pos) = vault("okilum://v/N/a.md?column=4");
        assert_eq!((pos.line, pos.column), (None, None));
        let (_, _, pos) = vault("okilum://v/N/a.md?line=0&page=x");
        assert_eq!((pos.line, pos.page), (None, None));
    }

    #[test]
    fn file_and_reserved_id_forms() {
        assert_eq!(
            parse("okilum://file//home/me/Notes/a%20b.md?line=2")
                .unwrap()
                .address,
            Address::File("/home/me/Notes/a b.md".into())
        );
        assert_eq!(
            parse("okilum://file/C:/Users/me/Notes/a.md")
                .unwrap()
                .address,
            Address::File("C:/Users/me/Notes/a.md".into())
        );
        assert_eq!(
            parse("okilum://project/p-42").unwrap().address,
            Address::Project("p-42".into())
        );
        assert_eq!(
            parse("okilum://note/n1").unwrap().address,
            Address::Note("n1".into())
        );
        assert_eq!(
            parse("okilum://task/t1").unwrap().address,
            Address::Task("t1".into())
        );
    }

    #[test]
    fn resolution_against_known_vaults() {
        use std::path::{Path, PathBuf};
        let files = [
            "/home/me/Notes/Projects/Plan.md",
            "/home/me/Notes/Inbox.md",
            "/work/Notes/Plan.md",
            "/home/me/Notes/Sub/Vault/Deep.md",
        ];
        let exists = |p: &Path| files.iter().any(|f| Path::new(f) == p);
        let known: Vec<PathBuf> = [
            "/home/me/Notes",
            "/home/me/Notes/Sub/Vault",
            "/data/Archive",
        ]
        .map(PathBuf::from)
        .to_vec();
        let r = |link: &str, known: &[PathBuf]| resolve(&parse(link).unwrap(), known, &exists);
        assert_eq!(
            r("okilum://v/Notes/Projects/Plan.md?line=3", &known),
            Resolution::Open {
                root: "/home/me/Notes".into(),
                rel: "Projects/Plan.md".into(),
                position: Position {
                    line: Some(3),
                    ..Default::default()
                }
            }
        );
        // `.md` may be omitted.
        assert!(
            matches!(r("okilum://v/Notes/Inbox", &known), Resolution::Open { rel, .. } if rel == "Inbox.md")
        );
        // Missing note and unknown vault are said, not created or guessed.
        assert!(
            matches!(r("okilum://v/Notes/Nope.md", &known), Resolution::Unavailable(m) if m.contains("Nope.md"))
        );
        assert!(
            matches!(r("okilum://v/Travel/a.md", &known), Resolution::Unavailable(m) if m.contains("Travel"))
        );
        // Two known vaults named Notes: a choice, never a guess.
        let mut two = known.clone();
        two.push("/work/Notes".into());
        match r("okilum://v/Notes/Plan.md", &two) {
            Resolution::Choose { vault, roots, .. } => {
                assert_eq!(vault, "Notes");
                assert_eq!(roots.len(), 2);
                // Each choice says whether the note is there: the home vault
                // has only Projects/Plan.md, the work vault has Plan.md.
                assert_eq!(roots[0], ("/home/me/Notes".into(), None));
                assert_eq!(roots[1], ("/work/Notes".into(), Some("Plan.md".into())));
            }
            other => panic!("{other:?}"),
        }
        // file/: the deepest known vault that contains it.
        assert!(matches!(
            r("okilum://file//home/me/Notes/Sub/Vault/Deep.md", &known),
            Resolution::Open { root, rel, .. } if root == Path::new("/home/me/Notes/Sub/Vault") && rel == "Deep.md"
        ));
        // Outside every known vault: the file on its own (#1414), never a vault.
        #[cfg(unix)]
        assert_eq!(
            r("okilum://file//work/Notes/Plan.md", &known),
            Resolution::SingleFile {
                file: "/work/Notes/Plan.md".into(),
                position: Position::default(),
            }
        );
        // A missing file is refused; `/work/…` has no drive on Windows, so
        // there it is not a path on this computer at all.
        let missing = |link: &str| resolve(&parse(link).unwrap(), &known, &|_| false);
        #[cfg(unix)]
        assert!(matches!(
            missing("okilum://file//work/Notes/Plan.md"),
            Resolution::Unavailable(m) if m.contains("no longer exists")
        ));
        #[cfg(windows)]
        {
            assert!(matches!(
                missing("okilum://file//work/Notes/Plan.md"),
                Resolution::Unavailable(m) if m.contains("not a path on this computer")
            ));
            assert!(matches!(
                missing("okilum://file/C:/work/Notes/Plan.md"),
                Resolution::Unavailable(m) if m.contains("no longer exists")
            ));
        }
        // Reserved ids answer with the newer-Okilum message.
        assert!(
            matches!(r("okilum://project/p1", &known), Resolution::Unavailable(m) if m.contains("newer"))
        );
    }

    #[test]
    fn refusals() {
        for (link, why) in [
            ("https://example.com/a.md", Refused::NotOkilum),
            ("okilum://open/Projects/Plan.md", Refused::Internal),
            ("okilum://attachment/a.png", Refused::Internal),
            ("okilum://footnote/1", Refused::Internal),
            ("okilum://settings/reset", Refused::UnknownKind),
            ("okilum://v/Notes", Refused::Malformed),
            ("okilum://v/Notes/", Refused::Malformed),
            ("okilum://v//a.md", Refused::Malformed),
            ("okilum://v/N/../secret.md", Refused::UnsafePath),
            ("okilum://v/N/a/%2E%2E/b.md", Refused::UnsafePath),
            ("okilum://v/N/a%2Fb.md", Refused::UnsafePath),
            ("okilum://v/N//a.md", Refused::UnsafePath),
            ("okilum://file/relative/a.md", Refused::Malformed),
            ("okilum://file//home/../etc/passwd", Refused::UnsafePath),
            ("okilum://v/N/a\u{0}.md", Refused::Malformed),
            ("okilum://note/", Refused::Malformed),
        ] {
            assert_eq!(parse(link), Err(why), "{link}");
            assert!(!why.message().is_empty());
        }
        let long = format!("okilum://v/N/{}.md", "a".repeat(MAX_LEN));
        assert_eq!(parse(&long), Err(Refused::TooLong));
        // Positive control: the same shape inside the limit parses.
        assert!(parse("okilum://v/N/a.md").is_ok());
    }

    #[test]
    fn md_is_appended_to_dotted_names_but_never_twice() {
        use std::path::{Path, PathBuf};
        let files = [
            "/home/me/Notes/v1.2.md",
            "/home/me/Notes/Daily/2026.10.29 Meeting.md",
            "/home/me/Notes/Both.v2",
            "/home/me/Notes/Both.v2.md",
            "/home/me/Notes/Plan.MD.md",
            "/home/me/Notes/Inbox.md.md",
            "/work/Notes/v1.2.md",
        ];
        let exists = |p: &Path| files.iter().any(|f| Path::new(f) == p);
        let home: Vec<PathBuf> = vec!["/home/me/Notes".into()];
        let r = |link: &str, known: &[PathBuf]| resolve(&parse(link).unwrap(), known, &exists);
        let opened = |res: Resolution| match res {
            Resolution::Open { rel, .. } => rel,
            other => panic!("{other:?}"),
        };
        assert_eq!(opened(r("okilum://v/Notes/v1.2", &home)), "v1.2.md");
        assert_eq!(
            opened(r("okilum://v/Notes/Daily/2026.10.29%20Meeting", &home)),
            "Daily/2026.10.29 Meeting.md"
        );
        // The exact name wins over the `.md` sibling.
        assert_eq!(opened(r("okilum://v/Notes/Both.v2", &home)), "Both.v2");
        // A name that already ends with `.md` (any case) never gets a second one.
        assert!(matches!(
            r("okilum://v/Notes/Plan.MD", &home),
            Resolution::Unavailable(m) if m.contains("Plan.MD")
        ));
        assert!(matches!(
            r("okilum://v/Notes/Inbox.md", &home),
            Resolution::Unavailable(m) if m.contains("Inbox.md")
        ));
        // Positive control: the doubled files are reachable by their exact names.
        assert_eq!(
            opened(r("okilum://v/Notes/Plan.MD.md", &home)),
            "Plan.MD.md"
        );
        // Two vaults named Notes stay a choice, each with its own answer.
        let two: Vec<PathBuf> = vec!["/home/me/Notes".into(), "/work/Notes".into()];
        match r("okilum://v/Notes/v1.2", &two) {
            Resolution::Choose { roots, .. } => assert_eq!(
                roots,
                vec![
                    ("/home/me/Notes".into(), Some("v1.2.md".into())),
                    ("/work/Notes".into(), Some("v1.2.md".into())),
                ]
            ),
            other => panic!("{other:?}"),
        }
        match r("okilum://v/Notes/Daily/2026.10.29%20Meeting", &two) {
            Resolution::Choose { roots, .. } => assert_eq!(
                roots,
                vec![
                    (
                        "/home/me/Notes".into(),
                        Some("Daily/2026.10.29 Meeting.md".into())
                    ),
                    ("/work/Notes".into(), None),
                ]
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn decoded_control_characters_are_malformed() {
        for control in ["%00", "%0A", "%0D", "%1B", "%7F", "%C2%80", "%C2%9F"] {
            for link in [
                format!("okilum://v/N{control}/a.md"),
                format!("okilum://v/N/a{control}.md"),
                format!("okilum://v/N/dir{control}/a.md"),
                format!("okilum://file//home/me/a{control}.md"),
                format!("okilum://file/C:/Users/a{control}.md"),
                format!("okilum://note/n{control}1"),
                format!("okilum://task/t{control}1"),
                format!("okilum://project/p{control}1"),
                format!("okilum://v/N/a.md#Next{control}steps"),
                format!("okilum://v/N/a.md#^abc{control}"),
            ] {
                assert_eq!(parse(&link), Err(Refused::Malformed), "{link}");
            }
        }
        // Positive control: spaces, Unicode and literal malformed escapes still
        // parse in every position checked above.
        let (v, p, pos) = vault(
            "okilum://v/My%20Brain/%D7%A9%D7%9C%D7%95%D7%9D%20a+b%zz.md#Next%20%D0%A8%D0%B0%D0%B3",
        );
        assert_eq!(
            (v.as_str(), p.as_str(), pos.heading.as_deref()),
            ("My Brain", "שלום a+b%zz.md", Some("Next Шаг"))
        );
        assert_eq!(
            vault("okilum://v/N/a.md#^abc%20d").2.block.as_deref(),
            Some("abc d")
        );
        assert_eq!(
            parse("okilum://file//home/me/a%20%zz.md").unwrap().address,
            Address::File("/home/me/a %zz.md".into())
        );
        assert_eq!(
            parse("okilum://note/n%201").unwrap().address,
            Address::Note("n 1".into())
        );
    }

    #[test]
    fn drive_relative_file_forms_are_malformed() {
        for link in [
            "okilum://file/C:relative/note.md",
            "okilum://file/C:",
            "okilum://file/c:note.md",
            "okilum://file/C%3Arelative%2Fnote.md",
        ] {
            assert_eq!(parse(link), Err(Refused::Malformed), "{link}");
        }
        // Positive control: absolute drive forms, with `/` or an encoded `\`.
        for (link, path) in [
            ("okilum://file/C:/Users/me/a.md", "C:/Users/me/a.md"),
            ("okilum://file/C:/", "C:/"),
            (
                "okilum://file/d:%5CUsers%5Cme%5Ca.md",
                "d:\\Users\\me\\a.md",
            ),
        ] {
            assert_eq!(
                parse(link).unwrap().address,
                Address::File(path.into()),
                "{link}"
            );
        }
        // Drive forms never resolve against Linux vault roots.
        let known = vec![std::path::PathBuf::from("/home/me/Notes")];
        let any = |_: &std::path::Path| true;
        let drive = resolve(
            &parse("okilum://file/C:/home/me/Notes/a.md").unwrap(),
            &known,
            &any,
        );
        assert!(!matches!(drive, Resolution::Open { .. }), "{drive:?}");
        #[cfg(unix)]
        assert!(
            matches!(&drive, Resolution::Unavailable(m) if m.contains("not a path on this computer")),
            "{drive:?}"
        );
    }

    fn pos(line: Option<u32>, column: Option<u32>, heading: Option<&str>) -> Position {
        Position {
            line,
            column,
            heading: heading.map(Into::into),
            ..Default::default()
        }
    }

    /// Every Copy position: the note, a heading, the caret, a block.
    fn copy_positions() -> Vec<Position> {
        vec![
            Position::default(),
            pos(None, None, Some("Distant checkpoint & שלום #2")),
            pos(Some(7), Some(3), None),
            Position {
                block: Some("abc-1".into()),
                ..Default::default()
            },
        ]
    }

    #[test]
    fn vault_copy_links_carry_the_identity_and_read_back_exactly() {
        let id = "0f8fad5b-d9cb-469f-a165-70867728950e";
        for position in copy_positions() {
            let link =
                build_vault_copy("Obsidian Vault", id, "Projects/Plan #1 שלום.md", &position)
                    .unwrap();
            assert!(
                link.starts_with("okilum://v/Obsidian%20Vault/Projects/"),
                "{link}"
            );
            assert!(
                link.contains("vault_id=0f8fad5b-d9cb-469f-a165-70867728950e"),
                "{link}"
            );
            let back = parse(&link).unwrap();
            assert_eq!(
                back.address,
                Address::VaultId {
                    id: id.into(),
                    vault: "Obsidian Vault".into(),
                    path: "Projects/Plan #1 שלום.md".into(),
                }
            );
            assert_eq!(back.position, position);
        }
        // Positive control: without the identity it is the legacy form.
        assert!(matches!(
            parse(&build("Brain", "Plan.md", &Position::default()))
                .unwrap()
                .address,
            Address::Vault { .. }
        ));
    }

    #[test]
    fn identity_parameters_are_strict() {
        for suffix in [
            "vault_id=bad",
            "vault_id=",
            "vault_id=00000000-0000-0000-0000-000000000001&vault_id=00000000-0000-0000-0000-000000000002",
        ] {
            assert_eq!(
                parse(&format!("okilum://v/Brain/Plan.md?{suffix}")),
                Err(Refused::Malformed),
                "{suffix}"
            );
        }
        assert_eq!(
            parse("okilum://file//a/b.md?vault_id=00000000-0000-0000-0000-000000000001"),
            Err(Refused::Malformed),
            "an identity names a vault, never a file"
        );
        // Mixed case and braces normalize to the canonical spelling.
        assert!(matches!(
            parse("okilum://v/Brain/Plan.md?vault_id=0F8FAD5B-D9CB-469F-A165-70867728950E").unwrap().address,
            Address::VaultId { id, .. } if id == "0f8fad5b-d9cb-469f-a165-70867728950e"
        ));
    }

    #[test]
    fn identity_links_open_across_folder_names_and_refuse_a_different_identity() {
        use crate::vault_link_identity::{for_copy, FILE};
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("Obsidian Vault");
        let second = temp.path().join("Brain");
        for root in [&first, &second] {
            std::fs::create_dir(root).unwrap();
            std::fs::write(root.join("Plan.md"), "# Plan").unwrap();
        }
        let id = for_copy(&first).unwrap();
        // The synced metadata on the other machine, with a field from a newer Okilum.
        std::fs::create_dir(second.join(".okilum")).unwrap();
        let synced = format!(r#"{{"id":"{id}","future":{{"keep":true}}}}"#);
        std::fs::write(second.join(FILE), &synced).unwrap();
        let position = pos(Some(3), None, None);
        let copied = build_vault_copy("Obsidian Vault", &id, "Plan.md", &position).unwrap();
        let link = parse(&copied).unwrap();
        let is_file = |p: &std::path::Path| p.is_file();
        // The other machine's folder is named differently: the identity finds it.
        for root in [&first, &second] {
            assert_eq!(
                resolve(&link, std::slice::from_ref(root), &is_file),
                Resolution::Open {
                    root: root.clone(),
                    rel: "Plan.md".into(),
                    position: position.clone(),
                }
            );
        }
        // Among unrelated vaults, only the one with the identity.
        let other = temp.path().join("Other");
        std::fs::create_dir(&other).unwrap();
        assert!(matches!(
            resolve(&link, &[other.clone(), second.clone()], &is_file),
            Resolution::Open { root, .. } if root == second
        ));
        // A losing identity: a namesake with the note exists, still refused.
        let namesake = temp.path().join("Obsidian Vault 2").join("Obsidian Vault");
        std::fs::create_dir_all(&namesake).unwrap();
        std::fs::write(namesake.join("Plan.md"), "# Plan").unwrap();
        let losing = for_copy(&namesake).unwrap();
        let namesake_bytes = std::fs::read(namesake.join(FILE)).unwrap();
        assert_ne!(losing, id);
        for known in [
            vec![namesake.clone()],
            vec![namesake.clone(), other.clone()],
        ] {
            match resolve(&link, &known, &is_file) {
                Resolution::Unavailable(message) => {
                    assert!(message.contains("different identity"), "{message}")
                }
                other => panic!("a different identity must be refused: {other:?}"),
            }
        }
        // An idless namesake is not a fallback either.
        std::fs::remove_dir_all(namesake.join(".okilum")).unwrap();
        assert!(matches!(
            resolve(&link, std::slice::from_ref(&namesake), &is_file),
            Resolution::Unavailable(m) if m.contains("different identity")
        ));
        assert!(
            !namesake.join(".okilum").exists(),
            "resolution never writes"
        );
        std::fs::create_dir(namesake.join(".okilum")).unwrap();
        std::fs::write(namesake.join(FILE), &namesake_bytes).unwrap();
        // No such vault at all.
        assert!(matches!(
            resolve(&link, std::slice::from_ref(&other), &is_file),
            Resolution::Unavailable(m) if m.contains("not on this computer")
        ));
        // Two vaults with one identity: ambiguous, nothing is picked.
        assert!(matches!(
            resolve(&link, &[first.clone(), second.clone()], &is_file),
            Resolution::Unavailable(m) if m.contains("Several vaults")
        ));
        // Broken metadata never matches and is never repaired.
        std::fs::write(second.join(FILE), "broken").unwrap();
        assert!(matches!(
            resolve(&link, std::slice::from_ref(&second), &is_file),
            Resolution::Unavailable(_)
        ));
        assert_eq!(
            std::fs::read_to_string(second.join(FILE)).unwrap(),
            "broken"
        );
        std::fs::write(second.join(FILE), &synced).unwrap();
        // The legacy folder-name link keeps working and writes nothing.
        let legacy = parse("okilum://v/Brain/Plan.md").unwrap();
        assert!(matches!(
            resolve(&legacy, std::slice::from_ref(&second), &is_file),
            Resolution::Open { root, .. } if root == second
        ));
        assert_eq!(std::fs::read_to_string(second.join(FILE)).unwrap(), synced);
        assert_eq!(std::fs::read(namesake.join(FILE)).unwrap(), namesake_bytes);
    }

    #[test]
    fn file_copy_links_read_back_and_open_the_file_on_its_own() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("Outside # 100% שלום");
        std::fs::create_dir(&folder).unwrap();
        let file = folder.join("Landing & more.md");
        std::fs::write(&file, "# Landing").unwrap();
        let canonical = crate::vault::canonical_root(&file).unwrap();
        for position in copy_positions() {
            let link = build_file_copy(&canonical, &position).unwrap();
            assert!(link.starts_with("okilum://file/"), "{link}");
            let back = parse(&link).unwrap();
            assert_eq!(back.position, position);
            // Its folder is not a known vault: the file opens on its own.
            assert_eq!(
                resolve(&back, &[], &|p| p.is_file()),
                Resolution::SingleFile {
                    file: canonical.clone(),
                    position: position.clone(),
                }
            );
            // Inside a known vault it opens there, as before.
            let root = crate::vault::canonical_root(temp.path()).unwrap();
            assert!(matches!(
                resolve(&back, &[root], &|p| p.is_file()),
                Resolution::Open { rel, .. } if rel == "Outside # 100% שלום/Landing & more.md"
            ));
        }
        // A relative path has no file link.
        assert_eq!(
            build_file_copy(std::path::Path::new("relative/a.md"), &Position::default()),
            Err(NotEncodable::Path)
        );
        // Too long for a link: refused, not truncated.
        let long = temp.path().join("x".repeat(MAX_LEN));
        assert_eq!(
            build_file_copy(&long, &Position::default()),
            Err(NotEncodable::TooLong)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_unicode_is_refused_not_replaced() {
        use std::os::unix::ffi::OsStrExt;
        let raw = std::path::Path::new(std::ffi::OsStr::from_bytes(b"/notes/caf\xe9.md"));
        assert_eq!(
            build_file_copy(raw, &Position::default()),
            Err(NotEncodable::Path)
        );
        // The real file with a replacement character is a different name and
        // stays copyable: the refusal above never points at it.
        let sibling = std::path::Path::new("/notes/caf\u{fffd}.md");
        let link = build_file_copy(sibling, &Position::default()).unwrap();
        assert_eq!(
            parse(&link).unwrap().address,
            Address::File("/notes/caf\u{fffd}.md".into())
        );
        // A name with a line break cannot round-trip: refused.
        assert_eq!(
            build_file_copy(std::path::Path::new("/notes/a\nb.md"), &Position::default()),
            Err(NotEncodable::Path)
        );
        assert_eq!(
            build_vault_copy(
                "Notes",
                "0f8fad5b-d9cb-469f-a165-70867728950e",
                "a\nb.md",
                &Position::default()
            ),
            Err(NotEncodable::Path)
        );
    }

    #[test]
    fn verbatim_windows_file_links_are_their_plain_form() {
        for (link, path) in [
            (
                "okilum://file/%5C%5C%3F%5CC:%5CUsers%5Cme%5Ca.md",
                r"C:\Users\me\a.md",
            ),
            (
                "okilum://file/%5C%5C%3F%5CUNC%5Cserver%5Cshare%5Ca.md",
                "//server/share/a.md",
            ),
        ] {
            assert_eq!(
                parse(link).unwrap().address,
                Address::File(path.into()),
                "{link}"
            );
        }
        // Other verbatim forms have no plain spelling and stay incomplete.
        assert_eq!(
            parse("okilum://file/%5C%5C%3F%5CVolume%7Bx%7D%5Ca.md"),
            Err(Refused::Malformed)
        );
    }

    #[cfg(windows)]
    #[test]
    fn plain_and_verbatim_file_links_match_the_same_windows_vault() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::vault::canonical_root(temp.path()).unwrap();
        let file = root.join("Plan.md");
        std::fs::write(&file, "# Plan").unwrap();
        let plain = crate::vault::display_path(&file);
        let verbatim = file.to_string_lossy().into_owned();
        assert!(verbatim.starts_with(r"\\?\"), "{verbatim}");
        for path in [plain.replace('\\', "/"), plain, verbatim] {
            let encoded = path.replace('\\', "%5C").replace('?', "%3F");
            let link = parse(&format!("okilum://file/{encoded}")).unwrap();
            assert!(
                matches!(resolve(&link, &[root.clone()], &|p| p.is_file()), Resolution::Open { rel, .. } if rel == "Plan.md"),
                "{path}"
            );
        }
        // The copied link of a file outside the vault reads back to that file.
        let link = build_file_copy(&file, &Position::default()).unwrap();
        assert!(matches!(
            resolve(&parse(&link).unwrap(), &[], &|p| p.is_file()),
            Resolution::SingleFile { file: f, .. } if f == file
        ));
    }
}
