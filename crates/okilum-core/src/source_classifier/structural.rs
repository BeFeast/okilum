//! Immutable authority from one canonical root parse, deliberately Reader-unwired.
//! No local parse, fragment fuel, replay, cache-key relaxation or global join.
use super::{Classification, Context, SourceReason, MAX_BYTES, MAX_DEPTH, MAX_NODES};
use crate::source_projection::{FallbackReason, Snapshot, MAX_RANGES, MAX_WORK};
use comrak::{
    nodes::{AstNode, NodeValue, Sourcepos},
    reference_audit::{DefinitionEvent, LookupEvent, ReferenceAudit, ReferenceFuel},
    Arena, Options,
};
use std::{ops::Range, sync::Arc};
use unicode_segmentation::UnicodeSegmentation;

/// A closed complete configuration: canonical_options(source, [], true), with
/// NO broken-link callback and no arbitrary Options accepted as equivalent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParseContext {
    LivePreview,
}

/// Unknown includes even an options value that happens to have identical fields.
/// An external callback's pointer/hash is never a semantic certificate.
pub(crate) enum ContextClaim {
    Known(ParseContext),
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    Source(SourceReason),
    Identity(FallbackReason),
    Coordinates,
    InvalidBoundary,
    Stale,
    UnknownContext,
    Incomplete,
    SequenceLimit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EditorRange {
    pub bytes: Range<usize>,
    pub utf16: Range<usize>,
}

#[derive(Clone, Debug)]
struct Boundaries {
    lines: Vec<usize>,
    pairs: Vec<(usize, usize)>,
}
impl Boundaries {
    fn new(source: &str) -> Self {
        let lines = std::iter::once(0)
            .chain(source.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        let mut units = 0;
        let mut pairs = Vec::new();
        for (byte, cluster) in source.grapheme_indices(true) {
            pairs.push((byte, units));
            units += cluster.encode_utf16().count();
        }
        pairs.push((source.len(), units));
        Self { lines, pairs }
    }
    fn byte_to_utf16(&self, byte: usize) -> Result<usize, Error> {
        self.pairs
            .binary_search_by_key(&byte, |p| p.0)
            .map(|i| self.pairs[i].1)
            .map_err(|_| Error::InvalidBoundary)
    }
    fn utf16_to_byte(&self, units: usize) -> Result<usize, Error> {
        self.pairs
            .binary_search_by_key(&units, |p| p.1)
            .map(|i| self.pairs[i].0)
            .map_err(|_| Error::InvalidBoundary)
    }
    fn range(&self, bytes: Range<usize>) -> Result<EditorRange, Error> {
        if bytes.start > bytes.end {
            return Err(Error::InvalidBoundary);
        }
        Ok(EditorRange {
            utf16: self.byte_to_utf16(bytes.start)?..self.byte_to_utf16(bytes.end)?,
            bytes,
        })
    }
    fn parser_range(&self, source: &str, pos: Sourcepos) -> Result<EditorRange, Error> {
        let start_line = *self
            .lines
            .get(pos.start.line.checked_sub(1).ok_or(Error::Coordinates)?)
            .ok_or(Error::Coordinates)?;
        let end_line = *self
            .lines
            .get(pos.end.line.checked_sub(1).ok_or(Error::Coordinates)?)
            .ok_or(Error::Coordinates)?;
        let start = start_line
            .checked_add(pos.start.column.checked_sub(1).ok_or(Error::Coordinates)?)
            .ok_or(Error::Coordinates)?;
        let end = end_line
            .checked_add(pos.end.column)
            .ok_or(Error::Coordinates)?;
        let line_limit = |line: usize| self.lines.get(line).copied().unwrap_or(source.len());
        if pos.end.column == 0
            || start >= end
            || start >= line_limit(pos.start.line)
            || end > line_limit(pos.end.line)
            || source.get(start..end).is_none()
        {
            return Err(Error::Coordinates);
        }
        self.range(start..end)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockKind {
    Paragraph,
    Heading,
    List,
    Quote,
    Frontmatter,
    Other,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Block {
    /// Original canonical position alongside its validated editor conversion.
    pub sourcepos: Sourcepos,
    /// Exact canonical AST extent, converted to editor boundaries.
    pub authored: EditorRange,
    /// Whole authored lines (indentation and newline included); gaps stay raw.
    pub fragment: EditorRange,
    pub kind: BlockKind,
}

/// Complete canonical observations, sealed to this exact root and closed context.
/// Prefix-stripped definition offsets remain observations, not editor ranges.
#[derive(Debug)]
pub(crate) struct ReferenceAuthority {
    snapshot: Snapshot,
    context: ParseContext,
    audit: ReferenceAudit,
    definition_contexts: Vec<EditorRange>,
    effective: Vec<usize>,
}
impl ReferenceAuthority {
    fn check(&self, current: &Snapshot, context: ContextClaim) -> Result<(), Error> {
        check(&self.snapshot, self.context, current, context)
    }
    pub fn definitions(
        &self,
        current: &Snapshot,
        context: ContextClaim,
    ) -> Result<&[DefinitionEvent], Error> {
        self.check(current, context)?;
        Ok(self.audit.definitions())
    }
    pub fn effective(
        &self,
        current: &Snapshot,
        context: ContextClaim,
    ) -> Result<Vec<&DefinitionEvent>, Error> {
        self.check(current, context)?;
        Ok(self
            .effective
            .iter()
            .map(|&i| &self.audit.definitions()[i])
            .collect())
    }
    pub fn definition_contexts(
        &self,
        current: &Snapshot,
        context: ContextClaim,
    ) -> Result<&[EditorRange], Error> {
        self.check(current, context)?;
        Ok(&self.definition_contexts)
    }
    pub fn lookups(
        &self,
        current: &Snapshot,
        context: ContextClaim,
    ) -> Result<&[LookupEvent], Error> {
        self.check(current, context)?;
        Ok(self.audit.lookups())
    }
    pub fn fuel(&self, current: &Snapshot, context: ContextClaim) -> Result<ReferenceFuel, Error> {
        self.check(current, context)?;
        Ok(self.audit.final_fuel())
    }
}

#[derive(Debug)]
pub(crate) struct BlockIndex {
    references: ReferenceAuthority,
    blocks: Vec<Block>,
    raw_gaps: Vec<EditorRange>,
    boundaries: Boundaries,
    classification: Classification,
}
fn check(
    sealed: &Snapshot,
    known: ParseContext,
    current: &Snapshot,
    claim: ContextClaim,
) -> Result<(), Error> {
    match claim {
        ContextClaim::Known(context) if context == known => {}
        _ => return Err(Error::UnknownContext),
    }
    if sealed != current {
        return Err(Error::Stale);
    }
    Ok(())
}
fn preflight(source: &Snapshot) -> Result<(), Error> {
    source.check_identity().map_err(Error::Identity)?;
    if source.source().len() > MAX_BYTES {
        return Err(Error::Source(SourceReason::InputLimit));
    }
    if source.source().contains('\0')
        || source
            .source()
            .as_bytes()
            .iter()
            .enumerate()
            .any(|(i, &b)| b == b'\r' && source.source().as_bytes().get(i + 1) != Some(&b'\n'))
    {
        return Err(Error::Source(SourceReason::ParserCoordinates));
    }
    Ok(())
}
impl BlockIndex {
    pub fn build(snapshot: Snapshot, context: ParseContext) -> Result<Arc<Self>, Error> {
        preflight(&snapshot)?;
        let source = snapshot.source();
        let arena = Arena::new();
        let options = match context {
            ParseContext::LivePreview => super::canonical_options(source, &[], true),
        };
        #[cfg(test)]
        super::TEST_PARSE_CALLS.with(|count| count.set(count.get() + 1));
        let (root, audit) = comrak::parse_document_with_reference_audit(&arena, source, &options);
        let classification = super::classify_parsed(&snapshot, &Context::new(source), root, true);
        if let Some(reason) = classification
            .reasons()
            .iter()
            .find(|r| !matches!(r, SourceReason::UnsupportedOrAmbiguous))
        {
            return Err(Error::Source(*reason));
        }
        if matches!(
            source.trim_start_matches('\u{feff}').lines().next(),
            Some("---" | "+++")
        ) && !root
            .children()
            .any(|node| matches!(node.data.borrow().value, NodeValue::FrontMatter(_)))
        {
            return Err(Error::Source(SourceReason::UnsupportedOrAmbiguous));
        }
        let boundaries = Boundaries::new(source);
        let mut blocks = Vec::new();
        let mut raw_gaps = Vec::new();
        let mut cursor = 0;
        for node in root.children() {
            let data = node.data.borrow();
            let authored = boundaries.parser_range(source, data.sourcepos)?;
            let start = *boundaries
                .lines
                .get(data.sourcepos.start.line - 1)
                .ok_or(Error::Coordinates)?;
            let end = boundaries
                .lines
                .get(data.sourcepos.end.line)
                .copied()
                .unwrap_or(source.len());
            if start < cursor || end < authored.bytes.end {
                return Err(Error::Coordinates);
            }
            if cursor < start {
                raw_gaps.push(boundaries.range(cursor..start)?);
            }
            let fragment = boundaries.range(start..end)?;
            let kind = match data.value {
                NodeValue::Paragraph => BlockKind::Paragraph,
                NodeValue::Heading(_) => BlockKind::Heading,
                NodeValue::List(_) => BlockKind::List,
                NodeValue::BlockQuote => BlockKind::Quote,
                NodeValue::FrontMatter(_) => BlockKind::Frontmatter,
                _ => BlockKind::Other,
            };
            blocks.push(Block {
                sourcepos: data.sourcepos,
                authored,
                fragment,
                kind,
            });
            cursor = end;
        }
        if cursor < source.len() {
            raw_gaps.push(boundaries.range(cursor..source.len())?);
        }
        if blocks.len() + raw_gaps.len() + audit.definitions().len() + audit.lookups().len()
            > MAX_RANGES
        {
            return Err(Error::Source(SourceReason::StructureLimit));
        }
        // Logical retained text/index allowance, not process RSS or parser fuel.
        let mut work = source
            .len()
            .saturating_add(boundaries.pairs.len().saturating_mul(16))
            .saturating_add(boundaries.lines.len().saturating_mul(8));
        for d in audit.definitions() {
            work = work
                .saturating_add(d.raw.len())
                .saturating_add(d.label.len())
                .saturating_add(d.url.len())
                .saturating_add(d.title.len());
        }
        for l in audit.lookups() {
            work = work
                .saturating_add(l.original.len())
                .saturating_add(l.normalized.len());
        }
        if work > MAX_WORK {
            return Err(Error::Source(SourceReason::StructureLimit));
        }
        let mut definition_contexts = Vec::new();
        let mut effective = Vec::new();
        for (index, event) in audit.definitions().iter().enumerate() {
            definition_contexts.push(boundaries.parser_range(source, event.paragraph)?);
            if event.inserted {
                effective.push(index);
            }
        }
        Ok(Arc::new(Self {
            references: ReferenceAuthority {
                snapshot,
                context,
                audit,
                definition_contexts,
                effective,
            },
            blocks,
            raw_gaps,
            boundaries,
            classification,
        }))
    }
    fn check(&self, current: &Snapshot, context: ContextClaim) -> Result<(), Error> {
        self.references.check(current, context)
    }
    pub fn blocks(&self, current: &Snapshot, context: ContextClaim) -> Result<&[Block], Error> {
        self.check(current, context)?;
        Ok(&self.blocks)
    }
    pub fn raw_gaps(
        &self,
        current: &Snapshot,
        context: ContextClaim,
    ) -> Result<&[EditorRange], Error> {
        self.check(current, context)?;
        Ok(&self.raw_gaps)
    }
    pub fn references(
        &self,
        current: &Snapshot,
        context: ContextClaim,
    ) -> Result<&ReferenceAuthority, Error> {
        self.check(current, context)?;
        Ok(&self.references)
    }
    pub fn classification(
        &self,
        current: &Snapshot,
        context: ContextClaim,
    ) -> Result<&Classification, Error> {
        self.check(current, context)?;
        Ok(&self.classification)
    }
    pub fn byte_to_utf16(
        &self,
        current: &Snapshot,
        context: ContextClaim,
        byte: usize,
    ) -> Result<usize, Error> {
        self.check(current, context)?;
        self.boundaries.byte_to_utf16(byte)
    }
    pub fn utf16_to_byte(
        &self,
        current: &Snapshot,
        context: ContextClaim,
        units: usize,
    ) -> Result<usize, Error> {
        self.check(current, context)?;
        self.boundaries.utf16_to_byte(units)
    }
}

/// Exactly one parse with the ORIGINAL supplied options/callbacks. No certificate,
/// ticket or adoption can be made from this report, even for pointer-equal options.
pub(crate) fn observe_untrusted(
    snapshot: &Snapshot,
    options: &Options<'_>,
) -> Result<ReferenceAudit, Error> {
    preflight(snapshot)?;
    let arena = Arena::new();
    #[cfg(test)]
    super::TEST_PARSE_CALLS.with(|count| count.set(count.get() + 1));
    let (root, audit) =
        comrak::parse_document_with_reference_audit(&arena, snapshot.source(), options);
    validate_tree(root)?;
    if audit.definitions().len() + audit.lookups().len() > MAX_RANGES {
        return Err(Error::Source(SourceReason::StructureLimit));
    }
    Ok(audit)
}

fn validate_tree<'a>(root: &'a AstNode<'a>) -> Result<(), Error> {
    for (count, node) in root.descendants().enumerate() {
        if count >= MAX_NODES || node.ancestors().take(MAX_DEPTH + 2).count() > MAX_DEPTH + 1 {
            return Err(Error::Source(SourceReason::StructureLimit));
        }
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct Ticket {
    source: Snapshot,
    context: ParseContext,
    sequence: u64,
    pub full_invalidation: bool,
}
pub(crate) struct Completed {
    ticket: Ticket,
    index: Arc<BlockIndex>,
}
impl Ticket {
    pub fn compute(self) -> Result<Completed, Error> {
        let index = BlockIndex::build(self.source.clone(), self.context)?;
        Ok(Completed {
            ticket: self,
            index,
        })
    }
}

#[derive(Default)]
pub(crate) struct Session {
    requested: Option<Ticket>,
    accepted: Option<Arc<BlockIndex>>,
    sequence: u64,
}
impl Session {
    pub fn begin(&mut self, source: Snapshot, context: ParseContext) -> Result<Ticket, Error> {
        preflight(&source)?;
        if let Some(old) = &self.requested {
            if old.source.document() == source.document()
                && source != old.source
                && source.generation() <= old.source.generation()
            {
                return Err(Error::Stale);
            }
        }
        let full_invalidation = self.requested.as_ref().is_none_or(|old| {
            old.context != context
                || old.source.document() != source.document()
                || changed_bytes(old.source.source(), source.source()).saturating_mul(5)
                    > old.source.source().len()
        });
        let sequence = self.sequence.checked_add(1).ok_or(Error::SequenceLimit)?;
        if full_invalidation {
            self.accepted = None;
        }
        let ticket = Ticket {
            source,
            context,
            sequence,
            full_invalidation,
        };
        self.sequence = sequence;
        self.requested = Some(ticket.clone());
        Ok(ticket)
    }
    fn check_ticket(&self, ticket: &Ticket) -> Result<(), Error> {
        let current = self.requested.as_ref().ok_or(Error::Stale)?;
        if ticket.sequence != current.sequence
            || ticket.source != current.source
            || ticket.context != current.context
        {
            return Err(Error::Stale);
        }
        Ok(())
    }
    pub fn reject_partial(&self, ticket: &Ticket) -> Result<(), Error> {
        self.check_ticket(ticket)?;
        Err(Error::Incomplete)
    }
    pub fn adopt(&mut self, completed: Completed) -> Result<Arc<BlockIndex>, Error> {
        self.check_ticket(&completed.ticket)?;
        completed.index.check(
            &completed.ticket.source,
            ContextClaim::Known(completed.ticket.context),
        )?;
        self.accepted = Some(completed.index.clone());
        Ok(completed.index)
    }
    pub fn current(&self) -> Option<&Arc<BlockIndex>> {
        let ticket = self.requested.as_ref()?;
        self.accepted.as_ref().filter(|index| {
            index
                .check(&ticket.source, ContextClaim::Known(ticket.context))
                .is_ok()
        })
    }
}
fn changed_bytes(before: &str, after: &str) -> usize {
    let prefix = before
        .bytes()
        .zip(after.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = before.as_bytes()[prefix..]
        .iter()
        .rev()
        .zip(after.as_bytes()[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    (before.len() - prefix - suffix).max(after.len() - prefix - suffix)
}

#[cfg(test)]
mod tests;
