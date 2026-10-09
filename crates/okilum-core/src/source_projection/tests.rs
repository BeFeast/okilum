// Coordinate APIs intentionally receive arrays containing one Range.
#![allow(clippy::single_range_in_vec_init)]
use super::*;

fn snapshot(text: &str) -> Snapshot {
    Snapshot::new("managed/brain/note.md", 7, text)
}
fn region(block: Range<usize>, markers: &[Range<usize>]) -> Region {
    Region::Conceal {
        block,
        markers: markers.into(),
    }
}
fn assert_roundtrips(snapshot: &Snapshot, p: &Projection) {
    for s in boundaries(snapshot.source()) {
        let d = p.source_to_display(snapshot, s).unwrap();
        let left = p.display_to_source(snapshot, d, Bias::Left).unwrap();
        let right = p.display_to_source(snapshot, d, Bias::Right).unwrap();
        assert!(
            left <= s && s <= right,
            "source {s} escaped anchor {left}..{right}"
        );
        assert_eq!(p.source_to_display(snapshot, left).unwrap(), d);
        assert_eq!(p.source_to_display(snapshot, right).unwrap(), d);
    }
    for d in boundaries(p.display()) {
        for bias in [Bias::Left, Bias::Right] {
            let s = p.display_to_source(snapshot, d, bias).unwrap();
            assert_eq!(p.source_to_display(snapshot, s).unwrap(), d);
        }
    }
}
#[test]
fn supplied_heading_emphasis_and_wikilink_spans_preserve_authored_bytes() {
    let text = "# Title\r\n\r\n**bold** and [[Target|label]]\r\nLast";
    let source = snapshot(text);
    let bold = text.find("**bold**").unwrap();
    let link = text.find("[[Target|").unwrap();
    let end = text.find("]]\r\n").unwrap();
    let plan = Plan::new(
        &source,
        vec![
            region(0..9, &[0..2]),
            region(
                bold..end + 4,
                &[
                    bold..bold + 2,
                    bold + 6..bold + 8,
                    link..link + 9,
                    end..end + 2,
                ],
            ),
        ],
    );
    let p = project(&source, &plan, &Active::default()).unwrap();
    assert_eq!(p.display(), "Title\r\n\r\nbold and label\r\nLast");
    assert_eq!(source.source(), text);
    assert_roundtrips(&source, &p);
    // Plain clipboard authority includes authored syntax and exact terminators.
    assert_eq!(
        source.copy_source(link..end + 2).unwrap(),
        "[[Target|label]]"
    );
}
#[test]
fn adjacent_hidden_ranges_have_one_explicit_bias_and_reveal_both_blocks() {
    let source = snapshot("**a****b**");
    let plan = Plan::new(
        &source,
        vec![region(0..5, &[0..2, 3..5]), region(5..10, &[5..7, 8..10])],
    );
    let p = project(&source, &plan, &Active::default()).unwrap();
    assert_eq!(p.display(), "ab");
    for (d, left, right) in [(0, 0, 2), (1, 3, 7), (2, 8, 10)] {
        assert_eq!(p.display_to_source(&source, d, Bias::Left).unwrap(), left);
        assert_eq!(p.display_to_source(&source, d, Bias::Right).unwrap(), right);
    }
    assert_roundtrips(&source, &p);
    let p = project(
        &source,
        &plan,
        &Active {
            selection: Some(5..5),
            composition: None,
        },
    )
    .unwrap();
    assert_eq!(p.display(), source.source());
    assert_eq!(p.source_to_display(&source, 5).unwrap(), 5);
}
#[test]
fn selection_and_composition_reveal_only_intersected_blocks() {
    let source = snapshot("**one**\n\n**two**\n\n**three**");
    let plan = Plan::new(
        &source,
        vec![
            region(0..7, &[0..2, 5..7]),
            region(9..16, &[9..11, 14..16]),
            region(18..27, &[18..20, 25..27]),
        ],
    );
    let p = project(
        &source,
        &plan,
        &Active {
            selection: Some(3..4),
            composition: Some(12..12),
        },
    )
    .unwrap();
    assert_eq!(p.display(), "**one**\n\n**two**\n\nthree");
    let all = project(
        &source,
        &plan,
        &Active {
            selection: Some(4..23),
            composition: None,
        },
    )
    .unwrap();
    assert_eq!(all.display(), source.source());
    assert_roundtrips(&source, &p);
}
#[test]
fn projected_selection_reveals_before_exact_source_copy_and_replacement() {
    let source = snapshot("before **bold** after\r\n");
    let plan = Plan::new(
        &source,
        vec![region(0..source.source().len(), &[7..9, 13..15])],
    );
    let p = project(&source, &plan, &Active::default()).unwrap();
    assert_eq!(p.display(), "before bold after\r\n");
    let active = p
        .reveal_selection(&source, 7..11, Bias::Left, Bias::Right)
        .unwrap();
    assert_eq!(active.selection, Some(7..15));
    let revealed = project(&source, &plan, &active).unwrap();
    assert_eq!(revealed.display(), source.source());
    assert_eq!(
        source
            .copy_source(active.selection.clone().unwrap())
            .unwrap(),
        "**bold**"
    );
    let edited = source
        .replace_source(&source, active.selection.unwrap(), "_новое 🧠_")
        .unwrap();
    assert_eq!(edited.source(), "before _новое 🧠_ after\r\n");
    assert_eq!(edited.document(), source.document());
    assert_eq!(edited.generation(), 8);
    let fallback = project(&edited, &plan, &Active::default()).unwrap_err();
    assert_eq!(fallback.reason, FallbackReason::StaleSnapshot);
    assert_eq!(fallback.source(), edited.source());
    assert_eq!(
        p.source_to_display(&edited, 0),
        Err(MapError::StaleSnapshot)
    );
}
#[test]
fn canonical_edits_preserve_unchanged_bytes_and_cannot_replay_on_newer_snapshot() {
    let source = snapshot("\u{feff}first\r\nlast");
    let edit = source.replace_source(&source, 3..8, "edited").unwrap();
    assert_eq!(edit.source(), "\u{feff}edited\r\nlast");
    assert_eq!(
        source.replace_source(&edit, 3..8, "wrong"),
        Err(MapError::StaleSnapshot)
    );
    let restored = edit.replace_source(&edit, 3..9, "first").unwrap();
    assert_eq!(restored.source(), source.source());
    assert_eq!(
        restored.generation(),
        9,
        "text returning to old bytes does not revive an old generation"
    );
    assert_eq!(
        source.replace_source(&restored, 3..8, "wrong"),
        Err(MapError::StaleSnapshot)
    );
}
#[test]
fn grapheme_boundaries_cover_combining_zwj_flags_and_multibyte_source() {
    for body in ["e\u{301}", "👩‍👩‍👧‍👦", "🇮🇱", "🧠", "אבג", "中文"] {
        let text = format!("**{body}**\r\n");
        let source = snapshot(&text);
        let end = 2 + body.len();
        let plan = Plan::new(&source, vec![region(0..text.len(), &[0..2, end..end + 2])]);
        let p = project(&source, &plan, &Active::default()).unwrap();
        assert_eq!(p.display(), format!("{body}\r\n"));
        assert_roundtrips(&source, &p);
        for offset in 0..=body.len() {
            if !boundaries(body).contains(&offset) {
                assert_eq!(
                    p.display_to_source(&source, offset, Bias::Left),
                    Err(MapError::InvalidBoundary)
                );
            }
        }
    }
}
#[test]
fn concealment_cannot_join_distinct_source_graphemes_into_a_display_flag() {
    let source = snapshot("🇮**🇱");
    let plan = Plan::new(&source, vec![region(0..source.source().len(), &[4..6])]);
    let fallback = project(&source, &plan, &Active::default()).unwrap_err();
    assert_eq!(fallback.reason, FallbackReason::JoinedGrapheme);
    assert_eq!(fallback.source(), "🇮**🇱");
    // Positive control: ordinary characters on either side have a stable anchor.
    let source = snapshot("a**b");
    let plan = Plan::new(&source, vec![region(0..4, &[1..3])]);
    assert_eq!(
        project(&source, &plan, &Active::default())
            .unwrap()
            .display(),
        "ab"
    );
}
#[test]
fn crlf_bom_and_final_newline_are_not_projection_markers_or_split_targets() {
    let source = snapshot("\u{feff}**a**\r\nlast");
    let length = source.source().len();
    for (marker, reason) in [
        (0..3, FallbackReason::ProtectedSource),
        (8..10, FallbackReason::ProtectedSource),
        (8..9, FallbackReason::InvalidBoundary),
    ] {
        let plan = Plan::new(&source, vec![region(0..length, &[marker])]);
        assert_eq!(
            project(&source, &plan, &Active::default())
                .unwrap_err()
                .reason,
            reason
        );
    }
    assert_eq!(source.copy_source(9..10), Err(MapError::InvalidBoundary));
    assert_eq!(
        source.replace_source(&source, 9..9, "x"),
        Err(MapError::InvalidBoundary)
    );
    let plan = Plan::new(&source, vec![region(0..length, &[3..5, 6..8])]);
    let p = project(&source, &plan, &Active::default()).unwrap();
    assert_eq!(p.display(), "\u{feff}a\r\nlast");
    assert_roundtrips(&source, &p);
}
#[test]
fn unsupported_regions_and_uncovered_text_stay_exact_source() {
    let text = "---\r\nkey: true\r\n---\r\n\n```rust\na<b\n```\n|a|b|\n![[embed]]\n**last**";
    let source = snapshot(text);
    let last = text.find("**last**").unwrap();
    let plan = Plan::new(
        &source,
        vec![
            Region::Source(0..last),
            region(last..text.len(), &[last..last + 2, last + 6..last + 8]),
        ],
    );
    let p = project(&source, &plan, &Active::default()).unwrap();
    assert_eq!(p.display(), format!("{}last", &text[..last]));
    assert_eq!(source.copy_source(0..last).unwrap(), &text[..last]);
}
#[test]
fn malformed_overlapping_unsorted_or_split_grapheme_ranges_fall_back_whole() {
    let source = snapshot("a\u{301}bc**d**");
    let len = source.source().len();
    for regions in [
        vec![region(0..len, &[0..1])],
        vec![region(0..len, &[0..usize::MAX])],
        vec![region(0..len, &[5..7, 6..8])],
        vec![region(0..len, &[7..9, 5..7])],
        vec![region(0..5, &[5..7])],
        vec![Region::Source(0..5), Region::Source(3..len)],
        vec![Region::Source(5..len), Region::Source(0..3)],
        vec![region(0..len, &[3..3])],
    ] {
        let fallback =
            project(&source, &Plan::new(&source, regions), &Active::default()).unwrap_err();
        assert_eq!(fallback.source(), source.source());
        assert!(matches!(
            fallback.reason,
            FallbackReason::InvalidBoundary | FallbackReason::OverlappingRanges
        ));
    }
}
#[test]
fn invalid_selection_or_ime_ranges_do_not_guess_a_nearby_boundary() {
    let source = snapshot("e\u{301}**x**");
    let plan = Plan::new(&source, vec![]);
    for active in [
        Active {
            selection: Some(1..1),
            composition: None,
        },
        Active {
            selection: None,
            composition: Some(3..usize::MAX),
        },
        Active {
            selection: Some(Range { start: 4, end: 3 }),
            composition: None,
        },
    ] {
        assert_eq!(
            project(&source, &plan, &active).unwrap_err().reason,
            FallbackReason::InvalidBoundary
        );
    }
}
#[test]
fn changed_document_generation_or_same_generation_bytes_invalidate_maps() {
    let source = snapshot("**old**");
    let plan = Plan::new(&source, vec![region(0..7, &[0..2, 5..7])]);
    let p = project(&source, &plan, &Active::default()).unwrap();
    for current in [
        Snapshot::new("other", 7, "**old**"),
        Snapshot::new(source.document(), 8, "**old**"),
        Snapshot::new(source.document(), 7, "**new**"),
    ] {
        let fallback = project(&current, &plan, &Active::default()).unwrap_err();
        assert_eq!(fallback.reason, FallbackReason::StaleSnapshot);
        assert_eq!(fallback.source(), current.source());
        assert_eq!(
            p.display_to_source(&current, 0, Bias::Right),
            Err(MapError::StaleSnapshot)
        );
    }
    let same = Snapshot::new(source.document(), 7, "**old**");
    assert_eq!(p.display_to_source(&same, 0, Bias::Right), Ok(2));
}
#[test]
fn hard_byte_range_and_work_limits_return_exact_source_with_positive_controls() {
    let source = snapshot(&"a".repeat(MAX_BYTES));
    let plan = Plan::new(&source, vec![]);
    assert_eq!(
        project(&source, &plan, &Active::default())
            .unwrap()
            .display(),
        source.source()
    );
    let large = snapshot(&"b".repeat(MAX_BYTES + 1));
    let fallback = project(&large, &Plan::new(&large, vec![]), &Active::default()).unwrap_err();
    assert_eq!(fallback.reason, FallbackReason::SourceLimit);
    assert_eq!(fallback.source(), large.source());
    assert_eq!(
        source.replace_source(&source, 0..0, "x"),
        Err(MapError::SourceLimit)
    );
    let source = snapshot(&"a".repeat(MAX_RANGES));
    let plan = Plan::new(
        &source,
        (0..MAX_RANGES).map(|n| Region::Source(n..n + 1)).collect(),
    );
    assert!(project(&source, &plan, &Active::default()).is_ok());
    let excessive = Plan::new(
        &source,
        vec![region(
            0..MAX_RANGES,
            &(0..MAX_RANGES).map(|n| n..n + 1).collect::<Vec<_>>(),
        )],
    );
    assert_eq!(
        project(&source, &excessive, &Active::default())
            .unwrap_err()
            .reason,
        FallbackReason::RangeLimit
    );
    assert_eq!(
        project_with_work_budget(&source, &plan, &Active::default(), 0)
            .unwrap_err()
            .reason,
        FallbackReason::WorkLimit
    );
    assert!(project_with_work_budget(&source, &plan, &Active::default(), usize::MAX).is_ok());
}
#[test]
fn collapsed_visual_caret_cannot_expand_into_hidden_source_by_mixed_bias() {
    let source = snapshot("**x**");
    let plan = Plan::new(&source, vec![region(0..5, &[0..2, 3..5])]);
    let p = project(&source, &plan, &Active::default()).unwrap();
    assert!(matches!(
        p.reveal_selection(&source, 0..0, Bias::Left, Bias::Right),
        Err(MapError::InvalidBoundary)
    ));
    let caret = p
        .reveal_selection(&source, 0..0, Bias::Right, Bias::Right)
        .unwrap();
    assert_eq!(caret.selection, Some(2..2));
    assert_eq!(
        project(&source, &plan, &caret).unwrap().display(),
        source.source()
    );
}
#[test]
fn empty_source_has_one_identity_boundary_and_generation_overflow_is_refused() {
    let source = Snapshot::new("empty", u64::MAX, "");
    let plan = Plan::new(&source, vec![]);
    let p = project(&source, &plan, &Active::default()).unwrap();
    assert_roundtrips(&source, &p);
    assert_eq!(source.copy_source(0..0), Ok(String::new()));
    assert_eq!(
        source.replace_source(&source, 0..0, "a"),
        Err(MapError::GenerationOverflow)
    );
}

