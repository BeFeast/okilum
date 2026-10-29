use super::*;
use gpui_component::input::projection::{LayoutStamp, PinnedProjection, SourceStamp};
use okilum_core::source_classifier::presentation::Session;

fn source(generation: u64, text: &str) -> SourceSnapshot {
    SourceSnapshot {
        stamp: SourceStamp {
            document: 77,
            generation,
        },
        text: Arc::from(text),
    }
}
fn publication(source: &SourceSnapshot, active: &ActiveSource) -> Publication {
    let snapshot = core_snapshot(source);
    let mut session = Session::default();
    let root = session
        .begin_root(snapshot.clone(), Context::LivePreview)
        .unwrap();
    session.adopt_root(root.compute().unwrap()).unwrap();
    let request = session
        .begin(&snapshot, Context::LivePreview, core_active(active))
        .unwrap();
    session.adopt(request.compute().unwrap()).unwrap()
}
fn provider(
    source: &SourceSnapshot,
    active: &ActiveSource,
    colors: Option<ProjectionColors>,
) -> PreparedProvider {
    let publication = publication(source, active);
    let id = publication.identity();
    PreparedProvider::new(source.clone(), active.clone(), publication, id, colors).unwrap()
}

#[test]
fn actual_trait_consumer_matches_all_legacy_metadata_and_mapping_with_positive_controls() {
    let mut seen = [false; 4];
    for text in [
        "# Heading\n\n**bold** *emphasis* ~~strike~~ `code` [[note|label]]\n\n- bullet\n- [ ] task\n\n> quote\n\n---\n\n```rust\nlet x = 1;\n```\n\nend\n",
        "# Heading\n\n| a | b |\n| - | - |\n| c | d |\n\n![alt](image.png)\n\n> [!note] Title\n> body\n\nend\n",
        "\u{feff}# e\u{301} 🧠\r\n\r\n**👩‍💻** 🇮🇱\r\n\r\nend\r\n",
        "---\nname: value\n---\n\n[x][id]\n\n[id]: /target 'title'\n\nend\n",
    ] {
        let source = source(1, text);
        let legacy = CachedProvider::classify(source.clone());
        for active in [
            ActiveSource::default(),
            ActiveSource { anchor: SourceByte(text.len()), head: SourceByte(text.len()), ..Default::default() },
            ActiveSource { anchor: SourceByte(text.len()), head: SourceByte(0), ..Default::default() },
            ActiveSource { composition: Some(SourceByte(0)..SourceByte(text.len())), ..Default::default() },
            ActiveSource { replacement: Some(SourceByte(0)..SourceByte(text.len())), ..Default::default() },
        ] {
            for colors in [None, Some(ProjectionColors { heading: gpui::hsla(0., 1., 0.5, 1.), link: gpui::hsla(0.66, 1., 0.5, 1.), muted: gpui::hsla(0.33, 1., 0.5, 1.) })] {
                let provider = provider(&source, &active, colors);
                let expected = legacy.compose_colored(&source, &active, colors, crate::source_presentation::CODE_FONT).unwrap();
                let actual = ProjectionProvider::compose(&provider, &source, &active).unwrap();
                assert_eq!(actual.text(), expected.text());
                assert_eq!(actual.styles(), expected.styles());
                assert_eq!(actual.line_scales(), expected.line_scales());
                assert_eq!(actual.blocks(), expected.blocks());
                assert_eq!(format!("{:?}", actual.markers()), format!("{:?}", expected.markers()));
                seen[0] |= !actual.styles().is_empty();
                seen[1] |= !actual.line_scales().is_empty();
                seen[2] |= !actual.markers().is_empty();
                seen[3] |= !actual.blocks().is_empty();
                for byte in 0..=text.len()+1 {
                    for bias in [ConcealBias::Left, ConcealBias::Right] {
                        assert_eq!(actual.to_projected(SourceByte(byte), bias), expected.to_projected(SourceByte(byte), bias));
                    }
                    assert_eq!(actual.source_boundary(&source, SourceByte(byte)), expected.source_boundary(&source, SourceByte(byte)));
                }
                for byte in 0..=actual.text().len()+1 {
                    for bias in [ConcealBias::Left, ConcealBias::Right] {
                        assert_eq!(actual.to_source(ProjectedByte(byte), bias), expected.to_source(ProjectedByte(byte), bias));
                    }
                }
                for marker in actual.markers() {
                    assert_eq!(actual.marker_scope_is_raw(&marker.scope), expected.marker_scope_is_raw(&marker.scope));
                }
                let builds = PREPARED_BUILDS.with(|v| v.get());
                let repeated = provider.compose(&source, &active).unwrap();
                assert!(Arc::ptr_eq(&actual, &repeated), "compose returns the actual prepared immutable trait object");
                assert_eq!(PREPARED_BUILDS.with(|v| v.get()), builds);
            }
        }
    }
    assert_eq!(
        seen, [true; 4],
        "nonempty style/heading/marker/block controls"
    );
}

