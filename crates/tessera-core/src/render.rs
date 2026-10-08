use crate::prose::replace_in_prose;
use crate::vault::{Resolution, Vault};
use comrak::nodes::{AstNode, NodeValue};
use comrak::options::Plugins;
use comrak::plugins::syntect::SyntectAdapter;
use comrak::{format_html_with_plugins, parse_document, Arena, Options};
use regex::Regex;

pub mod block_embed;

pub const WIKI_SCHEME: &str = "tessera://open/";
pub const UNRESOLVED_SCHEME: &str = "tessera://unresolved/";
/// A link that names several notes. The target is carried verbatim so a client
/// can ask the vault for the candidates and let the reader choose; encoding one
/// of them into the URL here would be the guess this scheme exists to avoid.
pub const AMBIGUOUS_SCHEME: &str = "tessera://ambiguous/";

pub fn comrak_options() -> Options<'static> {
    let mut o = Options::default();
    o.extension.strikethrough = true;
    o.extension.table = true;
    o.extension.tasklist = true;
    o.extension.autolink = true;
    o.extension.footnotes = true;
    o.extension.front_matter_delimiter = Some("---".into());
    o.extension.wikilinks_title_after_pipe = true;
    o.extension.alerts = true;
    o.render.r#unsafe = true;
    o.render.hardbreaks = false;
    o
}

/// Obsidian embeds `![[x]]` are not comrak wikilinks; turn image embeds into
/// standard images and other embeds into plain wikilinks before parsing.
///
/// Code spans and blocks are left alone: an embed written inside one is an
/// example, not an embed (#20).
pub fn preprocess(text: &str) -> String {
    let embed_re = Regex::new(r"!\[\[([^\]\[]+?)\]\]").unwrap();
    replace_in_prose(text, |prose| {
        embed_re
            .replace_all(prose, |cap: &regex::Captures| {
                let inner = cap[1].trim();
                let base = crate::document_links::wiki_parts(inner)
                    .0
                    .split('#')
                    .next()
                    .unwrap_or("")
                    .trim();
                if crate::excalidraw::is_drawing(base) {
                    let alias = crate::document_links::wiki_parts(inner).1.unwrap_or("");
                    let title = alias.replace('"', "&quot;");
                    return format!(
                        "![]({} \"tessera-drawing-size:{}\")",
                        crate::document_links::encode(base),
                        title
                    );
                }
                let ext = base.rsplit('.').next().unwrap_or("").to_lowercase();
                if [
                    "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "heic", "heif",
                ]
                .contains(&ext.as_str())
                {
                    format!("![]({})", base.replace(' ', "%20"))
                } else {
                    format!("[[{inner}]]")
                }
            })
            .into_owned()
    })
}

/// Rewrite wikilink + local image URLs in the AST in place.
pub fn rewrite_links<'a>(root: &'a AstNode<'a>, vault: &Vault, note_rel: &str, source: &str) {
    let lines: Vec<_> = std::iter::once(0)
        .chain(source.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    for node in root.descendants() {
        let mut data = node.data.borrow_mut();
        let pos = data.sourcepos;
        match &mut data.value {
            NodeValue::WikiLink(w) => {
                let authored = lines
                    .get(pos.start.line.saturating_sub(1))
                    .zip(lines.get(pos.end.line.saturating_sub(1)))
                    .and_then(|(a, b)| {
                        source.get((a + pos.start.column.saturating_sub(1))..(b + pos.end.column))
                    })
                    .and_then(|s| s.strip_prefix("[[")?.strip_suffix("]]"))
                    .and_then(|s| s.split('|').next())
                    .map(str::trim);
                w.url = authored
                    .map(|target| crate::document_links::resolve(target, true, vault, note_rel).url)
                    .unwrap_or_else(|| format!("{UNRESOLVED_SCHEME}invalid-source-range"));
            }
            NodeValue::Image(img) => {
                if !img.url.contains("://") {
                    img.url = image_file_url(&img.url, vault, note_rel)
                        .unwrap_or_else(|| "tessera-asset://unavailable".into());
                }
            }
            NodeValue::Link(link) => {
                link.url = crate::document_links::resolve(&link.url, false, vault, note_rel).url;
            }
            _ => {}
        }
    }
}

pub(crate) fn percent_decode(s: &str) -> String {
    crate::document_links::decode(s)
}

/// Split a wikilink target into `(note, heading)`: `a#h` -> `("a", "h")`,
/// `a` -> `("a", "")`, `#h` -> `("", "h")`. A block reference (`a#^id`) has no
/// heading to scroll to and comes back with an empty fragment (#49).
pub fn split_fragment(target: &str) -> (&str, &str) {
    match target.split_once('#') {
        Some((base, frag)) => {
            let frag = frag.trim();
            if frag.starts_with('^') {
                (base.trim(), "")
            } else {
                (base.trim(), frag)
            }
        }
        None => (target.trim(), ""),
    }
}

/// `#heading` for a `tessera://open/` URL, or nothing for an empty fragment.
fn fragment_suffix(fragment: &str) -> String {
    if fragment.is_empty() {
        String::new()
    } else {
        format!("#{}", fragment.replace(' ', "%20"))
    }
}

/// Index of the top-level block that is the heading `[[note#target]]` names,
/// in the Markdown `source` a reader renders — the same index the reader's
/// block list uses, so the shell can scroll straight to it (#49). Only real
/// headings count: a `#` inside a code span or a fence is text.
pub fn heading_block_index(source: &str, target: &str) -> Option<usize> {
    crate::document_links::heading(source, target)
        .ok()
        .map(|h| h.block)
}

/// Split a `tessera://open/` payload (scheme already stripped) into
/// `(rel, heading)`, percent decoded once. The inverse of the emitted URL.
pub fn split_open_url(rest: &str) -> (String, Option<String>) {
    match rest.split_once('#') {
        Some((rel, frag)) if !frag.is_empty() => (percent_decode(rel), Some(percent_decode(frag))),
        Some((rel, _)) => (percent_decode(rel), None),
        None => (percent_decode(rest), None),
    }
}

/// Whether a heading's text is the one a `[[note#Heading]]` link names.
/// Obsidian's rule, not GitHub's slug: the texts match case-insensitively
/// after trimming, and runs of whitespace count as one space (#49).
pub fn heading_matches(heading_text: &str, target: &str) -> bool {
    fn norm(s: &str) -> String {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    }
    let t = norm(target);
    !t.is_empty() && norm(heading_text) == t
}

