use super::*;
use ::core::prelude::v1::test;
use sha2::{Digest, Sha256};

fn setup(
    cx: &mut TestAppContext,
) -> (
    Entity<BrainView>,
    &mut VisualTestContext,
    std::path::PathBuf,
) {
    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
    });
    let directory = std::env::temp_dir().join(format!("find277-{}", uuid()));
    let workspace = json!({"brain_id":uuid(),"root":"/isolated/find277","records_dir":"records","managed":true});
    let store = editor_recovery::EditorRecovery::at(directory.clone(), &workspace).unwrap();
    let (view, visual) = cx
        .add_window_view(|window, cx| BrainView::new_managed_test(&workspace, &store, window, cx));
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        let original = "\u{feff}# Find\r\n😀 needle\r\nOther needle\r\n";
        let base = json!({"schema":SCHEMA,"brain_id":workspace["brain_id"],"path":"find.md",
            "revision":format!("sha256:{:x}",Sha256::digest(original.as_bytes())),
            "content_base64":STANDARD.encode(original),"media_type":"text/markdown"});
        v.busy = false;
        v.surface = Surface::Source;
        v.capabilities = json!({"source_write":true});
        v.load_source(base, window, cx);
        v.sync_source_policy(cx);
        v.source.focus(window, cx);
    });
    visual.run_until_parked();
    (view, visual, directory)
}

fn query(v: &mut BrainView, text: &str, window: &mut Window, cx: &mut Context<BrainView>) {
    let query = v.source_find.panel.as_ref().unwrap().query.clone();
    query.update(cx, |q, cx| {
        q.set_value(text.to_owned(), window, cx);
        q.focus(window, cx);
    });
    v.refresh_source_find(window, cx);
}

#[gpui::test]
fn find_keys_preserve_exact_dirty_bytes_direction_and_undo(cx: &mut TestAppContext) {
    let (view, visual, directory) = setup(cx);
    let (original, dirty, stamp, cursor) = view.update_in(visual, |v, window, cx| {
        let original = v.source.value(cx);
        v.source.managed().unwrap().update(cx, |s, cx| {
            s.replace_text_in_range(Some(0..0), "unsaved ", window, cx);
            s.set_selected_range(Range { start: 7, end: 0 }, cx);
        });
        v.source_projection.live = true;
        v.schedule_source_projection(window, cx);
        (
            original,
            v.source.value(cx),
            v.source.stamp(cx),
            v.find_cursor(window, cx).unwrap(),
        )
    });
    visual.run_until_parked();
    #[cfg(not(target_os = "macos"))]
    visual.simulate_keystrokes("ctrl-f");
    #[cfg(target_os = "macos")]
    visual.simulate_keystrokes("cmd-f");
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        assert!(v.source_find.active(), "real platform shortcut opened Find");
        assert!(v.source_projection.live, "previous LP mode is retained");
        query(v, "needle", window, cx);
    });
    visual.run_until_parked();
    view.update_in(visual, |v, _, cx| {
        let p = v.source_find.panel.as_ref().unwrap();
        assert_eq!(p.ranges.len(), 2);
        assert!(!p.pending);
        assert_eq!(p.current, None, "completion cannot move the caret");
        assert_eq!(v.source.stamp(cx), stamp);
    });
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        let p = v.source_find.panel.as_ref().unwrap();
        assert_eq!(p.current, Some(0));
        assert!(p.query.focus_handle(cx).is_focused(window));
        assert_eq!(
            v.source_find.highlight.as_ref().unwrap().get_ranges(cx),
            vec![p.ranges[0].clone()]
        );
        assert_eq!(
            &v.source.value(cx)[v.find_cursor(window, cx).unwrap().bytes],
            "needle"
        );
    });
    visual.simulate_keystrokes("shift-enter");
    visual.run_until_parked();
    view.update_in(visual, |v, _, _| {
        assert_eq!(v.source_find.panel.as_ref().unwrap().current, Some(1))
    });
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    view.update_in(visual, |v, _, _| {
        assert_eq!(v.source_find.panel.as_ref().unwrap().current, Some(0))
    });
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        assert!(!v.source_find.active());
        assert!(v
            .source_find
            .highlight
            .as_ref()
            .unwrap()
            .get_ranges(cx)
            .is_empty());
        assert_eq!(v.find_cursor(window, cx).unwrap(), cursor);
        assert_eq!(v.source.stamp(cx), stamp);
        assert_eq!(v.source.value(cx), dirty);
        assert!(v.source_projection.live);
        assert!(v.pending_source_write.is_none());
    });
    #[cfg(not(target_os = "macos"))]
    visual.simulate_keystrokes("ctrl-z");
    #[cfg(target_os = "macos")]
    visual.simulate_keystrokes("cmd-z");
    visual.run_until_parked();
    view.update_in(visual, |v, _, cx| assert_eq!(v.source.value(cx), original));
    #[cfg(not(target_os = "macos"))]
    visual.simulate_keystrokes("ctrl-y");
    #[cfg(target_os = "macos")]
    visual.simulate_keystrokes("cmd-shift-z");
    visual.run_until_parked();
    view.update_in(visual, |v, _, cx| assert_eq!(v.source.value(cx), dirty));
    std::fs::remove_dir_all(directory).unwrap();
}

