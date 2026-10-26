use super::*;
use crate::source_projection::Active;
use comrak::reference_audit::{CallbackOutcome, CanonicalLookup};
use std::sync::atomic::{AtomicUsize, Ordering};

fn known() -> ContextClaim {
    ContextClaim::Known(ParseContext::LivePreview)
}
fn source(generation: u64, text: &str) -> Snapshot {
    Snapshot::new("note.md", generation, text)
}
fn work() -> usize {
    super::super::TEST_PARSE_CALLS.with(|v| v.get())
}
fn build(source: &Snapshot) -> Arc<BlockIndex> {
    BlockIndex::build(source.clone(), ParseContext::LivePreview).unwrap()
}
fn fresh_equivalent(index: &BlockIndex, source: &Snapshot) {
    let fresh = super::super::classify(source);
    let current = index.classification(source, known()).unwrap();
    assert_eq!(current.snapshot(), fresh.snapshot());
    assert_eq!(current.plan.regions(), fresh.plan.regions());
    assert_eq!(current.styles, fresh.styles);
    assert_eq!(current.links, fresh.links);
    assert_eq!(current.decorations, fresh.decorations);
    assert_eq!(current.marker_scopes, fresh.marker_scopes);
    assert_eq!(current.contexts, fresh.contexts);
    assert_eq!(current.references, fresh.references);
    assert_eq!(current.reasons, fresh.reasons);
    let a = super::super::RetainedPresentation::new(current)
        .prepare_reveal(&Active::default())
        .unwrap();
    let b = super::super::RetainedPresentation::new(&fresh)
        .prepare_reveal(&Active::default())
        .unwrap();
    assert_eq!(a.projection().display(), b.projection().display());
}

#[test]
fn one_root_parse_supplies_complete_ordered_authority_and_fresh_classification() {
    let old = source(1, "[forward][a b] [street][strasse]\n\n[a\u{a0}b]: <first&amp;x> 'clean &amp; title'\n[a b]: /wrong\n[Straẞe]: /street\n[STRASSE]: /wrong\n[unused]: /unused\n");
    let before = work();
    let index = build(&old);
    assert_eq!(
        work() - before,
        1,
        "real cold root parse, not fragment reparses"
    );
    let references = index.references(&old, known()).unwrap();
    let definitions = references.definitions(&old, known()).unwrap();
    assert_eq!(
        definitions.len(),
        5,
        "unused definitions must not disappear"
    );
    assert_eq!(
        definitions.iter().map(|d| d.inserted).collect::<Vec<_>>(),
        [true, false, true, false, true]
    );
    assert_eq!(definitions[0].label, "a b");
    assert_eq!(definitions[0].url, "first&x");
    assert_eq!(definitions[0].title, "clean & title");
    let effective = references.effective(&old, known()).unwrap();
    assert_eq!(
        effective
            .iter()
            .map(|d| d.label.as_str())
            .collect::<Vec<_>>(),
        ["a b", "strasse", "unused"]
    );
    assert_eq!(
        references.definition_contexts(&old, known()).unwrap().len(),
        definitions.len()
    );
    let before = work();
    assert_eq!(references.definitions(&old, known()).unwrap(), definitions);
    assert_eq!(
        index.blocks(&old, ContextClaim::Unknown),
        Err(Error::UnknownContext)
    );
    assert_eq!(
        work(),
        before,
        "read/failed context check performs no parse/lookup"
    );
    fresh_equivalent(&index, &old);
    let used = source(2, &format!("{}\n[new][unused]\n", old.source()));
    let current = build(&used);
    assert_eq!(
        current
            .references(&used, known())
            .unwrap()
            .effective(&used, known())
            .unwrap(),
        effective
    );
    assert!(current
        .references(&used, known())
        .unwrap()
        .lookups(&used, known())
        .unwrap()
        .iter()
        .any(|v| v.normalized == "unused" && v.canonical == CanonicalLookup::Resolved));
    fresh_equivalent(&current, &used);
}