/// Full HTML pipeline used by the Qt and GPUI slices.
pub fn render_html(vault: &Vault, note_rel: &str, theme: &str) -> anyhow::Result<String> {
    render_html_from_source(vault, note_rel, &vault.read_note(note_rel)?, theme)
}

/// Render an already captured primary source without reopening the note.
pub fn render_html_from_source(
    vault: &Vault,
    note_rel: &str,
    raw: &str,
    theme: &str,
) -> anyhow::Result<String> {
    let mut text = preprocess(raw);
    for (link, destination) in crate::document_links::fallback_links(&text, vault, note_rel)
        .into_iter()
        .rev()
    {
        let (path, heading) = link
            .target
            .split_once('#')
            .map_or((link.target.as_str(), None), |(p, h)| (p, Some(h)));
        let mut encoded = crate::document_links::encode(&crate::document_links::decode(path));
        if let Some(heading) = heading {
            encoded.push('#');
            encoded.push_str(&crate::document_links::encode(
                &crate::document_links::decode(heading),
            ));
        }
        text.replace_range(destination, &encoded);
    }
    let arena = Arena::new();
    let opts = comrak_options();
    let root = parse_document(&arena, &text, &opts);
    rewrite_links(root, vault, note_rel, &text);

    let adapter = SyntectAdapter::new(Some(theme));
    let mut plugins = Plugins::default();
    plugins.render.codefence_syntax_highlighter = Some(&adapter);

    let mut out = String::new();
    format_html_with_plugins(root, &opts, &mut out, &plugins)?;
    Ok(out)
}

/// Rewrite `[[wikilinks]]` and relative `[text](note.md)` links in Markdown
/// *source* into `tessera://` links, for a renderer that consumes Markdown
/// rather than an AST (the GPUI shell does). `from_note` is the vault-relative
/// path of the note being rendered, or `""` for a file outside the vault: bare
/// and qualified links still resolve against the vault, and a `../` link —
/// meaningless without a source location — comes back unresolved.
///
/// Kept in core, next to the AST-based [`rewrite_links`], so both renderers
/// share one resolver and one set of schemes. The shell used to carry its own
/// copy, and its absolute-path branch skipped it entirely (#15).
///
/// Only prose is rewritten. A `[[link]]` inside inline code, a fence or an
/// indented block is text about a link and comes through byte-for-byte; the
/// boundary and link ranges come from Comrak's parsed document.
pub fn rewrite_source_links(text: &str, vault: &Vault, from_note: &str) -> String {
    rewrite_source_links_collect(text, vault, from_note, &mut Vec::new())
}

fn rewrite_source_links_collect(
    text: &str,
    vault: &Vault,
    from_note: &str,
    identities: &mut Vec<crate::document_links::prepared::LinkIdentity>,
) -> String {
    let mut output = text.to_owned();
    let mut resolutions = std::collections::BTreeMap::new();
    for link in crate::document_links::parse_in_vault(text, vault, from_note)
        .into_iter()
        .rev()
    {
        let resolved = resolutions
            .entry((link.wiki, link.target.clone()))
            .or_insert_with(|| {
                crate::document_links::resolve(&link.target, link.wiki, vault, from_note)
            });
        let identity = crate::document_links::prepared::LinkIdentity {
            from: from_note.into(),
            target: link.target.clone(),
            wiki: link.wiki,
            url: resolved.url.clone(),
        };
        if !identities.contains(&identity) {
            identities.push(identity);
        }
        // An unverified range keeps the link as authored rather than splice
        // the rewrite into the wrong bytes (#650).
        if resolved.status == "external" || !link.exact_range {
            continue;
        }
        let label = if link.wiki {
            link.label.replace("\\|", "|").replace('|', "\\|")
        } else {
            link.label
        };
        let title = if link.title.is_empty() {
            String::new()
        } else {
            format!(
                " \"{}\"",
                link.title.replace('\\', "\\\\").replace('"', "\\\"")
            )
        };
        let replacement = format!("[{label}]({}{title})", resolved.url);
        output.replace_range(link.range, &replacement);
    }
    output
}

/// Rewrite relative `![alt](image.png)` references in Markdown source to
/// absolute `file://` URLs resolved against the vault. Remote URLs are left
/// alone; an asset that cannot be found stays as written. Code spans and
/// blocks are not touched (#20).
pub fn rewrite_source_images(text: &str, vault: &Vault, note_rel: &str) -> String {
    rewrite_source_images_with(text, |url| {
        if url.contains("://") {
            return None;
        }
        let raw = percent_decode(url);
        if crate::excalidraw::is_drawing(&raw) {
            let link = crate::document_links::resolve(&raw, true, vault, note_rel);
            let mut candidates = if link.status == "ambiguous" {
                link.candidates.clone()
            } else {
                Vec::new()
            };
            let candidate = match link {
                link if link.status == "attachment" && link.candidates.len() == 1 => {
                    Some(vault.root.join(&link.candidates[0]))
                }
                _ => match vault.resolve_from(&raw, note_rel) {
                    crate::Resolution::Resolved { path } => Some(vault.root.join(path)),
                    crate::Resolution::Ambiguous {
                        candidates: matches,
                    } => {
                        candidates = matches;
                        None
                    }
                    _ => None,
                },
            };
            return Some(
                candidate
                    .and_then(|p| p.canonicalize().ok())
                    .filter(|p| {
                        vault
                            .root
                            .canonicalize()
                            .is_ok_and(|root| p.starts_with(root))
                    })
                    .and_then(|p| url::Url::from_file_path(p).ok().map(String::from))
                    .unwrap_or_else(|| {
                        format!(
                            "tessera-drawing-unavailable:{}",
                            crate::document_links::encode(
                                &serde_json::to_string(&crate::excalidraw::UnavailableDrawing {
                                    target: raw.clone(),
                                    candidates,
                                })
                                .expect("serializable drawing failure")
                            )
                        )
                    }),
            );
        }
        Some(
            image_file_url(url, vault, note_rel)
                .unwrap_or_else(|| "tessera-asset://unavailable".into()),
        )
    })
}

fn image_file_url(target: &str, vault: &Vault, from: &str) -> Option<String> {
    // Cached first paint already owns these image identities. Preserve it while
    // inventory verification is pending; link actions still await verified identity.
    if !vault.inventory_complete && !vault.single_file {
        return vault
            .resolve_asset(&crate::document_links::decode(target), from)
            .and_then(|path| url::Url::from_file_path(path).ok())
            .map(String::from);
    }
    let resolved = crate::document_links::resolve(target, false, vault, from);
    (resolved.status == "attachment")
        .then(|| resolved.candidates.first())
        .flatten()
        .and_then(|path| url::Url::from_file_path(vault.root.join(path)).ok())
        .map(String::from)
}

