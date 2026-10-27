use comrak::reference_audit::{CallbackOutcome, CanonicalLookup as Lookup};
use comrak::{parse_document_with_reference_audit, Arena};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

mod cases {
    include!("fixtures/comrak_api_cases.rs");
}

#[test]
fn audited_and_default_parse_keep_exact_ast_positions_and_outputs() {
    for source in cases::cases() {
        assert!(source.len() <= okilum_core::source_classifier::MAX_BYTES);
        let options = cases::options(&source);
        let a = Arena::new();
        let default = comrak::parse_document(&a, &source, &options);
        let b = Arena::new();
        let (audited, evidence) = parse_document_with_reference_audit(&b, &source, &options);
        assert_eq!(evidence.source(), source);
        assert_eq!(
            default
                .descendants()
                .map(|node| node.data.borrow().sourcepos)
                .collect::<Vec<_>>(),
            audited
                .descendants()
                .map(|node| node.data.borrow().sourcepos)
                .collect::<Vec<_>>(),
            "compare actual positions; XML sourcepos formatting is optional"
        );
        assert_eq!(
            cases::output(default, &options),
            cases::output(audited, &options)
        );
        let mut sequence: Vec<_> = evidence
            .definitions()
            .iter()
            .map(|v| v.sequence)
            .chain(evidence.lookups().iter().map(|v| v.sequence))
            .collect();
        sequence.sort_unstable();
        assert_eq!(sequence, (0..sequence.len()).collect::<Vec<_>>());
    }
}

#[test]
fn unused_forward_duplicate_and_unicode_definitions_keep_actual_precedence() {
    let source = "[forward][a b] [street][strasse]\n\n[a\u{a0}b]: <first&amp;x> 'clean &amp; title'\n[a b]: /second\n[Straẞe]: /street\n[STRASSE]: /wrong\n[unused]: /unused \"unused title\"\n";
    let options = cases::options(source);
    let arena = Arena::new();
    let (root, audit) = parse_document_with_reference_audit(&arena, source, &options);
    let definitions = audit.definitions();
    assert_eq!(definitions.len(), 5, "unused definition positive control");
    assert_eq!(
        definitions.iter().map(|d| d.inserted).collect::<Vec<_>>(),
        [true, false, true, false, true]
    );
    assert_eq!(definitions[0].label, "a b");
    assert_eq!(definitions[0].url, "first&x");
    assert_eq!(definitions[0].title, "clean & title");
    assert_eq!(definitions[2].label, "strasse");
    assert_eq!(definitions[3].label, "strasse");
    assert!(definitions.iter().all(|d| d.raw.len() == d.consumed_bytes));
    assert!(definitions[0].paragraph.start.line > 1);
    assert!(definitions[1].content_offset > definitions[0].content_offset);
    let html = &cases::output(root, &options)[1];
    assert!(html.contains("href=\"first&amp;x\""));
    assert!(!html.contains("href=\"/second\""));
    assert!(html.contains("href=\"/street\""));
    assert!(!audit.lookups().iter().any(|v| v.normalized == "unused"));
    let used = format!("{source}\n[new][unused]\n");
    let arena = Arena::new();
    let (_, fresh) = parse_document_with_reference_audit(&arena, &used, &options);
    assert_eq!(fresh.definitions(), audit.definitions());
    assert!(fresh
        .lookups()
        .iter()
        .any(|v| v.normalized == "unused" && v.canonical == Lookup::Resolved));
    assert_eq!(audit.source(), source, "held old root is immutable");
}

#[test]
fn frontmatter_fences_and_indented_code_do_not_supply_definitions_but_containers_do() {
    let source = "---\n[front]: /wrong\n---\n\n```\n[fence]: /wrong\n```\n\n    [indented]: /wrong\n\n> [quote]: /q\n\n- [list]: /l\n\n[normal]: /n\n\n[x][quote] [y][list]\n";
    let arena = Arena::new();
    let (_, evidence) =
        parse_document_with_reference_audit(&arena, source, &cases::options(source));
    assert_eq!(
        evidence
            .definitions()
            .iter()
            .map(|d| d.label.as_str())
            .collect::<Vec<_>>(),
        ["quote", "list", "normal"]
    );
    assert!(evidence
        .lookups()
        .iter()
        .all(|v| v.canonical == Lookup::Resolved));
    assert!(evidence.definitions()[0].paragraph.start.column > 1);
}