#[test]
fn trait_lease_refuses_changed_bytes_stamps_full_active_and_foreign_publication() {
    let source = source(1, "# heading\n\nend\n");
    let active = ActiveSource {
        anchor: SourceByte(0),
        head: SourceByte(3),
        ..Default::default()
    };
    let publication = publication(&source, &active);
    let foreign = self::publication(&source, &active);
    assert!(PreparedProvider::new(
        source.clone(),
        active.clone(),
        publication.clone(),
        foreign.identity(),
        None
    )
    .is_none());
    let provider = PreparedProvider::new(
        source.clone(),
        active.clone(),
        publication.clone(),
        publication.identity(),
        None,
    )
    .unwrap();
    let before = PREPARED_BUILDS.with(|v| v.get());
    for other in [
        SourceSnapshot {
            stamp: source.stamp,
            text: Arc::from("# changed\n\nend\n"),
        },
        SourceSnapshot {
            stamp: SourceStamp {
                document: 99,
                ..source.stamp
            },
            text: source.text.clone(),
        },
        SourceSnapshot {
            stamp: SourceStamp {
                generation: 2,
                ..source.stamp
            },
            text: source.text.clone(),
        },
    ] {
        assert!(provider.compose(&other, &active).is_none());
    }
    for other in [
        ActiveSource {
            anchor: active.head,
            head: active.anchor,
            ..active.clone()
        },
        ActiveSource {
            affinity: WrapAffinity::End,
            ..active.clone()
        },
        ActiveSource {
            composition: Some(SourceByte(0)..SourceByte(3)),
            ..active.clone()
        },
        ActiveSource {
            replacement: Some(SourceByte(0)..SourceByte(3)),
            ..active.clone()
        },
    ] {
        assert!(provider.compose(&source, &other).is_none());
        assert!(PreparedProvider::new(
            source.clone(),
            other,
            publication.clone(),
            publication.identity(),
            None
        )
        .is_none());
    }
    assert_eq!(
        PREPARED_BUILDS.with(|v| v.get()),
        before,
        "failed leases do not prepare a new result"
    );
}

#[test]
fn held_native_pin_keeps_its_epoch_but_never_authorizes_a_new_source_or_layout() {
    let old = source(1, "- bullet\n\nend\n");
    let active = ActiveSource {
        anchor: SourceByte(old.text.len()),
        head: SourceByte(old.text.len()),
        ..Default::default()
    };
    let provider = provider(&old, &active, None);
    let projection = provider.compose(&old, &active).unwrap();
    let layout = LayoutStamp {
        source: old.stamp,
        presentation: 17,
    };
    let pin = PinnedProjection {
        stamp: layout,
        projection,
    };
    let scope = pin.projection.markers()[0].scope.clone();
    assert!(!pin.marker_scope_is_raw(layout, &old, &scope, &active));
    let other_layout = LayoutStamp {
        presentation: 18,
        ..layout
    };
    assert!(pin.marker_scope_is_raw(other_layout, &old, &scope, &active));
    let second = source(2, "prefix\n\n- bullet\n\nend\n");
    let third = source(3, "more prefix\n\n- bullet\n\nend\n");
    for current in [&second, &third] {
        assert!(provider.compose(current, &active).is_none());
        assert!(pin.marker_scope_is_raw(layout, current, &scope, &active));
        assert!(pin
            .projection
            .source_boundary(current, SourceByte(0))
            .is_none());
    }
    assert_eq!(pin.projection.source().stamp, old.stamp);
    assert!(!pin.marker_scope_is_raw(layout, &old, &scope, &active));
}

#[test]
fn invalid_ime_and_oversize_inputs_never_prepare_an_adapter() {
    let source = source(1, "e\u{301} 🧠\r\nend\n");
    let snapshot = core_snapshot(&source);
    let mut session = Session::default();
    let root = session
        .begin_root(snapshot.clone(), Context::LivePreview)
        .unwrap();
    session.adopt_root(root.compute().unwrap()).unwrap();
    let before = PREPARED_BUILDS.with(|v| v.get());
    for offset in [
        1,
        source.text.find('🧠').unwrap() + 1,
        source.text.find("\r\n").unwrap() + 1,
    ] {
        let active = ActiveSource {
            composition: Some(SourceByte(offset)..SourceByte(offset)),
            replacement: Some(SourceByte(0)..SourceByte(source.text.len())),
            ..Default::default()
        };
        assert!(session
            .begin(&snapshot, Context::LivePreview, core_active(&active))
            .is_err());
        assert!(CachedProvider::classify(source.clone())
            .compose(&source, &active)
            .is_none());
    }
    let oversize = self::source(2, &"x".repeat(source_classifier::MAX_BYTES + 1));
    assert!(session
        .begin_root(core_snapshot(&oversize), Context::LivePreview)
        .is_err());
    assert_eq!(PREPARED_BUILDS.with(|v| v.get()), before);
}
