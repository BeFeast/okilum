//! App-wide document text zoom. Chrome and PDF keep their existing scale.
//!
//! In normal note width the column grows with the zoom (#1652, see
//! `reader_ui_state::note_column_width`), and every change shows a small
//! Typora-like overlay: "−  110%  +  Reset". Clicking the percentage takes an
//! exact value; the overlay hides a few seconds after the last interaction.
use super::*;
use gpui::{DispatchPhase, InteractiveElement, PinchEvent};

gpui::actions!(note_text_zoom, [ZoomIn, ZoomOut, Reset]);

/// The overlay stays this long after the last change, hover or exact entry.
const HUD_TIMEOUT: Duration = Duration::from_secs(3);

/// Fenced code has its own themed font size; scale it with the document.
/// Inline code inherits the surrounding text size, including its zoom.
pub(super) fn document_style(cx: &App) -> TextViewStyle {
    let mut style = reader_text_style(cx.theme());
    style.code_block.text.font_size =
        Some((cx.theme().mono_font_size * reader_ui_state::text_zoom(cx)).into());
    #[cfg(test)]
    CODE_STYLES.with(|styles| {
        if let Some(styles) = styles.borrow_mut().as_mut() {
            styles.push(style.code_block.text.font_size);
        }
    });
    style
}

#[cfg(test)]
thread_local! {
    static CODE_STYLES: std::cell::RefCell<Option<Vec<Option<AbsoluteLength>>>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn bind_keys(cx: &mut App) {
    let context = Some("Reader && !ManagerWeb");
    // GPUI menus show the last binding of an action: ⌘= / Ctrl+= stays last.
    cx.bind_keys([
        KeyBinding::new("secondary-+", ZoomIn, context),
        KeyBinding::new("secondary-shift-=", ZoomIn, context),
        KeyBinding::new("secondary-=", ZoomIn, context),
        KeyBinding::new("secondary--", ZoomOut, context),
        KeyBinding::new("secondary-0", Reset, context),
    ]);
}

pub(crate) fn reader<E: InteractiveElement>(view: E, cx: &mut Context<Reader>) -> E {
    view.on_action(cx.listener(|this, _: &ZoomIn, window, cx| {
        change(
            this,
            step(reader_ui_state::text_zoom(cx), 1.),
            None,
            window,
            cx,
        );
    }))
    .on_action(cx.listener(|this, _: &ZoomOut, window, cx| {
        change(
            this,
            step(reader_ui_state::text_zoom(cx), -1.),
            None,
            window,
            cx,
        );
    }))
    .on_action(cx.listener(|this, _: &Reset, window, cx| {
        change(this, 1., None, window, cx);
    }))
}

/// Keyboard, menu and overlay steps land on whole 10% values, so a zoom left
/// at 113% by a precision wheel goes to 120% / 110%, not 123% / 103%.
fn step(zoom: f32, direction: f32) -> f32 {
    ((zoom * 10.).round() + direction) / 10.
}

/// The overlay's state, one per Reader window.
#[derive(Default)]
pub(crate) struct Hud {
    visible: bool,
    hovered: bool,
    hide: Option<Task<()>>,
    exact: Option<Exact>,
}

/// The percentage being typed. Focus returns to `restore` afterwards.
struct Exact {
    input: Entity<InputState>,
    restore: Option<FocusHandle>,
    _subscription: Subscription,
}

impl Hud {
    #[cfg(test)]
    pub(crate) fn visible(&self) -> bool {
        self.visible
    }
}

/// Note text zoom applies here: notes in every mode, code and plain text
/// files. PDF keeps its own zoom; images and other previews have none.
fn applies(this: &Reader) -> bool {
    this.editing.is_some()
        || this
            .file_preview
            .as_ref()
            .is_none_or(|preview| preview.code.is_some() || preview.text.is_some())
}

fn show_hud(this: &mut Reader, window: &mut Window, cx: &mut Context<Reader>) {
    // Repeated wheel steps only push the deadline out; the overlay is not
    // re-created, so it does not flicker.
    this.zoom_hud.visible = true;
    schedule_hide(this, window, cx);
    cx.notify();
}

fn schedule_hide(this: &mut Reader, window: &mut Window, cx: &mut Context<Reader>) {
    // Replacing the task drops (cancels) the previous deadline.
    this.zoom_hud.hide = None;
    if !this.zoom_hud.visible || this.zoom_hud.hovered || this.zoom_hud.exact.is_some() {
        return;
    }
    this.zoom_hud.hide = Some(cx.spawn_in(window, async move |this, cx| {
        cx.background_executor().timer(HUD_TIMEOUT).await;
        let _ = this.update(cx, |this, cx| {
            if this.zoom_hud.hovered || this.zoom_hud.exact.is_some() {
                return;
            }
            this.zoom_hud.visible = false;
            this.zoom_hud.hide = None;
            cx.notify();
        });
    }));
}

fn open_exact(this: &mut Reader, window: &mut Window, cx: &mut Context<Reader>) {
    if this.zoom_hud.exact.is_some() {
        return;
    }
    let restore = window.focused(cx);
    let value = format!("{:.0}", reader_ui_state::text_zoom(cx) * 100.);
    let input = cx.new(|cx| {
        let mut input = InputState::new(window, cx);
        input.set_value(value, window, cx);
        input
    });
    let subscription = cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
        InputEvent::PressEnter { .. } => apply_exact(this, window, cx),
        // Clicking elsewhere cancels; focus already went where the user clicked.
        InputEvent::Blur => {
            this.zoom_hud.exact = None;
            schedule_hide(this, window, cx);
            cx.notify();
        }
        _ => {}
    });
    input.update(cx, |input, cx| {
        input.focus(window, cx);
        input.select_all(window, cx);
    });
    this.zoom_hud.exact = Some(Exact {
        input,
        restore,
        _subscription: subscription,
    });
    this.zoom_hud.visible = true;
    schedule_hide(this, window, cx);
    cx.notify();
}