/// Apply a caller-owned image URL resolver while keeping the reader's prose/code
/// boundary. None preserves the original URL; services can return opaque asset
/// references after reading bytes through their own bounded source store.
pub fn rewrite_source_images_with(
    text: &str,
    mut resolve: impl FnMut(&str) -> Option<String>,
) -> String {
    let arena = Arena::new();
    let root = parse_document(&arena, text, &comrak_options());
    let lines: Vec<_> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    let mut replacements = Vec::new();
    for node in root.descendants() {
        // Image labels are alt text. Avoid overlapping replacement ranges for
        // nested image syntax inside a label.
        if node
            .ancestors()
            .skip(1)
            .any(|parent| matches!(parent.data.borrow().value, NodeValue::Image(_)))
        {
            continue;
        }
        let data = node.data.borrow();
        let NodeValue::Image(image) = &data.value else {
            continue;
        };
        let pos = data.sourcepos;
        let Some(start) = lines.get(pos.start.line.saturating_sub(1)) else {
            continue;
        };
        let Some(end) = lines.get(pos.end.line.saturating_sub(1)) else {
            continue;
        };
        let range = start + pos.start.column.saturating_sub(1)..end + pos.end.column;
        if text.get(range.clone()).is_none() {
            continue;
        }
        let Some(url) = resolve(&image.url) else {
            continue;
        };
        // Preserve authored alt formatting, including brackets in code spans.
        let label = node
            .first_child()
            .zip(node.last_child())
            .and_then(|(first, last)| {
                let a = first.data.borrow().sourcepos.start;
                let b = last.data.borrow().sourcepos.end;
                text.get(
                    (lines.get(a.line.checked_sub(1)?)? + a.column.checked_sub(1)?)
                        ..(lines.get(b.line.checked_sub(1)?)? + b.column),
                )
            })
            .unwrap_or("");
        let title = if image.title.is_empty() {
            String::new()
        } else {
            format!(
                " \"{}\"",
                image.title.replace('\\', "\\\\").replace('"', "\\\"")
            )
        };
        let destination = if url.contains(['(', ')', ' ', '\t', '\n']) {
            format!("<{url}>")
        } else {
            url
        };
        replacements.push((range, format!("![{}]({}{})", label, destination, title)));
    }
    let mut output = text.to_owned();
    for (range, replacement) in replacements.into_iter().rev() {
        output.replace_range(range, &replacement);
    }
    output
}

/// Rewrite Obsidian `==highlight==` spans in Markdown *source* into inline
/// `<mark>highlight</mark>` (#47). markdown-rs has no highlight extension, and
/// `<mark>` is what the shell's HTML renderer already understands.
///
/// Only balanced spans on one line are rewritten, and the text inside may not
/// start or end with whitespace: `==x==` and `==two words==` are highlights,
/// `== x ==`, `==x` and a bare `==` are literal. Code spans and blocks are
/// never touched, so `` `==x==` `` documents the syntax instead of using it
/// (the same boundary as links, drawn by [`crate::prose::prose_spans`]).
pub fn rewrite_highlights(text: &str) -> String {
    let re = Regex::new(r"==([^\s=](?:[^=\n]*[^\s=])?)==").unwrap();
    replace_in_prose(text, |prose| {
        re.replace_all(prose, "<mark>$1</mark>").into_owned()
    })
}

/// Render one paragraph or heading of (already rewritten) Markdown source to
/// a single line of HTML, for a reader that shows highlighted blocks through
/// its HTML path (#47).
///
/// `source` is the block's byte range as the parser reports it, so its
/// continuation lines still carry the container prefixes of the surrounding
/// quote or list (`> `, indentation). Those are stripped: inside one paragraph
/// a leading `>` or run of spaces can only be a prefix, never block structure.
/// Soft line breaks are collapsed to spaces so a hard-wrapped paragraph reflows
/// instead of rendering one visual line per source line.
pub fn block_html(source: &str) -> String {
    let mut markdown = String::with_capacity(source.len());
    for (i, line) in source.lines().enumerate() {
        if i > 0 {
            markdown.push('\n');
            markdown.push_str(line.trim_start_matches([' ', '\t', '>']));
        } else {
            markdown.push_str(line);
        }
    }
    let arena = Arena::new();
    let opts = comrak_options();
    let root = parse_document(&arena, &markdown, &opts);
    let mut out = String::new();
    if comrak::format_html(root, &opts, &mut out).is_err() {
        return String::new();
    }
    out.trim_end().replace('\n', " ")
}

/// Strip leading YAML frontmatter for rendering without normalizing source bytes.
/// Accept LF/CRLF and a UTF-8 BOM. An unterminated block remains visible.
pub fn without_frontmatter(s: &str) -> &str {
    let candidate = s.strip_prefix('\u{feff}').unwrap_or(s);
    let Some(rest) = candidate
        .strip_prefix("---\r\n")
        .or_else(|| candidate.strip_prefix("---\n"))
    else {
        return s;
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line == "---\n" || line == "---\r\n" {
            return &rest[offset + line.len()..];
        }
        offset += line.len();
    }
    s
}

#[cfg(test)]
mod frontmatter_tests {
    use super::without_frontmatter;
    #[test]
    fn render_frontmatter_accepts_crlf_bom_and_preserves_body_bytes() {
        for opening in ["---\n", "---\r\n", "\u{feff}---\r\n"] {
            for closing in ["---\n", "---\r\n"] {
                let source = format!("{opening}type: Note\r\n{closing}# Body\r\n[[Link]]\n");
                assert_eq!(without_frontmatter(&source), "# Body\r\n[[Link]]\n");
            }
        }
        for source in [
            "---\r\nunfinished",
            "# Note\n---\nbody",
            "---\nmetadata\n---",
        ] {
            assert_eq!(without_frontmatter(source), source);
        }
    }
}

