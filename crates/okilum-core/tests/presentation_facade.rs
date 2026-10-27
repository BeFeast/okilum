use okilum_core::{
    source_classifier::{classify, presentation::*, RetainedPresentation},
    source_projection::{Active, Bias, Snapshot},
};

fn accept(session: &mut Session, source: &Snapshot) {
    let request = session
        .begin_root(source.clone(), Context::LivePreview)
        .unwrap();
    session.adopt_root(request.compute().unwrap()).unwrap();
}
fn publish(session: &mut Session, source: &Snapshot, active: NativeActive) -> Publication {
    let request = session.begin(source, Context::LivePreview, active).unwrap();
    session.adopt(request.compute().unwrap()).unwrap()
}

#[test]
fn public_facade_matches_fresh_canonical_mapping_metadata_and_raw_policy() {
    for text in [
        "# title\n\n**strong** *em* ~~strike~~ `code` [[note|wiki]]\n\n- [ ] task\n\n> quote\n\nend\n",
        "\u{feff}# e\u{301} 🧠\r\n\r\n> **quote** 👩‍💻 🇮🇱\r\n\r\nend\r\n",
        "---\nname: yes\n---\n\n[forward][id]\n\n[id]: /a 'title'\n\n| a | b |\n| - | - |\n| c | d |\n\n![alt](a.png)\n\nend\n",
    ] {
        let source = Snapshot::new("note", 1, text);
        let mut session = Session::default();
        accept(&mut session, &source);
        for native in [NativeActive::default(), NativeActive { anchor: text.len(), head: text.len(), ..Default::default() }] {
            let publication = publish(&mut session, &source, native.clone());
            let id = publication.identity();
            let view = publication.view(&source, Context::LivePreview, &native, &id).unwrap();
            let classified = classify(&source);
            let active = Active { selection: Some(native.anchor..native.head), composition: None };
            let retained = RetainedPresentation::new(&classified);
            let expected = retained.prepare_reveal(&active).unwrap();
            assert_eq!(view.display(), expected.projection().display());
            assert_eq!(view.styles(), classified.styles_for(&source).unwrap());
            assert_eq!(view.decorations(), classified.decorations_for(&source).unwrap());
            assert_eq!(view.reasons(), classified.reasons());
            for byte in 0..=text.len() + 1 {
                assert_eq!(view.source_to_display(byte), expected.projection().source_to_display(&source, byte));
            }
            for byte in 0..=view.display().len() + 1 {
                for bias in [Bias::Left, Bias::Right] {
                    assert_eq!(view.display_to_source(byte, bias), expected.projection().display_to_source(&source, byte, bias));
                }
            }
            for marker in view.decorations() {
                assert_eq!(view.scope_is_raw(marker.scope.clone()).unwrap(), expected.is_raw(&source, &marker.scope));
            }
        }
    }
}

#[test]
fn full_input_and_publication_identity_cannot_be_replaced_by_normalized_selection() {
    let source = Snapshot::new("note", 1, "# title\n\n**bold**\n");
    let mut session = Session::default();
    accept(&mut session, &source);
    let active = NativeActive {
        anchor: 0,
        head: 3,
        ..Default::default()
    };
    let held = publish(&mut session, &source, active.clone());
    let id = held.identity();
    for changed in [
        NativeActive {
            anchor: 3,
            head: 0,
            ..active.clone()
        },
        NativeActive {
            affinity: Affinity::End,
            ..active.clone()
        },
        NativeActive {
            replacement: Some(0..3),
            ..active.clone()
        },
        NativeActive {
            composition: Some(0..3),
            ..active.clone()
        },
    ] {
        assert!(held
            .view(&source, Context::LivePreview, &changed, &id)
            .is_err());
        let current = publish(&mut session, &source, changed.clone());
        assert!(current
            .view(&source, Context::LivePreview, &changed, &current.identity())
            .is_ok());
    }
    for foreign in [
        Snapshot::new("other", 1, source.source()),
        Snapshot::new("note", 2, source.source()),
        Snapshot::new("note", 1, "# other\n\n**bold**\n"),
    ] {
        assert!(held
            .view(&foreign, Context::LivePreview, &active, &id)
            .is_err());
    }
    assert!(held.view(&source, Context::Unknown, &active, &id).is_err());
    let mut other = Session::default();
    accept(&mut other, &source);
    let foreign = publish(&mut other, &source, active.clone());
    assert!(held
        .view(&source, Context::LivePreview, &active, &foreign.identity())
        .is_err());
    assert!(held
        .view(&source, Context::LivePreview, &active, &id)
        .is_ok());
}