#[test]
fn canonical_top_level_containers_and_raw_gaps_cover_authored_source_exactly() {
    let snapshot = source(1, "[unused]: /a\n\n# h\n\n> q\n>\n> **b**\n\n- a\n- b\n");
    let index = build(&snapshot);
    let blocks = index.blocks(&snapshot, known()).unwrap();
    assert_eq!(
        blocks.iter().map(|b| b.kind).collect::<Vec<_>>(),
        [BlockKind::Heading, BlockKind::Quote, BlockKind::List]
    );
    assert_eq!(
        blocks
            .iter()
            .map(|b| &snapshot.source()[b.fragment.bytes.clone()])
            .collect::<Vec<_>>(),
        ["# h\n", "> q\n>\n> **b**\n", "- a\n- b\n"]
    );
    let mut all: Vec<_> = blocks
        .iter()
        .map(|b| b.fragment.bytes.clone())
        .chain(
            index
                .raw_gaps(&snapshot, known())
                .unwrap()
                .iter()
                .map(|g| g.bytes.clone()),
        )
        .collect();
    all.sort_by_key(|r| r.start);
    let mut cursor = 0;
    let mut joined = String::new();
    for range in all {
        assert_eq!(range.start, cursor);
        cursor = range.end;
        joined.push_str(&snapshot.source()[range]);
    }
    assert_eq!(cursor, snapshot.source().len());
    assert_eq!(joined, snapshot.source());
    let arena = Arena::new();
    let options = super::super::canonical_options(snapshot.source(), &[], true);
    let root = comrak::parse_document(&arena, snapshot.source(), &options);
    assert_eq!(
        blocks.iter().map(|b| b.sourcepos).collect::<Vec<_>>(),
        root.children()
            .map(|node| node.data.borrow().sourcepos)
            .collect::<Vec<_>>()
    );
    fresh_equivalent(&index, &snapshot);
}

#[test]
fn frontmatter_fence_and_container_definition_context_matches_canonical_root() {
    for text in ["---\n[hidden]: /wrong\n---\n\n```\n[fenced]: /wrong\n```\n\n> [quote]: /q\n\n- [list]: /l\n\n[x][quote] [y][list]\n", "+++\nname = 'x'\n+++\n\n# title\n"] {
        let snapshot = source(1,text);let index = build(&snapshot);
        assert_eq!(index.blocks(&snapshot,known()).unwrap()[0].kind,BlockKind::Frontmatter);
        let effective = index.references(&snapshot,known()).unwrap().effective(&snapshot,known()).unwrap();
        assert!(!effective.iter().any(|d| matches!(d.label.as_str(),"hidden"|"fenced")));
        if text.contains("[quote]") { assert_eq!(effective.iter().map(|d| d.label.as_str()).collect::<Vec<_>>(),["quote","list"]); }
        fresh_equivalent(&index,&snapshot);
    }
}

#[test]
fn unicode_crlf_editor_byte_grapheme_and_utf16_boundaries_never_round_or_split() {
    let snapshot = source(1, "# e\u{301} 👩‍💻 🇮🇱 🧠\r\n\r\n**bold**\r\n");
    let index = build(&snapshot);
    let valid: Vec<_> = snapshot
        .source()
        .grapheme_indices(true)
        .map(|(i, _)| i)
        .chain(std::iter::once(snapshot.source().len()))
        .collect();
    let utf16: Vec<_> = valid
        .iter()
        .map(|&b| snapshot.source()[..b].encode_utf16().count())
        .collect();
    for b in 0..=snapshot.source().len() + 1 {
        match valid.binary_search(&b) {
            Ok(i) => assert_eq!(index.byte_to_utf16(&snapshot, known(), b), Ok(utf16[i])),
            Err(_) => assert_eq!(
                index.byte_to_utf16(&snapshot, known(), b),
                Err(Error::InvalidBoundary)
            ),
        }
    }
    for u in 0..=snapshot.source().encode_utf16().count() + 1 {
        match utf16.binary_search(&u) {
            Ok(i) => assert_eq!(index.utf16_to_byte(&snapshot, known(), u), Ok(valid[i])),
            Err(_) => assert_eq!(
                index.utf16_to_byte(&snapshot, known(), u),
                Err(Error::InvalidBoundary)
            ),
        }
    }
    let b = Boundaries::new(snapshot.source());
    for bad in [
        (0, 1, 1, 1),
        (1, 0, 1, 1),
        (1, 1, 1, 0),
        (99, 1, 99, 1),
        (1, 1, 1, usize::MAX),
        (1, 4, 1, 4),
    ] {
        assert!(
            b.parser_range(snapshot.source(), bad.into()).is_err(),
            "invalid coordinate {bad:?}"
        );
    }
    let cluster = source(1, "e\u{301}");
    assert_eq!(
        Boundaries::new(cluster.source()).parser_range(cluster.source(), (1, 1, 1, 1).into()),
        Err(Error::InvalidBoundary)
    );
    fresh_equivalent(&index, &snapshot);
}