/// Produce the Markdown source a reader should render for `rel`, with links and
/// images already rewritten to `tessera://` and `file://`, and `==highlights==`
/// rewritten to `<mark>` (#47).
///
/// `rel` may be **vault-relative** or an **absolute path to a file outside the
/// vault** (a fixture, a scratch note). The only thing an absolute path changes
/// is where the bytes come from — it does not change how links are rewritten.
/// That distinction is the whole of #15: the shell used to branch on
/// `is_absolute()` and skip link rewriting entirely on that path, so one file
/// rendered two different ways depending on how it was addressed.
///
/// A file outside the vault has no vault-relative identity, so relative `../`
/// links in it resolve to nothing rather than being guessed at by suffix.
pub fn note_source(vault: &Vault, rel: &str) -> anyhow::Result<String> {
    let (raw, from) = read_source(vault, rel)?;
    Ok(process_source(
        without_frontmatter(&raw),
        vault,
        from,
        false,
    ))
}

/// [`note_source`] with `![[note]]` and `![[note#Heading]]` embeds expanded
/// inline for the desktop reader (#49). The MCP `read_note` keeps returning
/// [`note_source`]: an agent asking for a note's source wants the embed as
/// written, not the other note's body spliced in.
pub fn reader_source(vault: &Vault, rel: &str) -> anyhow::Result<String> {
    Ok(reader_document(vault, rel)?.rendered)
}

/// One primary-file snapshot for rendering and exact select-all Source copy.
/// Embedded notes contribute only to `rendered`; `original_body` retains their
/// authored embed syntax. Both values exclude the same leading frontmatter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderDocument {
    /// Exact primary-file bytes, shared with native view selection and revision evidence.
    pub canonical_source: std::sync::Arc<str>,
    pub rendered: String,
    pub original_body: String,
    pub links: Vec<crate::document_links::prepared::LinkIdentity>,
    /// The leading YAML frontmatter text, for read-only properties (#386).
    pub frontmatter: Option<String>,
}

/// Read the primary note once and derive both Reader representations from it.
/// This avoids pairing displayed input with a later external edit when copying.
/// As with [`reader_source`], `rel` accepts vault-relative and absolute paths.
pub fn reader_document(vault: &Vault, rel: &str) -> anyhow::Result<ReaderDocument> {
    let (raw, from) = read_source(vault, rel)?;
    Ok(reader_document_from_source(vault, from, &raw))
}

/// Derive Reader input from a captured source. A provisional vault keeps embeds
/// pending until reconciliation, so a warm first publication need not hydrate them.
pub fn reader_document_from_source(vault: &Vault, from: &str, raw: &str) -> ReaderDocument {
    let body = without_frontmatter(raw);
    let mut links = Vec::new();
    let rendered = process_source_collect(body, vault, from, true, true, &mut links);
    ReaderDocument {
        canonical_source: raw.into(),
        rendered,
        original_body: body.to_string(),
        links,
        frontmatter: crate::properties::frontmatter_block(raw).map(str::to_owned),
    }
}

#[cfg(test)]
mod reader_document_tests {
    use super::*;

    #[test]
    fn captured_primary_source_renders_without_reopening_the_file() {
        let fixture = tempfile::tempdir().unwrap();
        let mut vault = Vault::from_note_paths(["note.md".into(), "Target.md".into()]);
        vault.root = fixture.path().to_owned();
        // The selected note is currently absent: only the persisted input exists.
        let source = "---\ntitle: Saved\n---\n# Saved\n\n[[Target]]\n\n![[Target]]\n";
        let document = reader_document_from_source(&vault, "note.md", source);
        assert_eq!(document.canonical_source.as_ref(), source);
        assert_eq!(document.original_body, without_frontmatter(source));
        assert_eq!(document.frontmatter.as_deref(), Some("title: Saved\n"));
        assert!(document.rendered.contains("Saved"));
        assert!(document.rendered.contains(EMBED_PENDING));
        assert!(
            render_html_from_source(&vault, "note.md", source, "InspiredGitHub")
                .unwrap()
                .contains("Saved")
        );
        assert!(reader_document(&vault, "note.md").is_err());
    }

    #[test]
    fn preserves_authored_body_across_reader_rewrites_and_path_forms() {
        let root = tempfile::Builder::new()
            .prefix("tessera-reader-source-")
            .tempdir()
            .unwrap();
        std::fs::write(root.path().join("target.md"), "# Target\n\nEmbedded text\n").unwrap();
        std::fs::write(root.path().join("image.png"), b"fixture").unwrap();
        let body = "\n  \n# Body\n\n==highlight== <mark>authored</mark> שלום café\n\n> [!note] Callout\n> [[target]]\n\n![image](image.png)\n\n![[target]]\n\n`==literal==`\n\n```md\n==code==\n```\n\n  \n";
        let path = root.path().join("note.md");
        for ending in ["\n", "\r\n"] {
            let body = body.replace('\n', ending);
            let raw = format!("\u{feff}---{ending}type: Note{ending}---{ending}{body}");
            std::fs::write(&path, &raw).unwrap();
            let vault = Vault::scan(root.path()).unwrap();
            for rel in ["note.md", path.to_str().unwrap()] {
                let document = reader_document(&vault, rel).unwrap();
                assert_eq!(document.canonical_source.as_bytes(), raw.as_bytes());
                assert_eq!(document.original_body.as_bytes(), body.as_bytes());
                assert!(document.rendered.contains("<mark>highlight</mark>"));
                assert!(document.rendered.contains("<mark>authored</mark>"));
                if ending == "\n" {
                    assert!(
                        document.rendered.contains("Embedded text"),
                        "{}",
                        document.rendered
                    );
                    assert!(!document.rendered.contains("![[target]]"));
                }
                assert!(document
                    .rendered
                    .contains(&format!("{WIKI_SCHEME}target.md")));
                assert!(document.rendered.contains("file://"));
                assert_eq!(reader_source(&vault, rel).unwrap(), document.rendered);
            }
        }
    }

    #[test]
    fn reload_keeps_equal_rendered_spellings_distinct_and_read_errors_explicit() {
        let root = tempfile::Builder::new()
            .prefix("tessera-reader-source-")
            .tempdir()
            .unwrap();
        let path = root.path().join("note.md");
        std::fs::write(&path, "==same==\n").unwrap();
        let vault = Vault::scan(root.path()).unwrap();
        let first = reader_document(&vault, "note.md").unwrap();
        std::fs::write(&path, "<mark>same</mark>\n").unwrap();
        let second = reader_document(&vault, "note.md").unwrap();
        assert_eq!(first.rendered, second.rendered);
        assert_eq!(first.original_body, "==same==\n");
        assert_eq!(second.original_body, "<mark>same</mark>\n");
        std::fs::remove_file(path).unwrap();
        assert!(reader_document(&vault, "note.md").is_err());
        assert_eq!(first.original_body, "==same==\n");
    }

