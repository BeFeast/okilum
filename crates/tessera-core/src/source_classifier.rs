//! Conservative raw Markdown classification for the source projection foundation.
//! No renderer output, decoded AST literal, native buffer, or backend participates.
use std::ops::Range;

use comrak::{nodes::AstNode, nodes::NodeValue, Arena, Options};
use unicode_segmentation::UnicodeSegmentation;

use crate::source_projection::{self, Active, MapError, Plan, Region, Snapshot};

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
    links: Vec<NoteLink>,
    headings: Vec<Heading>,
    reasons: Vec<SourceReason>,
}

impl Classification {
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
        links: vec![],
        headings: vec![],
        reasons: vec![reason],
    }
}

/// Parsing is capped by input size, but comrak has no cancellation/fuel API.
/// AST node/depth caps are post-parse validation, not a parser time bound.
/// A future native caller must schedule this away from the input/paint path.
pub fn classify(snapshot: &Snapshot) -> Classification {
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
    options.extension.wikilinks_title_after_pipe = true;
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
    let mut links = Vec::new();
    let mut headings = Vec::new();
    let mut reasons = Vec::new();
    let mut range_count = 0;
    for node in root.children() {
        let value = &node.data.borrow().value;
        if !(matches!(value, NodeValue::Paragraph)
            || matches!(value, NodeValue::Heading(h) if !h.setext))
        {
            // Unsupported containers are never traversed for formatting. Their
            // exact bytes remain uncovered Source, including unusual positions.
            reasons.push(SourceReason::UnsupportedOrAmbiguous);
            continue;
        }
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
            links.extend(candidate.links);
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
    let plan = Plan::new(snapshot, regions);
    if source_projection::project(snapshot, &plan, &Active::default()).is_err() {
        return fallback(snapshot, SourceReason::ProjectionBoundary);
    }
    Classification {
        snapshot: snapshot.clone(),
        plan,
        styles,
        links,
        headings,
        reasons,
    }
}

#[derive(Default)]
struct Candidate {
    markers: Vec<Range<usize>>,
    styles: Vec<StyleSpan>,
    links: Vec<NoteLink>,
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
        Some(())
    }
    fn inline<'a>(&self, node: &'a AstNode<'a>, candidate: &mut Candidate) -> Option<()> {
        let data = node.data.borrow();
        if matches!(data.value, NodeValue::SoftBreak) {
            return Some(());
        }
        let range = self.range(node)?;
        let raw = self.source.get(range.clone())?;
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
        if raw.contains(['\r', '\n']) {
            return None;
        }
        let children: Vec<_> = node.children().collect();
        if children.len() != 1 || !matches!(children[0].data.borrow().value, NodeValue::Text(_)) {
            return None;
        }
        let label = self.range(children[0])?;
        let label_raw = self.source.get(label.clone())?;
        plain(label_raw)?;
        let prefix = self.source.get(range.start..label.start)?;
        let suffix = self.source.get(label.end..range.end)?;
        if wiki {
            if suffix != "]]" || label_raw.contains('|') {
                return None;
            }
            if prefix != "[[" {
                let target = prefix.strip_prefix("[[")?.strip_suffix('|')?;
                if target.is_empty() || target.contains(['[', ']', '|', '\\']) {
                    return None;
                }
            } else if label_raw.contains(['|', '\\']) {
                return None;
            }
        } else {
            if prefix != "[" {
                return None;
            }
            let destination = suffix.strip_prefix("](")?.strip_suffix(')')?;
            if destination.is_empty()
                || destination
                    .chars()
                    .any(|c| c.is_whitespace() || "()[]\\\"'<>`".contains(c))
            {
                return None;
            }
        }
        let target = if wiki {
            if prefix == "[[" {
                label_raw
            } else {
                prefix.strip_prefix("[[")?.strip_suffix('|')?
            }
        } else {
            suffix.strip_prefix("](")?.strip_suffix(')')?
        };
        candidate.links.push(NoteLink {
            range: range.clone(),
            label: label.clone(),
            target: target.to_owned(),
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
fn plain(raw: &str) -> Option<()> {
    let mut bytes = raw.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'\\' {
            let next = bytes.next()?;
            if !next.is_ascii_punctuation() {
                return None;
            }
        } else if b"*_[]~`=$".contains(&byte) {
            return None;
        }
    }
    Some(())
}

#[cfg(test)]
mod tests;