#[gpui::test]
fn find_edit_query_races_readonly_and_owner_reset_never_apply_old_ranges(cx: &mut TestAppContext) {
    let (view, visual, directory) = setup(cx);
    view.update_in(visual, |v, window, cx| {
        v.open_source_find(window, cx);
        query(v, "needle", window, cx);
        query(v, "missing", window, cx);
        query(v, "needle", window, cx);
    });
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        assert_eq!(v.source_find.panel.as_ref().unwrap().ranges.len(), 2);
        v.step_source_find(false, window, cx);
        v.source.managed().unwrap().update(cx, |s, cx| {
            s.replace_text_in_range(Some(0..0), "needle ", window, cx);
        });
        let caret = v.find_cursor(window, cx).unwrap();
        v.step_source_find(false, window, cx);
        assert_eq!(
            v.find_cursor(window, cx).unwrap(),
            caret,
            "stale ranges cannot navigate before queued mutation is delivered"
        );
        assert!(v.source_find.panel.as_ref().unwrap().ranges.is_empty());
        assert!(v
            .source_find
            .highlight
            .as_ref()
            .unwrap()
            .get_ranges(cx)
            .is_empty());
        assert!(v.source_find.panel.as_ref().unwrap().pending);
    });
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        assert_eq!(v.source_find.panel.as_ref().unwrap().ranges.len(), 3);
        assert_eq!(v.source_find.panel.as_ref().unwrap().current, None);
        let caret = v.find_cursor(window, cx).unwrap();
        v.close_source_find(window, cx);
        assert_eq!(
            v.find_cursor(window, cx).unwrap(),
            caret,
            "editing suppresses old selection restoration"
        );
        v.source_editable = false;
        v.sync_source_policy(cx);
        let before = v.source.value(cx);
        v.open_source_find(window, cx);
        query(v, "needle", window, cx);
        assert_eq!(v.source.value(cx), before);
    });
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        let before = v.source.value(cx);
        v.step_source_find(false, window, cx);
        assert_eq!(v.source_find.panel.as_ref().unwrap().current, Some(0));
        assert_eq!(v.source.value(cx), before);
        assert!(!v.source.managed().unwrap().read(cx).is_editable());
        query(v, "Other", window, cx);
        v.source_snapshot.as_mut().unwrap()["path"] = json!("different.md");
        v.sync_source_find(window, cx);
        assert!(!v.source_find.active());
    });
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        assert!(
            !v.source_find.active(),
            "old worker cannot reopen a different owner"
        );
        v.open_source_find(window, cx);
        query(v, "needle", window, cx);
        v.source.reset("reset without an owner change", window, cx);
    });
    visual.run_until_parked();
    view.update_in(visual, |v, _, _| {
        assert!(!v.source_find.active(), "reset clears the old query")
    });
    std::fs::remove_dir_all(directory).unwrap();
}

#[gpui::test]
fn find_composition_and_explicit_limits_are_guarded(cx: &mut TestAppContext) {
    let (view, visual, directory) = setup(cx);
    view.update_in(visual, |v, window, cx| {
        v.source.managed().unwrap().update(cx, |s, cx| {
            s.replace_and_mark_text_in_range(Some(0..0), "最新", None, window, cx)
        });
        v.open_source_find(window, cx);
        assert!(!v.source_find.active());
        assert!(v
            .source
            .managed()
            .unwrap()
            .update(cx, |s, cx| s.marked_text_range(window, cx).is_some()));
        v.source.managed().unwrap().update(cx, |s, cx| {
            s.replace_text_in_range(None, "最新", window, cx)
        });
    });
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        v.open_source_find(window, cx);
        let q = v.source_find.panel.as_ref().unwrap().query.clone();
        q.update(cx, |q, cx| {
            q.replace_and_mark_text_in_range(Some(0..0), "入力", None, window, cx)
        });
        v.close_source_find(window, cx);
        v.open_source_find(window, cx);
        assert!(v.source_find.active());
        assert!(q.update(cx, |q, cx| q.marked_text_range(window, cx).is_some()));
        q.update(cx, |q, cx| {
            q.replace_text_in_range(None, "入力", window, cx)
        });
        query(v, &"x".repeat(MAX_QUERY_BYTES + 1), window, cx);
        let p = v.source_find.panel.as_ref().unwrap();
        assert!(p.message.unwrap().contains("2048"));
        assert!(!p.pending && p.ranges.is_empty());
        query(v, "", window, cx);
        assert!(!v.source_find.panel.as_ref().unwrap().pending);
    });
    visual.run_until_parked();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn find_long_source_and_worst_case_range_memory_measurement() {
    let long = format!(
        "start needle\r\n{}😀 needle",
        "ordinary line\r\n".repeat(10_000)
    );
    let ranges = search_ranges(Rope::from(long.as_str()), "needle");
    assert!(long.len() > 64 * 1024);
    assert_eq!(ranges.len(), 2);
    assert_eq!(&long[ranges[1].clone()], "needle");
    let started = std::time::Instant::now();
    let ranges = search_ranges(Rope::from("a".repeat(MAX_SOURCE_BYTES)), "a");
    assert_eq!(ranges.len(), MAX_SOURCE_BYTES);
    assert_eq!(
        ranges.last(),
        Some(&(MAX_SOURCE_BYTES - 1..MAX_SOURCE_BYTES))
    );
    eprintln!("find277 worst-case: source_bytes={MAX_SOURCE_BYTES} matches={} range_capacity_bytes={} elapsed_ms={}",
        ranges.len(), ranges.capacity() * std::mem::size_of::<Range<usize>>(), started.elapsed().as_millis());
}
