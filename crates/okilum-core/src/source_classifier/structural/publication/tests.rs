use super::*;
use crate::source_classifier::{
    classify,
    retained::{TEST_RETAINED_BUILDS, TEST_REVEAL_PREPARES},
};
use crate::source_projection::TEST_INDEX_BUILDS;

fn known() -> ContextClaim {
    ContextClaim::Known(ParseContext::LivePreview)
}
fn source(generation: u64, text: &str) -> Snapshot {
    Snapshot::new("note.md", generation, text)
}
fn roots(source: &Snapshot) -> Roots {
    let mut roots = Roots::default();
    let request = roots
        .begin(source.clone(), ParseContext::LivePreview)
        .unwrap();
    roots.adopt(request.compute().unwrap()).unwrap();
    roots
}
fn work() -> (usize, usize, usize, usize) {
    (
        crate::source_classifier::TEST_PARSE_CALLS.with(|v| v.get()),
        TEST_INDEX_BUILDS.with(|v| v.get()),
        TEST_RETAINED_BUILDS.with(|v| v.get()),
        TEST_REVEAL_PREPARES.with(|v| v.get()),
    )
}
fn view<'a>(source: &'a Snapshot, active: &'a Active, identity: &'a PublicationId) -> View<'a> {
    View {
        source,
        context: known(),
        active,
        identity,
    }
}
fn publish(
    store: &mut Session,
    roots: &Roots,
    source: &Snapshot,
    active: Active,
) -> Arc<Publication> {
    let request = store
        .begin(roots, source, known(), ActiveInput::Source(active))
        .unwrap();
    let complete = request.compute().unwrap();
    store.adopt(roots, complete).unwrap()
}
fn equivalent(publication: &Publication, source: &Snapshot, active: &Active) {
    let identity = publication.identity();
    let view = view(source, active, &identity);
    let fresh = RetainedPresentation::new(&classify(source))
        .prepare_reveal(active)
        .unwrap();
    let expected = fresh.projection();
    assert_eq!(publication.display(&view).unwrap(), expected.display());
    for byte in 0..=source.source().len() + 1 {
        assert_eq!(
            publication.source_to_display(&view, byte),
            expected
                .source_to_display(source, byte)
                .map_err(Error::Mapping)
        );
    }
    for byte in 0..=expected.display().len() + 1 {
        for bias in [Bias::Left, Bias::Right] {
            assert_eq!(
                publication.display_to_source(&view, byte, bias),
                expected
                    .display_to_source(source, byte, bias)
                    .map_err(Error::Mapping)
            );
        }
    }
    let classified = classify(source);
    for marker in classified.decorations_for(source).unwrap() {
        assert_eq!(
            publication
                .scope_is_raw(&view, marker.scope.clone())
                .unwrap(),
            fresh.is_raw(source, &marker.scope)
        );
    }
}

#[test]
fn actual_cold_repeat_reveal_and_same_root_reuse_are_distinct_work() {
    let source = source(1, "# title\n\n**bold** and [[target|label]]\n");
    let roots = roots(&source);
    let mut store = Session::default();
    let before = work();
    let first = publish(&mut store, &roots, &source, Active::default());
    let cold = work();
    assert_eq!(cold.0, before.0, "publication must not parse");
    assert!(
        cold.1 > before.1,
        "real projection index construction positive control"
    );
    assert_eq!(cold.2 - before.2, 1);
    assert_eq!(cold.3 - before.3, 1);
    let before = work();
    let request = store
        .begin(
            &roots,
            &source,
            known(),
            ActiveInput::Source(Active::default()),
        )
        .unwrap();
    assert!(
        store.current(&roots).is_none(),
        "new request remains pending"
    );
    assert_eq!(
        store.reject_partial(&roots, &request),
        Err(Error::Incomplete)
    );
    let same = store.adopt(&roots, request.compute().unwrap()).unwrap();
    assert!(Arc::ptr_eq(&first, &same));
    assert_eq!(first.identity(), same.identity());
    assert_eq!(
        work(),
        before,
        "reuse performs no retained/prepare/index/parser action"
    );
    let active = Active {
        selection: Some(3..3),
        composition: None,
    };
    let before = work();
    let revealed = publish(&mut store, &roots, &source, active.clone());
    let after = work();
    assert_eq!(after.0, before.0);
    assert_eq!(after.2, before.2, "same-root validated retained base reuse");
    assert_eq!(after.3 - before.3, 1);
    assert_ne!(first.identity(), revealed.identity());
    assert!(Arc::ptr_eq(&first.retained, &revealed.retained));
    let id = revealed.identity();
    assert!(revealed
        .display(&view(&source, &active, &id))
        .unwrap()
        .starts_with("# title"));
    let mut independent = Session::default();
    let before = work();
    let repeated = publish(&mut independent, &roots, &source, Active::default());
    let after = work();
    assert_eq!(after.2 - before.2, 1);
    assert_eq!(after.3 - before.3, 1);
    assert!(after.1 > before.1);
    assert_ne!(
        repeated.identity(),
        first.identity(),
        "actual repeated cold action is not reuse"
    );
    equivalent(&first, &source, &Active::default());
    equivalent(&revealed, &source, &active);
}

