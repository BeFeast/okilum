//! App-wide document text zoom. Chrome and PDF keep their existing scale.
use super::*;
use gpui::{DispatchPhase, InteractiveElement, PinchEvent};
use gpui_component::{notification::Notification, WindowExt};

gpui::actions!(note_text_zoom, [ZoomIn, ZoomOut, Reset]);
struct Indicator;

pub(crate) fn bind_keys(cx: &mut App) {
    let context = Some("Reader && !ManagerWeb");
    cx.bind_keys([
        KeyBinding::new("secondary-=", ZoomIn, context),
        KeyBinding::new("secondary-+", ZoomIn, context),
        KeyBinding::new("secondary-shift-=", ZoomIn, context),
        KeyBinding::new("secondary--", ZoomOut, context),
        KeyBinding::new("secondary-0", Reset, context),
    ]);
}

pub(crate) fn reader<E: InteractiveElement>(view: E, cx: &mut Context<Reader>) -> E {
    view.on_action(cx.listener(|this, _: &ZoomIn, window, cx| {
        change(this, reader_ui_state::text_zoom(cx) + 0.1, None, window, cx);
    }))
    .on_action(cx.listener(|this, _: &ZoomOut, window, cx| {
        change(this, reader_ui_state::text_zoom(cx) - 0.1, None, window, cx);
    }))
    .on_action(cx.listener(|this, _: &Reset, window, cx| {
        change(this, 1., None, window, cx);
    }))
}

fn modifier(modifiers: gpui::Modifiers, mac: bool) -> bool {
    !modifiers.alt
        && !modifiers.shift
        && if mac {
            modifiers.platform && !modifiers.control
        } else {
            modifiers.control && !modifiers.platform
        }
}

fn wheel_delta(delta: gpui::ScrollDelta) -> Option<f32> {
    let amount = match delta {
        // System line-count preferences must not multiply a physical wheel tick.
        gpui::ScrollDelta::Lines(lines) => {
            if lines.y.is_finite() && lines.y != 0. {
                lines.y.signum() * 0.1
            } else {
                return None;
            }
        }
        // Precision devices retain smooth deltas, bounded to 10% per event.
        gpui::ScrollDelta::Pixels(pixels) => (f32::from(pixels.y) / 400.).clamp(-0.1, 0.1),
    };
    (amount.is_finite() && amount != 0.).then_some(amount)
}

pub(crate) fn document<E: ParentElement + InteractiveElement>(
    view: E,
    cx: &mut Context<Reader>,
) -> E {
    let weak = cx.entity().downgrade();
    // Register before the document's own scroll handlers: a zoom wheel event
    // must not also move its editor/list, and must respect occluding overlays.
    view.child(
        div().absolute().inset_0().child(
            canvas(
                |bounds, window, _| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal),
                move |_, hitbox, window, _| {
                    let weak = weak.clone();
                    window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                        if phase != DispatchPhase::Capture
                            || !hitbox.is_hovered(window)
                            || !modifier(event.modifiers, cfg!(target_os = "macos"))
                        {
                            return;
                        }
                        let Some(delta) = wheel_delta(event.delta) else {
                            return;
                        };
                        let _ = weak.update(cx, |this, cx| {
                            let zoom = reader_ui_state::text_zoom(cx) + delta;
                            change(this, zoom, Some(event.position), window, cx);
                        });
                    });
                },
            )
            .size_full(),
        ),
    )
    .capture_pinch(cx.listener(|this, event: &PinchEvent, window, cx| {
        if cfg!(target_os = "macos") && event.delta.is_finite() && event.delta != 0. {
            let zoom = reader_ui_state::text_zoom(cx) * (1. + event.delta.clamp(-0.2, 0.2));
            change(this, zoom, Some(event.position), window, cx);
        }
    }))
}

