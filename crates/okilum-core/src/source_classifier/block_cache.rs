//! Cache independently classified blocks, not whole-document parse boundaries.
//!
//! The segmented caller must establish top-level block boundaries and supply
//! document-wide reference context. This cache does not discover boundaries or
//! authorize applying a fragment's coordinates to its parent document. Repeated
//! content is reusable, but returned metadata always binds to the requested
//! fragment snapshot, including its document and generation.
use super::{Classification, Reference, MAX_BYTES};
use crate::source_projection::{Plan, Snapshot};
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::Arc,
};

#[derive(Clone)]
struct Key {
    snapshot: Snapshot,
    references: Arc<[Reference]>,
}

// Identity is deliberately not the parse key. HashMap still checks exact bytes
// and the complete reference context on a hash collision; hashes are not leases.
impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.snapshot.source() == other.snapshot.source() && self.references == other.references
    }
}
impl Eq for Key {}
impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.snapshot.source().hash(state);
        self.references.hash(state);
    }
}

struct Entry {
    classification: Classification,
    touched: u64,
    bytes: usize,
}

/// Counts distinguish an actual reuse from a test that merely produces output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub hits: usize,
    pub misses: usize,
    pub evictions: usize,
}

/// A bounded, in-memory block cache. Both limits are supplied by its owner.
///
/// `max_source_bytes` accounts for retained source and reference-context text,
/// not process RSS. Metadata is additionally bounded by `max_entries` and the
/// existing per-classification node/style/range limits. A block that cannot fit
/// is still classified conservatively; it simply is not retained in the cache.
pub struct BlockCache {
    entries: HashMap<Key, Entry>,
    max_entries: usize,
    max_source_bytes: usize,
    source_bytes: usize,
    clock: u64,
    stats: Stats,
}