#[test]
fn fresh_monolithic_source_display_both_biases_and_reveal_match_every_boundary() {
    for text in [
        "# title\n\n**bold** and [[target|label]]\n",
        "\u{feff}# e\u{301} 🧠\r\n\r\n> **quote** 👩‍💻 🇮🇱\r\n",
        "[forward][id]\n\n[id]: /a \"title\"\n\n- **list**\n- *tail*\n",
        "---\nname: value\n---\n\n# heading\n\n`code` and [link](target.md)\n",
    ] {
        let source = source(1, text);
        let roots = roots(&source);
        let mut store = Session::default();
        let end = text.len();
        for active in [
            Active::default(),
            Active {
                selection: Some(0..0),
                composition: None,
            },
            Active {
                selection: Some(0..end),
                composition: None,
            },
            Active {
                selection: Some(end..end),
                composition: Some(0..end),
            },
        ] {
            let before = work().0;
            let publication = publish(&mut store, &roots, &source, active.clone());
            assert_eq!(work().0, before);
            equivalent(&publication, &source, &active);
        }
    }
}

#[test]
fn invalid_utf16_utf8_grapheme_crlf_active_inputs_refuse_without_derivation() {
    let text = "# e\u{301} 🧠\r\n\n**b**\n";
    let source = source(1, text);
    let roots = roots(&source);
    let mut store = Session::default();
    let held = publish(&mut store, &roots, &source, Active::default());
    let emoji = text.find('🧠').unwrap();
    let crlf = text.find("\r\n").unwrap();
    let emoji_units = text[..emoji].encode_utf16().count();
    let crlf_units = text[..crlf].encode_utf16().count();
    for input in [
        ActiveInput::Source(Active {
            selection: Some(3..3),
            composition: None,
        }),
        ActiveInput::Source(Active {
            selection: Some(emoji + 1..emoji + 1),
            composition: None,
        }),
        ActiveInput::Source(Active {
            selection: Some(crlf + 1..crlf + 1),
            composition: None,
        }),
        ActiveInput::Source(Active {
            selection: Some(4..2),
            composition: None,
        }),
        ActiveInput::Source(Active {
            selection: None,
            composition: Some(usize::MAX..usize::MAX),
        }),
        ActiveInput::Utf16(Active {
            selection: Some(emoji_units + 1..emoji_units + 1),
            composition: None,
        }),
        ActiveInput::Utf16(Active {
            selection: Some(crlf_units + 1..crlf_units + 1),
            composition: None,
        }),
    ] {
        let before = work();
        assert!(store.begin(&roots, &source, known(), input).is_err());
        assert_eq!(work(), before);
        assert!(
            store.current(&roots).is_none(),
            "refused current input cannot leave a complete current view"
        );
    }
    let valid = Active {
        selection: Some(emoji_units..emoji_units + 2),
        composition: None,
    };
    let held_id = held.identity();
    assert!(held
        .display(&view(&source, &Active::default(), &held_id))
        .is_ok());
    let request = store
        .begin(&roots, &source, known(), ActiveInput::Utf16(valid))
        .unwrap();
    let new = store.adopt(&roots, request.compute().unwrap()).unwrap();
    assert_eq!(new.active().selection, Some(emoji..emoji + 4));
    equivalent(&new, &source, new.active());
    let id = new.identity();
    assert!(new
        .scope_is_raw(&view(&source, new.active(), &id), crlf + 1..crlf + 1)
        .is_err());
}

#[test]
fn source_context_active_and_publication_identity_mismatches_never_map() {
    let source = source(1, "# title\n\n**bold**\n");
    let roots = roots(&source);
    let mut a = Session::default();
    let mut b = Session::default();
    let first = publish(&mut a, &roots, &source, Active::default());
    let other = publish(&mut b, &roots, &source, Active::default());
    let first_id = first.identity();
    let other_id = other.identity();
    let active = Active::default();
    assert_eq!(
        first.display(&view(&source, &active, &other_id)).err(),
        Some(Error::PublicationMismatch)
    );
    let changed = Active {
        selection: Some(0..0),
        composition: None,
    };
    assert_eq!(
        first.display(&view(&source, &changed, &first_id)).err(),
        Some(Error::ActiveMismatch)
    );
    assert_eq!(
        first
            .display(&View {
                source: &source,
                context: ContextClaim::Unknown,
                active: &active,
                identity: &first_id
            })
            .err(),
        Some(Error::Authority(super::super::Error::UnknownContext))
    );
    for foreign in [
        Snapshot::new("other.md", 1, source.source()),
        Snapshot::new(source.document(), 2, source.source()),
        Snapshot::new(source.document(), 1, "# changed\n\n**bold**\n"),
    ] {
        let before = work();
        assert!(first.display(&view(&foreign, &active, &first_id)).is_err());
        assert!(a
            .begin(
                &roots,
                &foreign,
                known(),
                ActiveInput::Source(active.clone())
            )
            .is_err());
        assert_eq!(work(), before);
    }
    assert!(first.display(&view(&source, &active, &first_id)).is_ok());
}

