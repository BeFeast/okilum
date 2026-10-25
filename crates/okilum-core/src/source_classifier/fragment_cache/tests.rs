#![allow(clippy::single_range_in_vec_init)]
use super::*;
use crate::source_projection::{project, TEST_INDEX_BUILDS};
use std::hash::{BuildHasherDefault, Hasher};
use unicode_segmentation::UnicodeSegmentation;

fn work() -> (usize, usize) {
    (
        super::super::TEST_PARSE_CALLS.with(|v| v.get()),
        TEST_INDEX_BUILDS.with(|v| v.get()),
    )
}
fn parent(document: &str, generation: u64, prefix: &str, text: &str) -> (Snapshot, Range<usize>) {
    (
        Snapshot::new(document, generation, format!("{prefix}{text}")),
        prefix.len()..prefix.len() + text.len(),
    )
}
fn context(source: &Snapshot) -> Context {
    Context::from_classification(&super::super::classify(source), false, Active::default()).unwrap()
}
fn fresh(
    source: &Snapshot,
    range: Range<usize>,
    context: &Context,
) -> (Classification, RevealSnapshot) {
    let local = Snapshot::new(
        source.document(),
        source.generation(),
        source.copy_source(range.clone()).unwrap(),
    );
    let c = classify_fragment(
        &local,
        &context.references,
        context.allow_frontmatter && range.start == 0,
    );
    let reveal = RetainedPresentation::new(&c)
        .prepare_reveal(&context.active)
        .unwrap();
    (c, reveal)
}
fn equivalent(lease: &Lease, source: &Snapshot, range: Range<usize>, context: &Context) {
    let (c, reveal) = fresh(source, range.clone(), context);
    let p = reveal.projection();
    assert_eq!(lease.source_range(source).unwrap(), range);
    assert_eq!(
        lease.source(source).unwrap(),
        &source.source()[range.clone()]
    );
    assert_eq!(lease.display(source).unwrap(), p.display());
    let shifted = |r: &Range<usize>| r.start + range.start..r.end + range.start;
    assert_eq!(
        lease.styles(source).unwrap(),
        c.styles
            .iter()
            .map(|s| StyleSpan {
                range: shifted(&s.range),
                style: s.style
            })
            .collect::<Vec<_>>()
    );
    assert_eq!(
        lease.markers(source).unwrap(),
        c.decorations
            .iter()
            .map(|m| Marker {
                range: shifted(&m.range),
                scope: shifted(&m.scope),
                kind: m.kind.clone()
            })
            .collect::<Vec<_>>()
    );
    assert_eq!(
        lease.links(source).unwrap(),
        c.links
            .iter()
            .map(|l| NoteLink {
                range: shifted(&l.range),
                label: shifted(&l.label),
                target: l.target.clone(),
                wiki: l.wiki
            })
            .collect::<Vec<_>>()
    );
    assert_eq!(lease.reasons(source).unwrap(), c.reasons());
    // Includes every invalid UTF-8/cluster interior and one out-of-range byte.
    for offset in 0..=p.snapshot().source().len() + 1 {
        assert_eq!(
            lease.source_to_local_display(source, range.start + offset),
            p.source_to_display(p.snapshot(), offset)
        );
    }
    for offset in 0..=p.display().len() + 1 {
        for bias in [Bias::Left, Bias::Right] {
            assert_eq!(
                lease.local_display_to_source(source, offset, bias),
                p.display_to_source(p.snapshot(), offset, bias)
                    .map(|i| i + range.start)
            );
        }
    }
    for marker in &c.decorations {
        assert_eq!(
            lease.scope_is_raw(source, shifted(&marker.scope)).unwrap(),
            reveal.is_raw(p.snapshot(), &marker.scope)
        );
    }
}

