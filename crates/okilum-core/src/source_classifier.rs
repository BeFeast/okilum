//! Conservative raw Markdown classification for the source projection foundation.
//! No renderer output, decoded AST literal, native buffer, or backend participates.
use std::ops::Range;

use comrak::{nodes::AstNode, nodes::NodeValue, Arena, Options};
use unicode_segmentation::UnicodeSegmentation;

use crate::source_projection::{self, Active, MapError, Plan, Region, Snapshot};

pub mod decorations;
mod retained;
pub use retained::{RetainedPresentation, RevealSnapshot};

pub const MAX_BYTES: usize = 64 * 1024;
pub const MAX_NODES: usize = 4096;
pub const MAX_DEPTH: usize = 32;
pub const MAX_STYLES_AND_REASONS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Heading(u8),
    Strong,
    Emphasis,
    Strike,
    Code,
    Link,
    WikiLink,
    /// A reference definition row: kept at source height and rendered quietly.
    Definition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyleSpan {
    pub range: Range<usize>,
    pub style: Style,
}

/// An authored link accepted by the same conservative block as its projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteLink {
    pub range: Range<usize>,
    pub label: Range<usize>,
    pub target: String,
    pub wiki: bool,
}

/// A reference link the full parse resolved from a document definition. A local
/// reparse cannot see definitions; it may reuse only these exact labels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reference {
    label: String,
    url: String,
    title: String,
}

struct References(Vec<Reference>);

