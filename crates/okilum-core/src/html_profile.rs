//! Versioned, script-free note and view HTML for native clients and publishing.
//!
//! Paths in `okilum-note:` and `okilum-asset:` are vault-relative and encoded
//! with [`document_links::encode`]. Note fragments name HTML element IDs. A
//! client must authorize every page and asset it serves, associate assets
//! with the correct revision, and intercept navigation out of the document.

use crate::callout::{self, CalloutHeader, Fold};
use crate::document_links::{self, HeadingInventory, IndexedResolver};
use crate::{obsidian, render, tex, Vault};
use comrak::html::{self, ChildRendering, Context};
use comrak::nodes::{AstNode, NodeValue};
use comrak::{Arena, Options};
use std::collections::{HashMap, HashSet};
use std::fmt::{self, Write};
use std::path::{Component, Path};

mod snapshot;
pub use crate::view::snapshot::SnapshotJson;
pub use snapshot::render_view_snapshot;

pub const VERSION: u8 = 1;

/// Render a vault-relative note as a complete HTML document. The vault is the
/// corpus visible to this render, including any notes expanded as embeds.
pub fn render_note(vault: &Vault, path: &str) -> anyhow::Result<String> {
    let root = crate::vault::canonical_root(&vault.root)?;
    let raw = read_note(vault, &root, path)?;
    let mut profile = Profile {
        vault,
        root,
        resolver: IndexedResolver::new(vault),
        headings: HashMap::from([(
            path.to_owned(),
            Some(HeadingInventory::new(&render::reader_heading_source(
                vault, path, &raw,
            ))),
        )]),
    };
    let mut out = start_document();
    Fragment::Vault {
        profile: &mut profile,
        page: true,
    }
    .render(path, &raw, &mut out)?;
    Ok(finish_document(out))
}

fn start_document() -> String {
    format!(
        "<!doctype html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<meta name=\"html_profile\" content=\"{VERSION}\">\n<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; img-src okilum-asset:; base-uri 'none'; form-action 'none'\">\n<style>\n{}</style>\n</head>\n<body>\n<main>\n",
        include_str!("html_profile.css")
    )
}

fn finish_document(mut out: String) -> String {
    out.push_str("</main>\n</body>\n</html>\n");
    out
}

fn read_note(vault: &Vault, root: &Path, path: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        !path.is_empty()
            && Path::new(path)
                .components()
                .all(|p| matches!(p, Component::Normal(_))),
        "Expected a vault-relative note path"
    );
    anyhow::ensure!(
        vault.notes.iter().any(|note| note.path == path),
        "Unknown note: {path}"
    );
    let file = root.join(path).canonicalize()?;
    anyhow::ensure!(
        file.starts_with(root) && file.is_file(),
        "Note is outside the vault or unavailable: {path}"
    );
    let text = crate::vault::read_display_source(&file)?.text;
    Ok(if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text
    })
}

struct Profile<'a> {
    vault: &'a Vault,
    root: std::path::PathBuf,
    resolver: IndexedResolver<'a>,
    headings: HashMap<String, Option<HeadingInventory>>,
}

#[derive(Default)]
struct Metadata {
    links: HashMap<usize, Link>,
    heading_ids: HashMap<usize, String>,
    block_ids: HashMap<usize, String>,
    callouts: HashMap<usize, CalloutHeader>,
    embeds: HashMap<usize, String>,
}

#[derive(Clone)]
enum Link {
    Href(String),
    Inert(&'static str),
    WikiImage { url: String, alt: String },
}

impl Link {
    fn write_open(&self, out: &mut dyn Write, relaxed_autolinks: bool) -> fmt::Result {
        match self {
            Self::Href(url) => {
                out.write_str("<a href=\"")?;
                html::escape_href(out, url, relaxed_autolinks)?;
                out.write_char('"')
            }
            Self::Inert(status) => write!(out, "<span data-link-status=\"{status}\""),
            Self::WikiImage { .. } => unreachable!("wiki images have no closing tag"),
        }
    }

    fn close_tag(&self) -> &'static str {
        match self {
            Self::Href(_) => "</a>",
            _ => "</span>",
        }
    }
}

fn note_url(path: &str) -> String {
    format!("okilum-note:{}", document_links::encode(path))
}

