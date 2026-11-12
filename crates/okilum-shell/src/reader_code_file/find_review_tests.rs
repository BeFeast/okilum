//! Adversarial regression coverage from the independent step-five review.
use super::view_tests::{fixture, settle};
use super::*;
use ::core::prelude::v1::test;
use std::sync::atomic::{AtomicBool, Ordering};

#[gpui::test]
fn code2810_root_separator_hits_never_borrow_a_visible_row_label(cx: &mut TestAppContext) {
    let source = r#"{"id":1,"name":"some long name value here","x":2}"#;
    let (code, visual, _temp) = fixture(cx, "data.json", source);
    code.update_in(visual, |code, window, cx| {
        code.set_view(View::Tree, window, cx)
    });
    let target = code.read_with(visual, |s, cx| s.find_target(code.clone(), cx));
    visual.update(|window, cx| target.set_query(",", window, cx));
    settle(visual);
    code.update_in(visual, |code, window, cx| {
        assert_eq!(code.matches.len(), 2);
        assert_eq!(
            code.document.as_ref().unwrap().row_at(&code.matches[1]),
            Some(0)
        );
        code.step_match(1, window, cx);
    });
    // Paint before checking ownership: restoring the original owner AND slice
    // behavior must reproduce the original UI-thread slice panic.
    settle(visual);
    code.read_with(visual, |code, _| {
        assert_eq!(
            code.structure.match_node, None,
            "hidden-root hits do not belong to row one"
        );
    });
    assert!(visual.debug_bounds("data-node-label-1").is_some());
}

