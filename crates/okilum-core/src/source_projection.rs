//! Bounded canonical-source mapping foundation, not a Markdown parser or editor.
//!
//! Callers classify blocks and supply approved conceal spans. A rejected plan
//! returns the current exact Source, never an approximate or stale projection.
use std::{ops::Range, sync::Arc};
pub(crate) mod boundary_index;
pub mod revision;
use boundary_index::Index;
use revision::{Edit, Text};

pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_RANGES: usize = 4096;
pub const MAX_WORK: usize = 8 * 1024 * 1024;
const MAX_DOCUMENT_ID_BYTES: usize = 4096;

/// Immutable identity and authored bytes. A generation alone is never a lease.
#[derive(Clone, Debug)]
pub struct Snapshot {
    document: Arc<str>,
    generation: u64,
    source: Text,
    edit: Option<Edit>,
    native: bool,
}
impl Snapshot {
    pub fn new(
        document: impl Into<Arc<str>>,
        generation: u64,
        source: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            document: document.into(),
            generation,
            source: Text::from(source.into()),
            edit: None,
            native: false,
        }
    }
    /// Contiguous source for parsers/workers. Input paths use text().
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn with_edit(mut self, edit: Option<Edit>) -> Self {
        self.edit = edit;
        self
    }
    pub fn text(&self) -> &Text {
        &self.source
    }
    pub fn from_rope(
        document: impl Into<Arc<str>>,
        generation: u64,
        text: Text,
        edit: Option<Edit>,
    ) -> Self {
        Self {
            document: document.into(),
            generation,
            source: text,
            edit,
            native: true,
        }
    }
    pub fn edit_from(&self, previous: &Self) -> Option<Edit> {
        if self.document != previous.document {
            return None;
        }
        if self.source.same(&previous.source) {
            return Some(Edit::unchanged(previous.source.id(), previous.source.len()));
        }
        self.edit
            .clone()
            .filter(|e| e.previous == previous.source.id())
    }
    pub(crate) fn change_from(&self, previous: &Self) -> Option<Edit> {
        if let Some(edit) = self.edit_from(previous) {
            return Some(edit);
        }
        if self.native || self.document != previous.document {
            return None;
        }
        // Compatibility for independent parser/test snapshots. Native input must
        // supply a revision receipt and never enters this discovery path.
        let old = previous.source();
        let new = self.source();
        let mut start = old
            .bytes()
            .zip(new.bytes())
            .inspect(|_| revision::charge(|w| w.scanned_bytes += 1))
            .take_while(|(a, b)| a == b)
            .count();
        while !old.is_char_boundary(start) || !new.is_char_boundary(start) {
            start -= 1;
        }
        let mut tail = old[start..]
            .bytes()
            .rev()
            .zip(new[start..].bytes().rev())
            .take_while(|(a, b)| a == b)
            .count();
        while !old.is_char_boundary(old.len() - tail) || !new.is_char_boundary(new.len() - tail) {
            tail -= 1;
        }
        Some(Edit {
            previous: previous.text().id(),
            old: start..old.len() - tail,
            inserted: new.len() - tail - start,
        })
    }
    pub fn document(&self) -> &str {
        &self.document
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn check_identity(&self) -> Result<(), FallbackReason> {
        if self.document.is_empty() || self.document.len() > MAX_DOCUMENT_ID_BYTES {
            return Err(FallbackReason::IdentityLimit);
        }
        Ok(())
    }
    fn matches(&self, other: &Self) -> bool {
        self.generation == other.generation
            && self.document == other.document
            && self.source == other.source
    }
    /// Copy authored bytes; rendered text is never a clipboard/edit authority.
    pub fn copy_source(&self, range: Range<usize>) -> Result<String, MapError> {
        self.check_edit_range(&range)?;
        Ok(self.source[range].into())
    }
    fn check_edit_range(&self, range: &Range<usize>) -> Result<(), MapError> {
        if self.source.len() > MAX_BYTES {
            return Err(MapError::SourceLimit);
        }
        if !valid_range(range, &Index::new(&self.source)) {
            return Err(MapError::InvalidBoundary);
        }
        Ok(())
    }
    /// Pure authored-source replacement. The native editor still owns undo,
    /// transaction grouping, IME and any application of this result to its buffer.
    pub fn replace_source(
        &self,
        current: &Self,
        range: Range<usize>,
        replacement: &str,
    ) -> Result<Self, MapError> {
        if self.source.len() > MAX_BYTES || current.source.len() > MAX_BYTES {
            return Err(MapError::SourceLimit);
        }
        if !self.matches(current) {
            return Err(MapError::StaleSnapshot);
        }
        self.check_edit_range(&range)?;
        let length = self.source.len() - range.len();
        if replacement.len() > MAX_BYTES - length {
            return Err(MapError::SourceLimit);
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(MapError::GenerationOverflow)?;
        let mut text = String::with_capacity(length + replacement.len());
        text.push_str(&self.source[..range.start]);
        text.push_str(replacement);
        text.push_str(&self.source[range.end..]);
        Ok(Self::new(self.document.clone(), generation, text))
    }
}

impl PartialEq for Snapshot {
    fn eq(&self, other: &Self) -> bool {
        self.matches(other)
    }
}
impl Eq for Snapshot {}

/// Regions must be sorted and disjoint; the semantic classifier is out of scope.
#[derive(Clone, Debug)]
pub enum Region {
    /// Unsupported or incomplete syntax stays exactly authored in place.
    Source(Range<usize>),
    Conceal {
        block: Range<usize>,
        markers: Vec<Range<usize>>,
    },
}
impl Region {
    pub(crate) fn block(&self) -> &Range<usize> {
        match self {
            Self::Source(range) | Self::Conceal { block: range, .. } => range,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Plan {
    snapshot: Snapshot,
    regions: Vec<Region>,
}
impl Plan {
    pub(crate) fn regions(&self) -> &[Region] {
        &self.regions
    }
    pub fn new(snapshot: &Snapshot, regions: Vec<Region>) -> Self {
        Self {
            snapshot: snapshot.clone(),
            regions,
        }
    }
}

/// Both ranges use canonical source coordinates, including zero-width carets.
/// A caret on a shared block boundary reveals both adjacent blocks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Active {
    pub selection: Option<Range<usize>>,
    pub composition: Option<Range<usize>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bias {
    Left,
    Right,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackReason {
    SourceLimit,
    IdentityLimit,
    RangeLimit,
    WorkLimit,
    StaleSnapshot,
    InvalidBoundary,
    OverlappingRanges,
    ProtectedSource,
    JoinedGrapheme,
}
#[derive(Clone, Debug)]
pub struct SourceFallback {
    pub reason: FallbackReason,
    snapshot: Snapshot,
}
impl SourceFallback {
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    pub fn source(&self) -> &str {
        self.snapshot.source()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    SourceLimit,
    StaleSnapshot,
    InvalidBoundary,
    GenerationOverflow,
}
#[derive(Debug)]
struct Anchor {
    source: Range<usize>,
    display: usize,
}
#[derive(Debug)]
pub struct Projection {
    snapshot: Snapshot,
    display: Text,
    anchors: Vec<Anchor>,
    source_boundaries: Index,
    display_boundaries: Index,
}
impl Projection {
    pub(crate) fn source_index_for(&self, snapshot: &Snapshot) -> Option<Index> {
        (self.snapshot.document == snapshot.document).then(|| {
            snapshot.change_from(&self.snapshot).map_or_else(
                || Index::new(snapshot.source()),
                |edit| {
                    self.source_boundaries
                        .update_edit(self.snapshot.text(), snapshot.text(), &edit)
                },
            )
        })
    }
    pub fn display(&self) -> &str {
        &self.display
    }
    pub fn display_text(&self) -> &Text {
        &self.display
    }
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    /// Newline coordinates from the retained index; no whole-display rescan.
    pub fn display_line_at(&self, byte: usize) -> usize {
        self.display_boundaries.line_at(byte)
    }
    pub fn display_line_start(&self, line: usize) -> Option<usize> {
        self.display_boundaries.line_start(line)
    }
    pub fn source_line_at(&self, byte: usize) -> usize {
        self.source_boundaries.line_at(byte)
    }
    fn check(&self, current: &Snapshot) -> Result<(), MapError> {
        if !self.snapshot.matches(current) {
            return Err(MapError::StaleSnapshot);
        }
        Ok(())
    }
    /// Validate active ranges against this already validated immutable snapshot.
    /// Callers may reuse the boundary index only for this exact source revision.
    pub(crate) fn validate_active(&self, active: &Active) -> Result<(), SourceFallback> {
        for range in [active.selection.as_ref(), active.composition.as_ref()]
            .into_iter()
            .flatten()
        {
            if !valid_range(range, &self.source_boundaries) {
                return Err(SourceFallback {
                    reason: FallbackReason::InvalidBoundary,
                    snapshot: self.snapshot.clone(),
                });
            }
        }
        Ok(())
    }

    /// Hidden source boundaries collapse to their one display anchor.
    pub fn source_to_display(&self, current: &Snapshot, offset: usize) -> Result<usize, MapError> {
        self.check(current)?;
        if self.source_boundaries.binary_search(&offset).is_err() {
            return Err(MapError::InvalidBoundary);
        }
        let index = self.anchors.partition_point(|a| a.source.end < offset);
        if let Some(anchor) = self.anchors.get(index).filter(|a| a.source.start <= offset) {
            return Ok(anchor.display);
        }
        let removed = index
            .checked_sub(1)
            .map(|i| self.anchors[i].source.end - self.anchors[i].display)
            .unwrap_or(0);
        Ok(offset - removed)
    }
    /// At a concealed anchor Left means before all hidden bytes; Right means
    /// after them. Adjacent concealed ranges share one combined anchor.
    pub fn display_to_source(
        &self,
        current: &Snapshot,
        offset: usize,
        bias: Bias,
    ) -> Result<usize, MapError> {
        self.check(current)?;
        if self.display_boundaries.binary_search(&offset).is_err() {
            return Err(MapError::InvalidBoundary);
        }
        let index = self.anchors.partition_point(|a| a.display < offset);
        if let Some(anchor) = self.anchors.get(index).filter(|a| a.display == offset) {
            return Ok(match bias {
                Bias::Left => anchor.source.start,
                Bias::Right => anchor.source.end,
            });
        }
        let removed = index
            .checked_sub(1)
            .map(|i| self.anchors[i].source.end - self.anchors[i].display)
            .unwrap_or(0);
        Ok(offset + removed)
    }
    /// Map a visual selection explicitly, then rebuild with the returned active
    /// range to reveal affected blocks before an editor performs a source edit.
    /// A collapsed caret uses the same bias at both endpoints and stays collapsed.
    pub fn reveal_selection(
        &self,
        current: &Snapshot,
        display: Range<usize>,
        start: Bias,
        end: Bias,
    ) -> Result<Active, MapError> {
        if display.start > display.end || (display.is_empty() && start != end) {
            return Err(MapError::InvalidBoundary);
        }
        let range = self.display_to_source(current, display.start, start)?
            ..self.display_to_source(current, display.end, end)?;
        if range.start > range.end {
            return Err(MapError::InvalidBoundary);
        }
        Ok(Active {
            selection: Some(range),
            composition: None,
        })
    }
}

#[cfg(test)]
fn boundaries(text: &str) -> Vec<usize> {
    use unicode_segmentation::UnicodeSegmentation;
    text.grapheme_indices(true)
        .map(|(at, _)| at)
        .chain(std::iter::once(text.len()))
        .collect()
}

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_INDEX_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
fn valid_range(range: &Range<usize>, boundaries: &Index) -> bool {
    range.start <= range.end
        && boundaries.binary_search(&range.start).is_ok()
        && boundaries.binary_search(&range.end).is_ok()
}
fn touches(active: &Range<usize>, block: &Range<usize>) -> bool {
    // Deliberately reveal both sides of a shared caret/selection boundary.
    active.start <= block.end && block.start <= active.end
}
struct Work(usize);
impl Work {
    fn charge(&mut self, units: usize) -> Result<(), FallbackReason> {
        self.0 = self.0.checked_sub(units).ok_or(FallbackReason::WorkLimit)?;
        Ok(())
    }
}

pub fn project(
    snapshot: &Snapshot,
    plan: &Plan,
    active: &Active,
) -> Result<Projection, SourceFallback> {
    project_with_work_budget(snapshot, plan, active, MAX_WORK)
}
/// The supplied budget can lower the hard work cap, never raise it. Work charges
/// cover bounded byte scans/copies and range validation. Plans are not sorted or
/// repaired; disjoint ordered input keeps processing linear apart from boundary
/// lookups. This is deterministic fuel, not a claimed native frame deadline.
pub fn project_with_work_budget(
    snapshot: &Snapshot,
    plan: &Plan,
    active: &Active,
    budget: usize,
) -> Result<Projection, SourceFallback> {
    project_reusing(snapshot, plan, active, budget, None)
}

/// Reuse only exact preceding indexes. Changed complete lines are segmented;
/// LF seams retain independent grapheme boundaries, including CRLF and Unicode.
pub(crate) fn project_reusing(
    snapshot: &Snapshot,
    plan: &Plan,
    active: &Active,
    budget: usize,
    previous: Option<&Projection>,
) -> Result<Projection, SourceFallback> {
    let previous = previous.filter(|p| p.snapshot.document == snapshot.document);
    let build = || -> Result<Projection, FallbackReason> {
        if snapshot.source.len() > MAX_BYTES || plan.snapshot.source.len() > MAX_BYTES {
            return Err(FallbackReason::SourceLimit);
        }
        if snapshot.document.is_empty()
            || snapshot.document.len() > MAX_DOCUMENT_ID_BYTES
            || plan.snapshot.document.len() > MAX_DOCUMENT_ID_BYTES
        {
            return Err(FallbackReason::IdentityLimit);
        }
        let mut work = Work(budget.min(MAX_WORK));
        work.charge(snapshot.source.len() + snapshot.document.len())?;
        if !snapshot.matches(&plan.snapshot) {
            return Err(FallbackReason::StaleSnapshot);
        }
        let mut count = plan.regions.len();
        if count > MAX_RANGES {
            return Err(FallbackReason::RangeLimit);
        }
        for region in &plan.regions {
            if let Region::Conceal { markers, .. } = region {
                count = count
                    .checked_add(markers.len())
                    .ok_or(FallbackReason::RangeLimit)?;
                if count > MAX_RANGES {
                    return Err(FallbackReason::RangeLimit);
                }
            }
        }
        // A range allowance covers its bounded boundary searches as well as validation.
        work.charge(count * 64 + snapshot.source.len())?;
        #[cfg(test)]
        TEST_INDEX_BUILDS.with(|count| count.set(count.get() + 1));
        let source_boundaries = previous.map_or_else(
            || Index::new(&snapshot.source),
            |p| {
                snapshot.change_from(&p.snapshot).map_or_else(
                    || Index::new(snapshot.source()),
                    |edit| {
                        p.source_boundaries
                            .update_edit(p.snapshot.text(), snapshot.text(), &edit)
                    },
                )
            },
        );
        for range in [active.selection.as_ref(), active.composition.as_ref()]
            .into_iter()
            .flatten()
        {
            if !valid_range(range, &source_boundaries) {
                return Err(FallbackReason::InvalidBoundary);
            }
        }
        let mut concealed = Vec::new();
        let mut previous_end = 0;
        for region in &plan.regions {
            let block = region.block();
            if !valid_range(block, &source_boundaries) || block.is_empty() {
                return Err(FallbackReason::InvalidBoundary);
            }
            if block.start < previous_end {
                return Err(FallbackReason::OverlappingRanges);
            }
            previous_end = block.end;
            let reveal = [active.selection.as_ref(), active.composition.as_ref()]
                .into_iter()
                .flatten()
                .any(|r| touches(r, block));
            if let Region::Conceal { markers, .. } = region {
                let mut marker_end = block.start;
                for marker in markers {
                    if !valid_range(marker, &source_boundaries)
                        || marker.is_empty()
                        || marker.start < block.start
                        || marker.end > block.end
                    {
                        return Err(FallbackReason::InvalidBoundary);
                    }
                    if marker.start < marker_end {
                        return Err(FallbackReason::OverlappingRanges);
                    }
                    marker_end = marker.end;
                    work.charge(marker.len())?;
                    if snapshot
                        .source
                        .slice(marker.clone())
                        .contains(['\r', '\n', '\u{feff}'])
                    {
                        return Err(FallbackReason::ProtectedSource);
                    }
                    if !reveal {
                        concealed.push(marker.clone());
                    }
                }
            }
        }
        let mut anchors: Vec<Anchor> = Vec::with_capacity(concealed.len());
        let mut removed = 0;
        for range in &concealed {
            if let Some(anchor) = anchors.last_mut().filter(|a| a.source.end == range.start) {
                anchor.source.end = range.end;
            } else {
                anchors.push(Anchor {
                    source: range.clone(),
                    display: range.start - removed,
                });
            }
            removed += range.len();
        }
        let (display, display_boundaries) =
            match previous.and_then(|p| snapshot.change_from(&p.snapshot).map(|edit| (p, edit))) {
                Some((p, edit)) => {
                    // Widen only around the canonical edit and changed concealment.
                    // The unchanged visible prefix/suffix retain their Rope nodes.
                    let mut start = edit.old.start;
                    let mut end = edit.old.end;
                    let maps = |r: &Range<usize>| -> Option<Range<usize>> {
                        if r.end <= edit.old.start {
                            Some(r.clone())
                        } else if r.start >= edit.old.end {
                            Some(edit.forward(r.start)..edit.forward(r.end))
                        } else {
                            None
                        }
                    };
                    for anchor in &p.anchors {
                        if !maps(&anchor.source)
                            .is_some_and(|mapped| anchors.iter().any(|a| a.source == mapped))
                        {
                            start = start.min(anchor.source.start);
                            end = end.max(anchor.source.end);
                        }
                    }
                    for anchor in &anchors {
                        if !p
                            .anchors
                            .iter()
                            .any(|a| maps(&a.source).as_ref() == Some(&anchor.source))
                        {
                            start = start.min(edit.backward(anchor.source.start));
                            end = end.max(edit.backward(anchor.source.end));
                        }
                    }
                    // Include anchors crossing either seam, so a replacement never
                    // cuts through a concealed range or joins a partial marker.
                    for a in &p.anchors {
                        if a.source.start <= start && start < a.source.end {
                            start = a.source.start;
                        }
                        if a.source.start < end && end <= a.source.end {
                            end = a.source.end;
                        }
                    }
                    let new_start = start;
                    let new_end = end - edit.old.end + edit.new_end();
                    let map = |at: usize, inventory: &[Anchor]| {
                        let ix = inventory.partition_point(|a| a.source.end <= at);
                        if let Some(a) = inventory.get(ix).filter(|a| a.source.start <= at) {
                            a.display
                        } else {
                            at - ix
                                .checked_sub(1)
                                .map_or(0, |i| inventory[i].source.end - inventory[i].display)
                        }
                    };
                    let old_display = map(start, &p.anchors)..map(end, &p.anchors);
                    let mut fragment = String::new();
                    let mut cursor = new_start;
                    for a in anchors
                        .iter()
                        .filter(|a| new_start <= a.source.start && a.source.end <= new_end)
                    {
                        fragment.push_str(&snapshot.source.slice(cursor..a.source.start));
                        cursor = a.source.end;
                    }
                    fragment.push_str(&snapshot.source.slice(cursor..new_end));
                    let display = if old_display.is_empty() && fragment.is_empty() {
                        p.display.clone()
                    } else {
                        p.display.replace(old_display.clone(), &fragment)
                    };
                    let display_edit = Edit {
                        previous: p.display.id(),
                        old: old_display,
                        inserted: fragment.len(),
                    };
                    let index =
                        p.display_boundaries
                            .update_edit(&p.display, &display, &display_edit);
                    (display, index)
                }
                None => {
                    let mut rope = snapshot.source.rope().clone();
                    for range in concealed.iter().rev() {
                        rope.remove(range.clone());
                    }
                    let display = Text::from_rope(rope);
                    let index = Index::new(&display);
                    (display, index)
                }
            };
        // Concealment must not join visible graphemes across the removed bytes
        // (for example two regional indicators becoming a single flag).
        if anchors
            .iter()
            .any(|a| display_boundaries.binary_search(&a.display).is_err())
        {
            return Err(FallbackReason::JoinedGrapheme);
        }
        Ok(Projection {
            snapshot: snapshot.clone(),
            display,
            anchors,
            source_boundaries,
            display_boundaries,
        })
    };
    build().map_err(|reason| SourceFallback {
        reason,
        snapshot: snapshot.clone(),
    })
}

#[cfg(test)]
mod tests;
