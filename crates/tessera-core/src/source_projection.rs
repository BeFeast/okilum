//! Bounded canonical-source mapping foundation, not a Markdown parser or editor.
//!
//! Callers classify blocks and supply approved conceal spans. A rejected plan
//! returns the current exact Source, never an approximate or stale projection.
use std::{ops::Range, sync::Arc};
use unicode_segmentation::UnicodeSegmentation;

pub const MAX_BYTES: usize = 256 * 1024;
pub const MAX_RANGES: usize = 4096;
pub const MAX_WORK: usize = 2 * 1024 * 1024;
const MAX_DOCUMENT_ID_BYTES: usize = 4096;

/// Immutable identity and authored bytes. A generation alone is never a lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    document: Arc<str>,
    generation: u64,
    source: Arc<str>,
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
            source: source.into(),
        }
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn document(&self) -> &str {
        &self.document
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    fn matches(&self, other: &Self) -> bool {
        self.generation == other.generation
            && self.document == other.document
            && (Arc::ptr_eq(&self.source, &other.source) || self.source == other.source)
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
        if !valid_range(range, &boundaries(&self.source)) {
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
    fn block(&self) -> &Range<usize> {
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
    pub fn new(snapshot: &Snapshot, regions: Vec<Region>) -> Self {
        Self {
            snapshot: snapshot.clone(),
            regions,
        }
    }
}

/// Both ranges use canonical source coordinates, including zero-width carets.
/// A caret on a shared block boundary reveals both adjacent blocks.
#[derive(Clone, Debug, Default)]
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
    display: String,
    anchors: Vec<Anchor>,
    source_boundaries: Vec<usize>,
    display_boundaries: Vec<usize>,
}
impl Projection {
    pub fn display(&self) -> &str {
        &self.display
    }
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    fn check(&self, current: &Snapshot) -> Result<(), MapError> {
        if !self.snapshot.matches(current) {
            return Err(MapError::StaleSnapshot);
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

fn boundaries(text: &str) -> Vec<usize> {
    text.grapheme_indices(true)
        .map(|(offset, _)| offset)
        .chain(std::iter::once(text.len()))
        .collect()
}
fn valid_range(range: &Range<usize>, boundaries: &[usize]) -> bool {
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
        let source_boundaries = boundaries(&snapshot.source);
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
                    if snapshot.source[marker.clone()].contains(['\r', '\n', '\u{feff}']) {
                        return Err(FallbackReason::ProtectedSource);
                    }
                    if !reveal {
                        concealed.push(marker.clone());
                    }
                }
            }
        }
        work.charge(snapshot.source.len() * 2)?;
        let mut display = String::with_capacity(snapshot.source.len());
        let mut anchors: Vec<Anchor> = Vec::with_capacity(concealed.len());
        let mut cursor = 0;
        for range in concealed {
            display.push_str(&snapshot.source[cursor..range.start]);
            if let Some(anchor) = anchors.last_mut().filter(|a| a.source.end == range.start) {
                anchor.source.end = range.end;
            } else {
                anchors.push(Anchor {
                    source: range.clone(),
                    display: display.len(),
                });
            }
            cursor = range.end;
        }
        display.push_str(&snapshot.source[cursor..]);
        let display_boundaries = boundaries(&display);
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
