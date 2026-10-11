//! External `okilum:` links (#1049, docs/deep-links.md). Parsing only: no file
//! system access, no resolution. Internal Reader URLs (`okilum://open/…` and
//! friends) are rejected here so they can never navigate from outside.

/// Where a link points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Address {
    /// Vault by folder name and a vault-relative path with `/` separators.
    Vault {
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
    /// A clear, user-facing refusal.
    Unavailable(String),
}

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
    // `.md` may be omitted when only the Markdown file exists.
    let pick = |root: &Path, rel: &str| -> Option<String> {
        if exists(&root.join(rel)) {
            return Some(rel.to_owned());
        }
        let md = format!("{rel}.md");
        (Path::new(rel).extension().is_none() && exists(&root.join(&md))).then_some(md)
    };
    match &link.address {
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
            let file = Path::new(file);
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
                None => Resolution::Unavailable(
                    "This file is not in a vault you have opened in Okilum.".into(),
                ),
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
            Address::Vault { vault, path }
        }
        "file" => {
            let path = decode(target);
            let windows =
                path.as_bytes().get(1) == Some(&b':') && path.as_bytes()[0].is_ascii_alphabetic();
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
    let mut position = Position::default();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let number = || decode(value).parse::<u32>().ok().filter(|n| *n > 0);
        match key {
            "line" => position.line = number(),
            "column" => position.column = number(),
            "page" => position.page = number(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
                // Each choice says whether the note is there.
                assert_eq!(roots[0].1.as_deref(), Some("Plan.md"));
                assert_eq!(roots[1].1, None, "Plan.md exists only in the first");
            }
            other => panic!("{other:?}"),
        }
        // file/: the deepest known vault that contains it.
        assert!(matches!(
            r("okilum://file//home/me/Notes/Sub/Vault/Deep.md", &known),
            Resolution::Open { root, rel, .. } if root == Path::new("/home/me/Notes/Sub/Vault") && rel == "Deep.md"
        ));
        assert!(matches!(
            r("okilum://file//work/Notes/Plan.md", &known),
            Resolution::Unavailable(m) if m.contains("not in a vault")
        ));
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
}
