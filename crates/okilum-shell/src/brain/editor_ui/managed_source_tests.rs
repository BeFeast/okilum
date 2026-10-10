//! Real managed Editor mutations through the application subscription boundary.
use super::*;
use ::core::prelude::v1::test;
use gpui::EntityInputHandler;

#[gpui::test]
fn native_edits_and_preedit_coalesce_and_old_events_cannot_protect_reset_base(
    cx: &mut TestAppContext,
) {
    let (dir, workspace, base, store) = super::tests::fixture();
    cx.update(gpui_component::init);
    let (view, cx) = cx
        .add_window_view(|window, cx| BrainView::new_managed_test(&workspace, &store, window, cx));
    cx.run_until_parked();
    view.update_in(cx, |v, window, cx| {
        v.load_source(base.clone(), window, cx);
        v.busy = false;
        v.sync_source_policy(cx);
        let editor = v.source.managed().unwrap().clone();
        let epoch = v.editor.input_epoch;
        editor.update(cx, |s, cx| {
            s.replace_text_in_range(Some(0..0), "first", window, cx);
            s.replace_and_mark_text_in_range(Some(0..5), "最新", None, window, cx);
        });
        assert_eq!(v.editor.input_epoch, epoch, "subscription is still queued");
    });
    cx.run_until_parked();
    view.update_in(cx, |v, window, cx| {
        assert_eq!(
            v.editor.input_epoch, 1,
            "only the latest native event applies"
        );
        assert!(v.editor.confirmed);
        assert_eq!(
            store.list().unwrap().drafts[0].text,
            v.source.value(cx).as_ref()
        );
        let epoch = v.editor.input_epoch;
        v.source.managed().unwrap().update(cx, |s, cx| {
            s.replace_text_in_range(Some(0..0), "obsolete edit", window, cx);
        });
        v.load_source(base.clone(), window, cx);
        v.source.reset("restored draft\r\n", window, cx);
        v.schedule_preview(window, cx);
        assert_eq!(v.editor.input_epoch, epoch);
    });
    cx.run_until_parked();
    view.update_in(cx, |v, _, cx| {
        assert_eq!(v.editor.input_epoch, 1, "reset invalidates queued old Edit");
        assert_eq!(v.source.value(cx).as_ref(), "restored draft\r\n");
        assert!(store
            .list()
            .unwrap()
            .drafts
            .iter()
            .all(|d| d.text != decode_source(&base).unwrap()));
    });
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
fn readonly_blocks_native_mutation_same_turn_and_flight_remains_editable(cx: &mut TestAppContext) {
    let (dir, workspace, base, store) = super::tests::fixture();
    cx.update(gpui_component::init);
    let (view, cx) = cx
        .add_window_view(|window, cx| BrainView::new_managed_test(&workspace, &store, window, cx));
    cx.run_until_parked();
    view.update_in(cx, |v, window, cx| {
        v.load_source(base, window, cx);
        let editor = v.source.managed().unwrap().clone();
        for barrier in 0..3 {
            v.busy = barrier == 0;
            v.source_loading = barrier == 1;
            v.source_editable = barrier != 2;
            v.sync_source_policy(cx);
            let before = v.source.value(cx);
            let stamp = v.source.stamp(cx);
            editor.update(cx, |s, cx| {
                s.replace_text_in_range(Some(0..0), "blocked", window, cx);
                s.replace_and_mark_text_in_range(Some(0..0), "blocked IME", None, window, cx);
            });
            assert_eq!(v.source.value(cx), before);
            assert_eq!(v.source.stamp(cx), stamp);
        }
        v.busy = false;
        v.source_loading = false;
        v.source_editable = true;
        v.editor.flight = true;
        v.sync_source_policy(cx);
        let before = v.source.stamp(cx);
        editor.update(cx, |s, cx| {
            s.replace_text_in_range(Some(0..0), "accepted", window, cx)
        });
        assert_ne!(
            v.source.stamp(cx),
            before,
            "positive control: ordinary flight permits typing"
        );
        // Typed at byte zero of a BOM note: after the BOM (#1093).
        assert!(v
            .source
            .value(cx)
            .trim_start_matches('\u{feff}')
            .starts_with("accepted"));
        v.editor.flight = false;
    });
    cx.run_until_parked();
    assert!(store.list().unwrap().drafts[0]
        .text
        .trim_start_matches('\u{feff}')
        .starts_with("accepted"));
    std::fs::remove_dir_all(dir).unwrap();
}