fn asset_link(path: &str) -> Link {
    Link::Href(format!("okilum-asset:{}", document_links::encode(path)))
}

fn http_link(url: &str) -> Link {
    if url.split_once(':').is_some_and(|(scheme, _)| {
        scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http")
    }) {
        Link::Href(url.to_owned())
    } else {
        Link::Inert("unsupported")
    }
}

fn key(node: &AstNode<'_>) -> usize {
    std::ptr::from_ref(node) as usize
}

// Desktop heading navigation matches case and whitespace insensitively and
// rejects duplicate matches. Encode that same identity into an HTML ID, using
// an ASCII escape introducer that also survives fragment navigation unchanged.
fn anchor(target: &str) -> String {
    let (prefix, text) = match target.strip_prefix('^') {
        Some(id) => ("b-", id.trim().to_ascii_lowercase()),
        None => ("h-", render::normalized_heading(target)),
    };
    let mut id = prefix.to_owned();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            id.push(char::from(byte));
        } else {
            write!(id, "~{byte:02X}").expect("writing to a string");
        }
    }
    id
}

fn options() -> Options<'static> {
    let mut options = render::comrak_options();
    options.render.r#unsafe = false;
    options.extension.alerts = false;
    options.extension.math_dollars = true;
    options
}

enum Fragment<'profile, 'vault> {
    Vault {
        profile: &'profile mut Profile<'vault>,
        page: bool,
    },
    Snapshot,
}

impl Fragment<'_, '_> {
    fn link(&mut self, target: &str, wiki: bool, from: &str) -> Link {
        match self {
            Self::Vault { profile, .. } => profile.link(target, wiki, from),
            Self::Snapshot => snapshot::excerpt_link(target, wiki, from),
        }
    }

    fn render(&mut self, path: &str, raw: &str, out: &mut String) -> fmt::Result {
        let text = match self {
            Self::Vault { .. } => render::without_frontmatter(raw),
            Self::Snapshot => raw,
        };
        let source = obsidian::strip_comments(text);
        let source = obsidian::rewrite_block_ids(&source);
        let source = render::rewrite_highlights(&source);
        let source = match self {
            Self::Vault { profile, .. } => render::html_link_source(&source, profile.vault, path),
            Self::Snapshot => source,
        };
        let lines = render::markdown_line_offsets(&source);
        let mut masked = document_links::wiki_parse_source(&source).into_bytes();
        let mut embed_starts = HashSet::new();
        for range in crate::prose::prose_spans(&source) {
            for found in render::embed_regex().find_iter(&source[range.clone()]) {
                let offset = range.start + found.start();
                if offset > 0 && source[..offset].ends_with('\\') {
                    continue;
                }
                masked[offset] = b' ';
                embed_starts.insert(offset + 1);
            }
        }
        let masked = String::from_utf8(masked).expect("only ASCII markers replaced");
        let options = options();
        let arena = Arena::new();
        let root = render::parse_html_source(&arena, &masked, &options);
        let mut metadata = Metadata::default();

        // Collect first so transforms can detach nodes while keeping the
        // traversal independent of changes to the AST.
        for node in root.descendants().collect::<Vec<_>>() {
            let data = node.data.borrow();
            let pos = data.sourcepos;
            match &data.value {
                NodeValue::WikiLink(wiki) => {
                    let range = render::html_source_range(&lines, pos);
                    let authored = render::html_wiki_source(&source, &lines, pos);
                    let (target, alias) = authored.unwrap_or((&wiki.url, None));
                    let embed_at = range
                        .as_ref()
                        .map(|r| r.start)
                        .filter(|at| embed_starts.contains(at));
                    let link = if authored.is_some() {
                        self.link(target, true, path)
                    } else {
                        Link::Inert("unresolved")
                    };
                    if let (Some(at), Link::Href(url)) = (embed_at, &link) {
                        if url
                            .strip_prefix("okilum-asset:")
                            .and_then(document_links::image_media_type)
                            .is_some()
                        {
                            metadata.links.insert(
                                key(node),
                                Link::WikiImage {
                                    url: url.clone(),
                                    alt: alias.unwrap_or(target).to_owned(),
                                },
                            );
                            continue;
                        }
                        if let (
                            Self::Vault {
                                profile,
                                page: true,
                            },
                            true,
                        ) = (&mut *self, url.starts_with("okilum-note:"))
                        {
                            if let Some(parent) = node.parent().filter(|parent| {
                                matches!(parent.data.borrow().value, NodeValue::Paragraph)
                                    && parent
                                        .first_child()
                                        .is_some_and(|first| first.same_node(node))
                                    && node.next_sibling().is_none()
                            }) {
                                let start = lines[pos.start.line - 1];
                                let end = lines.get(pos.end.line).copied().unwrap_or(source.len());
                                if source[start..at - 1].trim().is_empty()
                                    && range.is_some_and(|r| source[r.end..end].trim().is_empty())
                                {
                                    if let Some(html) = profile.embed(target, path, url)? {
                                        metadata.embeds.insert(key(parent), html);
                                        continue;
                                    }
                                }
                            }
                        }
                    }
                    if authored.is_some() {
                        for child in node.children().collect::<Vec<_>>() {
                            child.detach();
                        }
                        node.append(arena.alloc(
                            NodeValue::Text(alias.unwrap_or(target).to_owned().into()).into(),
                        ));
                    }
                    metadata.links.insert(key(node), link);
                }
                NodeValue::Link(link) => {
                    metadata
                        .links
                        .insert(key(node), self.link(&link.url, false, path));
                }
                NodeValue::Image(image) => {
                    let mut link = self.link(&image.url, false, path);
                    if matches!(&link, Link::Href(url) if url.starts_with("okilum-note:")
                        || matches!(self, Self::Snapshot) && !url.starts_with("okilum-asset:"))
                    {
                        link = Link::Inert("unsupported");
                    }
                    metadata.links.insert(key(node), link);
                }
                NodeValue::BlockQuote => {
                    let header = render::html_source_range(&lines, pos)
                        .and_then(|range| source.get(range.start..))
                        .and_then(|tail| tail.lines().next())
                        .and_then(callout::parse_header);
                    if let Some(header) = header {
                        remove_callout_header(node, pos.start.line);
                        metadata.callouts.insert(key(node), header);
                    }
                }
                _ => {}
            }
        }

        if let Self::Vault {
            profile,
            page: true,
        } = self
        {
            profile.mark_anchors(path, raw, root, &mut metadata);
        }
        MobileFormatter::format_document(root, &options, out, metadata).map(|_| ())
    }
}