/// "150", "150%" or "150 %" → 1.5. Anything else is not a zoom.
fn parse_percent(text: &str) -> Option<f32> {
    let value = text
        .trim()
        .trim_end_matches('%')
        .trim()
        .parse::<f32>()
        .ok()?;
    (value.is_finite() && value > 0.).then_some(value / 100.)
}

fn close_exact(this: &mut Reader, window: &mut Window, cx: &mut Context<Reader>) {
    let Some(exact) = this.zoom_hud.exact.take() else {
        return;
    };
    // The editor, Reader or file preview gets its focus back.
    match exact.restore {
        Some(handle) => handle.focus(window, cx),
        None => this.focus_handle.focus(window, cx),
    }
    schedule_hide(this, window, cx);
    cx.notify();
}

fn apply_exact(this: &mut Reader, window: &mut Window, cx: &mut Context<Reader>) {
    let Some(exact) = &this.zoom_hud.exact else {
        return;
    };
    let zoom = parse_percent(&exact.input.read(cx).value());
    close_exact(this, window, cx);
    if let Some(zoom) = zoom {
        change(this, zoom, None, window, cx);
    }
}

/// The overlay at the bottom right of the note pane, while it is shown.
pub(crate) fn hud(this: &Reader, cx: &mut Context<Reader>) -> Option<AnyElement> {
    if !this.zoom_hud.visible || !applies(this) {
        return None;
    }
    let zoom = reader_ui_state::text_zoom(cx);
    let theme = cx.theme();
    let (surface, foreground, border) = (theme.popover, theme.popover_foreground, theme.border);
    let value = match &this.zoom_hud.exact {
        Some(exact) => div()
            .w(px(64.))
            .debug_selector(|| "note-zoom-hud-input".into())
            .child(Input::new(&exact.input).small())
            .into_any_element(),
        None => Button::new("note-zoom-hud-value")
            .ghost()
            .tab_stop(false)
            .label(format!("{:.0}%", zoom * 100.))
            .tooltip("Set exact zoom")
            // Fixed width: 90% → 110% must not move the + button.
            .w(px(56.))
            .h(px(28.))
            .rounded(px(6.))
            .debug_selector(|| "note-zoom-hud-value".into())
            .on_click(cx.listener(|this, _, window, cx| open_exact(this, window, cx)))
            .into_any_element(),
    };
    // Buttons cancel focus on mouse down, so the editor keeps the caret.
    let step_button = |id: &'static str, icon: IconName, tip: SharedString, direction: f32| {
        reader_icon_button(id, icon, tip, cx)
            .tab_stop(false)
            .debug_selector(move || id.into())
            .on_click(cx.listener(move |this, _, window, cx| {
                let zoom = step(reader_ui_state::text_zoom(cx), direction);
                change(this, zoom, None, window, cx);
            }))
    };
    let out = step_button(
        "note-zoom-hud-out",
        IconName::Minus,
        reader_shortcuts::hint("Zoom out", &ZoomOut, cx),
        -1.,
    );
    let into = step_button(
        "note-zoom-hud-in",
        IconName::Plus,
        reader_shortcuts::hint("Zoom in", &ZoomIn, cx),
        1.,
    );
    let reset = Button::new("note-zoom-hud-reset")
        .ghost()
        .tab_stop(false)
        .label("Reset")
        .tooltip(reader_shortcuts::hint("Actual size", &Reset, cx))
        .disabled(zoom == 1.)
        .h(px(28.))
        .rounded(px(6.))
        .debug_selector(|| "note-zoom-hud-reset".into())
        .on_click(cx.listener(|this, _, window, cx| change(this, 1., None, window, cx)));
    Some(
        h_flex()
            .id("note-zoom-hud")
            .debug_selector(|| "note-zoom-hud".into())
            .absolute()
            .right(px(24.))
            .bottom(px(20.))
            // Clicks on the overlay never reach the note under it.
            .occlude()
            .gap_0p5()
            .p_1()
            .rounded(px(10.))
            .bg(surface)
            .text_color(foreground)
            .text_size(px(13.))
            .border_1()
            .border_color(border)
            .shadow_lg()
            .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                this.zoom_hud.hovered = *hovered;
                schedule_hide(this, window, cx);
            }))
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Escape, window, cx| {
                    if this.zoom_hud.exact.is_some() {
                        close_exact(this, window, cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .child(out)
            .child(value)
            .child(into)
            .child(reset)
            .into_any_element(),
    )
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
        // At a limit the overlay still answers, showing where zoom stands.
        if applies(this) {
            show_hud(this, window, cx);
        }
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
    show_hud(this, window, cx);
    cx.stop_propagation();
    cx.notify();
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::{ScrollDelta, TestAppContext, TouchPhase};
    use std::time::Duration;

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
        let temp = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            // Reader::new does not run the application startup that installs
            // the persisted appearance store used by document zoom.
            reader_ui_state::install(&temp.path().join("state"), cx);
            assert!(reader_ui_state::installed(cx));
            gpui_component::init(cx);
            super::super::bind_keys(cx);
        });
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
            assert!(
                (reader_ui_state::text_zoom(cx) - 1.1).abs() < 0.001,
                "keyboard zoom updates the installed store: {}",
                reader_ui_state::text_zoom(cx)
            );
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
        // cursor_layout reports content coordinates; painting applies the
        // scroll offset afterward. Compare the actual on-screen caret.
        let caret_y = |state: &gpui_component::input::EditorState, _: &App| {
            state.cursor_layout().unwrap().0.origin.y + state.scroll_offset().y
        };
        let old_y = input.read_with(visual, caret_y);
        input.read_with(visual, |state, _| {
            let bounds = state.input_bounds();
            assert!(
                old_y >= bounds.top() && old_y < bounds.bottom(),
                "visible Source caret positive control: {old_y:?} in {bounds:?}"
            );
        });
        visual.simulate_keystrokes("secondary-=");
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert!(
                (reader_ui_state::text_zoom(cx) - 1.1).abs() < 0.001,
                "Source keyboard zoom positive control"
            );
        });
        visual.update(|window, cx| window.simulate_next_frame(cx));
        let new_y = input.read_with(visual, caret_y);
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

        // Even an unreadable PDF retains its own shortcut scope and scale.
        // PDF preview starts real worker and native file-watcher threads.
        visual.executor().allow_parking();
        std::fs::write(vault.join("report.pdf"), b"%PDF-fixture").unwrap();
        reader.update_in(visual, |reader, window, cx| {
            reader.preview_file("report.pdf", window, cx);
            reader.focus_handle.focus(window, cx);
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.simulate_next_frame(cx));
        let pdf = reader.read_with(visual, |reader, _| {
            reader.file_preview.as_ref().unwrap().pdf.clone().unwrap()
        });
        let pdf_zoom = pdf.read_with(visual, |pdf, _| pdf.zoom_step());
        visual.simulate_keystrokes("secondary-=");
        assert_eq!(
            pdf.read_with(visual, |pdf, _| pdf.zoom_step()),
            pdf_zoom + 1
        );
        visual.update(|_, cx| assert!((reader_ui_state::text_zoom(cx) - 1.1).abs() < 0.001));
        visual.simulate_keystrokes("secondary-0");
        assert_eq!(pdf.read_with(visual, |pdf, _| pdf.zoom_step()), pdf_zoom);
        visual.update(|_, cx| assert!((reader_ui_state::text_zoom(cx) - 1.1).abs() < 0.001));
        reader.update(visual, |reader, _| reader.file_preview = None);
    }

    /// A Reader window over one Markdown note, with the zoom store installed.
    fn open_note<'a>(
        cx: &'a mut TestAppContext,
        temp: &tempfile::TempDir,
        width: f32,
    ) -> (Entity<Reader>, &'a mut gpui::VisualTestContext) {
        cx.update(|cx| {
            reader_ui_state::install(&temp.path().join("state"), cx);
            gpui_component::init(cx);
            super::super::bind_keys(cx);
        });
        let vault = temp.path().join("vault");
        std::fs::create_dir_all(&vault).unwrap();
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
        visual.simulate_resize(size(px(width), px(800.)));
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            reader.focus_handle.focus(window, cx)
        });
        frame(visual);
        (reader, visual)
    }

    fn frame(visual: &mut gpui::VisualTestContext) {
        visual.run_until_parked();
        visual.update(|window, cx| window.simulate_next_frame(cx));
        visual.run_until_parked();
    }

    /// Measure actual shaped text, including the mixed-font inline paragraph.
    fn read_code_widths(reader: &Reader, cx: &App) -> [Pixels; 3] {
        let state = reader.content.read(cx);
        let bounds = state.bounds();
        let mut segments = std::collections::BTreeSet::new();
        for y in (0..f32::from(bounds.size.height).min(500.) as usize).step_by(4) {
            if let Some((segment, _, _)) = state.rendered_source_at(Some(point(
                bounds.left() + px(20.),
                bounds.top() + px(y as f32),
            ))) {
                segments.insert(segment);
            }
        }
        let mut paragraph = None;
        let mut fence = None;
        for segment in segments {
            let at = |index| state.rendered_position_for_source(segment, index);
            if let (Some(start), Some(body_end), Some(code_start), Some(code_end), Some(_)) =
                (at(0), at(4), at(5), at(9), at(14))
            {
                paragraph = Some([body_end.x - start.x, code_end.x - code_start.x]);
            } else if let (Some(start), Some(end), None) = (at(0), at(4), at(9)) {
                fence = Some(end.x - start.x);
            }
        }
        let [body, inline] = paragraph.expect("mixed-font paragraph geometry positive control");
        let fence = fence.expect("fenced code geometry positive control");
        assert!(body > px(0.) && inline > px(0.) && fence > px(0.));
        [body, inline, fence]
    }

    #[gpui::test]
    fn code_fonts_follow_document_zoom_in_read_and_live_preview(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        let vault = temp.path().join("vault");
        std::fs::create_dir(&vault).unwrap();
        let source = "MMMM `WWWW` MMMM\n\n```text\nWWWW\n```\n\n> [!note]\n> ```text\n> nested code\n> ```\n\nCaret here.\n";
        std::fs::write(vault.join("start.md"), source).unwrap();
        cx.update(|cx| {
            reader_ui_state::install(&temp.path().join("state"), cx);
            gpui_component::init(cx);
            super::super::bind_keys(cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(vault.clone()),
                        note: Some("start.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
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
        visual.simulate_resize(size(px(1400.), px(1000.)));
        frame(visual);
        let base = reader.read_with(visual, read_code_widths);
        for zoom in [2., 0.5, 1.] {
            reader.update_in(visual, |r, window, cx| change(r, zoom, None, window, cx));
            frame(visual);
            let actual = reader.read_with(visual, read_code_widths);
            for (label, (actual, base)) in ["body", "inline code", "fenced code"]
                .into_iter()
                .zip(actual.into_iter().zip(base))
            {
                assert!(
                    (actual - base * zoom).abs() < px(1.),
                    "{label} must scale with document zoom {zoom}: {actual:?} vs base {base:?}"
                );
            }
        }
        reader.update_in(visual, |r, window, cx| {
            r.open_live_preview(window, cx);
            r.editing.as_ref().unwrap().input().update(cx, |input, cx| {
                input.set_selected_range(source.len()..source.len(), cx);
            });
        });
        frame(visual);
        let ranges = [
            0..4,
            6..10,
            source.find("\nWWWW\n").unwrap() + 1..source.find("\nWWWW\n").unwrap() + 5,
        ];
        let widths = |r: &Reader, cx: &App| {
            assert!(r.source_live_preview());
            let input = r.editing.as_ref().unwrap().input().read(cx);
            ranges.clone().map(|range| {
                input
                    .range_to_bounds(&range)
                    .expect("actual LP body/inline/fence layout positive control")
                    .size
                    .width
            })
        };
        let base = reader.read_with(visual, widths);
        for zoom in [2., 0.5, 1.] {
            CODE_STYLES.with(|styles| *styles.borrow_mut() = Some(Vec::new()));
            reader.update_in(visual, |r, window, cx| change(r, zoom, None, window, cx));
            frame(visual);
            let actual = reader.read_with(visual, widths);
            for (actual, base) in actual.into_iter().zip(base) {
                assert!((actual - base * zoom).abs() < px(1.), "LP code zoom {zoom}");
            }
            let styles = CODE_STYLES.with(|styles| styles.borrow_mut().take().unwrap());
            assert!(
                !styles.is_empty(),
                "actual LP callout block renderer positive control"
            );
            let expected = visual.update(|_, cx| cx.theme().mono_font_size * zoom);
            assert!(styles.iter().all(|size| *size == Some(expected.into())));
        }
        assert_eq!(
            std::fs::read_to_string(vault.join("start.md")).unwrap(),
            source
        );
    }

    fn zoom_is(visual: &mut gpui::VisualTestContext, expected: f32) {
        visual.update(|_, cx| {
            let zoom = reader_ui_state::text_zoom(cx);
            assert!(
                (zoom - expected).abs() < 0.001,
                "zoom {zoom}, expected {expected}"
            );
        });
    }

    fn source_mode(
        reader: &Entity<Reader>,
        visual: &mut gpui::VisualTestContext,
    ) -> Entity<gpui_component::input::EditorState> {
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx);
            reader.set_live_preview(false, window, cx);
        });
        frame(visual);
        let input = reader.read_with(visual, |reader, _| {
            reader.editing.as_ref().unwrap().input().clone()
        });
        visual.update(|window, cx| {
            input.update(cx, |state, cx| state.focus(window, cx));
        });
        frame(visual);
        input
    }

    #[test]
    fn steps_land_on_whole_tens_and_percent_parses() {
        assert!((step(1., 1.) - 1.1).abs() < 1e-6);
        assert!((step(1.13, 1.) - 1.2).abs() < 1e-6);
        assert!((step(1.13, -1.) - 1.).abs() < 1e-6);
        assert_eq!(parse_percent("150"), Some(1.5));
        assert_eq!(parse_percent(" 75 % "), Some(0.75));
        assert_eq!(parse_percent("abc"), None);
        assert_eq!(parse_percent("0"), None);
        assert_eq!(parse_percent(""), None);
    }

    #[gpui::test]
    fn column_follows_zoom_in_normal_width_and_is_clamped(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        let (reader, visual) = open_note(cx, &temp, 2400.);
        let column = |visual: &mut gpui::VisualTestContext| {
            frame(visual);
            visual
                .debug_bounds("reader-column")
                .expect("reader column painted")
                .size
                .width
        };
        let pane = visual.debug_bounds("reader-document").unwrap().size.width;
        visual.update(|_, cx| reader_ui_state::set_reading_width(740., cx));
        let normal = column(visual);
        assert!(
            (normal - px(740.)).abs() < px(1.),
            "positive control: {normal:?}"
        );
        assert!(pane > px(1200.), "wide pane positive control: {pane:?}");

        visual.update(|_, cx| reader_ui_state::set_text_zoom(1.5, cx));
        let zoomed = column(visual);
        assert!(
            (zoomed - px(1110.)).abs() < px(1.),
            "740 × 150%: {zoomed:?}"
        );

        // Source/Live Preview text follows the same column through its margins.
        let margin_at = |visual: &mut gpui::VisualTestContext, zoom: f32| {
            visual.update(|_, cx| reader_ui_state::set_text_zoom(zoom, cx));
            frame(visual);
            reader.update_in(visual, |reader, window, cx| {
                reader.note_text_margins(window, cx).0
            })
        };
        let left_100 = margin_at(visual, 1.);
        let left_150 = margin_at(visual, 1.5);
        assert!(
            ((left_100 - left_150) - px(185.)).abs() < px(1.),
            "editor margins follow the zoomed column: {left_100:?} → {left_150:?}"
        );

        // Never wider than the pane: 3× of 1200 px is clamped like full width.
        visual.update(|_, cx| {
            reader_ui_state::set_reading_width(1200., cx);
            reader_ui_state::set_text_zoom(3., cx);
        });
        let clamped = column(visual);
        assert!(
            clamped < px(3600.) && clamped <= pane,
            "clamped: {clamped:?} in {pane:?}"
        );
        visual.update(|_, cx| reader_ui_state::set_full_note_width(true, cx));
        let full = column(visual);
        assert_eq!(
            clamped, full,
            "a clamped column fills the pane like full width"
        );

        // Full width does not depend on zoom.
        visual.update(|_, cx| reader_ui_state::set_text_zoom(1., cx));
        assert_eq!(column(visual), full, "full width unchanged by zoom");
        visual.update(|_, cx| reader_ui_state::set_text_zoom(1.5, cx));
        assert_eq!(column(visual), full, "full width unchanged by zoom");
    }

    #[gpui::test]
    fn hud_shows_after_zoom_and_hides_after_the_timeout(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        let (reader, visual) = open_note(cx, &temp, 1400.);
        let visible = |visual: &mut gpui::VisualTestContext| {
            frame(visual);
            let state = reader.read_with(visual, |reader, _| reader.zoom_hud.visible());
            let painted = visual.debug_bounds("note-zoom-hud").is_some();
            assert_eq!(state, painted, "state and paint agree");
            state
        };
        assert!(!visible(visual), "no overlay before a zoom");
        visual.simulate_keystrokes("secondary-=");
        zoom_is(visual, 1.1);
        assert!(visible(visual), "overlay after a zoom");
        let value = visual.debug_bounds("note-zoom-hud-value").unwrap();
        let document = visual.debug_bounds("reader-document").unwrap();
        assert!(
            value.right() <= document.right()
                && value.bottom() <= document.bottom()
                && value.left() > document.center().x
                && value.top() > document.center().y,
            "bottom right of the note pane: {value:?} in {document:?}"
        );

        visual.executor().advance_clock(Duration::from_secs(2));
        assert!(visible(visual), "still shown at 2 s");
        // Another step pushes the deadline out without hiding in between.
        visual.simulate_keystrokes("secondary-=");
        zoom_is(visual, 1.2);
        visual.executor().advance_clock(Duration::from_secs(2));
        assert!(
            visible(visual),
            "4 s after the first step, 2 s after the last"
        );
        visual.executor().advance_clock(Duration::from_millis(1500));
        assert!(!visible(visual), "hidden 3 s after the last change");

        // Hover keeps it; leaving starts the timeout again.
        visual.simulate_keystrokes("secondary--");
        zoom_is(visual, 1.1);
        assert!(visible(visual));
        let hud = visual.debug_bounds("note-zoom-hud").unwrap();
        visual.simulate_mouse_move(hud.center(), None, Default::default());
        visual.executor().advance_clock(Duration::from_secs(6));
        assert!(visible(visual), "hovered overlay stays");
        visual.simulate_mouse_move(
            document.origin + point(px(40.), px(120.)),
            None,
            Default::default(),
        );
        visual.executor().advance_clock(Duration::from_millis(3500));
        assert!(!visible(visual), "hidden after the pointer leaves");

        // Wheel zoom shows it too.
        let position = visual.debug_bounds("reader-column").unwrap().center();
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
        zoom_is(visual, 1.2);
        assert!(visible(visual), "overlay after a wheel step");
    }

    #[gpui::test]
    fn exact_value_sets_the_zoom_and_escape_cancels(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        let (reader, visual) = open_note(cx, &temp, 1400.);
        visual.simulate_keystrokes("secondary-=");
        frame(visual);
        let exact_input = |visual: &mut gpui::VisualTestContext| {
            let value = visual
                .debug_bounds("note-zoom-hud-value")
                .expect("percentage button");
            visual.simulate_click(value.center(), Modifiers::default());
            frame(visual);
            assert!(
                visual.debug_bounds("note-zoom-hud-input").is_some(),
                "exact input shown"
            );
            reader.read_with(visual, |reader, _| {
                reader.zoom_hud.exact.as_ref().unwrap().input.clone()
            })
        };
        let input = exact_input(visual);
        visual.update(|window, cx| {
            assert!(
                input.read(cx).focus_handle(cx).is_focused(window),
                "input takes focus"
            );
            assert_eq!(input.read(cx).value().as_ref(), "110");
        });
        // The overlay waits while the value is being typed.
        visual.executor().advance_clock(Duration::from_secs(6));
        frame(visual);
        assert!(visual.debug_bounds("note-zoom-hud-input").is_some());
        visual.update(|window, cx| {
            input.update(cx, |input, cx| input.set_value("150", window, cx));
        });
        visual.simulate_keystrokes("enter");
        frame(visual);
        zoom_is(visual, 1.5);
        assert!(visual.debug_bounds("note-zoom-hud-input").is_none());
        reader.update_in(visual, |reader, window, _| {
            assert!(
                reader.focus_handle.is_focused(window),
                "focus goes back to the note"
            );
        });

        let input = exact_input(visual);
        visual.update(|window, cx| {
            input.update(cx, |input, cx| input.set_value("70", window, cx));
        });
        visual.simulate_keystrokes("escape");
        frame(visual);
        zoom_is(visual, 1.5);
        assert!(
            visual.debug_bounds("note-zoom-hud-input").is_none(),
            "Esc closes the input"
        );
        assert!(
            visual.debug_bounds("note-zoom-hud").is_some(),
            "the overlay itself stays"
        );
        reader.update_in(visual, |reader, window, _| {
            assert!(reader.focus_handle.is_focused(window));
        });

        // Reset button.
        let reset = visual.debug_bounds("note-zoom-hud-reset").unwrap();
        visual.simulate_click(reset.center(), Modifiers::default());
        zoom_is(visual, 1.);
    }

    #[gpui::test]
    fn exact_zoom_while_editing_restores_caret_and_subsequent_input(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        let (reader, visual) = open_note(cx, &temp, 1400.);
        let editor = source_mode(&reader, visual);
        editor.update(visual, |state, cx| state.set_selected_range(6..6, cx));
        visual.simulate_input("before-");
        frame(visual);
        let mut expected = editor.read_with(visual, |state, _| state.text().to_string());
        assert!(
            expected.starts_with("Alpha before-bravo"),
            "typing positive control"
        );
        visual.simulate_keystrokes("secondary-=");
        frame(visual);
        zoom_is(visual, 1.1);

        for (key, value, zoom, suffix) in [
            ("enter", "137", 1.37, "after-enter-"),
            ("escape", "222", 1.37, "after-escape-"),
        ] {
            let caret = editor.read_with(visual, |state, _| state.selected_range());
            assert_eq!(caret.start, caret.end, "collapsed editor caret");
            let value_button = visual.debug_bounds("note-zoom-hud-value").unwrap();
            visual.simulate_click(value_button.center(), Modifiers::default());
            frame(visual);
            let exact = reader.read_with(visual, |reader, _| {
                reader.zoom_hud.exact.as_ref().unwrap().input.clone()
            });
            visual.update(|window, cx| {
                assert!(exact.read(cx).focus_handle(cx).is_focused(window));
                assert!(!editor.read(cx).focus_handle(cx).is_focused(window));
            });
            // Route real text input through the focused HUD, not set_value.
            visual.simulate_input(value);
            frame(visual);
            exact.read_with(visual, |state, _| assert_eq!(state.value().as_ref(), value));
            editor.read_with(visual, |state, _| {
                assert_eq!(
                    state.text().to_string(),
                    expected,
                    "HUD input is not note text"
                );
                assert_eq!(state.selected_range(), caret, "HUD preserves the caret");
            });
            visual.simulate_keystrokes(key);
            frame(visual);
            zoom_is(visual, zoom);
            assert!(visual.debug_bounds("note-zoom-hud-input").is_none());
            visual.update(|window, cx| {
                assert!(
                    editor.read(cx).focus_handle(cx).is_focused(window),
                    "{key} restores editor focus"
                );
                assert_eq!(
                    editor.read(cx).selected_range(),
                    caret,
                    "{key} restores caret"
                );
            });
            // No explicit refocus: continued typing must reach the same caret.
            visual.simulate_input(suffix);
            frame(visual);
            expected.insert_str(caret.start, suffix);
            editor.read_with(visual, |state, _| {
                assert_eq!(
                    state.text().to_string(),
                    expected,
                    "typing after {key} is retained"
                );
                assert_eq!(
                    state.selected_range(),
                    caret.start + suffix.len()..caret.start + suffix.len()
                );
            });
        }
    }

    #[gpui::test]
    fn view_menu_items_zoom_note_text(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        let (reader, visual) = open_note(cx, &temp, 1400.);
        let items = visual.update(|_, cx| crate::view_menu(cx).items);
        let actions = items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { name, action, .. } => {
                    Some((name.to_string(), action.name().to_string()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for (label, action) in [
            ("Zoom In", ZoomIn.name()),
            ("Zoom Out", ZoomOut.name()),
            ("Actual Size", Reset.name()),
        ] {
            assert!(
                actions.contains(&(label.to_string(), action.to_string())),
                "{label} in View: {actions:?}"
            );
        }
        // The menu shows ⌘= / Ctrl+=, ⌘− / Ctrl+− and ⌘0 / Ctrl+0.
        visual.update(|_, cx| {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            let shown = |action: &dyn Action| {
                keymap
                    .bindings()
                    .rfind(|binding| binding.action().partial_eq(action))
                    .map(|binding| binding.keystrokes()[0].unparse())
            };
            let secondary = if cfg!(target_os = "macos") {
                "cmd"
            } else {
                "ctrl"
            };
            assert_eq!(shown(&ZoomIn), Some(format!("{secondary}-=")));
            assert_eq!(shown(&ZoomOut), Some(format!("{secondary}--")));
            assert_eq!(shown(&Reset), Some(format!("{secondary}-0")));
        });
        // A menu item dispatches its action to the focused Reader.
        reader.update_in(visual, |reader, window, cx| {
            reader.focus_handle.focus(window, cx)
        });
        visual.dispatch_action(ZoomIn);
        zoom_is(visual, 1.1);
        visual.dispatch_action(ZoomIn);
        zoom_is(visual, 1.2);
        visual.dispatch_action(ZoomOut);
        zoom_is(visual, 1.1);
        visual.dispatch_action(Reset);
        zoom_is(visual, 1.);
        frame(visual);
        assert!(
            visual.debug_bounds("note-zoom-hud").is_some(),
            "menu zoom shows the overlay"
        );
    }

    #[gpui::test]
    fn secondary_zero_resets_zoom_while_editing_and_hud_keeps_editor_focus(
        cx: &mut TestAppContext,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let (reader, visual) = open_note(cx, &temp, 1400.);
        let source = std::fs::read_to_string(temp.path().join("vault/start.md")).unwrap();
        let input = source_mode(&reader, visual);
        let editor_focused = |visual: &mut gpui::VisualTestContext| {
            visual.update(|window, cx| input.read(cx).focus_handle(cx).is_focused(window))
        };
        assert!(editor_focused(visual), "positive control: editor has focus");
        visual.update(|_, cx| reader_ui_state::set_text_zoom(1.4, cx));
        frame(visual);
        visual.simulate_keystrokes("secondary-0");
        frame(visual);
        zoom_is(visual, 1.);
        assert!(editor_focused(visual), "Cmd/Ctrl+0 keeps editing");
        assert_eq!(
            std::fs::read_to_string(temp.path().join("vault/start.md")).unwrap(),
            source,
            "Cmd/Ctrl+0 does not turn a block into a paragraph"
        );
        input.read_with(visual, |state, _| {
            assert_eq!(state.text().to_string(), source, "editor text untouched")
        });

        // Overlay buttons zoom without taking the caret from the editor.
        visual.simulate_keystrokes("secondary-=");
        frame(visual);
        zoom_is(visual, 1.1);
        let plus = visual
            .debug_bounds("note-zoom-hud-in")
            .expect("overlay over Source");
        visual.simulate_click(plus.center(), Modifiers::default());
        frame(visual);
        zoom_is(visual, 1.2);
        assert!(
            editor_focused(visual),
            "overlay click leaves focus in the editor"
        );
        input.read_with(visual, |state, _| {
            assert_eq!(
                state.text().to_string(),
                source,
                "click did not reach the editor"
            )
        });
    }
}