#[test]
fn every_small_conceal_geometry_roundtrips_and_reveals_before_editing() {
    // An independent byte-removal oracle covers no markers, whole concealment,
    // prefixes, suffixes, separated and adjacent anchors, without Markdown parsing.
    let source = snapshot("abcdef");
    for mask in 0u8..64 {
        let markers: Vec<_> = (0..6)
            .filter(|i| mask & (1 << i) != 0)
            .map(|i| i..i + 1)
            .collect();
        let plan = Plan::new(&source, vec![region(0..6, &markers)]);
        let p = project(&source, &plan, &Active::default()).unwrap();
        let expected: String = "abcdef"
            .chars()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) == 0)
            .map(|(_, c)| c)
            .collect();
        assert_eq!(p.display(), expected);
        assert_roundtrips(&source, &p);
        for offset in 0..=expected.len() {
            for bias in [Bias::Left, Bias::Right] {
                let active = p
                    .reveal_selection(&source, offset..offset, bias, bias)
                    .unwrap();
                let canonical_caret = active.selection.clone().unwrap();
                assert!(canonical_caret.is_empty());
                let revealed = project(&source, &plan, &active).unwrap();
                assert_eq!(revealed.display(), source.source());
                let edited = source
                    .replace_source(&source, canonical_caret.clone(), "X")
                    .unwrap();
                let mut expected_edit = String::from("abcdef");
                expected_edit.insert(canonical_caret.start, 'X');
                assert_eq!(edited.source(), expected_edit);
            }
        }
    }
}
#[test]
fn document_identity_limits_preserve_current_source() {
    for document in [String::new(), "x".repeat(MAX_DOCUMENT_ID_BYTES + 1)] {
        let source = Snapshot::new(document, 1, "authored bytes\r\n");
        let fallback =
            project(&source, &Plan::new(&source, vec![]), &Active::default()).unwrap_err();
        assert_eq!(fallback.reason, FallbackReason::IdentityLimit);
        assert_eq!(fallback.source(), "authored bytes\r\n");
    }
}