#[test]
fn real_cold_work_then_hit_rebinds_new_parent_offsets_and_keeps_old_leases() {
    let mut cache = FragmentCache::new(8, 8192);
    let body = "**e\u{301}** 🧠\r\n- [ ] task\n> quote\n";
    let prefix = "prefix\n\n";
    let gap = "\nseparator\n\n";
    let bytes = format!("{prefix}{body}{gap}{body}");
    let first_range = prefix.len()..prefix.len() + body.len();
    let second_range = prefix.len() + body.len() + gap.len()..bytes.len();
    let old = Snapshot::new("first.md", 7, bytes.clone());
    let range = first_range.clone();
    let context = context(&old);
    let before = work();
    let held = cache.get_or_build(&old, range.clone(), &context).unwrap();
    assert!(
        work().0 > before.0 && work().1 > before.1,
        "cold parser/index positive control"
    );
    assert_ne!(held.display(&old).unwrap(), body);
    equivalent(&held, &old, range, &context);
    for (id, generation, range) in [("first.md", 8, second_range), ("second.md", 9, first_range)] {
        let current = Snapshot::new(id, generation, bytes.clone());
        assert_eq!(
            cache.get_or_build(&current, range.clone(), &context).err(),
            Some(Error::Source(MapError::StaleSnapshot))
        );
        let context = Context::from_classification(
            &super::super::classify(&current),
            false,
            Active::default(),
        )
        .unwrap();
        let before = work();
        let rebound = cache
            .get_or_build(&current, range.clone(), &context)
            .unwrap();
        assert_eq!(
            work(),
            before,
            "a hit must reuse real parser/projection/index work"
        );
        assert_eq!(held.display(&current), Err(MapError::StaleSnapshot));
        assert_eq!(rebound.styles(&old), Err(MapError::StaleSnapshot));
        assert_eq!(
            held.local_display_to_source(&current, 0, Bias::Left),
            Err(MapError::StaleSnapshot)
        );
        equivalent(&rebound, &current, range, &context);
        assert_eq!(
            held.display(&old).unwrap(),
            "e\u{301} 🧠\r\n- [ ] task\n> quote\n"
        );
    }
    assert_eq!(
        cache.stats(),
        Stats {
            hits: 2,
            misses: 1,
            evictions: 0
        }
    );
    // Same identity is insufficient when bytes outside the fragment changed.
    let foreign = Snapshot::new(
        old.document(),
        old.generation(),
        format!("changed\n\n{body}"),
    );
    assert_eq!(held.source_range(&foreign), Err(MapError::StaleSnapshot));
    assert_eq!(held.markers(&foreign), Err(MapError::StaleSnapshot));
    // A warm value must not authorize an invalid document id.
    for id in [String::new(), "x".repeat(4097)] {
        let (bad, r) = parent(&id, 7, "", body);
        assert_eq!(
            cache.get_or_build(&bad, r, &context).err(),
            Some(Error::Projection(FallbackReason::IdentityLimit))
        );
    }
}

#[derive(Default)]
struct Collision;
impl Hasher for Collision {
    fn finish(&self) -> u64 {
        0
    }
    fn write(&mut self, _: &[u8]) {}
}