fn fuel_source(references: usize) -> String {
    format!(
        "[r]: /{}\n\n{}\n\n# fragment boundary\n\n{}",
        "x".repeat(999),
        "[r] ".repeat(references.min(50)),
        "[r] ".repeat(references.saturating_sub(50))
    )
}

#[test]
fn canonical_fuel_below_at_and_above_limit_is_shared_across_fragments() {
    for count in [99, 100, 101] {
        let source = fuel_source(count);
        let arena = Arena::new();
        let (root, audit) =
            parse_document_with_reference_audit(&arena, &source, &cases::options(&source));
        let lookups = audit.lookups();
        assert_eq!(
            lookups
                .iter()
                .filter(|v| v.canonical == Lookup::Resolved)
                .count(),
            count.min(100)
        );
        assert_eq!(
            lookups
                .iter()
                .filter(|v| v.canonical == Lookup::Exhausted)
                .count(),
            count.saturating_sub(100)
        );
        assert_eq!(audit.final_fuel().used, count.min(100) * 1000);
        assert_eq!(audit.final_fuel().maximum, 100000);
        assert_eq!(
            lookups[50].before.used, 50000,
            "not a fresh fragment budget"
        );
        for pair in lookups.windows(2) {
            assert_eq!(pair[0].after, pair[1].before);
        }
        assert_eq!(
            cases::output(root, &cases::options(&source))[1]
                .matches("href=")
                .count(),
            count.min(100)
        );
        let before = audit.final_fuel();
        // Exact-root validation/readback cannot charge fuel or advance a parser.
        for _ in 0..3 {
            assert_ne!(audit.source(), format!("x{source}"));
            assert_eq!(audit.lookups(), lookups);
            assert_eq!(audit.final_fuel(), before);
        }
    }
}

#[test]
fn exhausted_and_missing_callback_fallback_keeps_one_call_and_never_charges_twice() {
    let calls = Arc::new(AtomicUsize::new(0));
    let source = format!("{}\n\n[missing] [unresolved]\n", fuel_source(101));
    let mut options = cases::options(&source);
    let observed = calls.clone();
    options.parse.broken_link_callback = Some(Arc::new(
        move |r: comrak::options::BrokenLinkReference<'_>| {
            observed.fetch_add(1, Ordering::SeqCst);
            (r.normalized != "unresolved").then(|| comrak::ResolvedReference {
                url: format!("/callback/{}", r.normalized),
                title: "fallback".into(),
            })
        },
    ));
    let arena = Arena::new();
    let default = comrak::parse_document(&arena, &source, &options);
    let default_calls = calls.swap(0, Ordering::SeqCst);
    let other = Arena::new();
    let (audited, audit) = parse_document_with_reference_audit(&other, &source, &options);
    assert_eq!(
        cases::output(default, &options),
        cases::output(audited, &options)
    );
    assert_eq!(calls.load(Ordering::SeqCst), default_calls);
    assert_eq!(
        default_calls, 3,
        "exhausted + missing + unresolved positive controls"
    );
    let exhausted = audit
        .lookups()
        .iter()
        .find(|v| v.canonical == Lookup::Exhausted)
        .unwrap();
    assert!(matches!(
        exhausted.callback,
        CallbackOutcome::Resolved { .. }
    ));
    assert_eq!(exhausted.before, exhausted.after);
    let missing = audit
        .lookups()
        .iter()
        .find(|v| v.normalized == "missing")
        .unwrap();
    assert_eq!(missing.canonical, Lookup::Missing);
    assert!(matches!(missing.callback, CallbackOutcome::Resolved { .. }));
    let unresolved = audit
        .lookups()
        .iter()
        .find(|v| v.normalized == "unresolved")
        .unwrap();
    assert_eq!(unresolved.callback, CallbackOutcome::Unresolved);
    assert_eq!(audit.final_fuel().used, 100000);
    assert_eq!(
        audit
            .lookups()
            .iter()
            .filter(|v| v.callback != CallbackOutcome::NotCalled)
            .count(),
        default_calls
    );
}
