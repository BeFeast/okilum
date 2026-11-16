//! Actual editor frames; deterministic source-work and geometry controls only.
use super::*;
use ::core::prelude::v1::test;
use gpui::{px, Bounds, Pixels};
use gpui_component::input::{Redo, Undo};

fn source() -> String {
    // Keep the proven two-long-rows + short-row scenario, including a complete
    // Unicode-correct shape and a TAB path rather than an ASCII-only fixture.
    format!(
        "//{{ {}שלום🙂\n// {}\tend\nshort\n",
        "x".repeat(300 * 1024),
        "y".repeat(300 * 1024)
    )
}

fn draw(v: &mut VisualTestContext) {
    v.update(|w, cx| {
        w.simulate_next_frame(cx);
        w.draw(cx).clear(cx);
    });
}

fn prepare(f: &tests::Fixture, v: &mut VisualTestContext) {
    f.input.update_in(v, |i, w, cx| {
        i.set_soft_wrap(false, w, cx);
        i.enable_layout_diagnostics();
        i.focus(w, cx);
    });
    tests::settle(v);
    draw(v);
    // The production syntax adapter debounces for 150 ms; establish its
    // published styles before comparing cold and reused geometry.
    v.executor().advance_clock(Duration::from_millis(151));
    v.run_until_parked();
    draw(v);
}

#[gpui::test]
fn edit2803_short_edit_reuses_unchanged_unicode_and_tab_rows(cx: &mut TestAppContext) {
    let original = source();
    let (f, v) = tests::open_file(cx, &original, "rows.rs");
    prepare(&f, v);
    let at = original.rfind("short").unwrap();
    f.input.update(v, |i, cx| i.set_selected_range(at..at, cx));
    draw(v);
    let before = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
    // A retained frame must fire the existing complete-frame cache first.
    f.input.update(v, |_, cx| cx.notify());
    draw(v);
    let cached = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
    assert!(cached.source_shape_cache_hits > before.source_shape_cache_hits);
    assert_eq!(cached.layout_calls, before.layout_calls);

    f.input
        .update_in(v, |i, w, cx| i.replace_source_range(at..at, "!", w, cx));
    draw(v);
    let edited = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
    assert!(
        edited.layout_calls > cached.layout_calls,
        "changed-row path must fire"
    );
    assert!(edited.source_row_materialized_bytes > cached.source_row_materialized_bytes);
    assert_eq!(
        edited.source_row_materialized_bytes - cached.source_row_materialized_bytes,
        "!short".len() as u64,
        "only changed row bytes may materialize; unchanged large rows must reuse layouts"
    );
    assert!(edited.source_row_cache_hits >= cached.source_row_cache_hits + 2);
    let expected = format!("{}!{}", &original[..at], &original[at..]);
    assert_eq!(f.input.read_with(v, |i, _| i.value().to_string()), expected);
    v.dispatch_action(Undo);
    v.run_until_parked();
    draw(v);
    assert_eq!(f.input.read_with(v, |i, _| i.value().to_string()), original);
    v.dispatch_action(Redo);
    v.run_until_parked();
    draw(v);
    assert_eq!(f.input.read_with(v, |i, _| i.value().to_string()), expected);

    // Use the real Edit -> Read -> Edit boundary, not a forced cache release.
    f.reader.update_in(v, |r, w, cx| r.toggle_source(w, cx));
    tests::settle(v);
    f.reader.update_in(v, |r, w, cx| r.toggle_source(w, cx));
    tests::settle(v);
    let reopened = f.reader.read_with(v, |r, cx| {
        r.editing
            .as_ref()
            .unwrap()
            .input
            .read(cx)
            .value()
            .to_string()
    });
    assert_eq!(reopened, expected);
    assert_eq!(
        std::fs::read_to_string(f.directory.path().join("vault/rows.rs")).unwrap(),
        expected
    );
}

fn geometry(
    f: &tests::Fixture,
    v: &mut VisualTestContext,
    at: usize,
) -> (Bounds<Pixels>, Pixels, Vec<Option<usize>>) {
    f.input.update(v, |i, cx| i.set_selected_range(at..at, cx));
    draw(v);
    f.input.read_with(v, |i, _| {
        assert_eq!(i.selected_range(), at..at);
        let (bounds, height) = i.cursor_layout().expect("actual painted caret");
        let points =
            [-2., -0.5, 0., 0.5, 2.].map(|dx| bounds.origin + gpui::point(px(dx), height / 2.));
        let hits = points.map(|p| i.source_offset_for_point(p)).to_vec();
        assert!(
            hits.iter().any(Option::is_some),
            "real hit testing must fire"
        );
        (bounds, height, hits)
    })
}

#[gpui::test]
fn edit2803_reused_row_geometry_matches_cold_layout_and_styles_invalidate(cx: &mut TestAppContext) {
    let original = source();
    let (f, v) = tests::open_file(cx, &original, "geometry.rs");
    prepare(&f, v);
    for at in [
        original.find("שלום").unwrap() + 2,
        original.find("\tend").unwrap() + 1,
    ] {
        let warm = geometry(&f, v, at);
        let before = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
        // A style/language round trip gives a cold Rust layout while keeping
        // the viewport and scroll state used by this strict geometry oracle.
        for language in ["plain", "rust"] {
            f.input.update(v, |i, cx| i.set_highlighter(language, cx));
            draw(v);
            v.executor().advance_clock(Duration::from_millis(151));
            v.run_until_parked();
            draw(v);
        }
        let after = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
        assert!(after.source_row_materialized_bytes > before.source_row_materialized_bytes);
        assert_eq!(
            geometry(&f, v, at),
            warm,
            "strict far-right caret and hit geometry"
        );
    }
    // Resize is a separate admission control: a viewport round trip changes
    // horizontal scroll state on main too, so it is not the geometry oracle.
    let before = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
    let viewport = v.update(|w, _| w.viewport_size());
    v.simulate_resize(gpui::size(viewport.width + px(80.), viewport.height));
    draw(v);
    v.simulate_resize(viewport);
    draw(v);
    let after = f.input.read_with(v, |i, _| i.layout_diagnostics().unwrap());
    assert!(
        after.source_row_materialized_bytes > before.source_row_materialized_bytes,
        "viewport changes must invalidate row layouts"
    );
    assert_eq!(f.input.read_with(v, |i, _| i.value().to_string()), original);
}