#[test]
fn forced_hash_collision_still_checks_bytes_and_complete_semantic_context() {
    let mut cache =
        FragmentCache::with_hasher(32, 32768, BuildHasherDefault::<Collision>::default());
    let (source, range) = parent("refs.md", 1, "", "[label][id]\n\n**bold**\n");
    let reference = Reference {
        label: "id".into(),
        url: "a.md".into(),
        title: "A".into(),
    };
    let a = Context {
        references: vec![reference.clone()].into(),
        ..context(&source)
    };
    let mut contexts = vec![a.clone()];
    for reference in [
        Reference {
            url: "b.md".into(),
            ..reference.clone()
        },
        Reference {
            title: "different title".into(),
            ..reference.clone()
        },
        Reference {
            label: "other".into(),
            ..reference.clone()
        },
    ] {
        contexts.push(Context {
            references: vec![reference].into(),
            ..a.clone()
        });
    }
    let other = Reference {
        url: "other.md".into(),
        ..reference.clone()
    };
    for refs in [
        vec![reference.clone(), other.clone()],
        vec![other, reference],
    ] {
        contexts.push(Context {
            references: refs.into(),
            ..a.clone()
        });
    }
    let at = source.source().find("bold").unwrap();
    contexts.push(Context {
        active: Active {
            selection: Some(at..at),
            composition: None,
        },
        ..a.clone()
    });
    contexts.push(Context {
        active: Active {
            selection: None,
            composition: Some(at..at + 1),
        },
        ..a.clone()
    });
    contexts.push(Context {
        allow_frontmatter: true,
        ..a.clone()
    });
    for context in &contexts {
        let misses = cache.stats().misses;
        let cold = cache.get_or_build(&source, range.clone(), context).unwrap();
        assert_eq!(
            cache.stats().misses,
            misses + 1,
            "semantic changes must miss even on hash collision"
        );
        equivalent(&cold, &source, range.clone(), context);
        let before = work();
        let hit = cache.get_or_build(&source, range.clone(), context).unwrap();
        assert_eq!(work(), before);
        equivalent(&hit, &source, range.clone(), context);
    }
    let changed = Snapshot::new("refs.md", 1, source.source().replace("bold", "edit"));
    let misses = cache.stats().misses;
    let a = Context {
        references: a.references.clone(),
        ..context(&changed)
    };
    let next = cache
        .get_or_build(&changed, 0..changed.source().len(), &a)
        .unwrap();
    assert_eq!(cache.stats().misses, misses + 1);
    equivalent(&next, &changed, 0..changed.source().len(), &a);
}

#[test]
fn frontmatter_policy_depends_on_document_position_and_is_not_cached_by_bytes_alone() {
    let mut cache = FragmentCache::new(4, 8192);
    let body = "---\n# visible only outside frontmatter\n---\n";
    let (first, r) = parent("a.md", 1, "", body);
    let context =
        Context::from_classification(&super::super::classify(&first), true, Active::default())
            .unwrap();
    let front = cache.get_or_build(&first, r.clone(), &context).unwrap();
    equivalent(&front, &first, r, &context);
    let (later, r) = parent("a.md", 2, "ordinary\n\n", body);
    let context =
        Context::from_classification(&super::super::classify(&later), true, Active::default())
            .unwrap();
    let rest = cache.get_or_build(&later, r.clone(), &context).unwrap();
    equivalent(&rest, &later, r, &context);
    assert_ne!(
        front.styles(&first).unwrap().len(),
        rest.styles(&later).unwrap().len()
    );
    assert_eq!(cache.stats().misses, 2);
}

#[test]
fn unicode_ranges_active_reveal_and_two_unpainted_revisions_match_fresh() {
    for cluster in ["e\u{301}", "👩‍👩‍👧‍👦", "🇮🇱", "🧠", "אבג", "中文"]
    {
        let body = format!("**{cluster}**\r\n");
        let (s, r) = parent("unicode.md", 1, "\u{feff}\r\n", &body);
        let mut cache = FragmentCache::new(16, 32768);
        let base = context(&s);
        let held = cache.get_or_build(&s, r.clone(), &base).unwrap();
        equivalent(&held, &s, r.clone(), &base);
        let mut active = base.clone();
        active.active.selection = Some(2..2);
        active.active.composition = Some(2..2 + cluster.len());
        let shown = cache.get_or_build(&s, r.clone(), &active).unwrap();
        assert_eq!(shown.display(&s).unwrap(), body);
        assert_ne!(held.display(&s).unwrap(), shown.display(&s).unwrap());
        equivalent(&shown, &s, r, &active);
        for (generation, text) in [(2, format!("x{body}")), (3, format!("xx{body}"))] {
            let (current, r) = parent("unicode.md", generation, "\u{feff}\r\n", &text);
            let current_context = context(&current);
            let value = cache
                .get_or_build(&current, r.clone(), &current_context)
                .unwrap();
            equivalent(&value, &current, r, &current_context);
            assert_eq!(held.display(&current), Err(MapError::StaleSnapshot));
        }
        let boundaries: Vec<_> = body
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain(std::iter::once(body.len()))
            .collect();
        for invalid in (0..body.len()).filter(|i| !boundaries.contains(i)) {
            let mut bad = base.clone();
            bad.active.selection = Some(invalid..invalid);
            assert_eq!(
                cache.get_or_build(&s, held.range.clone(), &bad).err(),
                Some(Error::Projection(FallbackReason::InvalidBoundary))
            );
        }
        let emoji = s.source().find(cluster).unwrap();
        for invalid in (1..cluster.len()).filter(|i| !cluster.is_char_boundary(*i)) {
            assert_eq!(
                cache
                    .get_or_build(&s, emoji + invalid..emoji + cluster.len(), &base)
                    .err(),
                Some(Error::Source(MapError::InvalidBoundary))
            );
        }
    }
}

