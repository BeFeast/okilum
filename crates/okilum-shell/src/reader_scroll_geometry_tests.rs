//! Reader wheel regressions assert painted text, not only the list's offset.
use super::{bind_keys, reader_ui_state, Opts, Reader, Root};
use gpui::{point, px, size, AppContext as _, ScrollDelta, ScrollWheelEvent, TouchPhase};

#[gpui::test]
fn wheel_crossing_document_top_preserves_painted_text_motion(cx: &mut gpui::TestAppContext) {
    let fixture = tempfile::tempdir().unwrap();
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault).unwrap();
    std::fs::write(
        vault.join("scroll.md"),
        format!(
            "# Scroll anchor\n\n{}",
            "Paragraph for continuous scrolling.\n\n```rust\nlet n = 1;\n```\n\n".repeat(80)
        ),
    )
    .unwrap();
    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
        // Production captures wheel events to persist Reader state on the next
        // frame. Omitting the store can hide repaint-dependent layout defects.
        reader_ui_state::install(&fixture.path().join("state"), cx);
    });
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(vault.clone()),
                    note: Some("scroll.md".into()),
                    session_directory: Some(fixture.path().join("session")),
                    index_dir: Some(fixture.path().join("index")),
                    panel_settings_override: Some(fixture.path().join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    let reader = reader.unwrap();
    visual.simulate_resize(size(px(1600.), px(860.)));
    visual.run_until_parked();
    reader.update(visual, |reader, cx| {
        assert_eq!(reader.current_rel, "scroll.md", "loaded-note control");
        assert!(reader.editing.is_none(), "Read mode control");
        reader.panels = Default::default();
        cx.notify();
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let document = visual.debug_bounds("reader-document").unwrap();
    let column = visual.debug_bounds("reader-column").unwrap();
    let anchor = reader.read_with(visual, |reader, cx| {
        reader.content.read(cx).rendered_source_at(None).unwrap()
    });
    // Exercise both the TextView handler and the document-margin forwarding
    // handler, moving away from and back across the opening-space boundary.
    for x in [column.center().x, document.left() + px(20.)] {
        let mut expected_y = anchor.2.y;
        for delta in [-1., -1., -20., 20., 1., 1.] {
            visual.simulate_event(ScrollWheelEvent {
                position: point(x, document.top() + px(120.)),
                delta: ScrollDelta::Pixels(point(px(0.), px(delta))),
                touch_phase: TouchPhase::Moved,
                ..Default::default()
            });
            visual.update(|window, cx| window.simulate_next_frame(cx));
            visual.run_until_parked();
            // Any ordinary Reader repaint must retain the same geometry.
            reader.update(visual, |_, cx| cx.notify());
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
            expected_y += px(delta);
            let (painted, offset) = reader.read_with(visual, |reader, cx| {
                let state = reader.content.read(cx);
                (
                    state
                        .rendered_position_for_source(anchor.0, anchor.1)
                        .unwrap(),
                    state.list_state().logical_scroll_top(),
                )
            });
            if delta == -1. {
                assert!(
                    offset.item_ix > 0 || offset.offset_in_item > px(0.),
                    "wheel actually scrolled the document"
                );
            }
            // 0.1 px allows layout arithmetic noise, not a product tolerance.
            assert!(
                (painted.y - expected_y).abs() < px(0.1),
                "wheel delta={delta}, x={x:?}: painted={:?}, expected={expected_y:?}, logical={offset:?}",
                painted.y
            );
        }
    }
}