impl comrak::options::BrokenLinkCallback for References {
    fn resolve(
        &self,
        reference: comrak::options::BrokenLinkReference,
    ) -> Option<comrak::ResolvedReference> {
        self.0
            .iter()
            .find(|known| known.label == reference.original)
            .map(|known| comrak::ResolvedReference {
                url: known.url.clone(),
                title: known.title.clone(),
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceReason {
    InputLimit,
    ParserCoordinates,
    StructureLimit,
    UnsupportedOrAmbiguous,
    ProjectionBoundary,
}

#[derive(Clone, Debug)]
struct Heading {
    text: String,
    offset: usize,
    accepted: bool,
}

#[derive(Clone, Debug)]
pub struct Classification {
    snapshot: Snapshot,
    plan: Plan,
    styles: Vec<StyleSpan>,
    marker_scopes: Vec<(Range<usize>, Range<usize>)>,
    /// Top-level list/quote containers, from their first line start. A local
    /// reparse must include the whole container its edited lines belong to.
    contexts: Vec<Range<usize>>,
    references: Vec<Reference>,
    decorations: Vec<decorations::Marker>,
    links: Vec<NoteLink>,
    headings: Vec<Heading>,
    reasons: Vec<SourceReason>,
}

impl Classification {
    /// Paint metadata is valid only for the exact classified source revision.
    pub fn decorations_for(&self, current: &Snapshot) -> Result<&[decorations::Marker], MapError> {
        if &self.snapshot != current {
            return Err(MapError::StaleSnapshot);
        }
        Ok(&self.decorations)
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    pub fn plan(&self) -> &Plan {
        &self.plan
    }
    pub fn styles_for(&self, current: &Snapshot) -> Result<&[StyleSpan], MapError> {
        if &self.snapshot != current {
            return Err(MapError::StaleSnapshot);
        }
        Ok(&self.styles)
    }
    pub fn links_for(&self, current: &Snapshot) -> Result<&[NoteLink], MapError> {
        if &self.snapshot != current {
            return Err(MapError::StaleSnapshot);
        }
        Ok(&self.links)
    }
    /// Exact authored byte offset of one supported top-level ATX heading.
    /// Count unsupported matching headings too: they cannot imply uniqueness.
    pub fn heading_offset(&self, target: &str) -> Result<usize, &'static str> {
        self.heading_offset_with_inventory(
            &crate::document_links::HeadingInventory::new(self.snapshot.source()),
            target,
        )
    }
    /// Reuse the exact source revision's inventory during batch link preparation.
    pub fn heading_offset_with_inventory(
        &self,
        inventory: &crate::document_links::HeadingInventory,
        target: &str,
    ) -> Result<usize, &'static str> {
        if self.snapshot.source().len() > MAX_BYTES {
            return Err("Heading navigation supports saved target notes up to 64 KiB.");
        }
        if target.starts_with('^') {
            return Err(
                "Block navigation is supported in Reader, but not in the managed Source editor.",
            );
        }
        let resolved = inventory
            .locate(target)
            .map_err(crate::document_links::HeadingFailure::reason)?;
        if resolved.setext {
            return Err("Setext heading navigation is supported in Reader, but not in the managed Source editor.");
        }
        let mut matches = self
            .headings
            .iter()
            .filter(|h| crate::render::heading_matches(&h.text, target));
        let heading = matches
            .next()
            .ok_or("The saved note has no matching supported ATX heading.")?;
        if matches.next().is_some() {
            return Err(
                "The saved note has multiple headings with that name. Use a unique heading.",
            );
        }
        if !heading.accepted {
            return Err(
                "That heading uses unsupported source syntax. Open the whole note instead.",
            );
        }
        Ok(heading.offset)
    }
    pub fn reasons(&self) -> &[SourceReason] {
        &self.reasons
    }
}

fn fallback(snapshot: &Snapshot, reason: SourceReason) -> Classification {
    Classification {
        snapshot: snapshot.clone(),
        plan: Plan::new(snapshot, vec![]),
        styles: vec![],
        marker_scopes: vec![],
        contexts: vec![],
        references: vec![],
        decorations: vec![],
        links: vec![],
        headings: vec![],
        reasons: vec![reason],
    }
}

/// Parsing is capped by input size, but comrak has no cancellation/fuel API.
/// AST node/depth caps are post-parse validation, not a parser time bound.
/// A future native caller must schedule this away from the input/paint path.
pub fn classify(snapshot: &Snapshot) -> Classification {
    classify_with(snapshot, &[])
}

/// `references` resolve uses whose definitions lie outside a local fragment.
pub(crate) fn classify_with(snapshot: &Snapshot, references: &[Reference]) -> Classification {
    let source = snapshot.source();
    if source.len() > MAX_BYTES {
        return fallback(snapshot, SourceReason::InputLimit);
    }
    if source.contains('\0')
        || source
            .as_bytes()
            .iter()
            .enumerate()
            .any(|(i, &b)| b == b'\r' && source.as_bytes().get(i + 1) != Some(&b'\n'))
    {
        return fallback(snapshot, SourceReason::ParserCoordinates);
    }
    let context = Context::new(source);
    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    // Task markers are authored container syntax, not paragraph text.
    options.extension.tasklist = true;
    options.extension.wikilinks_title_after_pipe = true;
    if !references.is_empty() {
        options.parse.broken_link_callback =
            Some(std::sync::Arc::new(References(references.to_vec())));
    }
    let first_line = source.trim_start_matches('\u{feff}').lines().next();
    let delimiter = if first_line == Some("+++") {
        "+++"
    } else {
        "---"
    };
    options.extension.front_matter_delimiter = Some(delimiter.into());
    let root = comrak::parse_document(&arena, source, &options);
    for (count, node) in root.descendants().enumerate() {
        if count >= MAX_NODES || node.ancestors().take(MAX_DEPTH + 2).count() > MAX_DEPTH + 1 {
            return fallback(snapshot, SourceReason::StructureLimit);
        }
    }
    // An unterminated frontmatter opener must not expose its body as Markdown.
    if first_line == Some(delimiter)
        && !root
            .children()
            .any(|n| matches!(n.data.borrow().value, NodeValue::FrontMatter(_)))
    {
        return fallback(snapshot, SourceReason::UnsupportedOrAmbiguous);
    }
    let mut regions = Vec::new();
    let mut styles = Vec::new();
    let mut marker_scopes = Vec::new();
    let mut links = Vec::new();
    let mut resolved = Vec::new();
    let mut headings = Vec::new();
    let mut reasons = Vec::new();
    let mut range_count = 0;
    let mut blocks = Vec::new();
    content_blocks(root, &mut blocks, &mut reasons);
    let mut contexts = Vec::new();
    for node in root.children() {
        if matches!(
            node.data.borrow().value,
            NodeValue::List(_) | NodeValue::BlockQuote
        ) {
            let Some(range) = context.range(node) else {
                return fallback(snapshot, SourceReason::ParserCoordinates);
            };
            contexts.push(context.lines[node.data.borrow().sourcepos.start.line - 1]..range.end);
        }
    }
    for node in blocks {
        let value = &node.data.borrow().value;
        let Some(mut block) = context.range(node) else {
            return fallback(snapshot, SourceReason::ParserCoordinates);
        };
        // Include authored indentation/BOM in the reveal block, while marker
        // coordinates continue to come from the exact AST/raw range.
        block.start = context.lines[node.data.borrow().sourcepos.start.line - 1];
        let mut candidate = Candidate::default();
        let valid = context.block(node, &mut candidate).is_some()
            && candidate
                .styles
                .iter()
                .all(|s| context.boundary(s.range.start) && context.boundary(s.range.end));
        if matches!(value, NodeValue::Heading(h) if !h.setext) {
            let mut text = String::new();
            for child in node.descendants() {
                match &child.data.borrow().value {
                    NodeValue::Text(value) => text.push_str(value),
                    NodeValue::Code(code) => text.push_str(&code.literal),
                    _ => {}
                }
            }
            headings.push(Heading {
                text,
                offset: block.start,
                accepted: valid,
            });
        }
        if valid {
            candidate.markers.sort_by_key(|r| r.start);
            range_count += 1 + candidate.markers.len();
            // Every descriptor has one Link style, so the existing style cap
            // bounds its count. Accepted link syntax ranges do not overlap.
            marker_scopes.extend(candidate.markers.iter().map(|marker| {
                let scope = candidate
                    .fragments
                    .iter()
                    .filter(|r| r.start <= marker.start && r.end >= marker.end)
                    .min_by_key(|r| r.len())
                    .cloned()
                    .unwrap_or_else(|| marker.clone());
                (marker.clone(), scope)
            }));
            links.extend(candidate.links);
            resolved.extend(candidate.references);
            styles.extend(candidate.styles);
            regions.push(Region::Conceal {
                block,
                markers: candidate.markers,
            });
        } else {
            range_count += 1;
            reasons.push(SourceReason::UnsupportedOrAmbiguous);
            regions.push(Region::Source(block));
        }
        if range_count > source_projection::MAX_RANGES
            || styles.len() + reasons.len() > MAX_STYLES_AND_REASONS
        {
            return fallback(snapshot, SourceReason::StructureLimit);
        }
    }
    // The parser consumes reference definitions; their rows remain raw Source
    // at their own height. Only top-level rows no block covers are styled.
    let mut covered = vec![false; context.lines.len()];
    for node in root.children() {
        let pos = node.data.borrow().sourcepos;
        for line in pos.start.line..=pos.end.line {
            if let Some(slot) = covered.get_mut(line.wrapping_sub(1)) {
                *slot = true;
            }
        }
    }
    for (index, &start) in context.lines.iter().enumerate() {
        let end = context
            .lines
            .get(index + 1)
            .copied()
            .unwrap_or(source.len());
        let row = source[start..end].trim_end_matches(['\r', '\n']);
        if !covered[index] && definition_row(row) {
            styles.push(StyleSpan {
                range: start..start + row.len(),
                style: Style::Definition,
            });
        }
    }
    styles.sort_by_key(|style| style.range.start);
    resolved.dedup_by(|a: &mut Reference, b: &mut Reference| a.label == b.label);
    if styles.len() + reasons.len() > MAX_STYLES_AND_REASONS {
        return fallback(snapshot, SourceReason::StructureLimit);
    }
    let plan = Plan::new(snapshot, regions);
    if source_projection::project(snapshot, &plan, &Active::default()).is_err() {
        return fallback(snapshot, SourceReason::ProjectionBoundary);
    }
    Classification {
        snapshot: snapshot.clone(),
        plan,
        styles,
        marker_scopes,
        contexts,
        references: resolved,
        decorations: decorations::extract(root, &context).unwrap_or_default(),
        links,
        headings,
        reasons,
    }
}

/// Paragraphs and ATX headings at top level or inside supported list, task and
/// quote containers. Container syntax on their lines stays visible Source.
/// Other blocks and containers are never traversed for formatting; their exact
/// bytes remain uncovered Source, including unusual positions.
fn content_blocks<'a>(
    node: &'a AstNode<'a>,
    blocks: &mut Vec<&'a AstNode<'a>>,
    reasons: &mut Vec<SourceReason>,
) {
    for child in node.children() {
        match &child.data.borrow().value {
            NodeValue::Paragraph => blocks.push(child),
            NodeValue::Heading(h) if !h.setext => blocks.push(child),
            NodeValue::List(_)
            | NodeValue::Item(_)
            | NodeValue::TaskItem(_)
            | NodeValue::BlockQuote => content_blocks(child, blocks, reasons),
            _ => reasons.push(SourceReason::UnsupportedOrAmbiguous),
        }
    }
}

/// `[label]: destination` after at most three spaces of indentation.
pub(crate) fn definition_row(row: &str) -> bool {
    let indent = row.len() - row.trim_start_matches(' ').len();
    let Some((label, tail)) = row[indent..]
        .strip_prefix('[')
        .and_then(|rest| rest.split_once("]:"))
    else {
        return false;
    };
    indent <= 3
        && !label.trim().is_empty()
        && !label.contains(['[', ']'])
        && !tail.trim().is_empty()
}

#[derive(Default)]
struct Candidate {
    markers: Vec<Range<usize>>,
    fragments: Vec<Range<usize>>,
    styles: Vec<StyleSpan>,
    links: Vec<NoteLink>,
    references: Vec<Reference>,
}

struct Context<'s> {
    source: &'s str,
    lines: Vec<usize>,
    boundaries: Vec<bool>,
}

impl<'s> Context<'s> {
    fn new(source: &'s str) -> Self {
        let lines = std::iter::once(0)
            .chain(source.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        let mut boundaries = vec![false; source.len() + 1];
        for (i, _) in source.grapheme_indices(true) {
            boundaries[i] = true;
        }
        boundaries[source.len()] = true;
        Self {
            source,
            lines,
            boundaries,
        }
    }
    fn boundary(&self, offset: usize) -> bool {
        self.boundaries.get(offset) == Some(&true)
    }
    fn range(&self, node: &AstNode<'_>) -> Option<Range<usize>> {
        let pos = node.data.borrow().sourcepos;
        let start_line = *self.lines.get(pos.start.line.checked_sub(1)?)?;
        let end_line = *self.lines.get(pos.end.line.checked_sub(1)?)?;
        let start = start_line.checked_add(pos.start.column.checked_sub(1)?)?;
        let end = end_line.checked_add(pos.end.column)?;
        let end_limit = self
            .lines
            .get(pos.end.line)
            .copied()
            .unwrap_or(self.source.len());
        let start_limit = self
            .lines
            .get(pos.start.line)
            .copied()
            .unwrap_or(self.source.len());
        if start >= start_limit || end > end_limit || start >= end {
            return None;
        }
        self.source.get(start..end)?;
        Some(start..end)
    }
    fn block<'a>(&self, node: &'a AstNode<'a>, candidate: &mut Candidate) -> Option<()> {
        let range = self.range(node)?;
        if let NodeValue::Heading(h) = &node.data.borrow().value {
            let raw = self.source.get(range.clone())?;
            let prefix = raw.bytes().take_while(|&b| b == b'#').count();
            if prefix != usize::from(h.level) || prefix == 0 || prefix > 6 {
                return None;
            }
            let first = self.range(node.first_child()?)?;
            let last = self.range(node.last_child()?)?;
            if !self
                .source
                .get(range.start + prefix..first.start)?
                .bytes()
                .all(|b| b == b' ' || b == b'\t')
            {
                return None;
            }
            candidate.markers.push(range.start..first.start);
            let suffix = self.source.get(last.end..range.end)?;
            if h.closed {
                let hashes = suffix.trim_matches([' ', '\t']);
                if hashes.is_empty() || !hashes.bytes().all(|b| b == b'#') {
                    return None;
                }
                candidate.markers.push(last.end..range.end);
            } else if !suffix.bytes().all(|b| b == b' ' || b == b'\t') {
                return None;
            }
            candidate.styles.push(StyleSpan {
                range: first.start..last.end,
                style: Style::Heading(h.level),
            });
        }
        for child in node.children() {
            self.inline(child, candidate)?;
        }
        // Brackets left as text are an unresolved reference (`[a][missing]`):
        // plain text, shown as written, as in CommonMark and Obsidian. A
        // malformed inline link, a footnote, or an embed or wikilink left as
        // text still keeps the block Source. The parser may split brackets
        // across text nodes, so look at the block's own text as a whole (#1092).
        let mut text = String::new();
        self.loose_text(node, &mut text)?;
        if ["](", "[^", "[["]
            .iter()
            .any(|syntax| text.contains(syntax))
        {
            return None;
        }
        // An unresolved reference is whole bracket pairs; a lone or nested
        // bracket (`[broken`) is malformed.
        let mut open = false;
        for byte in text.bytes() {
            match (byte, open) {
                (b'[', false) => open = true,
                (b']', true) => open = false,
                (b'[' | b']', _) => return None,
                _ => {}
            }
        }
        if open {
            return None;
        }
        Some(())
    }
    /// The text of `node` outside links, images and code, in source order.
    fn loose_text<'a>(&self, node: &'a AstNode<'a>, text: &mut String) -> Option<()> {
        for child in node.children() {
            match &child.data.borrow().value {
                NodeValue::Text(_) => text.push_str(self.source.get(self.range(child)?)?),
                NodeValue::Link(_)
                | NodeValue::WikiLink(_)
                | NodeValue::Image(_)
                | NodeValue::Code(_) => text.push('\u{0}'),
                _ => self.loose_text(child, text)?,
            }
        }
        Some(())
    }
    fn inline<'a>(&self, node: &'a AstNode<'a>, candidate: &mut Candidate) -> Option<()> {
        let data = node.data.borrow();
        if matches!(data.value, NodeValue::SoftBreak) {
            return Some(());
        }
        let range = self.range(node)?;
        let raw = self.source.get(range.clone())?;
        if !matches!(data.value, NodeValue::Text(_)) {
            candidate.fragments.push(range.clone());
        }
        let (width, style) = match &data.value {
            NodeValue::Text(_) => return plain(raw),
            NodeValue::Strong => (2, Style::Strong),
            NodeValue::Emph => (1, Style::Emphasis),
            NodeValue::Strikethrough => (2, Style::Strike),
            NodeValue::Code(code) => {
                if raw.contains(['\r', '\n']) {
                    return None;
                }
                let width = code.num_backticks;
                if width == 0
                    || raw.len() <= width * 2
                    || !raw.as_bytes()[..width].iter().all(|&b| b == b'`')
                    || !raw.as_bytes()[raw.len() - width..]
                        .iter()
                        .all(|&b| b == b'`')
                {
                    return None;
                }
                candidate.markers.extend([
                    range.start..range.start + width,
                    range.end - width..range.end,
                ]);
                candidate.styles.push(StyleSpan {
                    range: range.start + width..range.end - width,
                    style: Style::Code,
                });
                return Some(());
            }
            NodeValue::Link(_) | NodeValue::WikiLink(_) => {
                return self.link(
                    node,
                    candidate,
                    matches!(data.value, NodeValue::WikiLink(_)),
                )
            }
            _ => return None,
        };
        let marker = *raw.as_bytes().first()?;
        if !matches!(
            (style, marker),
            (Style::Strong | Style::Emphasis, b'*' | b'_') | (Style::Strike, b'~')
        ) || raw.len() <= width * 2
            || !raw.as_bytes()[..width].iter().all(|&b| b == marker)
            || !raw.as_bytes()[raw.len() - width..]
                .iter()
                .all(|&b| b == marker)
        {
            return None;
        }
        let content = range.start + width..range.end - width;
        if self.range(node.first_child()?)?.start != content.start
            || self.range(node.last_child()?)?.end != content.end
        {
            return None;
        }
        candidate
            .markers
            .extend([range.start..content.start, content.end..range.end]);
        candidate.styles.push(StyleSpan {
            range: content,
            style,
        });
        for child in node.children() {
            self.inline(child, candidate)?;
        }
        Some(())
    }
    fn link<'a>(&self, node: &'a AstNode<'a>, candidate: &mut Candidate, wiki: bool) -> Option<()> {
        let range = self.range(node)?;
        let raw = self.source.get(range.clone())?;
        let children: Vec<_> = node.children().collect();
        let (label, target) = if wiki {
            if raw.contains(['\r', '\n'])
                || children.len() != 1
                || !matches!(children[0].data.borrow().value, NodeValue::Text(_))
            {
                return None;
            }
            // Comrak trims label text before assigning its child coordinates.
            // Whitespace around the label is syntax to conceal, not evidence
            // of an unsupported link. Keep the target and source bytes intact.
            let inner = raw.strip_prefix("[[")?.strip_suffix("]]")?;
            let (target, authored_label) = inner.split_once('|').unwrap_or((inner, inner));
            if target.trim().is_empty() || target.contains(['[', ']', '|', '\\']) {
                return None;
            }
            let visible = authored_label.trim();
            if visible.is_empty() || visible.contains('|') {
                return None;
            }
            let label_start = range.end - 2 - authored_label.trim_start().len();
            let label = label_start..label_start + visible.len();
            // Comrak's child position starts before leading whitespace even
            // though its text is trimmed. Derive offsets from the checked raw
            // delimiters and require agreement with the literal parser text.
            if !matches!(&children[0].data.borrow().value,
                NodeValue::Text(text) if text == visible)
            {
                return None;
            }
            (label, target.to_owned())
        } else {
            // A label may wrap (#766): plain text and soft breaks only, with
            // text at both ends. The concealed delimiters stay on one line.
            let text = |n: &AstNode<'_>| matches!(n.data.borrow().value, NodeValue::Text(_));
            if !children.first().is_some_and(|n| text(n))
                || !children.last().is_some_and(|n| text(n))
                || !children
                    .iter()
                    .all(|n| text(n) || matches!(n.data.borrow().value, NodeValue::SoftBreak))
            {
                return None;
            }
            let label =
                self.range(children[0])?.start..self.range(children[children.len() - 1])?.end;
            let prefix = self.source.get(range.start..label.start)?;
            let suffix = self.source.get(label.end..range.end)?;
            // A wrapped label inside a quote would also cover the next quote
            // prefix; keep such labels raw rather than styling container syntax.
            if prefix != "["
                || suffix.contains(['\r', '\n'])
                || self
                    .source
                    .get(label.clone())?
                    .split('\n')
                    .skip(1)
                    .any(|line| line.trim_start().starts_with('>'))
            {
                return None;
            }
            if let Some(destination) = suffix.strip_prefix("](").and_then(|s| s.strip_suffix(')')) {
                if destination.is_empty()
                    || destination
                        .chars()
                        .any(|c| c.is_whitespace() || "()[]\\\"'<>`".contains(c))
                {
                    return None;
                }
                (label, destination.to_owned())
            } else {
                // Resolved reference forms: [label][ref], [label][] and [label].
                // Unresolved references are not links and stay raw text.
                let NodeValue::Link(link) = &node.data.borrow().value else {
                    return None;
                };
                let reference = match suffix {
                    "]" | "][]" => self.source.get(label.clone())?,
                    _ => suffix.strip_prefix("][")?.strip_suffix(']')?,
                };
                if reference.trim().is_empty()
                    || reference.contains(['[', ']', '\\', '\r', '\n'])
                    || link.url.is_empty()
                    || link
                        .url
                        .chars()
                        .any(|c| c.is_whitespace() || c.is_control())
                {
                    return None;
                }
                candidate.references.push(Reference {
                    label: reference.to_owned(),
                    url: link.url.clone(),
                    title: link.title.clone(),
                });
                (label, link.url.clone())
            }
        };
        plain(self.source.get(label.clone())?)?;
        candidate.links.push(NoteLink {
            range: range.clone(),
            label: label.clone(),
            target,
            wiki,
        });
        candidate
            .markers
            .extend([range.start..label.start, label.end..range.end]);
        candidate.styles.push(StyleSpan {
            range: label,
            style: if wiki { Style::WikiLink } else { Style::Link },
        });
        Some(())
    }
}

/// Keep authored escapes/entities. Unclassified markup-looking text makes the
/// containing block Source, including valid formatting next to a malformed link.
/// Brackets are the exception: the parser leaves an unresolved reference
/// (`[ref][id]`, `[id][]`, `[id]`) as text, and like CommonMark and Obsidian
/// Live Preview shows it as written while its neighbours render. A malformed
/// inline link (`](`), a footnote (`[^`), and an embed or wikilink left as
/// text (`[[`, `![[`) still make the block Source (`Context::block`, #1092).
fn plain(raw: &str) -> Option<()> {
    let mut bytes = raw.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'\\' {
            let next = bytes.next()?;
            if !next.is_ascii_punctuation() {
                return None;
            }
        } else if b"*_~`=$".contains(&byte) {
            return None;
        }
    }
    Some(())
}

#[cfg(test)]
mod tests;
