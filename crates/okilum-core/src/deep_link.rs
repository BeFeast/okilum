//! External `okilum:` links (#1049, docs/deep-links.md). Parsing only: no file
//! system access, no resolution. Internal Reader URLs (`okilum://open/…` and
//! friends) are rejected here so they can never navigate from outside.

/// Where a link points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Address {
    /// Vault by folder name and a vault-relative path with `/` separators.
    /// `id` is the vault's own id (`?id=`), which names it when the folder has
    /// another name on this computer; the folder name is the fallback.
    Vault {
        vault: String,
        path: String,
        id: Option<String>,
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
    /// A file in no known vault (`file/…`): it opens on its own, as the
    /// single-file view. `path` is canonical.
    Single {
        path: std::path::PathBuf,
        position: Position,
    },
    /// Several known vaults match the link (same name, or same id): the user
    /// picks one (never guessed). Each root carries the note's path there, or
    /// `None` when the note is not in that vault.
    Choose {
        vault: String,
        roots: Vec<(std::path::PathBuf, Option<String>)>,
        position: Position,
    },
    /// A clear, user-facing refusal.
    Unavailable(String),
}

/// What resolving a link asks of the machine; injectable for tests.
pub struct Env<'a> {
    /// Whether a path is a file.
    pub is_file: &'a dyn Fn(&std::path::Path) -> bool,
    /// The canonical form of an existing path (on Windows the verbatim
    /// `\\?\` form, as the known vault roots have it); `None` when it is
    /// missing.
    pub canonical: &'a dyn Fn(&std::path::Path) -> Option<std::path::PathBuf>,
    /// The id a vault carries in its own files, if it has one.
    pub vault_id: &'a dyn Fn(&std::path::Path) -> Option<String>,
}

/// Match `link` against `known` vault roots (canonical paths).
pub fn resolve(link: &Link, known: &[std::path::PathBuf], env: &Env) -> Resolution {
    resolve_with_name_case(link, known, env, cfg!(any(target_os = "macos", windows)))
}