#[test]
fn independent_invalid_native_ranges_are_not_hidden_by_a_valid_union() {
    let text = "# e\u{301} 🧠\r\n\nend\n";
    let source = Snapshot::new("note", 1, text);
    let mut session = Session::default();
    accept(&mut session, &source);
    let emoji = text.find('🧠').unwrap();
    let crlf = text.find("\r\n").unwrap();
    for offset in [3, emoji + 1, crlf + 1, usize::MAX] {
        let pending = session
            .begin(&source, Context::LivePreview, NativeActive::default())
            .unwrap()
            .compute()
            .unwrap();
        let active = NativeActive {
            composition: Some(offset..offset),
            replacement: Some(0..text.len()),
            ..Default::default()
        };
        assert!(session
            .begin(&source, Context::LivePreview, active)
            .is_err());
        assert!(session.adopt(pending).is_err());
        assert!(session.current().is_none());
        let active = NativeActive {
            composition: Some(0..text.len()),
            replacement: Some(offset..offset),
            ..Default::default()
        };
        assert!(session
            .begin(&source, Context::LivePreview, active)
            .is_err());
    }
    let units = text[..emoji].encode_utf16().count();
    assert!(session
        .begin_utf16(
            &source,
            Context::LivePreview,
            NativeActive {
                composition: Some(units + 1..units + 1),
                replacement: Some(0..text.encode_utf16().count()),
                ..Default::default()
            }
        )
        .is_err());
    let request = session
        .begin_utf16(
            &source,
            Context::LivePreview,
            NativeActive {
                anchor: units + 2,
                head: units,
                ..Default::default()
            },
        )
        .unwrap();
    let valid = session.adopt(request.compute().unwrap()).unwrap();
    assert_eq!(valid.active().anchor, emoji + 4);
    assert_eq!(valid.active().head, emoji);
    assert!(session
        .begin(
            &source,
            Context::LivePreview,
            NativeActive {
                replacement: Some(std::ops::Range { start: 4, end: 2 }),
                ..Default::default()
            }
        )
        .is_err());
}

#[test]
fn pending_partial_foreign_refused_retry_and_two_unpainted_roots_do_not_adopt() {
    let source = Snapshot::new("note", 1, "# title\n\nend\n");
    let mut a = Session::default();
    let mut b = Session::default();
    let request = a.begin_root(source.clone(), Context::LivePreview).unwrap();
    assert!(a.reject_partial_root(&request).is_err());
    let foreign = b
        .begin_root(source.clone(), Context::LivePreview)
        .unwrap()
        .compute()
        .unwrap();
    assert!(a.adopt_root(foreign).is_err());
    a.adopt_root(request.compute().unwrap()).unwrap();
    let held = publish(&mut a, &source, NativeActive::default());
    accept(&mut b, &source);
    let foreign = b
        .begin(&source, Context::LivePreview, NativeActive::default())
        .unwrap()
        .compute()
        .unwrap();
    assert!(a.adopt(foreign).is_err());
    let stale = a
        .begin(&source, Context::LivePreview, NativeActive::default())
        .unwrap()
        .compute()
        .unwrap();
    let retry = a
        .begin(&source, Context::LivePreview, NativeActive::default())
        .unwrap();
    assert!(a.reject_partial(&retry).is_err());
    assert!(a.adopt(stale).is_err());
    a.adopt(retry.compute().unwrap()).unwrap();
    let stale = a
        .begin(&source, Context::LivePreview, NativeActive::default())
        .unwrap()
        .compute()
        .unwrap();
    assert!(a
        .begin(&source, Context::Unknown, NativeActive::default())
        .is_err());
    assert!(a.adopt(stale).is_err());
    let old_root = a
        .begin_root(source.clone(), Context::LivePreview)
        .unwrap()
        .compute()
        .unwrap();
    assert!(a.begin_root(source.clone(), Context::Unknown).is_err());
    assert!(a.adopt_root(old_root).is_err());
    accept(&mut a, &source);
    let stale = a
        .begin(&source, Context::LivePreview, NativeActive::default())
        .unwrap()
        .compute()
        .unwrap();
    let second = Snapshot::new("note", 2, "prefix\n\n# title\n\nend\n");
    let third = Snapshot::new("note", 3, "more prefix\n\n# title\n\nend\n");
    let old = a
        .begin_root(second, Context::LivePreview)
        .unwrap()
        .compute()
        .unwrap();
    let current = a.begin_root(third.clone(), Context::LivePreview).unwrap();
    assert!(a.adopt_root(old).is_err());
    assert!(a.adopt(stale).is_err());
    a.adopt_root(current.compute().unwrap()).unwrap();
    let now = publish(&mut a, &third, NativeActive::default());
    assert!(held
        .view(
            &source,
            Context::LivePreview,
            &NativeActive::default(),
            &held.identity()
        )
        .is_ok());
    assert!(held
        .view(
            &third,
            Context::LivePreview,
            &NativeActive::default(),
            &held.identity()
        )
        .is_err());
    assert_ne!(held.identity(), now.identity());
    for bad in [
        Snapshot::new("note", 4, "\0"),
        Snapshot::new("note", 4, "x".repeat(65537)),
    ] {
        assert!(a.begin_root(bad, Context::LivePreview).is_err());
        assert!(a.current().is_none());
    }
}