impl Profile<'_> {
    fn mark_anchors<'a>(
        &mut self,
        path: &str,
        raw: &str,
        root: &'a AstNode<'a>,
        metadata: &mut Metadata,
    ) {
        let inventory = self.headings.entry(path.to_owned()).or_insert_with(|| {
            Some(HeadingInventory::new(&render::reader_heading_source(
                self.vault, path, raw,
            )))
        });
        if let Some(inventory) = inventory {
            for node in root.descendants() {
                let data = node.data.borrow();
                match &data.value {
                    NodeValue::Heading(_) if node.parent().is_some_and(|p| p.same_node(root)) => {
                        let text = document_links::heading_text(node);
                        if inventory.locate(&text).is_ok() {
                            metadata.heading_ids.insert(key(node), anchor(&text));
                        }
                    }
                    NodeValue::HtmlInline(value)
                    | NodeValue::HtmlBlock(comrak::nodes::NodeHtmlBlock {
                        literal: value, ..
                    }) => {
                        if let Some(id) = obsidian::marker_id(value) {
                            let target = format!("^{id}");
                            if inventory.locate(&target).is_ok() {
                                metadata.block_ids.insert(key(node), anchor(&target));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    fn link(&mut self, target: &str, wiki: bool, from: &str) -> Link {
        if !wiki && document_links::decode(target).starts_with("//") {
            return Link::Inert("unsupported");
        }
        let resolved = self.resolver.resolve(target, wiki, from);
        match (resolved.status, resolved.candidates.as_slice()) {
            ("resolved", [path]) => {
                let mut url = note_url(path);
                if let Some(heading) = resolved.heading {
                    let inventory = self.headings.entry(path.clone()).or_insert_with(|| {
                        let raw = read_note(self.vault, &self.root, path).ok()?;
                        Some(HeadingInventory::new(&render::reader_heading_source(
                            self.vault, path, &raw,
                        )))
                    });
                    match inventory.as_ref().map(|i| i.locate(&heading)) {
                        Some(Ok(_)) => {
                            url.push('#');
                            url.push_str(&anchor(&heading));
                        }
                        Some(Err(
                            document_links::HeadingFailure::Ambiguous
                            | document_links::HeadingFailure::AmbiguousBlock,
                        )) => {
                            return Link::Inert("ambiguous");
                        }
                        Some(Err(document_links::HeadingFailure::Unsupported)) => {
                            return Link::Inert("unsupported");
                        }
                        _ => return Link::Inert("unresolved"),
                    }
                }
                Link::Href(url)
            }
            ("attachment", [path]) => asset_link(path),
            ("external", _) => http_link(&resolved.url),
            ("ambiguous", _) => Link::Inert("ambiguous"),
            ("unresolved", _) => Link::Inert("unresolved"),
            _ => Link::Inert("unsupported"),
        }
    }

    fn embed(&mut self, target: &str, from: &str, url: &str) -> Result<Option<String>, fmt::Error> {
        let Some((path, _)) = url.strip_prefix("okilum-note:").map(render::split_open_url) else {
            return Ok(None);
        };
        if path == from {
            return Ok(None);
        }
        let Ok(raw) = read_note(self.vault, &self.root, &path) else {
            return Ok(None);
        };
        let (_, heading) = render::split_fragment(target);
        let block = target
            .split_once('#')
            .and_then(|(_, fragment)| fragment.trim().strip_prefix('^'));
        let Some(body) = render::embed_section(&raw, block, heading) else {
            return Ok(None);
        };
        let mut html = String::from("<section class=\"note-embed\">\n");
        Fragment::Vault {
            profile: self,
            page: false,
        }
        .render(&path, &body, &mut html)?;
        html.push_str("</section>\n");
        Ok(Some(html))
    }
}

fn remove_callout_header<'a>(node: &'a AstNode<'a>, line: usize) {
    let Some(paragraph) = node
        .first_child()
        .filter(|child| matches!(child.data.borrow().value, NodeValue::Paragraph))
    else {
        return;
    };
    while let Some(child) = paragraph.first_child() {
        if child.data.borrow().sourcepos.start.line != line {
            break;
        }
        child.detach();
    }
    if paragraph.first_child().is_none() {
        paragraph.detach();
    }
}

comrak::create_formatter!(MobileFormatter<Metadata>, {
    NodeValue::Link(ref link) => |context, node, entering| {
        return render_link(context, node, entering, &link.title);
    },
    NodeValue::WikiLink(_) => |context, node, entering| {
        return render_link(context, node, entering, "");
    },
    NodeValue::Image(ref image) => |context, node, entering| {
        if let Some(Link::Href(url)) = context.user.links.get(&key(node)) {
            if entering {
                let url = url.clone();
                context.write_str("<img src=\"")?;
                context.escape_href(&url)?;
                context.write_str("\" alt=\"")?;
                return Ok(ChildRendering::Plain);
            }
            if !image.title.is_empty() {
                context.write_str("\" title=\"")?;
                context.escape(&image.title)?;
            }
            context.write_str("\" />")?;
        } else {
            return render_link(context, node, entering, &image.title);
        }
    },
    NodeValue::Heading(ref heading) => |context, node, entering| {
        let level = heading.level;
        if entering {
            context.cr()?;
            write!(context, "<h{level}")?;
            if let Some(id) = context.user.heading_ids.get(&key(node)).cloned() {
                write!(context, " id=\"{id}\"")?;
            }
            context.write_str(">")?;
        } else {
            writeln!(context, "</h{level}>")?;
        }
    },
    NodeValue::HtmlInline(ref html) => |context, node, entering| {
        if entering && matches!(html.as_str(), "<mark>" | "</mark>") {
            context.write_str(html)?;
        } else {
            render_block_id(context, node, entering)?;
        }
    },
    NodeValue::HtmlBlock(_) => |context, node, entering| {
        render_block_id(context, node, entering)?;
    },
    NodeValue::Paragraph => |context, node, entering| {
        if let Some(embed) = context.user.embeds.get_mut(&key(node)) {
            if entering {
                let embed = std::mem::take(embed);
                context.cr()?;
                context.write_str(&embed)?;
            }
            return Ok(ChildRendering::Skip);
        }
        return html::format_node_default(context, node, entering);
    },
    NodeValue::BlockQuote => |context, node, entering| {
        if let Some(header) = context.user.callouts.get(&key(node)).cloned() {
            render_callout(context, &header, entering)?;
        } else {
            return html::format_node_default(context, node, entering);
        }
    },
    NodeValue::Math(ref math) => |context, entering| {
        if entering {
            if math.dollar_math && !math.display_math && !tex::is_inline_formula(&math.literal) {
                context.write_char('$')?;
                context.escape(&math.literal)?;
                context.write_char('$')?;
            } else {
                render_math(context, &math.literal, math.display_math)?;
            }
        }
    },
    NodeValue::CodeBlock(ref code) => |context, node, entering| {
        if code.info.split_whitespace().next() == Some(obsidian::MATH_LANG) {
            if entering {
                context.cr()?;
                render_math(context, code.literal.trim_end(), true)?;
                context.cr()?;
            }
        } else {
            return html::format_node_default(context, node, entering);
        }
    }
});

fn render_block_id(
    context: &mut Context<Metadata>,
    node: &AstNode<'_>,
    entering: bool,
) -> fmt::Result {
    if entering {
        if let Some(id) = context.user.block_ids.get(&key(node)).cloned() {
            write!(context, "<span id=\"{id}\"></span>")?;
        }
    }
    Ok(())
}

fn render_link(
    context: &mut Context<Metadata>,
    node: &AstNode<'_>,
    entering: bool,
    title: &str,
) -> Result<ChildRendering, fmt::Error> {
    let link = context.user.links.get(&key(node));
    if entering {
        match link {
            Some(Link::WikiImage { url, alt }) => {
                let (url, alt) = (url.clone(), alt.clone());
                context.write_str("<img src=\"")?;
                context.escape_href(&url)?;
                context.write_str("\" alt=\"")?;
                context.escape(&alt)?;
                context.write_str("\" />")?;
                return Ok(ChildRendering::Skip);
            }
            Some(link) => {
                let link = link.clone();
                let relaxed = context.options.parse.relaxed_autolinks;
                link.write_open(context, relaxed)?;
            }
            None => context.write_str("<span")?,
        }
        if !title.is_empty() {
            context.write_str(" title=\"")?;
            context.escape(title)?;
            context.write_str("\"")?;
        }
        context.write_str(">")?;
    } else if !matches!(link, Some(Link::WikiImage { .. })) {
        context.write_str(link.map_or("</span>", Link::close_tag))?;
    }
    Ok(if matches!(node.data.borrow().value, NodeValue::Image(_)) {
        ChildRendering::Plain
    } else {
        ChildRendering::HTML
    })
}

fn render_callout(
    context: &mut Context<Metadata>,
    header: &CalloutHeader,
    entering: bool,
) -> fmt::Result {
    if entering {
        context.cr()?;
        match header.fold {
            Fold::None => {
                context.write_str("<aside class=\"callout\">\n<p class=\"callout-title\">")?
            }
            Fold::Open => context.write_str("<details class=\"callout\" open>\n<summary>")?,
            Fold::Closed => context.write_str("<details class=\"callout\">\n<summary>")?,
        }
        context.escape(&header.title)?;
        context.write_str(if header.fold == Fold::None {
            "</p>\n"
        } else {
            "</summary>\n"
        })?;
    } else {
        context.write_str(if header.fold == Fold::None {
            "</aside>\n"
        } else {
            "</details>\n"
        })?;
    }
    Ok(())
}

fn render_math(context: &mut Context<Metadata>, source: &str, display: bool) -> fmt::Result {
    let class = if display {
        "math-display"
    } else {
        "math-inline"
    };
    write!(context, "<span class=\"math {class}\" title=\"")?;
    context.escape(source)?;
    context.write_str("\">")?;
    match tex::linear(source) {
        Ok(math) => context.escape(&math.text)?,
        Err(_) => context.escape(source)?,
    }
    context.write_str("</span>")
}