    #[test]
    fn preserves_whitespace_only_body_and_unterminated_frontmatter() {
        let root = tempfile::Builder::new()
            .prefix("tessera-reader-source-")
            .tempdir()
            .unwrap();
        let path = root.path().join("note.md");
        std::fs::write(&path, "").unwrap();
        let vault = Vault::scan(root.path()).unwrap();
        for raw in [
            "",
            " \r\n\t\r\n",
            "---\nkey: unfinished\n",
            "---\r\nkey: value\r\n---\r\n \r\n\t",
        ] {
            std::fs::write(&path, raw).unwrap();
            assert_eq!(
                reader_document(&vault, "note.md").unwrap().original_body,
                without_frontmatter(raw)
            );
        }
    }
}

fn read_source<'a>(vault: &Vault, rel: &'a str) -> anyhow::Result<(String, &'a str)> {
    let path = std::path::Path::new(rel);
    if path.is_absolute() {
        Ok((std::fs::read_to_string(path)?, ""))
    } else {
        Ok((vault.read_note(rel)?, rel))
    }
}

/// The rewrite pipeline for one note body, frontmatter already stripped.
/// `expand` runs the embed pass first, while `![[x]]` is still an embed:
/// [`preprocess`] turns whatever is left into a plain link, which is also
/// how an embed inside an embedded body becomes a link (depth 1).
fn process_source(body: &str, vault: &Vault, from: &str, expand: bool) -> String {
    process_source_collect(body, vault, from, expand, false, &mut Vec::new())
}

/// `reader` adds the Obsidian presentation passes (#651): comments hidden,
/// math as source, footnotes numbered, block IDs marked. The MCP source keeps
/// the note as written.
fn process_source_collect(
    body: &str,
    vault: &Vault,
    from: &str,
    expand: bool,
    reader: bool,
    identities: &mut Vec<crate::document_links::prepared::LinkIdentity>,
) -> String {
    let body = if reader {
        std::borrow::Cow::Owned(crate::obsidian::before_links(body))
    } else {
        std::borrow::Cow::Borrowed(body)
    };
    let s = if expand {
        expand_embeds_collect(&body, vault, from, identities)
    } else {
        body.into_owned()
    };
    let s = preprocess(&s);
    let s = rewrite_source_images(&s, vault, from);
    let s = rewrite_source_links_collect(&s, vault, from, identities);
    let s = rewrite_highlights(&s);
    if reader {
        crate::obsidian::after_links(&s)
    } else {
        s
    }
}

/// Heading-relevant Reader transforms over an already-read source snapshot.
/// Standalone embeds retain the same fenced block boundaries as rendering, but
/// their bodies are not read: fenced contents contribute no document anchors.
pub fn reader_heading_source(vault: &Vault, rel: &str, raw: &str) -> String {
    let body = crate::obsidian::before_links(without_frontmatter(raw));
    let structural = expand_embeds_structure(&body, vault, rel, &mut Vec::new(), false);
    let s = process_source(&structural, vault, rel, false);
    crate::obsidian::after_links(&s)
}

/// Info-string tag of the fence an expanded embed is wrapped in. The shell's
/// block parser matches a code node with this language and renders it as an
/// embed; every other renderer shows the body as a code block, which is
/// legible if not pretty.
pub const EMBED_LANG: &str = "embed";
/// Info-string marker for an embed whose target is not in the vault; the rest
/// of the line is the target as written.
pub const EMBED_MISSING: &str = "missing";
/// Inventory is still loading; absence has not been established.
pub const EMBED_PENDING: &str = "pending";

/// Expand `![[note]]` and `![[note#Heading]]` embeds that stand alone on a
/// line into the target note's body (or that heading's section), each already
/// rewritten against *its own* note and wrapped in a tilde fence tagged
/// [`EMBED_LANG`] with the `tessera://open/` payload (`path#heading`) as the
/// info string (#49).
///
/// - Only targets that resolve to a note are expanded; an image embed, an
///   ambiguous target, or an embed inside code is left for [`preprocess`] as
///   before. An unresolved target, a resolved note that cannot be read, or a
///   heading the note does not have, renders as an `embed missing` fence.
/// - Depth is 1: embeds inside the embedded body are not expanded, so they
///   come out as plain links. A note embedding itself is the one cycle depth
///   cannot break and is reported as missing.
/// - The fence is longer than any run of tildes starting a line of the body,
///   so the body cannot close it early.
pub fn expand_embeds(text: &str, vault: &Vault, from_note: &str) -> String {
    expand_embeds_collect(text, vault, from_note, &mut Vec::new())
}

fn expand_embeds_collect(
    text: &str,
    vault: &Vault,
    from_note: &str,
    identities: &mut Vec<crate::document_links::prepared::LinkIdentity>,
) -> String {
    expand_embeds_structure(text, vault, from_note, identities, true)
}