impl BlockCache {
    pub fn new(max_entries: usize, max_source_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            max_entries,
            max_source_bytes,
            source_bytes: 0,
            clock: 0,
            stats: Stats::default(),
        }
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// The segmented owner clears context after a nonlocal structural change.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.source_bytes = 0;
    }

    pub fn classify(&mut self, snapshot: &Snapshot) -> Classification {
        self.classify_with(snapshot, &[])
    }

    pub(crate) fn classify_with(
        &mut self,
        snapshot: &Snapshot,
        references: &[Reference],
    ) -> Classification {
        self.clock = self.clock.saturating_add(1);
        let key = Key {
            snapshot: snapshot.clone(),
            references: references.to_vec().into(),
        };
        if let Some(entry) = self.entries.get_mut(&key) {
            self.stats.hits += 1;
            entry.touched = self.clock;
            let mut result = entry.classification.clone();
            // Neither a previous document identity nor its plan may escape a
            // content-cache hit. All fragment coordinates remain unchanged.
            result.snapshot = snapshot.clone();
            result.plan = Plan::new(snapshot, result.plan.regions().to_vec());
            return result;
        }
        self.stats.misses += 1;
        let result = super::classify_with(snapshot, references);
        let bytes = references.iter().fold(snapshot.source().len(), |sum, r| {
            sum.saturating_add(r.label.len())
                .saturating_add(r.url.len())
                .saturating_add(r.title.len())
        });
        if self.max_entries == 0
            || bytes > self.max_source_bytes
            || snapshot.source().len() > MAX_BYTES
        {
            return result;
        }
        while self.entries.len() >= self.max_entries
            || bytes > self.max_source_bytes - self.source_bytes
        {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else { break };
            let removed = self.entries.remove(&oldest).expect("entry was present");
            self.source_bytes -= removed.bytes;
            self.stats.evictions += 1;
        }
        self.source_bytes += bytes;
        self.entries.insert(
            key,
            Entry {
                classification: result.clone(),
                touched: self.clock,
                bytes,
            },
        );
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_projection::{project, Active, MapError};

    #[test]
    fn cache_hit_rebinds_both_metadata_and_plan_to_requested_identity() {
        let mut cache = BlockCache::new(8, 4096);
        let old = Snapshot::new("first.md", 1, "# **same**\r\n");
        let current = Snapshot::new("other.md", 9, old.source());
        cache.classify(&old);
        let hit = cache.classify(&current);
        assert_eq!(
            cache.stats(),
            Stats {
                hits: 1,
                misses: 1,
                evictions: 0
            }
        );
        assert_eq!(hit.snapshot(), &current);
        assert_eq!(hit.styles_for(&old), Err(MapError::StaleSnapshot));
        assert_eq!(
            project(&current, hit.plan(), &Active::default())
                .unwrap()
                .display(),
            "same\r\n"
        );
        assert!(project(&old, hit.plan(), &Active::default()).is_err());
    }

    #[test]
    fn reference_context_is_part_of_the_exact_parse_key() {
        let mut cache = BlockCache::new(8, 4096);
        let source = Snapshot::new("note.md", 1, "[label][id]");
        let first = [Reference {
            label: "id".into(),
            url: "first.md".into(),
            title: String::new(),
        }];
        let second = [Reference {
            label: "id".into(),
            url: "second.md".into(),
            title: String::new(),
        }];
        let a = cache.classify_with(&source, &first);
        let b = cache.classify_with(&source, &second);
        assert_eq!(a.links_for(&source).unwrap()[0].target, "first.md");
        assert_eq!(b.links_for(&source).unwrap()[0].target, "second.md");
        let hit = cache.classify_with(&source, &first);
        assert_eq!(hit.links_for(&source).unwrap()[0].target, "first.md");
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(cache.stats().misses, 2);
    }

    #[test]
    fn utf8_bom_and_line_endings_are_not_normalized_in_the_key() {
        let mut cache = BlockCache::new(8, 4096);
        for text in ["# é\n", "# é\r\n", "\u{feff}# é\n", "# e\u{301}\n"] {
            let s = Snapshot::new("unicode.md", 1, text);
            let cold = super::super::classify(&s);
            let cached = cache.classify(&s);
            assert_eq!(cached.snapshot().source().as_bytes(), text.as_bytes());
            assert_eq!(cached.styles_for(&s).unwrap(), cold.styles_for(&s).unwrap());
        }
        assert_eq!(cache.stats().misses, 4);
        assert_eq!(cache.stats().hits, 0);
    }

    #[test]
    fn eviction_does_not_change_classification_or_keep_the_wrong_revision() {
        let mut cache = BlockCache::new(2, 1024);
        let a = Snapshot::new("note.md", 1, "**a**");
        let b = Snapshot::new("note.md", 2, "**b**");
        let c = Snapshot::new("note.md", 3, "**c**");
        cache.classify(&a);
        cache.classify(&b);
        cache.classify(&a); // a is newest, so adding c evicts b.
        cache.classify(&c);
        assert_eq!(cache.stats().evictions, 1);
        cache.classify(&a);
        assert_eq!(cache.stats().hits, 2);
        let reloaded = cache.classify(&b);
        assert_eq!(reloaded.snapshot(), &b);
        assert_eq!(cache.stats().misses, 4);
        assert_eq!(
            project(&b, reloaded.plan(), &Active::default())
                .unwrap()
                .display(),
            "b"
        );
    }

    #[test]
    fn byte_budget_and_explicit_clear_bound_retention() {
        let mut cache = BlockCache::new(8, 5);
        for text in ["**a**", "**b**"] {
            cache.classify(&Snapshot::new("note.md", 1, text));
        }
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.source_bytes, 5);
        cache.clear();
        assert!(cache.entries.is_empty());
        assert_eq!(cache.source_bytes, 0);
        let mut disabled = BlockCache::new(0, 1024);
        let s = Snapshot::new("note.md", 1, "**a**");
        disabled.classify(&s);
        disabled.classify(&s);
        assert_eq!(disabled.stats().hits, 0);
        assert!(disabled.entries.is_empty());
    }

    #[test]
    fn oversized_block_is_not_relabelled_as_a_complete_classification() {
        let mut cache = BlockCache::new(8, 2 * MAX_BYTES);
        let s = Snapshot::new("large.md", 1, "x".repeat(MAX_BYTES + 1));
        let result = cache.classify(&s);
        assert!(result
            .reasons()
            .contains(&super::super::SourceReason::InputLimit));
        assert!(cache.entries.is_empty());
    }
}