// Keep the platform's vault-name fallback injectable so tests can exercise
// macOS/Windows name matching and canonical containment together on Linux.
fn resolve_with_name_case(
    link: &Link,
    known: &[std::path::PathBuf],
    env: &Env,
    fold_names: bool,
) -> Resolution {
    use std::path::{Path, PathBuf};
    let exists = env.is_file;
    let position = link.position.clone();
    let name_of = |root: &Path| {
        root.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let same_name = |a: &str, b: &str| {
        if fold_names {
            a.to_lowercase() == b.to_lowercase()
        } else {
            a == b
        }
    };
    // `.md` may be omitted when only the Markdown file exists.
    let pick = |root: &Path, rel: &str| -> Option<String> {
        if exists(&root.join(rel)) {
            return Some(rel.to_owned());
        }
        let md = format!("{rel}.md");
        (Path::new(rel).extension().is_none() && exists(&root.join(&md))).then_some(md)
    };
    match &link.address {
        Address::Vault { vault, path, id } => {
            let mut roots: Vec<PathBuf> = Vec::new();
            // A vault that carries the link's id is that vault, whatever its
            // folder is called here. Without one, the folder name decides; a
            // vault of that name with another id is another vault, so it is
            // not offered: the user would open the wrong notes.
            if let Some(id) = id {
                for root in known {
                    if (env.vault_id)(root).as_deref() == Some(id.as_str()) && !roots.contains(root)
                    {
                        roots.push(root.clone());
                    }
                }
            }
            if roots.is_empty() {
                for root in known {
                    if same_name(&name_of(root), vault)
                        && !roots.contains(root)
                        && (id.is_none() || (env.vault_id)(root).is_none())
                    {
                        roots.push(root.clone());
                    }
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
            // The file as the file system knows it: a link may spell a
            // Windows path `C:/…` while the known roots are `\\?\C:\…`, or
            // go through a symlink.
            let Some(file) = (env.canonical)(Path::new(file)).filter(|file| exists(file)) else {
                return Resolution::Unavailable(
                    "This file does not exist on this computer.".into(),
                );
            };
            // Only Windows paths need spelling/prefix compatibility. POSIX
            // containment must compare native path components: lossy UTF-8 can
            // collapse two different directory names into the same identity.
            let matched = if let Some(file_text) = file.to_str().filter(|path| windows_path(path)) {
                let file_parts = parts(file_text);
                known
                    .iter()
                    .filter_map(|root| {
                        let root_text = root.to_str().filter(|path| windows_path(path))?;
                        let root_parts = parts(root_text);
                        (file_parts.len() > root_parts.len()
                            && root_parts
                                .iter()
                                .zip(&file_parts)
                                .all(|(a, b)| same_part(a, b, true)))
                        .then(|| {
                            (
                                root,
                                file_parts[root_parts.len()..].join("/"),
                                root_parts.len(),
                            )
                        })
                    })
                    .max_by_key(|(_, _, depth)| *depth)
                    .map(|(root, rel, _)| (root, Some(rel)))
            } else {
                // The deepest containing root wins even if its relative path
                // cannot be represented by the Reader's UTF-8 note identity.
                known
                    .iter()
                    .filter_map(|root| {
                        let relative = file.strip_prefix(root).ok()?;
                        (!relative.as_os_str().is_empty()).then_some((root, relative))
                    })
                    .max_by_key(|(root, _)| root.components().count())
                    .map(|(root, relative)| {
                        (
                            root,
                            relative.to_str().map(|_| crate::vault::note_path(relative)),
                        )
                    })
            };
            match matched {
                Some((root, Some(rel))) => Resolution::Open {
                    root: root.clone(),
                    rel,
                    position,
                },
                Some((_, None)) => {
                    Resolution::Unavailable("The document path is not valid UTF-8.".into())
                }
                // No known vault has it: the file opens on its own.
                None => Resolution::Single {
                    path: file,
                    position,
                },
            }
        }
        Address::Note(_) | Address::Task(_) | Address::Project(_) => {
            Resolution::Unavailable(Refused::UnknownKind.message().into())
        }
    }
}

/// The parts of an absolute path as written on any OS. A Windows path
/// (`C:/x`, `C:\x`, `\\server\share`, or the verbatim `\\?\C:\x` and
/// `\\?\UNC\server\share` forms of #1391) is split at `/` and `\` with the
/// verbatim prefix dropped, so every spelling of one file gives the same
/// parts; any other path is split at `/` only (a backslash is a name
/// character there).
fn parts(path: &str) -> Vec<String> {
    let path = if windows_path(path) {
        let path = path.replace('\\', "/");
        match path.strip_prefix("//?/") {
            Some(rest) => rest.strip_prefix("UNC/").unwrap_or(rest).to_owned(),
            None => path,
        }
    } else {
        path.to_owned()
    };
    path.split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

fn windows_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with(r"\\")
        || path.starts_with("//")
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

fn same_part(a: &str, b: &str, fold: bool) -> bool {
    if fold {
        a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

/// A vault id as written in a link: what `Uuid` prints, but any run of
/// letters, digits and `-` of a sane length is read, so another writer's ids
/// still match.
pub fn valid_vault_id(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
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
        Some((rest, fragment)) => (rest, Some(decode(fragment))),
        None => (rest, None),
    };
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (kind, target) = rest.split_once('/').unwrap_or((rest, ""));
    if INTERNAL.contains(&kind) {
        return Err(Refused::Internal);
    }
    let address = match kind {
        "v" => {
            let (vault, path) = target.split_once('/').ok_or(Refused::Malformed)?;
            let vault = decode(vault);
            let path = segments(path)?;
            if vault.is_empty() || vault.contains('/') {
                return Err(Refused::Malformed);
            }
            Address::Vault {
                vault,
                path,
                id: None,
            }
        }
        "file" => {
            let path = decode(target);
            // A verbatim path (`\\?\C:\…`, `\\?\UNC\…`) as Windows hands it
            // over; every other form must be absolute.
            let windows = path.as_bytes().get(1) == Some(&b':')
                && path.as_bytes()[0].is_ascii_alphabetic()
                || path.starts_with(r"\\?\");
            if !(path.starts_with('/') || windows) {
                return Err(Refused::Malformed);
            }
            if path.split(['/', '\\']).any(|s| s == "..") {
                return Err(Refused::UnsafePath);
            }
            Address::File(path)
        }
        "note" | "task" | "project" => {
            let id = decode(target);
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
    let mut address = address;
    let mut position = Position::default();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let number = || decode(value).parse::<u32>().ok().filter(|n| *n > 0);
        match key {
            "line" => position.line = number(),
            "column" => position.column = number(),
            "page" => position.page = number(),
            // The vault's own id; one that is not an id is ignored, so the
            // folder name still decides.
            "id" => {
                if let Address::Vault { id, .. } = &mut address {
                    *id = Some(decode(value)).filter(|value| valid_vault_id(value));
                }
            }
            // Forward compatible: unknown parameters are ignored.
            _ => {}
        }
    }
    if position.line.is_none() {
        position.column = None;
    }
    match fragment.filter(|f| !f.is_empty()) {
        Some(block) if block.starts_with('^') => position.block = Some(block[1..].to_owned()),
        Some(heading) => position.heading = Some(heading),
        None => {}
    }
    Ok(Link { address, position })
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
    build_with_id(vault, None, rel, position)
}

/// `build`, with the vault's own id as `?id=` when it has one: the link then
/// finds the vault on a machine where the folder has another name. A link
/// without it (every link made before ids) keeps working by folder name.
pub fn build_with_id(vault: &str, id: Option<&str>, rel: &str, position: &Position) -> String {
    let path = rel.split('/').map(encode).collect::<Vec<_>>().join("/");
    let mut link = format!("okilum://v/{}/{path}", encode(vault));
    let mut query = Vec::new();
    if let Some(line) = position.line {
        query.push(format!("line={line}"));
        if let Some(column) = position.column {
            query.push(format!("column={column}"));
        }
    }
    if let Some(page) = position.page {
        query.push(format!("page={page}"));
    }
    if let Some(id) = id.filter(|id| valid_vault_id(id)) {
        query.push(format!("id={id}"));
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

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// A vault-relative path: decoded per segment, never empty, never escaping.
fn segments(path: &str) -> Result<String, Refused> {
    let parts: Vec<String> = path.split('/').map(decode).collect();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(link: &str) -> (String, String, Position) {
        match parse(link).unwrap() {
            Link {
                address: Address::Vault { vault, path, .. },
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
                    id: None,
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
        let canonical = |p: &Path| Some(p.to_owned());
        let no_id = |_: &Path| None;
        let env = Env {
            is_file: &exists,
            canonical: &canonical,
            vault_id: &no_id,
        };
        let r = |link: &str, known: &[PathBuf]| resolve(&parse(link).unwrap(), known, &env);
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
        // A file in no known vault opens on its own (docs/deep-links.md).
        assert_eq!(
            r("okilum://file//work/Notes/Plan.md?line=4", &known),
            Resolution::Single {
                path: "/work/Notes/Plan.md".into(),
                position: Position {
                    line: Some(4),
                    ..Default::default()
                }
            }
        );
        // A file that is not there is said, never opened or guessed.
        assert!(matches!(
            r("okilum://file//work/Notes/Gone.md", &known),
            Resolution::Unavailable(m) if m.contains("does not exist")
        ));
        // Reserved ids answer with the newer-Okilum message.
        assert!(
            matches!(r("okilum://project/p1", &known), Resolution::Unavailable(m) if m.contains("newer"))
        );
    }

    #[test]
    fn a_vault_id_rides_in_the_link_and_a_bad_one_is_ignored() {
        let id = "6f1c2a9e-3b7d-4c55-9a10-0d2e8f4b7c31";
        let link = build_with_id("Brain", Some(id), "Dev/Plan.md", &Position::default());
        assert_eq!(link, format!("okilum://v/Brain/Dev/Plan.md?id={id}"));
        assert_eq!(
            parse(&link).unwrap().address,
            Address::Vault {
                vault: "Brain".into(),
                path: "Dev/Plan.md".into(),
                id: Some(id.into())
            }
        );
        // With a position the id sits beside it, and both read back.
        let at = Position {
            line: Some(7),
            column: Some(2),
            ..Default::default()
        };
        let link = build_with_id("Brain", Some(id), "Plan.md", &at);
        assert_eq!(
            link,
            format!("okilum://v/Brain/Plan.md?line=7&column=2&id={id}")
        );
        let parsed = parse(&link).unwrap();
        assert_eq!(parsed.position, at);
        assert!(matches!(parsed.address, Address::Vault { id: Some(_), .. }));
        // A link made before ids has none and still parses; `build` makes it.
        assert_eq!(
            build("Brain", "Plan.md", &Position::default()),
            "okilum://v/Brain/Plan.md"
        );
        assert!(matches!(
            parse("okilum://v/Brain/Plan.md").unwrap().address,
            Address::Vault { id: None, .. }
        ));
        // Not an id (too short, odd characters, empty): ignored, the folder
        // name decides, the link is not refused.
        for bad in [
            "",
            "abc",
            "a b c d e f g h",
            "../../etc/passwd",
            "ünïcode-ünïcode",
        ] {
            let parsed = parse(&format!("okilum://v/Brain/Plan.md?id={bad}")).unwrap();
            assert!(
                matches!(parsed.address, Address::Vault { id: None, .. }),
                "{bad:?}"
            );
        }
        assert_eq!(
            build_with_id("Brain", Some("short"), "Plan.md", &at),
            build("Brain", "Plan.md", &at)
        );
        // The id belongs to a vault address only.
        assert_eq!(
            parse(&format!("okilum://file//a/b.md?id={id}"))
                .unwrap()
                .address,
            Address::File("/a/b.md".into())
        );
    }

    /// The same synced vault is «Obsidian Vault» on three machines and «Brain»
    /// on idunn: its id finds it under either name (#1414).
    #[test]
    fn a_vault_is_found_by_its_id_whatever_its_folder_is_called() {
        use std::path::{Path, PathBuf};
        let id = "6f1c2a9e-3b7d-4c55-9a10-0d2e8f4b7c31";
        let other = "0a9b8c7d-1111-4222-8333-444455556666";
        let files = [
            "/home/god/Obsidian Vault/Dev/Plan.md",
            "/home/god/Notes/Dev/Plan.md",
            "/srv/copy/Brain/Dev/Plan.md",
            "/mnt/Brain/Dev/Plan.md",
        ];
        let exists = |p: &Path| files.iter().any(|f| Path::new(f) == p);
        let canonical = |p: &Path| Some(p.to_owned());
        let ids = |p: &Path| match p.to_str().unwrap() {
            "/home/god/Obsidian Vault" | "/srv/copy/Brain" => Some(id.to_owned()),
            "/home/god/Notes" => Some(other.to_owned()),
            _ => None,
        };
        let env = Env {
            is_file: &exists,
            canonical: &canonical,
            vault_id: &ids,
        };
        let r = |link: &str, known: &[&str]| {
            let known: Vec<PathBuf> = known.iter().map(PathBuf::from).collect();
            resolve(&parse(link).unwrap(), &known, &env)
        };
        let from_idunn = format!("okilum://v/Brain/Dev/Plan.md?id={id}&line=5");
        // Another folder name here: found by id.
        match r(&from_idunn, &["/home/god/Obsidian Vault", "/data/Archive"]) {
            Resolution::Open {
                root,
                rel,
                position,
            } => {
                assert_eq!(root, Path::new("/home/god/Obsidian Vault"));
                assert_eq!(rel, "Dev/Plan.md");
                assert_eq!(position.line, Some(5));
            }
            other => panic!("{other:?}"),
        }
        // Positive control: the same link without the id is looked up by the
        // name «Brain» only, and this machine has no such folder.
        assert!(matches!(
            r("okilum://v/Brain/Dev/Plan.md", &["/home/god/Obsidian Vault"]),
            Resolution::Unavailable(m) if m.contains("Brain")
        ));
        // An old link (folder name, no id) still opens the vault of that name.
        assert!(matches!(
            r(
                "okilum://v/Obsidian%20Vault/Dev/Plan.md",
                &["/home/god/Obsidian Vault"]
            ),
            Resolution::Open { .. }
        ));
        // A folder with the link's name but another id is another vault: not
        // offered, so the link never opens the wrong notes.
        assert!(matches!(
            r(
                &format!("okilum://v/Notes/Dev/Plan.md?id={id}"),
                &["/home/god/Notes"]
            ),
            Resolution::Unavailable(m) if m.contains("not on this computer")
        ));
        // A folder with the name and no id yet (never linked from, or not
        // synced yet): the name still decides.
        assert!(matches!(
            r(
                &format!("okilum://v/Brain/Dev/Plan.md?id={id}"),
                &["/mnt/Brain"]
            ),
            Resolution::Open { root, .. } if root == Path::new("/mnt/Brain")
        ));
        // Two copies of the vault carry the id: a choice, never a guess, and
        // each says whether the note is there.
        match r(
            &from_idunn,
            &["/home/god/Obsidian Vault", "/srv/copy/Brain"],
        ) {
            Resolution::Choose { roots, .. } => {
                assert_eq!(roots.len(), 2);
                assert_eq!(roots[0].1.as_deref(), Some("Dev/Plan.md"));
                assert_eq!(roots[1].1.as_deref(), Some("Dev/Plan.md"));
            }
            other => panic!("{other:?}"),
        }
    }

    /// #1414: canonical paths on case-sensitive macOS volumes must not
    /// borrow the case-insensitive folder-name fallback used by `v/` links.
    /// Injected canonical paths exercise the same containment on every OS.
    #[test]
    fn canonical_posix_file_containment_preserves_case() {
        use std::path::{Path, PathBuf};
        let file = PathBuf::from("/data/notes/a.md");
        let canonical = |_: &Path| Some(file.clone());
        let is_file = |path: &Path| path == file;
        let env = Env {
            is_file: &is_file,
            canonical: &canonical,
            vault_id: &|_| None,
        };
        let link = parse("okilum://file//data/notes/a.md?line=3").unwrap();
        let position = link.position.clone();
        let other = PathBuf::from("/data/Notes");
        // Use the macOS/Windows vault-name policy even on Linux. That
        // policy must never fold canonical POSIX containment as a side effect.
        let resolve =
            |link: &Link, roots: &[PathBuf]| resolve_with_name_case(link, roots, &env, true);
        assert_eq!(
            resolve(&link, std::slice::from_ref(&other)),
            Resolution::Single {
                path: file.clone(),
                position: position.clone(),
            },
            "a differently cased sibling is not the file's vault"
        );
        // Positive control: the containing vault matches; the differently
        // cased root listed last cannot win a tie at the same depth.
        let root = PathBuf::from("/data/notes");
        assert_eq!(
            resolve(&link, &[root.clone(), other]),
            Resolution::Open {
                root: root.clone(),
                rel: "a.md".into(),
                position,
            }
        );
        assert!(
            matches!(
                resolve(
                    &parse("okilum://v/Notes/a.md").unwrap(),
                    std::slice::from_ref(&root)
                ),
                Resolution::Open { root: matched, .. } if matched == root
            ),
            "positive control: case-insensitive vault-name fallback is active"
        );
    }

    /// #1414: a UTF-8 link through a symlink may canonicalize to raw POSIX
    /// bytes. Distinct byte names must not alias through replacement characters.
    #[cfg(unix)]
    #[test]
    fn canonical_posix_file_containment_preserves_raw_bytes() {
        use std::{
            ffi::OsString,
            os::unix::{ffi::OsStringExt, fs::symlink},
            path::Path,
        };
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path().canonicalize().unwrap();
        let target = base.join(OsString::from_vec(vec![0xff]));
        let other = base.join(OsString::from_vec(vec![0xfe]));
        for root in [&target, &other] {
            std::fs::create_dir(root).unwrap();
            std::fs::write(root.join("a.md"), "# Different notes in different roots").unwrap();
        }
        assert_ne!(target, other);
        assert_eq!(
            target.to_string_lossy(),
            other.to_string_lossy(),
            "positive control: lossy names collide"
        );
        let alias = base.join("utf8-alias");
        symlink(&target, &alias).unwrap();
        let link = parse(&format!(
            "okilum://file/{}?line=3",
            alias.join("a.md").to_str().unwrap()
        ))
        .unwrap();
        let file = target.join("a.md");
        assert_eq!(
            alias.join("a.md").canonicalize().unwrap(),
            file,
            "the link really reaches the raw-byte target"
        );
        let env = Env {
            is_file: &Path::is_file,
            canonical: &|path| path.canonicalize().ok(),
            vault_id: &|_| None,
        };
        assert_eq!(
            resolve(&link, std::slice::from_ref(&other), &env),
            Resolution::Single {
                path: file,
                position: link.position.clone(),
            },
            "a lossy-name sibling must not capture the link"
        );
        assert_eq!(
            resolve(&link, &[target.clone(), other], &env),
            Resolution::Open {
                root: target,
                rel: "a.md".into(),
                position: link.position.clone(),
            },
            "positive control: the genuinely containing raw-byte root matches"
        );
        assert_eq!(
            resolve(&link, &[base], &env),
            Resolution::Unavailable("The document path is not valid UTF-8.".into()),
            "an unrepresentable relative identity is refused, never replaced"
        );
    }

    /// #1391/#1414: a Windows file link may spell the path `C:/…`, `C:\…` or
    /// `\\?\C:\…`, while the known roots are verbatim (`\\?\C:\…`): all of
    /// them are the same file.
    #[test]
    fn windows_spellings_of_one_path_are_the_same_parts() {
        let same = [
            r"C:/Users/me/Notes/a.md",
            r"C:\Users\me\Notes\a.md",
            r"\\?\C:\Users\me\Notes\a.md",
        ];
        for spelling in same {
            assert_eq!(
                parts(spelling),
                ["C:", "Users", "me", "Notes", "a.md"],
                "{spelling}"
            );
        }
        assert_eq!(parts(r"\\?\UNC\srv\share\Notes"), ["srv", "share", "Notes"]);
        assert_eq!(parts(r"\\srv\share\Notes"), ["srv", "share", "Notes"]);
        // Elsewhere a backslash is part of a name.
        assert_eq!(parts("/home/me/a\\b.md"), ["home", "me", "a\\b.md"]);
        assert_eq!(parts("/"), Vec::<String>::new());
        // Folding the case is the caller's decision.
        assert!(same_part("Notes", "notes", true) && !same_part("Notes", "notes", false));

        // The resolver: a verbatim root and a plain spelling of a file in it.
        let root = std::path::PathBuf::from(r"\\?\C:\Users\me\Notes");
        let exists = |_: &std::path::Path| true;
        let canonical = |p: &std::path::Path| {
            // What `canonicalize` returns on Windows.
            let text = p.to_str().unwrap().replace('/', r"\");
            Some(std::path::PathBuf::from(format!(r"\\?\{text}")))
        };
        let no_id = |_: &std::path::Path| None;
        let env = Env {
            is_file: &exists,
            canonical: &canonical,
            vault_id: &no_id,
        };
        let known = [root.clone()];
        for link in [
            "okilum://file/C:/Users/me/Notes/Sub/a.md?line=3",
            "okilum://file/C:%5CUsers%5Cme%5CNotes%5CSub%5Ca.md",
            "okilum://file/c:/users/ME/notes/Sub/a.md",
        ] {
            match resolve(&parse(link).unwrap(), &known, &env) {
                Resolution::Open { root: r, rel, .. } => {
                    assert_eq!(r, root, "{link}");
                    assert_eq!(rel, "Sub/a.md", "{link}");
                }
                other => panic!("{link}: {other:?}"),
            }
        }
        // Another folder on the same drive is not inside the vault.
        assert!(matches!(
            resolve(
                &parse("okilum://file/C:/Users/me/NotesOld/a.md").unwrap(),
                &known,
                &env
            ),
            Resolution::Single { .. }
        ));
        // The verbatim form itself is accepted as a link, and `..` is not.
        assert_eq!(
            parse("okilum://file/%5C%5C%3F%5CC%3A%5CUsers%5Cme%5Ca.md")
                .unwrap()
                .address,
            Address::File(r"\\?\C:\Users\me\a.md".into())
        );
        assert_eq!(
            parse("okilum://file/%5C%5C%3F%5CC%3A%5CUsers%5C..%5Ca.md").unwrap_err(),
            Refused::UnsafePath
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
}