fn change(
    this: &mut Reader,
    zoom: f32,
    pointer: Option<Point<Pixels>>,
    window: &mut Window,
    cx: &mut Context<Reader>,
) {
    if this
        .file_preview
        .as_ref()
        .is_some_and(|preview| preview.pdf.is_some())
    {
        cx.propagate();
        return;
    }
    let zoom = zoom.clamp(0.5, 3.);
    if zoom == reader_ui_state::text_zoom(cx) {
        cx.stop_propagation();
        return;
    }
    if let Some(editing) = &this.editing {
        editing
            .input()
            .update(cx, |state, _| state.preserve_zoom_anchor(pointer));
    } else if let Some(preview) = &this.file_preview {
        if let Some(code) = &preview.code {
            code.update(cx, |code, cx| code.preserve_zoom_anchor(pointer, cx));
        } else if let Some(text) = &preview.text {
            text.update(cx, |text, cx| text.preserve_zoom_anchor(pointer, cx));
        } else {
            cx.propagate();
            return;
        }
    } else {
        this.content
            .update(cx, |state, _| state.preserve_zoom_anchor(pointer));
    }
    reader_ui_state::set_text_zoom(zoom, cx);
    window.push_notification(
        Notification::new()
            .id::<Indicator>()
            .message(format!("{:.0}%", zoom * 100.))
            .timeout(Duration::from_secs(1)),
        cx,
    );
    cx.stop_propagation();
    cx.notify();
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::{ScrollDelta, TestAppContext, TouchPhase};

    #[gpui::test]
    fn scale_persists_without_changing_base_font_or_other_presentation(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            reader_ui_state::install(temp.path(), cx);
            reader_ui_state::set_reading(20., 960., cx);
            reader_ui_state::set_text_zoom(1.4, cx);
            assert_eq!(reader_ui_state::font_size(cx), 28.);
            assert_eq!(reader_ui_state::base_font_size(cx), 20.);
            reader_ui_state::flush(cx);
            reader_ui_state::install(temp.path(), cx);
            assert_eq!(reader_ui_state::text_zoom(cx), 1.4);
            assert_eq!(reader_ui_state::base_font_size(cx), 20.);
            assert_eq!(reader_ui_state::reading_width(cx), 960.);
            reader_ui_state::set_text_zoom(f32::NAN, cx);
            assert_eq!(reader_ui_state::text_zoom(cx), 1.4);
            reader_ui_state::set_text_zoom(9., cx);
            assert_eq!(reader_ui_state::text_zoom(cx), 3.);
            reader_ui_state::set_text_zoom(1., cx);
            assert_eq!(reader_ui_state::font_size(cx), 20.);
        });
        assert_eq!(wheel_delta(ScrollDelta::Lines(point(0., 1.))), Some(0.1));
        assert_eq!(wheel_delta(ScrollDelta::Lines(point(0., 3.))), Some(0.1));
        assert_eq!(wheel_delta(ScrollDelta::Lines(point(0., 0.))), None);
        assert_eq!(
            wheel_delta(ScrollDelta::Pixels(point(px(0.), px(1000.)))),
            Some(0.1)
        );
        assert_eq!(
            wheel_delta(ScrollDelta::Pixels(point(px(0.), px(4.)))),
            Some(0.01)
        );
        assert!(modifier(
            gpui::Modifiers {
                platform: true,
                ..Default::default()
            },
            true
        ));
        assert!(!modifier(
            gpui::Modifiers {
                control: true,
                ..Default::default()
            },
            true
        ));
        assert!(modifier(
            gpui::Modifiers {
                control: true,
                ..Default::default()
            },
            false
        ));
        assert!(!modifier(
            gpui::Modifiers {
                platform: true,
                ..Default::default()
            },
            false
        ));
    }

    #[gpui::test]
    fn keyboard_and_modified_wheel_zoom_only_document_text(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            super::super::bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let vault = temp.path().join("vault");
        std::fs::create_dir(&vault).unwrap();
        let source =
            "Alpha bravo charlie delta echo foxtrot golf hotel india juliet.\n\n".repeat(80);
        std::fs::write(vault.join("start.md"), &source).unwrap();
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
        let reader = reader.unwrap();
        visual.simulate_resize(size(px(1200.), px(700.)));
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            reader.focus_handle.focus(window, cx)
        });
        let base = visual.update(|_, cx| reader_ui_state::base_font_size(cx));
        visual.simulate_keystrokes("secondary-=");
        visual.update(|_, cx| {
            assert!((reader_ui_state::text_zoom(cx) - 1.1).abs() < 0.001);
            assert_eq!(reader_ui_state::base_font_size(cx), base);
        });
        visual.simulate_keystrokes("secondary-0");
        visual.run_until_parked();
        reader.update(visual, |reader, cx| {
            reader.content.read(cx).list_state().scroll_to(ListOffset {
                item_ix: 8,
                offset_in_item: px(24.),
            });
            cx.notify();
        });
        visual.update(|window, cx| window.simulate_next_frame(cx));
        visual.run_until_parked();
        let position = reader.read_with(visual, |reader, cx| {
            reader.content.read(cx).bounds().center()
        });
        let anchor = reader.read_with(visual, |reader, cx| {
            reader
                .content
                .read(cx)
                .rendered_source_at(Some(position))
                .expect("rendered geometry positive control")
        });
        visual.simulate_mouse_move(position, None, Default::default());
        let modifiers = if cfg!(target_os = "macos") {
            gpui::Modifiers {
                platform: true,
                ..Default::default()
            }
        } else {
            gpui::Modifiers {
                control: true,
                ..Default::default()
            }
        };
        visual.simulate_event(ScrollWheelEvent {
            position,
            delta: ScrollDelta::Lines(point(0., 1.)),
            modifiers,
            touch_phase: TouchPhase::Moved,
        });
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert!(
                reader_ui_state::text_zoom(cx) > 1.,
                "capture-phase wheel positive control"
            )
        });
        visual.update(|window, cx| window.simulate_next_frame(cx));
        visual.run_until_parked();
        let anchored = reader.read_with(visual, |reader, cx| {
            reader
                .content
                .read(cx)
                .rendered_position_for_source(anchor.0, anchor.1)
                .expect("same text remains laid out")
        });
        assert!(
            (anchored.y - anchor.2.y).abs() < px(2.),
            "pointer text y retained: {:?} vs {:?}",
            anchor.2,
            anchored
        );
        visual.simulate_keystrokes("secondary-0");
        visual.update(|_, cx| assert_eq!(reader_ui_state::text_zoom(cx), 1.));
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx);
            reader.set_live_preview(false, window, cx);
        });
        visual.run_until_parked();
        let input = reader.read_with(visual, |reader, _| {
            reader.editing.as_ref().unwrap().input().clone()
        });
        visual.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_cursor_position(
                    gpui_component::input::Position {
                        line: 30,
                        character: 8,
                    },
                    window,
                    cx,
                )
            })
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.simulate_next_frame(cx));
        let old_y = input.read_with(visual, |state, _| state.cursor_layout().unwrap().0.origin.y);
        visual.simulate_keystrokes("secondary-=");
        visual.run_until_parked();
        visual.update(|window, cx| window.simulate_next_frame(cx));
        let new_y = input.read_with(visual, |state, _| state.cursor_layout().unwrap().0.origin.y);
        assert!(
            (new_y - old_y).abs() < px(2.),
            "Source caret stays anchored: {:?} vs {:?}",
            old_y,
            new_y
        );
        assert_eq!(
            std::fs::read_to_string(vault.join("start.md")).unwrap(),
            source
        );
    }
}