#[test]
fn pending_partial_stale_and_foreign_completions_do_not_publish_or_charge_again() {
    let source = source(1, "[x][id]\n\n[id]: /somewhere 'title'\n");
    let roots = roots(&source);
    let index = roots.current().unwrap().clone();
    let fuel = index
        .references(&source, known())
        .unwrap()
        .fuel(&source, known())
        .unwrap();
    assert!(fuel.used > 0);
    let mut store = Session::default();
    let old = store
        .begin(
            &roots,
            &source,
            known(),
            ActiveInput::Source(Active::default()),
        )
        .unwrap();
    let old_done = old.compute().unwrap();
    let active = Active {
        selection: Some(0..0),
        composition: None,
    };
    let current = store
        .begin(
            &roots,
            &source,
            known(),
            ActiveInput::Source(active.clone()),
        )
        .unwrap();
    assert!(store.current(&roots).is_none());
    assert_eq!(
        store.reject_partial(&roots, &current),
        Err(Error::Incomplete)
    );
    let before = work();
    assert_eq!(
        store.adopt(&roots, old_done).err(),
        Some(Error::StaleRequest)
    );
    assert_eq!(work(), before);
    assert_eq!(
        index
            .references(&source, known())
            .unwrap()
            .fuel(&source, known())
            .unwrap(),
        fuel
    );
    let accepted = store.adopt(&roots, current.compute().unwrap()).unwrap();
    let retry = store
        .begin(
            &roots,
            &source,
            known(),
            ActiveInput::Source(active.clone()),
        )
        .unwrap()
        .compute()
        .unwrap();
    let latest = store
        .begin(
            &roots,
            &source,
            known(),
            ActiveInput::Source(active.clone()),
        )
        .unwrap();
    assert!(store.current(&roots).is_none());
    let before = work();
    assert_eq!(store.adopt(&roots, retry).err(), Some(Error::StaleRequest));
    assert_eq!(work(), before);
    let current = store.adopt(&roots, latest.compute().unwrap()).unwrap();
    assert!(Arc::ptr_eq(&accepted, &current));
    let mut foreign = Session::default();
    let foreign_done = foreign
        .begin(
            &roots,
            &source,
            known(),
            ActiveInput::Source(active.clone()),
        )
        .unwrap()
        .compute()
        .unwrap();
    let before = work();
    assert_eq!(
        store.adopt(&roots, foreign_done).err(),
        Some(Error::StaleRequest)
    );
    assert_eq!(work(), before);
}

#[test]
fn root_retry_and_new_revision_require_new_accepted_root_before_publication() {
    let old = source(1, "# title\n\n**bold**\n");
    let mut roots = roots(&old);
    let mut store = Session::default();
    let active = Active::default();
    let held = publish(&mut store, &roots, &old, active.clone());
    let held_id = held.identity();
    let pending = store
        .begin(&roots, &old, known(), ActiveInput::Source(active.clone()))
        .unwrap()
        .compute()
        .unwrap();
    let root_retry = roots.begin(old.clone(), ParseContext::LivePreview).unwrap();
    assert!(roots.current().is_none());
    assert!(store.current(&roots).is_none());
    let before = work();
    assert!(store.adopt(&roots, pending).is_err());
    assert!(store
        .begin(&roots, &old, known(), ActiveInput::Source(active.clone()))
        .is_err());
    assert_eq!(work(), before);
    roots.adopt(root_retry.compute().unwrap()).unwrap();
    assert!(store.current(&roots).is_none());
    let before = work();
    let repeated = publish(&mut store, &roots, &old, active.clone());
    assert_eq!(work().0, before.0);
    assert_eq!(
        work().2 - before.2,
        1,
        "new accepted root has no old derived base reuse"
    );
    assert_ne!(held.identity(), repeated.identity());
    let stale = store
        .begin(&roots, &old, known(), ActiveInput::Source(active.clone()))
        .unwrap()
        .compute()
        .unwrap();
    let new = source(2, "prefix\n\n# title\n\n**bold**\n");
    let next = roots.begin(new.clone(), ParseContext::LivePreview).unwrap();
    roots.adopt(next.compute().unwrap()).unwrap();
    let before = work();
    assert!(store.adopt(&roots, stale).is_err());
    assert!(store.current(&roots).is_none());
    assert_eq!(work(), before);
    assert!(held.display(&view(&old, &active, &held_id)).is_ok());
    assert!(held.display(&view(&new, &active, &held_id)).is_err());
    let current = publish(&mut store, &roots, &new, active.clone());
    equivalent(&current, &new, &active);
}

#[test]
fn unsafe_source_join_never_acquires_complete_authority_or_publication() {
    let source = source(1, "🇦**🇧**");
    let mut roots = Roots::default();
    let ticket = roots
        .begin(source.clone(), ParseContext::LivePreview)
        .unwrap();
    assert!(ticket.compute().is_err());
    assert!(roots.current().is_none());
    let mut store = Session::default();
    let before = work();
    assert!(store
        .begin(
            &roots,
            &source,
            known(),
            ActiveInput::Source(Active::default())
        )
        .is_err());
    assert_eq!(work(), before);
}
