//! Bounded immutable fragment derivation, deliberately not wired to the Reader.
//!
//! The caller supplies a structural fragment and its complete semantic context.
//! Cache values have LOCAL coordinates. Each returned lease checks a new exact
//! parent snapshot and rebases source coordinates; no old row/hit-map lease escapes.
//! Parent range validation/copy and key hashing still do bounded work per request;
//! reuse avoids local parsing/projection/index construction, not all byte scans.
use super::{
    classify_fragment, decorations::Marker, Classification, NoteLink, Reference,
    RetainedPresentation, RevealSnapshot, SourceReason, StyleSpan, MAX_BYTES,
};
use crate::source_projection::{Active, Bias, FallbackReason, MapError, Snapshot};
use std::{
    collections::{hash_map::RandomState, HashMap},
    hash::BuildHasher,
    ops::Range,
    sync::Arc,
};

#[derive(Clone)]
pub(crate) struct Context {
    authority: Snapshot,
    references: Arc<[Reference]>,
    allow_frontmatter: bool,
    /// Canonical UTF-8 offsets relative to the fragment, already converted by
    /// the native adapter. Replacement ranges must be validated before union.
    active: Active,
}
impl Context {
    /// Only canonical current-parent classification can authorize reference
    /// context in this first slice. Structural discovery supplies a future API.
    pub fn from_classification(
        classified: &Classification,
        allow_frontmatter: bool,
        active: Active,
    ) -> Result<Self, Error> {
        if let Some(reason) = classified
            .reasons()
            .iter()
            .find(|r| !matches!(r, SourceReason::UnsupportedOrAmbiguous))
        {
            return Err(Error::Context(*reason));
        }
        Ok(Self {
            authority: classified.snapshot().clone(),
            references: classified.references.clone().into(),
            allow_frontmatter,
            active,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
// Request authority is checked before lookup and is never retained in the
// cache key. An entry owns only bounded local bytes and semantic inputs.
struct Key {
    text: Arc<str>,
    references: Arc<[Reference]>,
    allow_frontmatter: bool,
    active: Active,
}

struct Derived {
    classification: Classification,
    reveal: RevealSnapshot,
}

struct Entry {
    derived: Arc<Derived>,
    bytes: usize,
    touched: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Stats {
    pub hits: usize,
    pub misses: usize,
    pub evictions: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    Source(MapError),
    Projection(FallbackReason),
    Context(SourceReason),
}
impl From<MapError> for Error {
    fn from(error: MapError) -> Self {
        Self::Source(error)
    }
}

/// A new authority over immutable LOCAL data, never a cached parent mapping.
/// Display offsets are local; source offsets/ranges are in the checked parent.
#[derive(Clone)]
pub(crate) struct Lease {
    parent: Snapshot,
    range: Range<usize>,
    derived: Arc<Derived>,
}
impl Lease {
    fn check(&self, current: &Snapshot) -> Result<(), MapError> {
        if &self.parent != current {
            return Err(MapError::StaleSnapshot);
        }
        Ok(())
    }
    fn local(&self, offset: usize) -> Result<usize, MapError> {
        if !self.range.contains(&offset) && offset != self.range.end {
            return Err(MapError::InvalidBoundary);
        }
        Ok(offset - self.range.start)
    }
    fn shifted(&self, range: &Range<usize>) -> Range<usize> {
        // Every relative range was validated against exactly these fragment
        // bytes; the parent range was validated before this lease was issued.
        range.start + self.range.start..range.end + self.range.start
    }
    pub fn source_range(&self, current: &Snapshot) -> Result<Range<usize>, MapError> {
        self.check(current)?;
        Ok(self.range.clone())
    }
    pub fn source<'a>(&self, current: &'a Snapshot) -> Result<&'a str, MapError> {
        self.check(current)?;
        Ok(&current.source()[self.range.clone()])
    }
    pub fn display(&self, current: &Snapshot) -> Result<&str, MapError> {
        self.check(current)?;
        Ok(self.derived.reveal.projection().display())
    }
    pub fn source_to_local_display(
        &self,
        current: &Snapshot,
        offset: usize,
    ) -> Result<usize, MapError> {
        self.check(current)?;
        let projection = self.derived.reveal.projection();
        projection.source_to_display(projection.snapshot(), self.local(offset)?)
    }
    pub fn local_display_to_source(
        &self,
        current: &Snapshot,
        offset: usize,
        bias: Bias,
    ) -> Result<usize, MapError> {
        self.check(current)?;
        let projection = self.derived.reveal.projection();
        Ok(self.range.start + projection.display_to_source(projection.snapshot(), offset, bias)?)
    }
    pub fn styles(&self, current: &Snapshot) -> Result<Vec<StyleSpan>, MapError> {
        self.check(current)?;
        Ok(self
            .derived
            .classification
            .styles
            .iter()
            .map(|s| StyleSpan {
                range: self.shifted(&s.range),
                style: s.style,
            })
            .collect())
    }
    pub fn markers(&self, current: &Snapshot) -> Result<Vec<Marker>, MapError> {
        self.check(current)?;
        Ok(self
            .derived
            .classification
            .decorations
            .iter()
            .map(|m| Marker {
                range: self.shifted(&m.range),
                scope: self.shifted(&m.scope),
                kind: m.kind.clone(),
            })
            .collect())
    }
    pub fn links(&self, current: &Snapshot) -> Result<Vec<NoteLink>, MapError> {
        self.check(current)?;
        Ok(self
            .derived
            .classification
            .links
            .iter()
            .map(|link| NoteLink {
                range: self.shifted(&link.range),
                label: self.shifted(&link.label),
                target: link.target.clone(),
                wiki: link.wiki,
            })
            .collect())
    }
    pub fn scope_is_raw(&self, current: &Snapshot, scope: Range<usize>) -> Result<bool, MapError> {
        self.check(current)?;
        if scope.start > scope.end {
            return Err(MapError::InvalidBoundary);
        }
        let scope = self.local(scope.start)?..self.local(scope.end)?;
        let reveal = &self.derived.reveal;
        Ok(reveal.is_raw(reveal.projection().snapshot(), &scope))
    }
    pub fn reasons(&self, current: &Snapshot) -> Result<&[SourceReason], MapError> {
        self.check(current)?;
        Ok(self.derived.classification.reasons())
    }
}

/// Entry count bounds metadata through the unchanged per-fragment caps. The
/// byte budget counts authored/reference/display text, not process RSS or leases
/// retained by callers. Reader policy, structural discovery and global joins
/// are deliberately outside this cache. No large-document admission is added.
pub(crate) struct FragmentCache<S = RandomState> {
    entries: HashMap<Key, Entry, S>,
    max_entries: usize,
    max_bytes: usize,
    bytes: usize,
    clock: u64,
    stats: Stats,
}
impl FragmentCache {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self::with_hasher(max_entries, max_bytes, RandomState::new())
    }
}
impl<S: BuildHasher> FragmentCache<S> {
    fn with_hasher(max_entries: usize, max_bytes: usize, hash: S) -> Self {
        Self {
            entries: HashMap::with_hasher(hash),
            max_entries,
            max_bytes,
            bytes: 0,
            clock: 0,
            stats: Stats::default(),
        }
    }
    pub fn stats(&self) -> Stats {
        self.stats
    }
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    pub fn get_or_build(
        &mut self,
        parent: &Snapshot,
        range: Range<usize>,
        context: &Context,
    ) -> Result<Lease, Error> {
        // First slice is only in-cap parent/fragment work. Do not silently turn
        // this API into a path around ordinary Reader admission.
        if parent.source().len() > MAX_BYTES {
            return Err(Error::Source(MapError::SourceLimit));
        }
        parent.check_identity().map_err(Error::Projection)?;
        if context.authority != *parent {
            return Err(Error::Source(MapError::StaleSnapshot));
        }
        if context.references.len() > crate::source_projection::MAX_RANGES {
            return Err(Error::Projection(FallbackReason::RangeLimit));
        }
        let mut context_bytes = parent.source().len() + parent.document().len();
        for reference in context.references.iter() {
            context_bytes = context_bytes
                .saturating_add(reference.label.len())
                .saturating_add(reference.url.len())
                .saturating_add(reference.title.len());
            if context_bytes > crate::source_projection::MAX_WORK {
                return Err(Error::Projection(FallbackReason::WorkLimit));
            }
        }
        let text: Arc<str> = parent.copy_source(range.clone())?.into();
        let key = Key {
            text,
            references: context.references.clone(),
            allow_frontmatter: context.allow_frontmatter && range.start == 0,
            active: context.active.clone(),
        };
        self.clock = self.clock.saturating_add(1);
        if let Some(entry) = self.entries.get_mut(&key) {
            self.stats.hits = self.stats.hits.saturating_add(1);
            entry.touched = self.clock;
            return Ok(Lease {
                parent: parent.clone(),
                range,
                derived: entry.derived.clone(),
            });
        }
        self.stats.misses = self.stats.misses.saturating_add(1);
        let local = Snapshot::new(parent.document(), parent.generation(), key.text.clone());
        let classification = classify_fragment(&local, &key.references, key.allow_frontmatter);
        let reveal = RetainedPresentation::new(&classification)
            .prepare_reveal(&key.active)
            .map_err(|fallback| Error::Projection(fallback.reason))?;
        let derived = Arc::new(Derived {
            classification,
            reveal,
        });
        let bytes = key
            .references
            .iter()
            .fold(key.text.len(), |sum, r| {
                sum.saturating_add(r.label.len())
                    .saturating_add(r.url.len())
                    .saturating_add(r.title.len())
            })
            .saturating_add(parent.document().len())
            .saturating_add(derived.reveal.projection().display().len());
        if self.max_entries > 0 && bytes <= self.max_bytes {
            while self.entries.len() >= self.max_entries
                || bytes > self.max_bytes.saturating_sub(self.bytes)
            {
                let oldest = self
                    .entries
                    .iter()
                    .min_by_key(|(_, e)| e.touched)
                    .map(|(k, _)| k.clone());
                let Some(oldest) = oldest else { break };
                let removed = self.entries.remove(&oldest).expect("present cache entry");
                self.bytes -= removed.bytes;
                self.stats.evictions = self.stats.evictions.saturating_add(1);
            }
            self.bytes += bytes;
            self.entries.insert(
                key,
                Entry {
                    derived: derived.clone(),
                    bytes,
                    touched: self.clock,
                },
            );
        }
        Ok(Lease {
            parent: parent.clone(),
            range,
            derived,
        })
    }
}

#[cfg(test)]
mod tests;