#[test]
fn held_authorities_and_shifted_ranges_never_rebind_by_generation_or_hash_alone() {
    let old = source(1, "# first\n\n**same**\n");
    let held = build(&old);
    let middle = source(2, "prefix\n\n# first\n\n**same**\n");
    let mid = build(&middle);
    let next = source(3, "long prefix\n\n# first\n\n**same**\n");
    let current = build(&next);
    let old_start = held
        .blocks(&old, known())
        .unwrap()
        .last()
        .unwrap()
        .fragment
        .bytes
        .start;
    let new_start = current
        .blocks(&next, known())
        .unwrap()
        .last()
        .unwrap()
        .fragment
        .bytes
        .start;
    assert!(new_start > old_start);
    assert_eq!(
        current
            .blocks(&next, known())
            .unwrap()
            .last()
            .unwrap()
            .fragment
            .utf16
            .start,
        next.source()[..new_start].encode_utf16().count()
    );
    for foreign in [
        Snapshot::new("other.md", 1, old.source()),
        source(2, old.source()),
        source(1, "# changed\n\n**same**\n"),
        middle.clone(),
        next.clone(),
    ] {
        assert_eq!(held.blocks(&foreign, known()), Err(Error::Stale));
        assert!(held.references(&foreign, known()).is_err());
    }
    assert!(mid.blocks(&middle, known()).is_ok());
    assert!(held.blocks(&old, known()).is_ok());
    fresh_equivalent(&current, &next);
}

#[test]
fn unknown_options_and_stateful_callbacks_get_fresh_observations_not_authority() {
    let snapshot = source(1, "[missing]\n");
    let certified = build(&snapshot);
    let mut options = super::super::canonical_options(snapshot.source(), &[], true);
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    options.parse.broken_link_callback = Some(Arc::new(
        move |_: comrak::options::BrokenLinkReference<'_>| {
            let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
            Some(comrak::ResolvedReference {
                url: format!("/state/{n}"),
                title: String::new(),
            })
        },
    ));
    let before = work();
    for number in [1, 2] {
        let observation = observe_untrusted(&snapshot, &options).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), number);
        assert_eq!(
            observation.lookups()[0].callback,
            CallbackOutcome::Resolved {
                url: format!("/state/{number}"),
                title: String::new()
            }
        );
        assert_eq!(
            certified.blocks(&snapshot, ContextClaim::Unknown),
            Err(Error::UnknownContext)
        );
    }
    assert_eq!(
        work() - before,
        2,
        "same options/callback pointer does NOT create a reuse token"
    );
    let mut changed = super::super::canonical_options(snapshot.source(), &[], true);
    changed.extension.table = false;
    observe_untrusted(&snapshot, &changed).unwrap();
    assert_eq!(
        certified
            .classification(&snapshot, ContextClaim::Unknown)
            .err(),
        Some(Error::UnknownContext)
    );
}