fn expand_embeds_structure(
    text: &str,
    vault: &Vault,
    from_note: &str,
    identities: &mut Vec<crate::document_links::prepared::LinkIdentity>,
    read_bodies: bool,
) -> String {
    let embed_re = Regex::new(r"^[ \t]*!\[\[([^\]\[]+?)\]\][ \t]*$").unwrap();
    replace_in_prose(text, |prose| {
        let mut out = String::with_capacity(prose.len());
        for line in prose.split_inclusive('\n') {
            let Some(cap) = embed_re.captures(line.trim_end_matches('\n')) else {
                out.push_str(line);
                continue;
            };
            let inner = cap[1].trim();
            let target = inner.split('|').next().unwrap_or("").trim();
            let (base, heading) = split_fragment(target);
            // `![[note#^id]]` embeds one block (#651); split_fragment drops it.
            let block = target
                .split_once('#')
                .and_then(|(_, f)| f.trim().strip_prefix('^'))
                .filter(|id| !id.is_empty());
            let heading = block.map_or(heading, |_| "");
            let ext = base.rsplit('.').next().unwrap_or("").to_lowercase();
            if crate::excalidraw::is_drawing(base)
                || (!base.is_empty() && base.contains('.') && ext != "md")
            {
                // An image or other asset: not ours.
                out.push_str(line);
                continue;
            }
            if !(vault.inventory_complete || vault.single_file && vault.inventory_scanned) {
                if let Some(id) = block {
                    let info = block_embed::Info {
                        path: None,
                        title: Vault::title_of(base),
                        id: id.into(),
                        status: block_embed::Status::Pending,
                    };
                    out.push_str(&format!(
                        "\n~~~~{EMBED_LANG} {}\n~~~~\n\n",
                        info.fence_meta()
                    ));
                    continue;
                }
                out.push_str(&format!(
                    "\n~~~~{EMBED_LANG} {EMBED_PENDING} {target}\n~~~~\n\n"
                ));
                continue;
            }
            let path = if base.is_empty() {
                from_note.to_string()
            } else {
                match vault.resolve_from(base, from_note) {
                    Resolution::Resolved { path } => path,
                    // Several notes answer: leave the ambiguous link for the
                    // reader to pick from, as with a plain wikilink.
                    Resolution::Ambiguous { .. } => {
                        out.push_str(line);
                        continue;
                    }
                    Resolution::Unresolved => String::new(),
                }
            };
            if !read_bodies {
                // Match the rendering branch's prose boundaries without reading
                // embedded notes (resolved, missing, self, and unreadable alike).
                out.push_str(&format!("\n~~~~{EMBED_LANG}\n~~~~\n\n"));
                continue;
            }
            if let Some(id) = block {
                use block_embed::{Info, Status};
                let mut info = Info {
                    path: (!path.is_empty()).then(|| path.clone()),
                    title: Vault::title_of(if path.is_empty() { base } else { &path }),
                    id: id.into(),
                    status: Status::MissingNote,
                };
                let body = if path.is_empty() {
                    None
                } else if path == from_note {
                    info.status = Status::SelfReference;
                    None
                } else {
                    match vault.read_note(&path) {
                        Ok(raw) => {
                            info.title = crate::note_title::from_bytes(
                                &raw.as_bytes()[..raw.len().min(16 * 1024)],
                            )
                            .unwrap_or(info.title);
                            match crate::obsidian::block_section_result(
                                without_frontmatter(&raw),
                                id,
                            ) {
                                Ok(body) => {
                                    info.status = Status::Ready;
                                    Some(body)
                                }
                                Err(crate::obsidian::BlockSectionFailure::Missing) => {
                                    info.status = Status::MissingBlock;
                                    None
                                }
                                Err(crate::obsidian::BlockSectionFailure::Ambiguous) => {
                                    info.status = Status::DuplicateBlock;
                                    None
                                }
                            }
                        }
                        Err(_) => {
                            info.status = Status::UnreadableNote;
                            None
                        }
                    }
                };
                let body = body
                    .map(|body| {
                        process_source_collect(&body, vault, &path, false, true, identities)
                    })
                    .unwrap_or_default();
                let fence = "~".repeat(tilde_fence_len(&body));
                out.push_str(&format!(
                    "\n{fence}{EMBED_LANG} {}\n{}{fence}\n\n",
                    info.fence_meta(),
                    if body.is_empty() {
                        String::new()
                    } else {
                        format!("{}\n", body.trim_end_matches('\n'))
                    }
                ));
                continue;
            }
            let body = if path.is_empty() || path == from_note {
                None
            } else {
                vault.read_note(&path).ok().and_then(|raw| {
                    let body = without_frontmatter(&raw);
                    if let Some(id) = block {
                        crate::obsidian::block_section(body, id)
                    } else if heading.is_empty() {
                        Some(body.to_string())
                    } else {
                        heading_section(body, heading)
                    }
                })
            };
            match body {
                Some(body) => {
                    let body = process_source_collect(&body, vault, &path, false, true, identities);
                    let fence = "~".repeat(tilde_fence_len(&body));
                    let fragment = match block {
                        Some(id) => format!("#^{id}"),
                        None => fragment_suffix(heading),
                    };
                    out.push_str(&format!(
                        "\n{fence}{EMBED_LANG} {}{}\n{}\n{fence}\n\n",
                        path.replace(' ', "%20"),
                        fragment,
                        body.trim_end_matches('\n')
                    ));
                }
                None => {
                    out.push_str(&format!(
                        "\n~~~~{EMBED_LANG} {EMBED_MISSING} {target}\n~~~~\n\n"
                    ));
                }
            }
        }
        out
    })
}

/// A tilde run one longer than any that starts a line of `body`, and at
/// least four, so it cannot be closed from inside.
fn tilde_fence_len(body: &str) -> usize {
    body.lines()
        .map(|l| l.trim_start().chars().take_while(|&c| c == '~').count())
        .max()
        .unwrap_or(0)
        .max(3)
        + 1
}

/// The section of `body` under the heading `[[note#target]]` names: the
/// heading line through the line before the next heading of the same or a
/// higher level. Headings inside code are text. `None` when no heading
/// matches.
pub fn heading_section(body: &str, target: &str) -> Option<String> {
    let prose = crate::prose::prose_spans(body);
    let is_prose = |off: usize| prose.iter().any(|r| r.contains(&off));
    let mut start = None;
    let mut level = 0;
    let mut off = 0;
    for line in body.split_inclusive('\n') {
        let at = off;
        off += line.len();
        let Some((lvl, text)) = atx_heading(line) else {
            continue;
        };
        if !is_prose(at) {
            continue;
        }
        match start {
            None if heading_matches(text, target) => {
                start = Some(at);
                level = lvl;
            }
            Some(s) if lvl <= level => return Some(body[s..at].to_string()),
            _ => {}
        }
    }
    start.map(|s| body[s..].to_string())
}

