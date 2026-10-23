//! Real event regressions for a MouseUp consumed by a compositor resize grab.
use super::*;
use ::core::prelude::v1::test;
use gpui::InputEvent as _;
use gpui_component::input::{Editor, EditorState};

fn with_reader(
    cx: &mut TestAppContext,
    check: impl FnOnce(Entity<Reader>, &mut VisualTestContext),
) {
    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
    });
    let temp = tempfile::tempdir().unwrap();
    let vault = temp.path().join("vault");
    std::fs::create_dir(&vault).unwrap();
    std::fs::write(vault.join("start.md"), "Alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima.\n\nSecond paragraph with more words.\n").unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(vault.clone()),
                    note: Some("start.md".into()),
                    session_directory: Some(temp.path().join("state")),
                    index_dir: Some(temp.path().join("index")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1200.), px(700.)));
    visual.update(|window, _| window.activate_window());
    visual.run_until_parked();
    visual.update(|window, _| {
        assert!(
            window.is_window_active(),
            "lifecycle positive control: active window"
        )
    });
    check(reader.unwrap(), visual);
}
fn reader_press_and_drag(
    reader: &Entity<Reader>,
    visual: &mut VisualTestContext,
) -> (Point<Pixels>, String) {
    let start = reader.read_with(visual, |reader, cx| {
        reader
            .content
            .read(cx)
            .list_state()
            .bounds_for_item(0)
            .unwrap()
            .origin
            + point(px(4.), px(10.))
    });
    visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(
        start + point(px(75.), px(0.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    visual.run_until_parked();
    let selection = reader.read_with(visual, |reader, cx| {
        let content = reader.content.read(cx);
        assert!(
            content.is_selecting(),
            "positive control: actual drag began"
        );
        content.selected_text()
    });
    assert!(
        !selection.is_empty() && selection.len() < 60,
        "partial selection: {selection:?}"
    );
    assert!(
        "Alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima."
            .contains(selection.trim()),
        "the selected text belongs to the fixture document"
    );
    (start, selection)
}

#[gpui::test]
#[ignore = "flaky on hosted Linux runners; tracked in #1549"]
fn lost_mouse_up_plain_motion_cannot_extend_read_selection(cx: &mut TestAppContext) {
    with_reader(cx, |reader, visual| {
        let (start, selected) = reader_press_and_drag(&reader, visual);
        // Deliberately omit MouseUp, as when the WM/compositor grabs the pointer.
        visual.simulate_mouse_move(start + point(px(180.), px(0.)), None, Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, cx| {
            let content = reader.content.read(cx);
            assert_eq!(
                content.selected_text(),
                selected,
                "no-button motion must not extend selection"
            );
            assert!(!content.is_selecting());
        });
        // A later held-button move cannot resurrect the cancelled gesture.
        visual.simulate_mouse_move(
            start + point(px(210.), px(0.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.run_until_parked();
        reader.read_with(visual, |reader, cx| {
            assert_eq!(reader.content.read(cx).selected_text(), selected)
        });
    });
}

#[gpui::test]
#[ignore = "flaky on hosted Linux runners; tracked in #1549"]
fn native_resize_focus_loss_and_pointer_leave_end_read_gestures(cx: &mut TestAppContext) {
    for signal in 0..3 {
        with_reader(cx, |reader, visual| {
            let (start, selected) = reader_press_and_drag(&reader, visual);
            match signal {
                0 => visual.simulate_resize(size(px(1100.), px(650.))),
                1 => visual.deactivate_window(),
                _ => visual.simulate_event(MouseExitEvent {
                    position: start,
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Modifiers::default(),
                }),
            }
            visual.run_until_parked();
            reader.read_with(visual, |reader, cx| {
                let content = reader.content.read(cx);
                assert!(!content.is_selecting(), "signal {signal} must end drag");
                assert_eq!(
                    content.selected_text(),
                    selected,
                    "signal {signal}: cancellation keeps selected text"
                );
            });
        });
    }
}

#[gpui::test]
#[ignore = "flaky on hosted Linux runners; tracked in #1549"]
fn cancellation_reflow_before_observer_delivery_keeps_selected_text(cx: &mut TestAppContext) {
    with_reader(cx, |reader, visual| {
        reader.update_in(visual, |reader, window, cx| {
            if !reader.panels.notes {
                reader.toggle_panel(reader_layout::Panel::Notes, window, cx);
            }
        });
        visual.run_until_parked();
        let (_, selected) = reader_press_and_drag(&reader, visual);
        visual.update(|window, cx| {
            // End uses the same cancellation path as resize/focus loss/leave.
            // Draw the changed layout within this update, before queued
            // SelectionChanged observers can update TextView's mirror flag.
            gpui_base::TextSelection::end(window, cx);
            assert!(
                reader.read(cx).content.read(cx).is_selecting(),
                "positive control: the observer has not run yet"
            );
            reader.update(cx, |reader, cx| {
                let width = reader.panel_widths.notes;
                reader.set_panel_width(reader_layout::Panel::Notes, width + 160.);
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear(cx);
            assert_eq!(
                reader.read(cx).content.read(cx).selected_text(),
                selected,
                "cancellation keeps the exact range even before observer delivery"
            );
        });
        visual.run_until_parked();
        reader.read_with(visual, |reader, cx| {
            let content = reader.content.read(cx);
            assert!(!content.is_selecting());
            assert_eq!(content.selected_text(), selected);
        });
    });
}

#[gpui::test]
#[ignore = "flaky on hosted Linux runners; tracked in #1549"]
fn cancellation_before_the_next_paint_keeps_the_last_drag_range(cx: &mut TestAppContext) {
    with_reader(cx, |reader, visual| {
        let (start, selected) = reader_press_and_drag(&reader, visual);
        visual.simulate_mouse_up(
            start + point(px(75.), px(0.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        // Direct dispatch in one update prevents a frame between the last
        // held-button move and cancellation of a compositor-consumed release.
        visual.update(|window, cx| {
            window.dispatch_event(
                MouseDownEvent {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                }
                .to_platform_input(),
                cx,
            );
            window.dispatch_event(
                MouseMoveEvent {
                    position: start + point(px(75.), px(0.)),
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Modifiers::default(),
                }
                .to_platform_input(),
                cx,
            );
            window.dispatch_event(
                MouseExitEvent {
                    position: start + point(px(75.), px(0.)),
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Modifiers::default(),
                }
                .to_platform_input(),
                cx,
            );
        });
        visual.run_until_parked();
        reader.read_with(visual, |reader, cx| {
            let content = reader.content.read(cx);
            assert!(!content.is_selecting());
            assert_eq!(
                content.selected_text(),
                selected,
                "unpainted cancellation keeps the exact range"
            );
        });
    });
}

struct EditorSurface {
    input: Entity<EditorState>,
}
impl Render for EditorSurface {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(Editor::new(&self.input).size_full())
    }
}

#[gpui::test]
fn lost_mouse_up_and_lifecycle_cancel_the_editor_gesture(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for signal in 0..4 {
        let mut input = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let state = cx.new(|cx| {
                let mut state = EditorState::new(window, cx);
                state.set_value(
                    "Alpha bravo charlie delta echo foxtrot golf hotel",
                    window,
                    cx,
                );
                state
            });
            input = Some(state.clone());
            let view = cx.new(|_| EditorSurface { input: state });
            Root::new(view, window, cx)
        });
        visual.simulate_resize(size(px(800.), px(400.)));
        visual.update(|window, _| window.activate_window());
        visual.run_until_parked();
        visual.update(|window, _| {
            assert!(
                window.is_window_active(),
                "lifecycle positive control: active editor window"
            )
        });
        let input = input.unwrap();
        let start = input.read_with(visual, |state, _| {
            state.range_to_bounds(&(0..1)).unwrap().center()
        });
        let end = input.read_with(visual, |state, _| {
            state.range_to_bounds(&(6..7)).unwrap().center()
        });
        let beyond = input.read_with(visual, |state, _| {
            state.range_to_bounds(&(20..21)).unwrap().center()
        });
        visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        let selected = input.read_with(visual, |state, _| state.selected_value().to_string());
        assert!(
            !selected.is_empty(),
            "positive control: editor selected text"
        );
        match signal {
            0 => visual.simulate_mouse_move(beyond, None, Modifiers::default()),
            1 => visual.simulate_resize(size(px(780.), px(400.))),
            2 => visual.deactivate_window(),
            _ => visual.simulate_event(MouseExitEvent {
                position: end,
                pressed_button: Some(MouseButton::Left),
                modifiers: Modifiers::default(),
            }),
        }
        visual.run_until_parked();
        // A stale selecting flag would resume on this move, even after focus
        // returns. Resize and pointer leave keep focus, so those are independent.
        visual.simulate_mouse_move(beyond, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        input.read_with(visual, |state, _| {
            assert_eq!(
                state.selected_value().as_ref(),
                selected.as_str(),
                "signal {signal}"
            )
        });
    }
}

#[gpui::test]
fn sidebar_resize_lost_mouse_up_does_not_keep_a_toolkit_drag(cx: &mut TestAppContext) {
    with_reader(cx, |reader, visual| {
        reader.update_in(visual, |reader, window, cx| {
            if !reader.panels.notes {
                reader.toggle_panel(reader_layout::Panel::Notes, window, cx);
            }
        });
        visual.run_until_parked();
        let handle = reader.read_with(visual, |reader, _| {
            assert!(reader.panels.notes, "positive control: sidebar shown");
            point(
                reader.body_bounds.left() + px(reader.panel_widths.notes) - px(2.),
                reader.body_bounds.top() + px(100.),
            )
        });
        visual.simulate_mouse_down(handle, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(
            handle + point(px(20.), px(0.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.run_until_parked();
        visual.simulate_mouse_move(
            handle + point(px(70.), px(0.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.run_until_parked();
        let width = reader.read_with(visual, |reader, _| {
            assert!(reader.resizing_panel.is_some(), "actual resize drag began");
            assert!(reader.panel_widths.notes > reader_layout::DEFAULT_PANEL_WIDTH);
            reader.panel_widths.notes
        });
        visual.simulate_mouse_move(handle + point(px(150.), px(0.)), None, Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.panel_widths.notes, width);
            assert!(reader.resizing_panel.is_none());
        });
        visual.update(|_, cx| {
            assert!(
                !cx.has_active_drag(),
                "lost release cancels the toolkit drag"
            )
        });
    });
}