#[test]
fn code2810_hit_label_rejects_disjoint_and_invalid_utf8_ranges() {
    crate::source_presentation::code_languages::install();
    let doc = Document::new(
        r#"{"id":1,"name":"some long name value here","x":2}"#.into(),
        "json",
    );
    for hit in [42..43, 500..501, std::ops::Range { start: 8, end: 7 }] {
        assert_eq!(doc.row_hit_label(1, &hit), doc.row_label(1));
    }
    let unicode = Document::new(r#"{"value":"é"}"#.into(), "json");
    let middle = unicode.text.find('é').unwrap() + 1;
    assert_eq!(
        unicode.row_hit_label(1, &(middle..middle + 1)),
        unicode.row_label(1)
    );
}

#[gpui::test]
fn code2810_whole_query_compile_failure_is_visible_and_finishes(cx: &mut TestAppContext) {
    let (code, visual, _temp) = fixture(cx, "data.json", r#"{"needle":1}"#);
    code.update_in(visual, |s, window, cx| s.set_view(View::Tree, window, cx));
    let target = code.read_with(visual, |s, cx| s.find_target(code.clone(), cx));
    visual.update(|window, cx| target.set_query(&"x".repeat(1_000_000), window, cx));
    visual.run_until_parked();
    visual
        .executor()
        .advance_clock(std::time::Duration::from_millis(70));
    visual.run_until_parked();
    code.read_with(visual, |s, cx| {
        assert!(!s.searching);
        assert!(s.matches.is_empty());
        assert_eq!(
            s.find_target(code.clone(), cx).count.unwrap().as_ref(),
            "Query is too long"
        );
    });
}

#[gpui::test]
fn code2810_whole_find_burst_and_duplicate_view_search_complete_one_scan(cx: &mut TestAppContext) {
    let (code, visual, _temp) = fixture(cx, "data.json", r#"{"value":"needle"}"#);
    code.update_in(visual, |s, window, cx| s.set_view(View::Tree, window, cx));
    let before = code.read_with(visual, |s, _| s.search_scans.load(Ordering::SeqCst));
    for end in 1..=6 {
        let target = code.read_with(visual, |s, cx| s.find_target(code.clone(), cx));
        visual.update(|window, cx| target.set_query(&"needle"[..end], window, cx));
        code.update_in(visual, |s, window, cx| {
            s.search(window, cx);
            s.search(window, cx);
        });
        visual.run_until_parked();
        visual
            .executor()
            .advance_clock(std::time::Duration::from_millis(5));
    }
    code.read_with(visual, |s, _| {
        assert_eq!(s.search_scans.load(Ordering::SeqCst), before)
    });
    settle(visual);
    code.read_with(visual, |s, _| {
        assert_eq!(s.search_scans.load(Ordering::SeqCst) - before, 1);
        assert_eq!(s.matches.len(), 1);
        assert!(!s.searching);
    });
}

#[test]
fn code2810_cancelled_snapshot_never_builds_a_result() {
    crate::source_presentation::code_languages::install();
    let doc = Document::new(r#"{"value":"needle"}"#.into(), "json");
    assert!(super::find::scan_snapshot(&doc, "needle", false, &AtomicBool::new(true)).is_none());
    assert_eq!(
        super::find::scan_snapshot(&doc, "needle", false, &AtomicBool::new(false))
            .unwrap()
            .matches
            .len(),
        1
    );
}

#[gpui::test]
fn code2810_paged_enter_bounds_hits_with_logarithmic_work(cx: &mut TestAppContext) {
    let source = format!("{{\"value\":\"{}\"}}", "x".repeat(1_000_000));
    let (code, visual, _temp) = fixture(cx, "data.json", &source);
    code.update_in(visual, |s, window, cx| {
        s.set_view(View::Raw, window, cx);
        s.matches = s.document.as_ref().unwrap().find("x");
        s.match_index = s.matches.len() / 2;
        super::find::PAGE_PROBES.with(|probes| probes.set(0));
        s.step_match(1, window, cx);
        let probes = super::find::PAGE_PROBES.with(|probes| probes.get());
        assert!(
            (1..=44).contains(&probes),
            "one million hits require logarithmic page probes, got {probes}"
        );
        let hit = &s.matches[s.match_index];
        let page = &s.document.as_ref().unwrap().pages[s.page];
        assert_eq!(
            s.input().unwrap().read(cx).selected_range(),
            hit.start - page.start..hit.end - page.start
        );
    });
}

#[gpui::test]
fn code2810_active_find_raw_node_jump_survives_adapter_completion(cx: &mut TestAppContext) {
    let source = r#"{"a":"needle","node":{"other":42},"z":"needle"}"#;
    let (code, visual, _temp) = fixture(cx, "data.json", source);
    code.update_in(visual, |s, window, cx| s.set_view(View::Tree, window, cx));
    let target = code.read_with(visual, |s, cx| s.find_target(code.clone(), cx));
    visual.update(|window, cx| target.set_query("needle", window, cx));
    settle(visual);
    let expected = code.update_in(visual, |s, window, cx| {
        let doc = s.document.as_ref().unwrap();
        let index = (0..doc.structure.len())
            .find(|i| doc.key_text(*i) == Some("node"))
            .unwrap();
        let expected = doc.structure[index].range.clone();
        s.structure.selected = Some(index);
        s.show_selected_raw(window, cx);
        expected
    });
    settle(visual);
    code.read_with(visual, |s, cx| {
        assert_eq!(s.view, View::Raw);
        assert_eq!(s.input().unwrap().read(cx).selected_range(), expected);
        assert_eq!(
            s.editor_find.as_ref().unwrap().read(cx).count(cx).1,
            2,
            "the real adapter completed"
        );
    });
    // The retained Raw input already selects this node on a second visit.
    // Range equality alone cannot detect that explicit navigation happened.
    code.update_in(visual, |s, window, cx| s.set_view(View::Tree, window, cx));
    settle(visual);
    code.update_in(visual, |s, window, cx| {
        let doc = s.document.as_ref().unwrap();
        let index = (0..doc.structure.len())
            .find(|i| doc.key_text(*i) == Some("node"))
            .unwrap();
        assert_eq!(s.input().unwrap().read(cx).selected_range(), expected);
        s.structure.selected = Some(index);
        s.show_selected_raw(window, cx);
    });
    settle(visual);
    code.read_with(visual, |s, cx| {
        assert_eq!(s.input().unwrap().read(cx).selected_range(), expected);
        assert_eq!(s.editor_find.as_ref().unwrap().read(cx).count(cx).1, 2);
    });
}

#[gpui::test]
fn code2810_childless_json_does_not_offer_an_empty_tree(cx: &mut TestAppContext) {
    for source in ["{}", "[]", "42", "true", "null", "\"value\""] {
        let (code, visual, _temp) = fixture(cx, "data.json", source);
        code.update_in(visual, |s, window, cx| {
            assert_eq!(s.availability(View::Tree), Availability::Absent, "{source}");
            s.set_view(View::Tree, window, cx);
            assert_ne!(s.view, View::Tree);
        });
    }
}

#[test]
fn code2810_non_json_paged_tree_keeps_the_expanded_root() {
    crate::source_presentation::code_languages::install();
    let source = format!("<root><child>{}</child><other/></root>", "x".repeat(70_000));
    let doc = Document::for_file(source, "xml", false);
    assert!(!doc.json_tree && doc.pages.len() > 1 && doc.structure.len() > 1);
    let mut tree = structure::Structure::default();
    tree.reset(&doc);
    assert!(!tree.folded(0));
    assert!(tree.visible.len() > 1);
}

#[gpui::test]
fn code2810_scrollbar_uses_visible_dedicated_roles_and_track(cx: &mut TestAppContext) {
    let source = format!("{{\"value\":\"{}\"}}", "needle ".repeat(1001));
    let (code, visual, _temp) = fixture(cx, "data.json", &source);
    code.update_in(visual, |s, window, cx| s.set_view(View::Tree, window, cx));
    let target = code.read_with(visual, |s, cx| s.find_target(code.clone(), cx));
    visual.update(|window, cx| target.set_query("needle", window, cx));
    settle(visual);
    structure::MARK_PAINTS.with(|paints| paints.borrow_mut().clear());
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    code.read_with(visual, |s, _| {
        assert_eq!(s.matches.len(), 1001);
        assert_eq!(s.scrollbar_marks.len(), 1000, "the sample cap is unchanged");
    });
    let track = visual.debug_bounds("data-find-mark-track").unwrap();
    assert!(track.size.width >= px(6.));
    let action = visual
        .debug_bounds("data-node-source-1")
        .expect("selected hit's raw action");
    assert!(
        action.right() <= track.left(),
        "marks must not cover row actions"
    );
    visual.update(|_, cx| {
        let palette = brand::reader_palette(cx);
        structure::MARK_PAINTS.with(|paints| {
            let paints = paints.borrow();
            assert!(!paints.is_empty());
            let frames = paints.iter().filter(|(role, _)| *role == 0).count();
            assert!(frames > 0);
            assert_eq!(
                paints.iter().filter(|(role, _)| *role == 1).count(),
                frames,
                "one opaque other-hit tick per coincident position, not 1000"
            );
            for (role, color) in [
                (0, palette.find_mark_track),
                (1, palette.find_mark),
                (2, palette.find_mark_current),
            ] {
                assert!(
                    paints.contains(&(role, color)),
                    "the real track/ticks/current paint requests must exist"
                );
                assert!(paints
                    .iter()
                    .filter(|(r, _)| *r == role)
                    .all(|(_, actual)| *actual == color));
            }
        });
    });
}

#[gpui::test]
fn code2810_editor_rejects_source_stamp_change_while_worker_is_held(cx: &mut TestAppContext) {
    let (code, visual, _temp) = fixture(cx, "code.rs", "needle");
    let input = code.read_with(visual, |s, _| s.input().unwrap());
    let adapter = code.read_with(visual, |s, _| s.editor_find.clone().unwrap());
    let (release, gate) = futures::channel::oneshot::channel();
    adapter.update_in(visual, |s, window, cx| {
        s.worker_gate = Some(gate);
        s.set_query("needle", window, cx);
    });
    visual.run_until_parked();
    visual
        .executor()
        .advance_clock(std::time::Duration::from_millis(70));
    visual.run_until_parked();
    adapter.update(visual, |s, _| {
        assert_eq!(
            s.scans_started.load(Ordering::SeqCst),
            1,
            "the old snapshot's worker is in flight"
        );
        // Isolate stamp rejection from the independent mutation cancellation.
        s.disconnect_source_updates_for_test();
    });
    input.update_in(visual, |s, window, cx| {
        s.set_value("different text", window, cx)
    });
    release.send(()).unwrap();
    settle(visual);
    assert_eq!(
        adapter.read_with(visual, |s, cx| s.count(cx).1),
        0,
        "held old-source ranges cannot be installed"
    );
}

#[gpui::test]
fn code2810_editor_rejects_old_generation_even_if_cancellation_is_defeated(
    cx: &mut TestAppContext,
) {
    let (code, visual, _temp) = fixture(cx, "code.rs", "needle other other");
    let adapter = code.read_with(visual, |s, _| s.editor_find.clone().unwrap());
    let (release, gate) = futures::channel::oneshot::channel();
    adapter.update_in(visual, |s, window, cx| {
        s.worker_gate = Some(gate);
        s.set_query("needle", window, cx);
    });
    visual.run_until_parked();
    visual
        .executor()
        .advance_clock(std::time::Duration::from_millis(70));
    visual.run_until_parked();
    adapter.update(visual, |s, _| {
        assert_eq!(s.scans_started.load(Ordering::SeqCst), 1);
        // Keep the old continuation alive solely to exercise generation guard.
        s.detach_pending_for_test();
    });
    adapter.update_in(visual, |s, window, cx| s.set_query("other", window, cx));
    settle(visual);
    assert_eq!(adapter.read_with(visual, |s, cx| s.count(cx).1), 2);
    release.send(()).unwrap();
    settle(visual);
    assert_eq!(
        adapter.read_with(visual, |s, cx| s.count(cx).1),
        2,
        "old generation cannot replace the new query"
    );
}

#[gpui::test]
fn code2810_editor_worker_respects_a_later_explicit_selection(cx: &mut TestAppContext) {
    let (code, visual, _temp) = fixture(cx, "code.rs", "needle selected node needle");
    let input = code.read_with(visual, |s, _| s.input().unwrap());
    let adapter = code.read_with(visual, |s, _| s.editor_find.clone().unwrap());
    let (release, gate) = futures::channel::oneshot::channel();
    adapter.update_in(visual, |s, window, cx| {
        s.worker_gate = Some(gate);
        s.set_query("needle", window, cx);
    });
    visual.run_until_parked();
    visual
        .executor()
        .advance_clock(std::time::Duration::from_millis(70));
    visual.run_until_parked();
    assert_eq!(
        adapter.read_with(visual, |s, _| s.scans_started.load(Ordering::SeqCst)),
        1
    );
    input.update(visual, |s, cx| s.set_selected_range(7..20, cx));
    release.send(()).unwrap();
    settle(visual);
    assert_eq!(adapter.read_with(visual, |s, cx| s.count(cx).1), 2);
    assert_eq!(input.read_with(visual, |s, _| s.selected_range()), 7..20);
}

#[test]
fn code2810_mark_segments_preserve_the_exact_tick_union_and_gaps() {
    let bounds = Bounds::new(point(px(0.), px(0.)), size(px(8.), px(32.)));
    let fractions = [0., 0., 0.03125, 0.0625, 0.5, 0.53125, 1.];
    let segments = structure::mark_segments(&fractions, bounds);
    assert_eq!(
        segments,
        [px(0.)..px(5.), px(16.)..px(20.), px(29.)..px(32.)]
    );
    for quarter in 0..128 {
        let y = px(quarter as f32 / 4.);
        let original = fractions.iter().any(|fraction| {
            let top = (bounds.size.height * *fraction).min(px(29.));
            (top..top + px(3.)).contains(&y)
        });
        assert_eq!(segments.iter().any(|r| r.contains(&y)), original);
    }
    assert!(structure::mark_segments(&[], bounds).is_empty());
}

#[gpui::test]
fn code2810_edit_entry_cancels_tree_find_before_debounce(cx: &mut TestAppContext) {
    let (reader, visual, temp) = crate::reader_viewer_chrome::tests::fixture_with_files(
        cx,
        "data.json",
        &[("data.json", r#"{"value":"needle other"}"#)],
    );
    let code = reader.read_with(visual, |r, _| {
        r.file_preview.as_ref().unwrap().code.clone().unwrap()
    });
    code.update_in(visual, |s, w, cx| s.set_view(View::Tree, w, cx));
    let target = code.read_with(visual, |s, cx| s.find_target(code.clone(), cx));
    visual.update(|w, cx| target.set_query("other", w, cx));
    settle(visual);
    let before = code.read_with(visual, |s, _| {
        assert_eq!(
            s.matches.len(),
            1,
            "positive control: real search installed"
        );
        (
            s.search_scans_started.load(Ordering::SeqCst),
            s.search_installs,
        )
    });
    visual.update(|w, cx| target.set_query("needle", w, cx));
    visual.run_until_parked();
    code.read_with(visual, |s, _| {
        assert!(s.searching && s.search_task.is_some())
    });
    reader.update_in(visual, |r, w, cx| r.toggle_source(w, cx));
    settle(visual);
    assert!(
        reader.read_with(visual, |r, _| r.editing.is_some()),
        "real Edit entry completed"
    );
    code.read_with(visual, |s, _| {
        assert!(!s.read_active);
        assert_eq!(
            s.search_scans_started.load(Ordering::SeqCst),
            before.0,
            "Edit before debounce must start zero scans"
        );
        assert_eq!(
            s.search_installs, before.1,
            "suspended preview installs zero results"
        );
        assert!(!s.searching);
        assert!(s.search_task.is_none());
        assert!(s.matches.is_empty());
    });
    // The real retained-preview exit must restart the pending query in Read.
    reader.update_in(visual, |r, w, cx| {
        let input = r.editing.as_ref().unwrap().input().clone();
        input.update(cx, |input, cx| input.replace_source_range(0..0, " ", w, cx));
        std::fs::write(
            temp.path().join("vault/data.json"),
            r#"{"incoming":"needle other"}"#,
        )
        .unwrap();
        assert!(!r.save_source(cx), "real conflict enables draft parking");
    });
    settle(visual);
    let park = visual
        .debug_bounds("source-park")
        .expect("real draft-park button")
        .center();
    visual.simulate_click(park, Modifiers::default());
    settle(visual);
    assert!(reader.read_with(visual, |r, _| r.editing.is_none()));
    code.read_with(visual, |s, _| {
        assert!(s.read_active);
        assert_eq!(s.search_scans_started.load(Ordering::SeqCst), before.0 + 1);
        assert_eq!(s.search_installs, before.1 + 1);
        assert_eq!(
            s.matches.len(),
            1,
            "Read resumes the retained pending query"
        );
    });
}

#[test]
fn code2810_snapshot_cancellation_interrupts_an_in_flight_scan() {
    crate::source_presentation::code_languages::install();
    let doc = Document::new(
        format!("{{\"value\":\"{}\"}}", "needle ".repeat(1000)),
        "json",
    );
    let cancelled = AtomicBool::new(false);
    let mut visited = 0;
    let result =
        super::find::scan_snapshot_with_progress(&doc, "needle", false, &cancelled, || {
            visited += 1;
            cancelled.store(true, Ordering::SeqCst);
        });
    assert_eq!(visited, 1, "cancellation interrupts the actual hit loop");
    assert!(
        result.is_none(),
        "an interrupted scan cannot build a result"
    );
}

#[gpui::test]
fn code2810_raw_jump_steps_from_the_external_selection(cx: &mut TestAppContext) {
    let source = r#"{"a":"needle","node":{"other":42},"z":"needle"}"#;
    let (code, visual, _temp) = fixture(cx, "data.json", source);
    code.update_in(visual, |s, w, cx| s.set_view(View::Tree, w, cx));
    let target = code.read_with(visual, |s, cx| s.find_target(code.clone(), cx));
    visual.update(|w, cx| target.set_query("needle", w, cx));
    settle(visual);
    let node = code.update_in(visual, |s, w, cx| {
        let doc = s.document.as_ref().unwrap();
        let row = (0..doc.structure.len())
            .find(|i| doc.key_text(*i) == Some("node"))
            .unwrap();
        let range = doc.structure[row].range.clone();
        s.structure.selected = Some(row);
        s.show_selected_raw(w, cx);
        range
    });
    settle(visual);
    let input = code.read_with(visual, |s, _| s.input().unwrap());
    assert_eq!(input.read_with(visual, |s, _| s.selected_range()), node);
    let target = code.read_with(visual, |s, cx| s.find_target(code.clone(), cx));
    target.input.update_in(visual, |s, w, cx| s.focus(w, cx));
    settle(visual);
    let adapter = code.read_with(visual, |s, _| s.editor_find.clone().unwrap());
    let scans = adapter.read_with(visual, |s, _| s.scans_started.load(Ordering::SeqCst));
    visual.simulate_keystrokes("enter");
    settle(visual);
    let first = source.find("needle").unwrap();
    let last = source.rfind("needle").unwrap();
    assert_eq!(
        input.read_with(visual, |s, _| s.selected_range()),
        last..last + 6,
        "Next after a node jump lands on z, not the first hit"
    );
    assert_eq!(
        adapter.read_with(visual, |s, _| s.scans_started.load(Ordering::SeqCst)),
        scans,
        "Enter must not rescan an unchanged query"
    );
    input.update(visual, |s, cx| s.set_selected_range(node.clone(), cx));
    visual.simulate_keystrokes("shift-enter");
    settle(visual);
    assert_eq!(
        input.read_with(visual, |s, _| s.selected_range()),
        first..first + 6,
        "Previous after external selection lands before the caret"
    );
    visual.simulate_keystrokes("enter");
    settle(visual);
    assert_eq!(
        input.read_with(visual, |s, _| s.selected_range()),
        last..last + 6,
        "ordinary subsequent steps still advance from the revealed hit"
    );
    input.update(visual, |s, cx| {
        s.set_selected_range(source.len()..source.len(), cx)
    });
    visual.simulate_keystrokes("enter");
    settle(visual);
    assert_eq!(
        input.read_with(visual, |s, _| s.selected_range()),
        first..first + 6,
        "Next wraps only when no hit follows the external caret"
    );
}