/// `(level, text)` of an ATX heading line, or `None`.
fn atx_heading(line: &str) -> Option<(usize, &str)> {
    let t = line.trim_start_matches(' ');
    let level = t.chars().take_while(|&c| c == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &t[level..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim();
    Some((level, text))
}

/// Strip inline Markdown from one line for plain display (backlink context,
/// #22). Emphasis markers go (`**x**`, `__x__`, `*x*`, `_x_` -> `x`), wikilinks
/// keep their alias or target (`[[a|b]]` -> `b`, `[[a#h]]` -> `a`), Markdown
/// links keep their text (`[t](url)` -> `t`), and leading block markers
/// (`>`, `-`, `*`, `+`, `1.`, `#`, task boxes) are trimmed. Code spans are
/// copied verbatim, backticks included: code stays code.
///
/// Hand-rolled rather than a Markdown parse because the input is a single
/// source line that may start mid-construct, and the goal is readable text,
/// not a faithful AST.
pub fn strip_inline_markdown(line: &str) -> String {
    strip_inline_markdown_tracking(line, None).0
}

/// `strip_inline_markdown`, also reporting where one wikilink landed.
///
/// `want` is a raw wikilink target as written (`[[target]]`, `[[target#h]]`,
/// `[[target|alias]]` all have target `target`). The returned range is the byte
/// span, inside the returned string, of the display text of the first wikilink
/// on the line whose target equals `want`; `None` when no such link is on the
/// line. Exists so the backlinks panel can mark the link that produced the
/// backlink inside its context line (#38) without a second pass that would
/// have to re-derive what this function stripped.
pub fn strip_inline_markdown_tracking(
    line: &str,
    want: Option<&str>,
) -> (String, Option<std::ops::Range<usize>>) {
    let body = strip_block_markers(line.trim());
    let mut found = None;
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '`' => {
                // Code span: copy through the matching closing run, or to the
                // end of the line if it never closes.
                let run = chars[i..].iter().take_while(|&&x| x == '`').count();
                let mut j = i + run;
                let mut close = None;
                while j < chars.len() {
                    if chars[j] == '`' {
                        let r = chars[j..].iter().take_while(|&&x| x == '`').count();
                        if r == run {
                            close = Some(j + r);
                            break;
                        }
                        j += r;
                    } else {
                        j += 1;
                    }
                }
                let end = close.unwrap_or(chars.len());
                out.extend(&chars[i..end]);
                i = end;
            }
            '[' if chars.get(i + 1) == Some(&'[') => {
                // Wikilink. `![[...]]` embeds arrive here with the `!` already
                // emitted; that is fine for a context line.
                match find_seq(&chars, i + 2, &[']', ']']) {
                    Some(end) => {
                        let inner: String = chars[i + 2..end].iter().collect();
                        let shown = match inner.split_once('|') {
                            Some((_, alias)) => alias.to_string(),
                            None => inner.split(['#', '^']).next().unwrap_or(&inner).to_string(),
                        };
                        let target = inner.split(['#', '^', '|']).next().unwrap_or("").trim();
                        let start = out.len();
                        out.push_str(shown.trim());
                        if found.is_none() && want == Some(target) {
                            found = Some(start..out.len());
                        }
                        i = end + 2;
                    }
                    None => {
                        out.push(c);
                        i += 1;
                    }
                }
            }
            '[' => {
                // `[text](url)`: keep the text. Anything else with a bracket
                // is left alone.
                match find_seq(&chars, i + 1, &[']', '(']) {
                    Some(mid) => match chars[mid + 2..].iter().position(|&x| x == ')') {
                        Some(off) => {
                            let text: String = chars[i + 1..mid].iter().collect();
                            let base = out.len();
                            let (inner, range) = strip_inline_markdown_tracking(&text, want);
                            out.push_str(&inner);
                            if found.is_none() {
                                found = range.map(|r| base + r.start..base + r.end);
                            }
                            i = mid + 2 + off + 1;
                        }
                        None => {
                            out.push(c);
                            i += 1;
                        }
                    },
                    None => {
                        out.push(c);
                        i += 1;
                    }
                }
            }
            '*' => i += 1,
            '_' => {
                // Emphasis, unless it sits inside a word (`snake_case`).
                let prev_word = i > 0 && chars[i - 1].is_alphanumeric();
                let next_word = chars.get(i + 1).is_some_and(|x| x.is_alphanumeric());
                if prev_word && next_word {
                    out.push(c);
                }
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    (out, found)
}

/// Leading block markers of a source line: quotes, headings, list bullets,
/// ordered-list numbers, task boxes — any of them, nested, in any order.
fn strip_block_markers(mut s: &str) -> &str {
    loop {
        let t = s.trim_start();
        let next = if let Some(r) = t.strip_prefix('>') {
            r
        } else if t.starts_with('#') {
            let r = t.trim_start_matches('#');
            match r.strip_prefix(' ') {
                Some(r) => r,
                None => return t,
            }
        } else if let Some(r) = t
            .strip_prefix("- [ ] ")
            .or_else(|| t.strip_prefix("- [x] "))
        {
            r
        } else if let Some(r) = t
            .strip_prefix("- ")
            .or_else(|| t.strip_prefix("* "))
            .or_else(|| t.strip_prefix("+ "))
        {
            r
        } else if let Some(r) = t.strip_prefix(|c: char| c.is_ascii_digit()) {
            let r = r.trim_start_matches(|c: char| c.is_ascii_digit());
            match r.strip_prefix(". ").or_else(|| r.strip_prefix(") ")) {
                Some(r) => r,
                None => return t,
            }
        } else {
            return t;
        };
        if next.len() == t.len() {
            return t;
        }
        s = next;
    }
}

fn find_seq(chars: &[char], from: usize, seq: &[char]) -> Option<usize> {
    if from >= chars.len() {
        return None;
    }
    chars[from..]
        .windows(seq.len())
        .position(|w| w == seq)
        .map(|p| from + p)
}

#[cfg(test)]
mod callout_passthrough_tests {
    //! A callout is a blockquote to this module, and must come through
    //! `note_source` byte-for-byte so the shell's block parser sees the
    //! `[!type]` marker it matches on (#46).
    use super::*;

    const CALLOUTS: &str = "\
# T

> [!warning] Careful
> Body with `code` and a [[missing]] link.
>
> - one
> - two

> [!tip]-
> folded [text](https://x.y/z)
";

    #[test]
    fn callouts_survive_source_rewriting() {
        let root = std::env::temp_dir().join(format!("tessera-callout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("n.md"), CALLOUTS).unwrap();
        let vault = Vault::scan(&root).unwrap();
        let out = note_source(&vault, "n.md").unwrap();
        assert!(
            out.contains(
                "> [!warning] Careful
> Body with `code` and a [missing](tessera://unresolved/missing) link.
>
> - one
> - two
"
            ),
            "{out}"
        );
        assert!(
            out.contains(
                "> [!tip]-
> folded [text](https://x.y/z)
"
            ),
            "{out}"
        );
        assert_eq!(preprocess(CALLOUTS), CALLOUTS);
        assert_eq!(rewrite_source_images(CALLOUTS, &vault, "n.md"), CALLOUTS);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_callout_marker_is_prose_not_code() {
        let spans = crate::prose::prose_spans(CALLOUTS);
        let marker = CALLOUTS.find("[!warning]").unwrap();
        assert!(spans.iter().any(|s| s.contains(&marker)), "{spans:?}");
    }
}

#[cfg(test)]
mod highlight_tests {
    //! `==x==` becomes `<mark>x</mark>` in prose, and nowhere else (#47).
    use super::*;

    #[test]
    fn balanced_spans_are_rewritten() {
        assert_eq!(rewrite_highlights("a ==b== c"), "a <mark>b</mark> c");
        assert_eq!(
            rewrite_highlights("==two words== and ==more==."),
            "<mark>two words</mark> and <mark>more</mark>."
        );
        assert_eq!(rewrite_highlights("==x=="), "<mark>x</mark>");
        assert_eq!(
            rewrite_highlights("- [ ] task with ==hl== inside"),
            "- [ ] task with <mark>hl</mark> inside"
        );
        assert_eq!(
            rewrite_highlights("**bold ==in== bold**"),
            "**bold <mark>in</mark> bold**"
        );
    }

    #[test]
    fn unbalanced_spaced_and_bare_markers_stay_literal() {
        for s in [
            "==x",
            "x==",
            "a == b",
            "== x ==",
            "==",
            "====",
            "== ==",
            "a ==\nb==",
        ] {
            assert_eq!(rewrite_highlights(s), s, "{s:?}");
        }
    }

    #[test]
    fn code_is_skipped() {
        assert_eq!(rewrite_highlights("use `==x==` here"), "use `==x==` here");
        assert_eq!(
            rewrite_highlights("```\n==x==\n```\n==y==\n"),
            "```\n==x==\n```\n<mark>y</mark>\n"
        );
        assert_eq!(
            rewrite_highlights("    ==indented code==\n"),
            "    ==indented code==\n"
        );
        assert_eq!(
            rewrite_highlights("mixed `==a==` and ==b=="),
            "mixed `==a==` and <mark>b</mark>"
        );
    }

    #[test]
    fn callout_body_is_prose() {
        assert_eq!(
            rewrite_highlights("> [!note] Title\n> body with ==hl== here\n"),
            "> [!note] Title\n> body with <mark>hl</mark> here\n"
        );
    }

    #[test]
    fn note_source_rewrites_highlights_after_links() {
        let root = std::env::temp_dir().join(format!("tessera-hl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("target.md"), "# T\n").unwrap();
        std::fs::write(
            root.join("n.md"),
            "==see== [[target]] and `==code==`\n\n- [ ] open\n- [x] done\n- [X] DONE\n  - [ ] nested\n",
        )
        .unwrap();
        let vault = Vault::scan(&root).unwrap();
        let out = note_source(&vault, "n.md").unwrap();
        assert_eq!(
            out,
            format!(
                "<mark>see</mark> [target]({WIKI_SCHEME}target.md) and `==code==`\n\n- [ ] open\n- [x] done\n- [X] DONE\n  - [ ] nested\n"
            )
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn block_html_strips_container_prefixes_on_continuation_lines() {
        assert_eq!(
            block_html("a <mark>b</mark>\n> c\n>   d"),
            "<p>a <mark>b</mark> c d</p>"
        );
        assert_eq!(
            block_html("item <mark>b</mark>\n  wrapped"),
            "<p>item <mark>b</mark> wrapped</p>"
        );
    }

    #[test]
    fn block_html_renders_headings() {
        assert_eq!(
            block_html("## Title <mark>hl</mark>"),
            "<h2>Title <mark>hl</mark></h2>"
        );
    }

    #[test]
    fn block_html_keeps_marks_links_and_code() {
        let html = block_html("a <mark>b</mark> [t](tessera://open/x.md) `c`\nwrapped **d**");
        assert_eq!(
            html,
            "<p>a <mark>b</mark> <a href=\"tessera://open/x.md\">t</a> <code>c</code> wrapped <strong>d</strong></p>"
        );
    }
}

#[cfg(test)]
mod strip_tests {
    use super::strip_inline_markdown as strip;

    #[test]
    fn strong_and_emphasis() {
        assert_eq!(strip("**Read this file first**"), "Read this file first");
        assert_eq!(strip("__x__ and _y_"), "x and y");
        assert_eq!(strip("*x* is *not* __y__"), "x is not y");
    }

    #[test]
    fn underscores_inside_words_survive() {
        assert_eq!(
            strip("see snake_case_name here"),
            "see snake_case_name here"
        );
    }

    #[test]
    fn code_spans_are_kept_verbatim() {
        assert_eq!(strip("run `**not bold**` now"), "run `**not bold**` now");
        assert_eq!(strip("``a ` b`` and *x*"), "``a ` b`` and x");
        assert_eq!(strip("unclosed `[[a|b]] *x*"), "unclosed `[[a|b]] *x*");
    }

    #[test]
    fn wikilinks() {
        assert_eq!(strip("[[AGENTS|AGENTS.md]] first"), "AGENTS.md first");
        assert_eq!(strip("see [[_AgentContract]]"), "see _AgentContract");
        assert_eq!(
            strip("[[note#Heading]] and [[note^block|alias]]"),
            "note and alias"
        );
        assert_eq!(strip("[[broken"), "[[broken");
    }

    #[test]
    fn markdown_links() {
        assert_eq!(
            strip("open [the runbook](https://x.y/z) now"),
            "open the runbook now"
        );
        assert_eq!(strip("[**bold** text](u)"), "bold text");
        assert_eq!(strip("[not a link] (x)"), "[not a link] (x)");
    }

    #[test]
    fn leading_markers() {
        assert_eq!(strip("> quoted"), "quoted");
        assert_eq!(strip("- bullet"), "bullet");
        assert_eq!(strip("* bullet"), "bullet");
        assert_eq!(strip("1. first"), "first");
        assert_eq!(strip("## Heading"), "Heading");
        assert_eq!(strip("> - [ ] nested task"), "nested task");
        assert_eq!(strip("#tag stays"), "#tag stays");
        assert_eq!(strip("2024 was a year"), "2024 was a year");
    }

    #[test]
    fn issue_example() {
        assert_eq!(
            strip("- **Read this file first**: [[AGENTS|AGENTS.md]] and `rg`"),
            "Read this file first: AGENTS.md and `rg`"
        );
    }
}

#[cfg(test)]
mod drawing_path_tests {
    #[test]
    fn drawing_embed_punctuation_survives_markdown_parsing() {
        let source = super::preprocess("![[folder/a (v2)%\\draft.excalidraw|300]]");
        let arena = comrak::Arena::new();
        let root = comrak::parse_document(&arena, &source, &super::comrak_options());
        let urls = root
            .descendants()
            .filter_map(|node| match &node.data.borrow().value {
                comrak::nodes::NodeValue::Image(link) => Some(link.url.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(urls, ["folder/a%20%28v2%29%25%5Cdraft.excalidraw"]);
    }
}
