//! Progressive top-level classification for ordinary Live Preview (#1640).
//!
//! markdown-rs discovers CommonMark/GFM block boundaries with every inline
//! construct disabled. Comrak remains the semantic classifier, on individual
//! bounded blocks only. Neither parser's decoded text is an editing authority.
use super::{
    block_cache::{BlockCache, Stats},
    decorations::Marker,
    Classification, Reference, SourceReason,
};
use crate::source_projection::{self, Active, Plan, Region, Snapshot};
use markdown::{mdast::Node, Constructs, ParseOptions};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

/// Includes the 1.2M-character performance fixture; not a per-block parse cap.
pub const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_BLOCKS: usize = 32 * 1024;

fn structural_options() -> ParseOptions {
    ParseOptions {
        constructs: Constructs {
            attention: false,
            autolink: false,
            character_escape: false,
            character_reference: false,
            code_text: false,
            gfm_strikethrough: false,
            gfm_autolink_literal: false,
            gfm_label_start_footnote: false,
            gfm_footnote_definition: false,
            gfm_table: true,
            gfm_task_list_item: false,
            hard_break_escape: false,
            hard_break_trailing: false,
            html_text: false,
            label_start_image: false,
            label_start_link: false,
            label_end: false,
            frontmatter: true,
            ..Constructs::default()
        },
        ..ParseOptions::default()
    }
}

/// Exact ranges from the document parser. Blank inter-block gaps stay Source.
#[derive(Clone, Debug)]
pub struct BlockIndex {
    snapshot: Snapshot,
    blocks: Vec<Range<usize>>,
    references: Vec<Reference>,
    // A fragment-local definition must not override an earlier document-wide
    // definition. Such a container remains raw rather than resolving wrongly.
    conflicting_definitions: Vec<Range<usize>>,
}

impl BlockIndex {
    pub fn new(snapshot: Snapshot) -> Result<Self, SourceReason> {
        let source = snapshot.source();
        if source.len() > MAX_DOCUMENT_BYTES {
            return Err(SourceReason::InputLimit);
        }
        if source.contains('\0')
            || source
                .as_bytes()
                .iter()
                .enumerate()
                .any(|(i, &b)| b == b'\r' && source.as_bytes().get(i + 1) != Some(&b'\n'))
        {
            return Err(SourceReason::ParserCoordinates);
        }
        let root = markdown::to_mdast(source, &structural_options())
            .map_err(|_| SourceReason::ParserCoordinates)?;
        let children = root.children().ok_or(SourceReason::ParserCoordinates)?;
        if children.len() > MAX_BLOCKS {
            return Err(SourceReason::StructureLimit);
        }
        let first_line = source.trim_start_matches('\u{feff}').lines().next();
        let unclosed_frontmatter = matches!(first_line, Some("---" | "+++"))
            && !matches!(children.first(), Some(Node::Yaml(_) | Node::Toml(_)));
        let mut blocks = Vec::with_capacity(children.len());
        if unclosed_frontmatter {
            // Match the classifier's existing conservative frontmatter rule.
            // The caller can keep this one malformed construct raw, not parse
            // its apparent headings as independent Markdown.
            if !source.is_empty() {
                blocks.push(0..source.len());
            }
        } else {
            for child in children {
                let pos = child.position().ok_or(SourceReason::ParserCoordinates)?;
                let (start, end) = (pos.start.offset, pos.end.offset);
                if start > end
                    || end > source.len()
                    || !source.is_char_boundary(start)
                    || !source.is_char_boundary(end)
                {
                    return Err(SourceReason::ParserCoordinates);
                }
                let start = source[..start].rfind('\n').map_or(0, |at| at + 1);
                let end = if end == 0 || source.as_bytes().get(end - 1) != Some(&b'\n') {
                    source[end..]
                        .find('\n')
                        .map_or(source.len(), |at| end + at + 1)
                } else {
                    end
                };
                if blocks
                    .last()
                    .is_some_and(|previous: &Range<usize>| previous.end > start)
                {
                    return Err(SourceReason::ParserCoordinates);
                }
                if start < end {
                    blocks.push(start..end);
                }
            }
        }
        let mut references: Vec<Reference> = Vec::new();
        let mut conflicting_definitions = Vec::new();
        let mut definitions = HashMap::new();
        let mut visited = vec![&root];
        while let Some(node) = visited.pop() {
            if let Node::Definition(definition) = node {
                let reference = Reference {
                    label: super::normalized_label(&definition.identifier),
                    url: definition.url.clone(),
                    title: definition.title.clone().unwrap_or_default(),
                };
                if let Some(&index) = definitions.get(&reference.label) {
                    let previous = &references[index];
                    if previous != &reference {
                        let pos = definition
                            .position
                            .as_ref()
                            .ok_or(SourceReason::ParserCoordinates)?;
                        conflicting_definitions.push(pos.start.offset..pos.end.offset);
                    }
                } else {
                    definitions.insert(reference.label.clone(), references.len());
                    references.push(reference);
                }
            }
            if let Some(children) = node.children() {
                visited.extend(children.iter().rev());
            }
        }
        Ok(Self {
            snapshot,
            blocks,
            references,
            conflicting_definitions,
        })
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub fn blocks(&self) -> &[Range<usize>] {
        &self.blocks
    }
}

/// Cache owner scoped to one editor, including the >20% invalidation rule.
pub struct Session {
    previous: Option<Snapshot>,
    cache: BlockCache,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            previous: None,
            cache: BlockCache::new(MAX_BLOCKS, 4 * MAX_DOCUMENT_BYTES),
        }
    }
}

impl Session {
    pub fn stats(&self) -> Stats {
        self.cache.stats()
    }

