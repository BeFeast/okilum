//! Default-size regressions from native synthetic demo QA (#2448).
use super::guided_tests::{guided_click, guided_frame};
use super::*;
use ::core::prelude::v1::test;
use okilum_core::remote_log::fake::{FakeKibana, Scenario};

fn visible(bounds: Bounds<Pixels>, viewport: Size<Pixels>) -> bool {
    bounds.size.width > px(0.)
        && bounds.size.height > px(0.)
        && bounds.left() >= px(0.)
        && bounds.top() >= px(0.)
        && bounds.right() <= viewport.width
        && bounds.bottom() <= viewport.height
}

#[gpui::test]
async fn fallback_scope_overlay_keeps_rows_and_header_count_range_wraps(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let server = FakeKibana::start(
        "127.0.0.1:0",
        12,
        Scenario {
            catalog_status: Some(403),
            ..Default::default()
        },
    )
    .unwrap();
    let mut source = Source::direct(
        "Synthetic",
        &server.endpoint,
        "remote-*:events",
        "@timestamp",
    )
    .unwrap();
    source.discover.time = TimeRange {
        from: "2026-11-01T00:00:00Z".into(),
        to: "2026-11-05T00:00:00Z".into(),
    };
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        reader_ui_state::install(dir.path(), cx);
    });
    let mut panel = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| Panel::new(dir.path().into(), dir.path().into(), window, cx));
        panel = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    let panel = panel.unwrap();
    let viewport = size(px(1200.), px(800.));
    visual.simulate_resize(viewport);
    panel.update_in(visual, |panel, window, cx| {
        panel.guided.test_key = Some(b"test-secret".to_vec());
        panel.choose(source, window, cx);
        panel.apply(window, cx);
    });
    visual
        .condition(&panel, |panel, _| {
            !panel.busy && panel.view.is_some() && panel.guided.catalog.is_some()
        })
        .await;
    panel.read_with(visual, |panel, _| {
        assert!(
            panel.guided.discovery.as_ref().unwrap().fallback,
            "positive control: catalog 403 exercised real fallback discovery"
        );
        assert_eq!(panel.data.as_ref().unwrap().events.len(), 12);
    });
    // Pinning opens the same picker as choosing the field through the menu.
    panel.update(visual, |panel, cx| panel.pin_scope("category".into(), cx));
    visual
        .condition(&panel, |panel, _| {
            panel.guided.counts.contains_key("category")
        })
        .await;
    guided_frame(visual);
    let overlay = visual.debug_bounds("remote-scope-overlay").unwrap();
    assert!(visible(overlay, viewport));
    let open_row = visual.debug_bounds("log-message-cell-0").unwrap();
    assert!(
        visible(open_row, viewport),
        "open picker must leave rows laid out: {open_row:?}"
    );
    assert!(
        overlay.right() < open_row.right(),
        "rows remain visible alongside the overlay"
    );
    // The predicate must reject a row beyond the viewport, not merely find a selector.
    let mut offscreen = open_row;
    offscreen.origin.y = viewport.height;
    assert!(
        !visible(offscreen, viewport),
        "positive control: hidden rows fail the bounds gate"
    );
    guided_click(visual, "remote-scope-close");
    let closed_row = visual.debug_bounds("log-message-cell-0").unwrap();
    assert_eq!(
        open_row, closed_row,
        "opening the picker cannot consume table height"
    );
    guided_click(visual, "remote-scope-0");
    guided_click(visual, "remote-scope-value-0");
    visual
        .condition(&panel, |panel, _| {
            !panel.busy
                && panel.guided.open_scope.is_none()
                && panel.guided.catalog.as_ref().is_some_and(Result::is_ok)
        })
        .await;
    panel.read_with(visual, |panel, _| {
        assert_eq!(panel.data.as_ref().unwrap().events.len(), 6)
    });

    let view = panel.read_with(visual, |panel, _| panel.view.clone().unwrap());
    view.update(visual, |view, cx| view.toggle_column("category", cx));
    guided_click(visual, "log-header-filter-0");
    let picker = view.read_with(visual, |view, _| view.filter_picker_for_test().unwrap());
    visual
        .condition(&picker, |picker, _| picker.counts_for_test().is_some())
        .await;
    picker.read_with(visual, |picker, _| {
        let counts = picker.counts_for_test().unwrap().unwrap();
        assert!(counts.label.starts_with("Kibana counts for window"));
        assert!(counts.label.contains("2026-11-05T00:00:00Z"));
    });
    guided_frame(visual);
    let summary = visual.debug_bounds("log-count-scope").unwrap();
    let popup = visual.debug_bounds("log-value-picker").unwrap();
    assert!(visible(summary, viewport));
    assert!(summary.left() >= popup.left() && summary.right() <= popup.right());
    assert!(
        summary.size.height > px(32.),
        "full range and metadata wrap over multiple lines: {summary:?}"
    );
    guided_click(visual, "log-count-loaded");
    visual
        .condition(&picker, |picker, _| picker.counts_for_test().is_some())
        .await;
    picker.read_with(visual, |picker, _| {
        assert!(picker
            .counts_for_test()
            .unwrap()
            .unwrap()
            .label
            .to_lowercase()
            .contains("loaded"))
    });
    guided_frame(visual);
    assert!(visible(
        visual.debug_bounds("log-count-scope").unwrap(),
        viewport
    ));
    visual.simulate_keystrokes("escape");
    guided_frame(visual);
    cx.quit();
}
