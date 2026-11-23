//! Panel layout: the model lives in `okilum_shell_kit::reader_layout` (#3233);
//! its tests drive real Readers and stay here, under the same path.
pub(crate) use okilum_shell_kit::reader_layout::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maximize_round_trip_restores_every_panel_combination() {
        for notes in [false, true] {
            for backlinks in [false, true] {
                let original = Panels {
                    notes,
                    backlinks,
                    active: if backlinks {
                        Panel::Backlinks
                    } else if notes {
                        Panel::Notes
                    } else {
                        Panel::Closed
                    },
                    ..Default::default()
                };
                let mut panels = original.clone();
                panels.toggle_maximized();
                assert!(panels.maximized());
                for width in [700., 1366.] {
                    assert!(!panels.visible(Panel::Notes, width));
                    assert!(!panels.visible(Panel::Backlinks, width));
                }
                assert_eq!(panels.preferences(), original);
                panels.close(Panel::Closed);
                assert!(
                    panels.maximized(),
                    "no panel to dismiss must not exit focus mode"
                );
                panels.toggle_maximized();
                assert_eq!(panels, original);
            }
        }
    }

    #[test]
    fn explicit_panel_open_exits_maximize_and_tiling_does_not_restore_an_overlay() {
        let mut panels = Panels::default();
        panels.open(Panel::Notes);
        panels.open(Panel::Backlinks);
        panels.toggle_maximized();
        panels.viewport_changed(1366., 700.);
        panels.toggle_maximized();
        assert!(panels.notes && panels.backlinks);
        assert!(!panels.visible(Panel::Notes, 700.));
        assert!(!panels.visible(Panel::Backlinks, 700.));
        panels.toggle_maximized();
        panels.open(Panel::Notes);
        assert!(!panels.maximized());
        assert!(panels.visible(Panel::Notes, 700.));
        assert!(panels.visible(Panel::Backlinks, 1366.));
    }

    #[gpui::test]
    fn maximize_shortcut_button_and_view_action_preserve_editor_and_saved_layout(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::*;
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        let state = fixture.path().join("state");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), "# Note\n\nKeep reading.\n").unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            reader_ui_state::install(&state, cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("note.md".into()),
                        session_directory: Some(state.clone()),
                        index_dir: Some(fixture.path().join("index")),
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
        visual.simulate_resize(size(px(1366.), px(768.)));
        visual.run_until_parked();
        let shortcut = crate::MAXIMIZE_NOTE_KEY;
        for source in [false, true] {
            reader.update_in(visual, |r, window, cx| {
                assert_eq!(
                    r.pane.current_rel, "note.md",
                    "loaded-document positive control"
                );
                r.panels.open(Panel::Notes);
                r.panels.open(Panel::Backlinks);
                if source {
                    r.panels.close(Panel::Notes);
                    r.toggle_source(window, cx);
                }
                r.panel_widths.set(Panel::Notes, 315.);
                r.panel_widths.set(Panel::Backlinks, 287.);
                if let Some(editing) = &r.pane.editing {
                    editing.input().focus_handle(cx).focus(window, cx);
                } else {
                    r.pane
                        .content
                        .read(cx)
                        .focus_handle()
                        .clone()
                        .focus(window, cx);
                }
                cx.notify();
            });
            visual.run_until_parked();
            // #1909: the document owns maximize beside its width control.
            // A hit in the application titlebar would fail these bounds even
            // if it dispatched the same action successfully.
            let document = visual.debug_bounds("document-header").unwrap();
            let application = visual.debug_bounds("reader-header").unwrap();
            let maximize = visual.debug_bounds("reader-maximize-note").unwrap();
            let width = visual.debug_bounds("note-width").unwrap();
            assert!(maximize.left() >= document.left() && maximize.right() <= document.right());
            assert!(maximize.top() >= document.top() && maximize.bottom() <= document.bottom());
            assert!(maximize.top() >= application.bottom());
            assert_eq!(maximize.center().y, width.center().y);
            assert!(maximize.left() >= width.right() && maximize.left() <= width.right() + px(5.));
            let before = reader.read_with(visual, |r, cx| {
                (
                    r.panels.clone(),
                    r.panel_widths.clone(),
                    r.pane
                        .editing
                        .as_ref()
                        .map(|e| (e.input().entity_id(), e.input().read(cx).source_stamp())),
                )
            });
            visual.simulate_keystrokes(shortcut);
            visual.run_until_parked();
            reader.update_in(visual, |r, window, cx| {
                assert!(
                    r.panels.maximized(),
                    "shortcut must execute in Reader and Source"
                );
                assert_eq!(
                    r.panels.widths(&r.panel_widths, 1366.),
                    Widths {
                        notes: 0.,
                        backlinks: 0.
                    }
                );
                assert_eq!(r.panel_widths, before.1);
                r.record_ui_state(true, cx);
                let (saved, _) = reader_ui_state::layout(&root, cx).unwrap();
                assert_eq!(
                    saved.panels, before.0,
                    "temporary hiding is not a saved preference"
                );
                assert_eq!(saved.widths, before.1);
                assert_eq!(
                    r.pane
                        .editing
                        .as_ref()
                        .map(|e| (e.input().entity_id(), e.input().read(cx).source_stamp())),
                    before.2,
                    "maximizing retains the current editor and canonical source stamp"
                );
                if let Some(editing) = &r.pane.editing {
                    assert!(editing.input().focus_handle(cx).is_focused(window));
                }
            });
            // Escape intentionally leaves Source (existing #483 behavior).
            // Keep this round-trip focused on the maximize/restore controls.
            if !source {
                visual.simulate_keystrokes("escape");
                visual.run_until_parked();
            }
            reader.read_with(visual, |r, _| assert!(r.panels.maximized()));
            let button = visual
                .debug_bounds("reader-maximize-note")
                .expect("restore control remains visible");
            visual.simulate_click(button.center(), Modifiers::default());
            visual.run_until_parked();
            reader.read_with(visual, |r, cx| {
                assert_eq!(r.panels, before.0);
                assert_eq!(r.panel_widths, before.1);
                assert_eq!(r.pane.current_rel, "note.md");
                assert_eq!(
                    r.pane
                        .editing
                        .as_ref()
                        .map(|e| (e.input().entity_id(), e.input().read(cx).source_stamp())),
                    before.2
                );
            });
        }
        visual.update(|window, cx| {
            let item = view_menu(cx)
                .items
                .into_iter()
                .find_map(|item| match item {
                    MenuItem::Action { action, .. } if action.partial_eq(&MaximizeNote) => {
                        Some(action)
                    }
                    _ => None,
                })
                .expect("View menu exposes the same action");
            window.dispatch_action(item, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert!(r.panels.maximized()));
    }

    #[gpui::test]
    fn right_panel_has_section_headings_without_a_panel_header(cx: &mut gpui::TestAppContext) {
        use crate::*;
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("properties.md"),
            "---\nstatus: active\n---\n# With properties\n",
        )
        .unwrap();
        std::fs::write(root.join("plain.md"), "# Plain\n\n[[properties]]\n").unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("properties.md".into()),
                        session_directory: Some(fixture.path().join("state")),
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
        visual.run_until_parked();
        for (path, properties) in [("properties.md", true), ("plain.md", false)] {
            reader.update_in(visual, |r, window, cx| r.open_note(path, None, window, cx));
            visual.run_until_parked();
            for width in [1400., 700.] {
                visual.simulate_resize(size(px(width), px(800.)));
                reader.update(visual, |r, cx| {
                    r.panels.open(Panel::Backlinks);
                    cx.notify();
                });
                visual.run_until_parked();
                reader.read_with(visual, |r, _| {
                    assert_eq!(r.selected_file(), path);
                    assert_eq!(r.has_properties(), properties);
                });
                let panel = visual
                    .debug_bounds("reader-backlinks-panel")
                    .expect("visible-panel positive control");
                assert!(visual.debug_bounds("right-contents-heading").is_some());
                assert!(visual.debug_bounds("right-linked-from-heading").is_some());
                assert_eq!(
                    visual.debug_bounds("properties-section").is_some(),
                    properties
                );
                assert!(
                    visual.debug_bounds("reader-backlinks-header").is_none(),
                    "#1910: no redundant panel header at {width}px"
                );
                let first = visual
                    .debug_bounds(if properties {
                        "properties-section"
                    } else {
                        "right-contents-heading"
                    })
                    .unwrap();
                assert!(
                    first.top() < panel.top() + px(32.),
                    "no empty header-height band"
                );
            }
        }
    }

    #[test]
    fn tiling_hides_panels_without_losing_wide_choices() {
        let mut panels = Panels::default();
        panels.open(Panel::Notes);
        panels.open(Panel::Backlinks);
        panels.viewport_changed(1366., 666.);
        assert!(!panels.visible(Panel::Notes, 666.));
        assert!(!panels.visible(Panel::Backlinks, 666.));
        assert!(panels.visible(Panel::Notes, 1366.));
        assert!(panels.visible(Panel::Backlinks, 1366.));
        panels.open(Panel::Notes);
        panels.viewport_changed(666., 640.);
        assert!(panels.visible(Panel::Notes, 640.));
        assert!(!panels.visible(Panel::Backlinks, 640.));
    }

    #[test]
    fn sides_are_independent_and_compact_visibility_restores_wide_choices() {
        let mut panels = Panels::default();
        panels.open(Panel::Notes);
        panels.open(Panel::Backlinks);
        assert!(panels.visible(Panel::Notes, 1366.));
        assert!(panels.visible(Panel::Backlinks, 1366.));
        assert!(!panels.visible(Panel::Notes, 640.));
        assert!(panels.visible(Panel::Backlinks, 640.));
        panels.open(Panel::Notes);
        assert!(panels.visible(Panel::Notes, 640.));
        assert!(!panels.visible(Panel::Backlinks, 640.));
        panels.close(Panel::Notes);
        assert_eq!(panels.dismiss_target(640.), Panel::Closed);
        assert!(panels.visible(Panel::Backlinks, 1366.));
    }

    #[test]
    fn simultaneous_panels_reserve_document_and_keep_preferences() {
        let mut panels = Panels::default();
        panels.open(Panel::Notes);
        panels.open(Panel::Backlinks);
        let preferred = Widths {
            notes: 650.,
            backlinks: 350.,
        };
        for available in [1000., 1100., 1366., 1600.] {
            let displayed = panels.widths(&preferred, available);
            assert!(displayed.notes >= PANEL_MIN_WIDTH);
            assert!(displayed.backlinks >= PANEL_MIN_WIDTH);
            assert!(available - displayed.notes - displayed.backlinks >= DOCUMENT_MIN_WIDTH - 0.01);
            assert!(
                panels.resize_width(Panel::Notes, 1200., &preferred, available)
                    + displayed.backlinks
                    + DOCUMENT_MIN_WIDTH
                    <= available + 0.01
            );
        }
        let narrow = panels.widths(&preferred, 640.);
        assert_eq!(narrow.notes, 0.);
        assert_eq!(narrow.backlinks, 350.);
        let restored = panels.widths(&preferred, 1600.);
        assert_eq!(restored.notes, 650.);
        assert_eq!(restored.backlinks, 350.);
    }

    #[test]
    fn preferred_widths_survive_clamps_and_persist_independently() {
        let root = std::env::temp_dir().join(format!("reader-widths-{}", uuid::Uuid::new_v4()));
        let path = root.join("reader-layout.json");
        let mut widths = Widths::default();
        widths.set(Panel::Notes, 650.);
        widths.set(Panel::Backlinks, 350.);
        assert_eq!(displayed_width(widths.notes, 640.), 592.);
        assert_eq!(displayed_width(widths.notes, 1000.), 520.);
        assert_eq!(displayed_width(widths.notes, 1366.), 650.);
        assert_eq!(widths.notes, 650.);
        widths.save(&path).unwrap();
        let loaded = Widths::load(&path);
        assert_eq!(loaded.notes, 650.);
        assert_eq!(loaded.backlinks, 350.);
        let mut stale_second_window = Widths::default();
        stale_second_window.set(Panel::Backlinks, 410.);
        stale_second_window
            .save_panel(Panel::Backlinks, &path)
            .unwrap();
        let merged = Widths::load(&path);
        assert_eq!(merged.notes, 650.);
        assert_eq!(merged.backlinks, 410.);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod ui_zoom_layout_tests {
    use super::*;

    #[test]
    fn zoom_to_150_percent_uses_exclusive_overlays_and_preserves_dock_choices() {
        let mut panels = Panels::default();
        panels.open(Panel::Notes);
        panels.open(Panel::Backlinks);
        let compact = 1440. / 1.5;
        panels.viewport_changed(1440., compact);
        assert!(overlay(compact));
        assert!(!panels.visible(Panel::Notes, compact));
        assert!(!panels.visible(Panel::Backlinks, compact));
        panels.open(Panel::Notes);
        assert!(panels.visible(Panel::Notes, compact));
        assert!(!panels.visible(Panel::Backlinks, compact));
        panels.open(Panel::Backlinks);
        assert!(!panels.visible(Panel::Notes, compact));
        assert!(panels.visible(Panel::Backlinks, compact));
        assert!(panels.visible(Panel::Notes, 1440.));
        assert!(panels.visible(Panel::Backlinks, 1440.));
    }
}