    pub fn classify(&mut self, source: Snapshot) -> Classification {
        let mut work = match self.begin(source.clone()) {
            Ok(work) => work,
            Err(reason) => return super::fallback(&source, reason),
        };
        work.advance(self, 0..0, usize::MAX);
        work.classification()
    }

    pub fn begin(&mut self, source: Snapshot) -> Result<Progressive, SourceReason> {
        let full_reparse = self.previous.as_ref().is_none_or(|old| {
            old.document() != source.document()
                || changed_bytes(old.source(), source.source()).saturating_mul(5)
                    > old.source().len()
        });
        if full_reparse {
            self.cache.clear();
        }
        let index = BlockIndex::new(source.clone())?;
        self.previous = Some(source);
        // Keep proven offscreen presentation on a small edit: publishing only
        // the active viewport must not erase cached block heights above it.
        let classified = index
            .blocks
            .iter()
            .map(|range| {
                let fragment = Snapshot::new(
                    index.snapshot.document(),
                    index.snapshot.generation(),
                    &index.snapshot.source()[range.clone()],
                );
                self.cache
                    .lookup_fragment(&fragment, &index.references, range.start == 0)
            })
            .collect();
        Ok(Progressive {
            classified,
            index,
            full_reparse,
        })
    }
}

fn changed_bytes(before: &str, after: &str) -> usize {
    let common = before
        .bytes()
        .zip(after.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = before.as_bytes()[common..]
        .iter()
        .rev()
        .zip(after.as_bytes()[common..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    (before.len() - common - suffix).max(after.len() - common - suffix)
}

pub struct Progressive {
    index: BlockIndex,
    classified: Vec<Option<Classification>>,
    full_reparse: bool,
}

impl Progressive {
    pub fn index(&self) -> &BlockIndex {
        &self.index
    }

    pub fn is_complete(&self) -> bool {
        self.classified.iter().all(Option::is_some)
    }

    pub fn full_reparse(&self) -> bool {
        self.full_reparse
    }

    /// Prioritize every intersecting block, then visit the remaining blocks in
    /// source order. An empty viewport still schedules its containing block.
    /// This is called off-thread; `limit` bounds inline work in a publication.
    pub fn advance(
        &mut self,
        session: &mut Session,
        viewport: Range<usize>,
        limit: usize,
    ) -> usize {
        let mut order: Vec<usize> = (0..self.index.blocks.len())
            .filter(|&i| self.classified[i].is_none())
            .collect();
        order.sort_by_key(|&i| {
            let b = &self.index.blocks[i];
            !(viewport.start <= b.end && b.start <= viewport.end)
        });
        let mut count = 0;
        for i in order.into_iter().take(limit) {
            let range = self.index.blocks[i].clone();
            let fragment = Snapshot::new(
                self.index.snapshot.document(),
                self.index.snapshot.generation(),
                &self.index.snapshot.source()[range.clone()],
            );
            let classified = if self
                .index
                .conflicting_definitions
                .iter()
                .any(|definition| range.start <= definition.start && definition.end <= range.end)
            {
                super::fallback(&fragment, SourceReason::UnsupportedOrAmbiguous)
            } else {
                session
                    .cache
                    .classify_fragment(&fragment, &self.index.references, range.start == 0)
            };
            self.classified[i] = Some(classified);
            count += 1;
        }
        count
    }

    /// Snapshot-bound metadata only; pending blocks have no inferred styles.
    /// Recombination applies authored byte offsets, never decoded AST positions.
    pub fn classification(&self) -> Classification {
        let snapshot = self.index.snapshot();
        let mut result = super::fallback(snapshot, SourceReason::PendingBlocks);
        result.reasons.clear();
        let mut regions = Vec::new();
        for (range, classified) in self.index.blocks.iter().zip(&self.classified) {
            let Some(classified) = classified else {
                result.reasons.push(SourceReason::PendingBlocks);
                continue;
            };
            let shift = |r: &Range<usize>| r.start + range.start..r.end + range.start;
            for region in classified.plan.regions() {
                regions.push(match region {
                    Region::Source(r) => Region::Source(shift(r)),
                    Region::Conceal { block, markers } => Region::Conceal {
                        block: shift(block),
                        markers: markers.iter().map(shift).collect(),
                    },
                });
            }
            result
                .styles
                .extend(classified.styles.iter().map(|s| super::StyleSpan {
                    range: shift(&s.range),
                    style: s.style,
                }));
            result.marker_scopes.extend(
                classified
                    .marker_scopes
                    .iter()
                    .map(|(m, s)| (shift(m), shift(s))),
            );
            result
                .contexts
                .extend(classified.contexts.iter().map(shift));
            result
                .links
                .extend(classified.links.iter().map(|link| super::NoteLink {
                    range: shift(&link.range),
                    label: shift(&link.label),
                    target: link.target.clone(),
                    wiki: link.wiki,
                }));
            result
                .headings
                .extend(classified.headings.iter().map(|heading| super::Heading {
                    text: heading.text.clone(),
                    offset: heading.offset + range.start,
                    accepted: heading.accepted,
                }));
            result
                .decorations
                .extend(classified.decorations.iter().map(|m| Marker {
                    range: shift(&m.range),
                    scope: shift(&m.scope),
                    kind: m.kind.clone(),
                }));
            result.reasons.extend(classified.reasons.iter().copied());
        }
        result.references = self.index.references.clone();
        let mut seen = HashSet::new();
        result.reasons.retain(|reason| seen.insert(*reason as u8));
        result.plan = Plan::new(snapshot, regions);
        if source_projection::project(snapshot, &result.plan, &Active::default()).is_err() {
            return super::fallback(snapshot, SourceReason::ProjectionBoundary);
        }
        result
    }
}

#[cfg(test)]
mod tests;