#[test]
fn exact_twenty_percent_retains_old_authority_but_greater_invalidates_it() {
    for (changed, full) in [(20, false), (21, true)] {
        let old = source(1, &"a".repeat(100));
        let mut session = Session::default();
        let initial = session
            .begin(old.clone(), ParseContext::LivePreview)
            .unwrap();
        assert!(initial.full_invalidation);
        let held = session.adopt(initial.compute().unwrap()).unwrap();
        let new = source(
            2,
            &format!("{}{}", "b".repeat(changed), "a".repeat(100 - changed)),
        );
        let before = work();
        let ticket = session
            .begin(new.clone(), ParseContext::LivePreview)
            .unwrap();
        assert_eq!(work(), before, "invalidation itself is not a parse");
        assert_eq!(ticket.full_invalidation, full);
        assert_eq!(session.accepted.is_none(), full);
        assert!(
            session.current().is_none(),
            "retained old data is not current authority"
        );
        assert_eq!(session.reject_partial(&ticket), Err(Error::Incomplete));
        let current = session.adopt(ticket.compute().unwrap()).unwrap();
        assert!(session.current().is_some());
        assert!(held.blocks(&old, known()).is_ok());
        assert_eq!(held.blocks(&new, known()), Err(Error::Stale));
        fresh_equivalent(&current, &new);
    }
}

#[test]
fn stale_partial_completed_and_retried_requests_cannot_adopt_or_charge_again() {
    let text = format!(
        "[r]: /{}\n\n{}\n\n# boundary\n\n{}",
        "x".repeat(999),
        "[r] ".repeat(50),
        "[r] ".repeat(51)
    );
    let old = source(1, &text);
    let new = source(2, &format!("{text}\n\nend\n"));
    let mut session = Session::default();
    let old_ticket = session
        .begin(old.clone(), ParseContext::LivePreview)
        .unwrap();
    let completed = old_ticket.clone().compute().unwrap();
    let held = completed.index.clone();
    let fuel = held
        .references(&old, known())
        .unwrap()
        .fuel(&old, known())
        .unwrap();
    assert_eq!(fuel.used, 100000);
    let new_ticket = session
        .begin(new.clone(), ParseContext::LivePreview)
        .unwrap();
    assert_eq!(session.reject_partial(&old_ticket), Err(Error::Stale));
    let before = work();
    assert_eq!(session.adopt(completed).err(), Some(Error::Stale));
    assert_eq!(work(), before);
    assert_eq!(
        held.references(&old, known())
            .unwrap()
            .fuel(&old, known())
            .unwrap(),
        fuel,
        "failed adoption does not replay/double-charge"
    );
    let next = session.adopt(new_ticket.compute().unwrap()).unwrap();
    fresh_equivalent(&next, &new);
    assert_eq!(
        next.references(&new, known())
            .unwrap()
            .lookups(&new, known())
            .unwrap()
            .iter()
            .filter(|v| v.canonical == CanonicalLookup::Exhausted)
            .count(),
        1
    );
    assert_eq!(
        session
            .begin(old.clone(), ParseContext::LivePreview)
            .err()
            .map(|_| Error::Stale),
        Some(Error::Stale)
    );
    let retry1 = session
        .begin(new.clone(), ParseContext::LivePreview)
        .unwrap();
    let obsolete = retry1.compute().unwrap();
    let retry2 = session
        .begin(new.clone(), ParseContext::LivePreview)
        .unwrap();
    assert_eq!(session.adopt(obsolete).err(), Some(Error::Stale));
    session.adopt(retry2.compute().unwrap()).unwrap();
}

#[test]
fn existing_input_identity_and_structure_caps_refuse_before_unsafe_publication() {
    let before = work();
    for bad in [
        source(1, &"a".repeat(MAX_BYTES + 1)),
        Snapshot::new("", 1, "text"),
        source(1, "bad\0source"),
        source(1, "bare\rcarriage"),
    ] {
        assert!(BlockIndex::build(bad, ParseContext::LivePreview).is_err());
    }
    assert_eq!(
        work(),
        before,
        "preflight refuses before canonical parser work"
    );
    for malformed in ["---\n# not metadata\n", "+++\n# unclosed\n"] {
        assert_eq!(
            BlockIndex::build(source(1, malformed), ParseContext::LivePreview).err(),
            Some(Error::Source(SourceReason::UnsupportedOrAmbiguous))
        );
    }
    let positive = source(1, &"a".repeat(MAX_BYTES));
    assert!(build(&positive).blocks(&positive, known()).is_ok());
    let excessive = source(1, &"[unused]: /a\n".repeat(MAX_RANGES));
    assert_eq!(
        BlockIndex::build(excessive, ParseContext::LivePreview).err(),
        Some(Error::Source(SourceReason::StructureLimit))
    );
}