#[test]
fn bounded_retention_eviction_clear_and_unsupported_inputs_keep_canonical_behavior() {
    let mut cache = FragmentCache::new(2, 8192);
    let mut held = None;
    for (generation, text) in [(1, "**a**"), (2, "**b**"), (3, "**c**")] {
        let s = Snapshot::new("a.md", generation, text);
        let c = context(&s);
        let value = cache.get_or_build(&s, 0..text.len(), &c).unwrap();
        equivalent(&value, &s, 0..text.len(), &c);
        if held.is_none() {
            held = Some((s, value));
        }
    }
    assert_eq!(cache.stats().evictions, 1);
    let (old, lease) = held.unwrap();
    let c = context(&old);
    let before = work();
    cache.get_or_build(&old, 0..old.source().len(), &c).unwrap();
    assert!(
        work().0 > before.0 && work().1 > before.1,
        "eviction forces real cold work"
    );
    assert_eq!(lease.display(&old).unwrap(), "a");
    cache.clear();
    let before = work();
    cache.get_or_build(&old, 0..old.source().len(), &c).unwrap();
    assert!(work().0 > before.0);
    for (entries, bytes) in [(0, 8192), (8, 0)] {
        let mut disabled = FragmentCache::new(entries, bytes);
        for _ in 0..2 {
            disabled
                .get_or_build(&old, 0..old.source().len(), &c)
                .unwrap();
        }
        assert_eq!(disabled.stats().hits, 0);
        assert_eq!(disabled.stats().misses, 2);
    }
    let oversized = Snapshot::new("big.md", 1, "x".repeat(MAX_BYTES + 1));
    assert_eq!(
        cache.get_or_build(&oversized, 0..1, &c).err(),
        Some(Error::Source(MapError::SourceLimit))
    );
    let bad = Snapshot::new("a.md", 1, "nul\0byte");
    assert_eq!(
        Context::from_classification(&super::super::classify(&bad), false, Active::default()).err(),
        Some(Error::Context(SourceReason::ParserCoordinates))
    );
    let unsupported = Snapshot::new("a.md", 1, "<div>raw HTML</div>\n");
    let raw = cache
        .get_or_build(
            &unsupported,
            0..unsupported.source().len(),
            &context(&unsupported),
        )
        .unwrap();
    assert_eq!(raw.display(&unsupported).unwrap(), unsupported.source());
    assert!(raw
        .reasons(&unsupported)
        .unwrap()
        .contains(&SourceReason::UnsupportedOrAmbiguous));
    let empty = Snapshot::new("empty.md", 1, "");
    let c = context(&empty);
    let value = cache.get_or_build(&empty, 0..0, &c).unwrap();
    equivalent(&value, &empty, 0..0, &c);
    // Existing deterministic work caps are untouched, even if a cache is warm.
    let local = Snapshot::new("small.md", 1, "**a**");
    let classified = super::super::classify(&local);
    assert_eq!(
        crate::source_projection::project_with_work_budget(
            &local,
            classified.plan(),
            &Active::default(),
            0
        )
        .err()
        .unwrap()
        .reason,
        FallbackReason::WorkLimit
    );
    assert_eq!(
        project(&local, classified.plan(), &Active::default())
            .unwrap()
            .display(),
        "a"
    );
}

#[test]
fn supplied_context_is_bounded_before_hashing_or_parser_work() {
    let mut cache = FragmentCache::new(2, 8192);
    let source = Snapshot::new("refs.md", 1, "[x][id]");
    let reference = Reference {
        label: "id".into(),
        url: "target.md".into(),
        title: String::new(),
    };
    for (references, reason) in [
        (
            vec![reference.clone(); crate::source_projection::MAX_RANGES + 1],
            FallbackReason::RangeLimit,
        ),
        (
            vec![Reference {
                url: "x".repeat(crate::source_projection::MAX_WORK + 1),
                ..reference
            }],
            FallbackReason::WorkLimit,
        ),
    ] {
        let context = Context {
            references: references.into(),
            ..context(&source)
        };
        let before = work();
        assert_eq!(
            cache
                .get_or_build(&source, 0..source.source().len(), &context)
                .err(),
            Some(Error::Projection(reason))
        );
        assert_eq!(
            work(),
            before,
            "unsupported context must not reach parser/projection"
        );
    }
}

#[test]
fn stale_reference_authority_is_rejected_before_a_new_revision_can_reuse_a_value() {
    let mut cache = FragmentCache::new(4, 8192);
    let body = "[label][id]\n\n";
    let old = Snapshot::new("a.md", 1, format!("{body}[id]: first.md \"first\"\n"));
    let current = Snapshot::new("a.md", 2, format!("{body}[id]: other.md \"other\"\n"));
    let old_context = context(&old);
    let held = cache
        .get_or_build(&old, 0..body.len(), &old_context)
        .unwrap();
    assert_eq!(held.links(&old).unwrap()[0].target, "first.md");
    let before = work();
    assert_eq!(
        cache
            .get_or_build(&current, 0..body.len(), &old_context)
            .err(),
        Some(Error::Source(MapError::StaleSnapshot))
    );
    assert_eq!(work(), before, "stale authority must not parse or project");
    let current_context = context(&current);
    let value = cache
        .get_or_build(&current, 0..body.len(), &current_context)
        .unwrap();
    assert_eq!(value.links(&current).unwrap()[0].target, "other.md");
    equivalent(&value, &current, 0..body.len(), &current_context);
    assert_eq!(held.links(&current), Err(MapError::StaleSnapshot));
    assert_eq!(held.links(&old).unwrap()[0].target, "first.md");
    assert_eq!(cache.stats().misses, 2);
}

#[test]
fn unreferenced_definition_change_cannot_hide_behind_identical_used_references() {
    let mut cache = FragmentCache::new(4, 8192);
    let body = "**bold**\n\n";
    let first = Snapshot::new("a.md", 1, format!("{body}[unused]: first.md\n"));
    let second = Snapshot::new("a.md", 2, format!("{body}[unused]: other.md\n"));
    let old = context(&first);
    let new = context(&second);
    assert_eq!(
        old.references, new.references,
        "used-reference subset is not full authority"
    );
    let held = cache.get_or_build(&first, 0..body.len(), &old).unwrap();
    let before = work();
    let current = cache.get_or_build(&second, 0..body.len(), &new).unwrap();
    assert!(
        work().0 > before.0 && work().1 > before.1,
        "uncertain reference context must recompute"
    );
    assert_eq!(cache.stats().hits, 0);
    assert_eq!(cache.stats().misses, 2);
    equivalent(&current, &second, 0..body.len(), &new);
    assert_eq!(held.display(&second), Err(MapError::StaleSnapshot));
}
