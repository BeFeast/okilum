//! Opt-in presentation over the existing exact Reader editor and FileEditor.
use super::*;
use crate::source_presentation::{CachedProvider, ProjectionColors};
use std::cell::Cell;

#[derive(Default)]
pub(super) struct LivePreview {
    pub enabled: bool,
    in_flight: Rc<Cell<bool>>,
    queued: bool,
    accepted: Option<Arc<CachedProvider>>,
    colors: Cell<Option<ProjectionColors>>,
}

fn projection_colors(cx: &App) -> ProjectionColors {
    let palette = brand::palette(cx);
    ProjectionColors {
        heading: palette.text,
        link: palette.link,
    }
}

impl Reader {
    pub(super) fn refresh_live_preview_colors(&self, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.as_ref().filter(|e| e.live_preview.enabled) else {
            return;
        };
        let colors = projection_colors(cx);
        if editing.live_preview.colors.get() == Some(colors) {
            return;
        }
        let Some(provider) = editing
            .live_preview
            .accepted
            .as_ref()
            .filter(|p| p.source().stamp == editing.input.read(cx).source_stamp())
        else {
            return;
        };
        // Publish the palette before notifying the input: the resulting render
        // observes it and cannot reinstall the same provider recursively.
        editing.live_preview.colors.set(Some(colors));
        let provider = provider.clone().with_colors(colors);
        editing.input.update(cx, |input, cx| {
            input.set_projection_provider(Some(provider), cx)
        });
    }

    pub(crate) fn render_live_preview_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(editing) = &self.editing else {
            return div().into_any_element();
        };
        let oversize =
            editing.input.read(cx).text().len() > tessera_core::source_classifier::MAX_BYTES;
        let limited = oversize
            || editing
                .live_preview
                .accepted
                .as_ref()
                .is_some_and(|p| p.limited());
        reader_icon_button(
            "reader-live-preview",
            IconName::BookOpen,
            if limited {
                "Live Preview uses Source for this note"
            } else if editing.live_preview.enabled {
                "Switch to Source"
            } else {
                "Switch to Live Preview"
            },
            cx,
        )
        .debug_selector(|| "reader-live-preview".into())
        .selected(editing.live_preview.enabled)
        .on_click(cx.listener(|this, _, window, cx| this.toggle_live_preview(window, cx)))
        .into_any_element()
    }

    pub(crate) fn toggle_live_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let colors = projection_colors(cx);
        let Some(editing) = &mut self.editing else {
            return;
        };
        editing.live_preview.colors.set(Some(colors));
        editing.live_preview.enabled = !editing.live_preview.enabled;
        let input = editing.input.clone();
        let enabled = editing.live_preview.enabled;
        input.update(cx, |input, cx| {
            // Native search operates on source offsets and suppresses projection.
            // Close it before adopting Live Preview; Find returns to Source.
            if enabled {
                input.close_search(cx);
            }
            input.set_searchable(!enabled, cx);
        });
        let provider = if enabled {
            editing
                .live_preview
                .accepted
                .as_ref()
                .filter(|p| p.source().stamp == input.read(cx).source_stamp())
                .map(|p| p.clone().with_colors(colors))
        } else {
            None
        };
        // None retains the native exact-source/grapheme opt-in from ExactSource.
        // Neither mode replaces the buffer or touches its undo transaction chain.
        input.update(cx, |input, cx| input.set_projection_provider(provider, cx));
        self.schedule_live_preview(cx);
        input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    pub(super) fn schedule_live_preview(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = &mut self.editing else {
            return;
        };
        let state = &mut editing.live_preview;
        if !state.enabled {
            return;
        }
        let input = editing.input.clone();
        let editor = input.read(cx);
        if editor.text().len() > tessera_core::source_classifier::MAX_BYTES {
            state.accepted = None;
            input.update(cx, |input, cx| input.set_projection_provider(None, cx));
            return;
        }
        if state.in_flight.get() {
            state.queued = true;
            return;
        }
        if state
            .accepted
            .as_ref()
            .is_some_and(|p| p.source().stamp == editor.source_stamp())
        {
            return;
        }
        let source = SourceSnapshot {
            stamp: editor.source_stamp(),
            text: Arc::from(editor.value().as_ref()),
        };
        let marker = state.in_flight.clone();
        marker.set(true);
        state.queued = false;
        cx.spawn(async move |this, cx| {
            let provider = cx
                .background_executor()
                .spawn(async move { Arc::new(CachedProvider::classify(source)) })
                .await;
            // Parked editors can temporarily be absent while a move applies.
            // Their marker must still settle, so a later edit can schedule again.
            marker.set(false);
            let _ = this.update(cx, |this, cx| {
                let colors = projection_colors(cx);
                let Some(editing) = &mut this.editing else {
                    return;
                };
                if editing.input != input {
                    return;
                }
                let current = input.read(cx);
                if current.source_stamp() == provider.source().stamp
                    && current.value().as_ref() == provider.source().text.as_ref()
                {
                    editing.live_preview.accepted = Some(provider.clone());
                    if editing.live_preview.enabled {
                        editing.live_preview.colors.set(Some(colors));
                        input.update(cx, |input, cx| {
                            input.set_projection_provider(Some(provider.with_colors(colors)), cx)
                        });
                    }
                }
                if std::mem::take(&mut editing.live_preview.queued) {
                    this.schedule_live_preview(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui_component::input::projection::ActiveSource;

    const ORIGINAL: &str =
        "\u{feff}# Привет 🧠\r\n\r\n**Жирный** e\u{301} и `code` [[target|ссылка]]\r\n\r\nTail\r\n";

    fn fixture<'a>(
        cx: &'a mut TestAppContext,
        text: &str,
    ) -> (Entity<Reader>, &'a mut VisualTestContext, tempfile::TempDir) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), text).unwrap();
        std::fs::write(root.join("target.md"), "# Other\n\n**target**\n").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("note.md")),
                        index_dir: Some(directory.path().join("index")),
                        session_directory: Some(directory.path().join("state")),
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
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        (reader, visual, directory)
    }

    #[gpui::test]
    fn find_returns_to_source_and_live_preview_closes_source_search(cx: &mut TestAppContext) {
        let (reader, visual, _) = fixture(cx, ORIGINAL);
        reader.update_in(visual, |r, window, cx| {
            r.open_source_find(cx);
            assert!(
                r.editing
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .search_session()
                    .open
            );
            r.toggle_live_preview(window, cx);
            let input = r.editing.as_ref().unwrap().input.clone();
            assert!(!input.read(cx).search_session().open);
            // Positive control: native search must be disabled while projected.
            input.update(cx, |input, cx| input.open_search(false, cx));
            assert!(!input.read(cx).search_session().open);
            let stamp = input.read(cx).source_stamp();
            r.open_source_find(cx);
            assert!(!r.editing.as_ref().unwrap().live_preview.enabled);
            assert!(input.read(cx).search_session().open);
            assert_eq!(input.read(cx).source_stamp(), stamp);
            assert_eq!(input.read(cx).value().as_ref(), ORIGINAL);
        });
    }

    #[gpui::test]
    fn theme_changes_reuse_classification_without_editing_source(cx: &mut TestAppContext) {
        let (reader, visual, directory) = fixture(cx, ORIGINAL);
        reader.update_in(visual, |r, window, cx| r.toggle_live_preview(window, cx));
        visual.run_until_parked();
        let (accepted, stamp, height) = reader.read_with(visual, |r, cx| {
            let editing = r.editing.as_ref().unwrap();
            (
                editing.live_preview.accepted.clone().unwrap(),
                editing.input.read(cx).source_stamp(),
                editing.input.read(cx).line_height(),
            )
        });
        for mode in [
            gpui_component::ThemeMode::Dark,
            gpui_component::ThemeMode::Light,
        ] {
            visual.update(|window, cx| set_appearance(Some(mode), None, window, cx));
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
            reader.read_with(visual, |r, cx| {
                let editing = r.editing.as_ref().unwrap();
                assert!(Arc::ptr_eq(
                    &accepted,
                    editing.live_preview.accepted.as_ref().unwrap()
                ));
                assert_eq!(
                    editing.live_preview.colors.get(),
                    Some(projection_colors(cx))
                );
                assert_eq!(editing.input.read(cx).source_stamp(), stamp);
                assert_eq!(editing.input.read(cx).value().as_ref(), ORIGINAL);
                assert_eq!(editing.input.read(cx).line_height(), height);
                assert!(!editing.store.dirty());
            });
            let epoch = reader.read_with(visual, |r, cx| {
                r.editing
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .presentation_epoch()
            });
            visual.update(|window, cx| window.draw(cx).clear(cx));
            reader.read_with(visual, |r, cx| {
                assert_eq!(
                    r.editing
                        .as_ref()
                        .unwrap()
                        .input
                        .read(cx)
                        .presentation_epoch(),
                    epoch,
                    "unchanged palette must not reinstall its provider on redraw"
                );
            });
        }
        assert_eq!(
            std::fs::read(directory.path().join("vault/note.md")).unwrap(),
            ORIGINAL.as_bytes()
        );
    }

    #[gpui::test]
    fn latest_only_classification_rejects_stale_notes_and_preserves_large_source(
        cx: &mut TestAppContext,
    ) {
        let (reader, visual, directory) = fixture(cx, ORIGINAL);
        let input = reader.update_in(visual, |r, window, cx| {
            r.toggle_live_preview(window, cx);
            let editing = r.editing.as_ref().unwrap();
            assert!(
                editing.live_preview.in_flight.get(),
                "positive control: actual background classification started"
            );
            let input = editing.input.clone();
            input.update(cx, |s, cx| {
                s.replace_text_in_range(Some(0..0), "first ", window, cx)
            });
            input.update(cx, |s, cx| {
                s.replace_text_in_range(Some(0..0), "latest ", window, cx)
            });
            input
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            let e = r.editing.as_ref().unwrap();
            assert!(!e.live_preview.in_flight.get() && !e.live_preview.queued);
            let p = e.live_preview.accepted.as_ref().unwrap();
            assert_eq!(p.source().stamp, input.read(cx).source_stamp());
            assert_eq!(p.source().text.as_ref(), format!("latest first {ORIGINAL}"));
            let display = p.compose(p.source(), &ActiveSource::default()).unwrap();
            assert_ne!(
                display.text(),
                p.source().text.as_ref(),
                "positive control: non-active Markdown is concealed"
            );
        });
        reader.update_in(visual, |r, window, cx| {
            input.update(cx, |s, cx| {
                s.replace_text_in_range(Some(0..0), "pending ", window, cx)
            });
            r.schedule_live_preview(cx);
            assert!(r.editing.as_ref().unwrap().live_preview.in_flight.get());
            assert!(r.leave_source(cx));
            r.open_note("target.md", None, window, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            r.toggle_source(window, cx);
            r.toggle_live_preview(window, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            let e = r.editing.as_ref().unwrap();
            assert_ne!(e.input, input);
            assert_eq!(
                e.live_preview
                    .accepted
                    .as_ref()
                    .unwrap()
                    .source()
                    .text
                    .as_ref(),
                "# Other\n\n**target**\n"
            );
            assert_eq!(e.input.read(cx).value().as_ref(), "# Other\n\n**target**\n");
        });
        let large = format!("# {}\r\n", "🧠".repeat(17_000));
        reader.update_in(visual, |r, window, cx| {
            r.editing
                .as_ref()
                .unwrap()
                .input
                .update(cx, |s, cx| s.set_value(large.clone(), window, cx));
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            let e = r.editing.as_ref().unwrap();
            assert!(e.live_preview.accepted.is_none());
            assert_eq!(e.input.read(cx).value().as_ref(), large);
        });
        assert_eq!(
            std::fs::read_to_string(directory.path().join("vault/target.md")).unwrap(),
            "# Other\n\n**target**\n"
        );
    }

    #[gpui::test]
    fn glyph_toggle_preserves_buffer_undo_copy_and_durable_conflict(cx: &mut TestAppContext) {
        let (reader, visual, directory) = fixture(cx, ORIGINAL);
        let input = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone());
        let stamp = input.read_with(visual, |i, _| i.source_stamp());
        let toggle = visual.debug_bounds("reader-live-preview").unwrap().center();
        visual.simulate_click(toggle, Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            assert!(r.editing.as_ref().unwrap().live_preview.enabled);
            assert_eq!(input.read(cx).source_stamp(), stamp);
            assert_eq!(input.read(cx).value().as_ref(), ORIGINAL);
            assert!(!r.editing.as_ref().unwrap().store.dirty());
        });
        reader.update_in(visual, |_, window, cx| {
            input.update(cx, |i, cx| {
                i.set_selected_range(0..ORIGINAL.len(), cx);
                assert_eq!(
                    i.text_for_range(0..ORIGINAL.encode_utf16().count(), &mut None, window, cx)
                        .as_deref(),
                    Some(ORIGINAL)
                );
            })
        });
        #[cfg(target_os = "macos")]
        visual.simulate_keystrokes("cmd-c");
        #[cfg(not(target_os = "macos"))]
        visual.simulate_keystrokes("ctrl-c");
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some(ORIGINAL)
            )
        });
        reader.update_in(visual, |_, window, cx| {
            input.update(cx, |i, cx| {
                i.set_selected_range(0..0, cx);
                i.replace_text_in_range(Some(0..0), "local ", window, cx);
            })
        });
        visual.run_until_parked();
        // Actual toolbar clicks must not save on focus loss or create history transactions.
        for _ in 0..2 {
            let toggle = visual.debug_bounds("reader-live-preview").unwrap().center();
            visual.simulate_click(toggle, Modifiers::default());
            visual.run_until_parked();
        }
        assert_eq!(
            std::fs::read_to_string(directory.path().join("vault/note.md")).unwrap(),
            ORIGINAL
        );
        #[cfg(target_os = "macos")]
        visual.simulate_keystrokes("cmd-z");
        #[cfg(not(target_os = "macos"))]
        visual.simulate_keystrokes("ctrl-z");
        visual.run_until_parked();
        input.read_with(visual, |i, _| assert_eq!(i.value().as_ref(), ORIGINAL));
        #[cfg(target_os = "macos")]
        visual.simulate_keystrokes("cmd-shift-z");
        #[cfg(not(target_os = "macos"))]
        visual.simulate_keystrokes("ctrl-y");
        visual.run_until_parked();
        input.read_with(visual, |i, _| {
            assert_eq!(i.value().as_ref(), format!("local {ORIGINAL}"))
        });
        std::fs::write(directory.path().join("vault/note.md"), "external").unwrap();
        reader.update_in(visual, |r, _, cx| {
            assert!(!r.save_source(cx));
            assert!(r.editing.as_ref().unwrap().conflict_detected);
        });
        let journal = std::fs::read_dir(directory.path().join("state/editor-drafts"))
            .unwrap()
            .flatten()
            .find(|e| e.path().extension().is_some_and(|x| x == "json"))
            .unwrap()
            .path();
        let draft: serde_json::Value =
            serde_json::from_slice(&std::fs::read(journal).unwrap()).unwrap();
        assert_eq!(
            draft["text"].as_str(),
            Some(format!("local {ORIGINAL}").as_str())
        );
        assert_eq!(
            std::fs::read_to_string(directory.path().join("vault/note.md")).unwrap(),
            "external"
        );
    }
    #[gpui::test]
    fn native_reveal_drag_graphemes_and_ime_bridge_keep_source_coordinates(
        cx: &mut TestAppContext,
    ) {
        let original = "# Title\n\n**Ж** e\u{301} 🧠\n\nend\n";
        let (reader, visual, _directory) = fixture(cx, original);
        reader.update_in(visual, |r, window, cx| r.toggle_live_preview(window, cx));
        visual.run_until_parked();
        let input = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone());
        let start = original.find('Ж').unwrap();
        let end = original.find('🧠').unwrap();
        let from = input.read_with(visual, |i, _| {
            i.range_to_bounds(&(start..start + 2)).unwrap().center()
        });
        visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        let to = input.read_with(visual, |i, _| {
            let bounds = i.range_to_bounds(&(end..end + 4)).unwrap();
            point(bounds.right(), bounds.center().y)
        });
        visual.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::default());
        visual.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        input.read_with(visual, |i, _| {
            let selection = i.selected_range();
            assert!(
                original.is_char_boundary(selection.start)
                    && original.is_char_boundary(selection.end)
            );
            assert!(
                original[selection].contains("e\u{301} 🧠"),
                "drag selects authored Unicode through reveal"
            );
            assert_eq!(i.value().as_ref(), original);
        });
        let combining = original.find("e\u{301}").unwrap();
        reader.update_in(visual, |_, _, cx| {
            input.update(cx, |i, cx| i.set_selected_range(combining..combining, cx))
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("right");
        input.read_with(visual, |i, _| {
            assert_eq!(i.cursor(), combining + "e\u{301}".len())
        });
        visual.simulate_keystrokes("left");
        input.read_with(visual, |i, _| assert_eq!(i.cursor(), combining));
        visual.simulate_keystrokes("home");
        input.read_with(visual, |i, _| {
            assert_eq!(i.cursor(), original.find("**Ж").unwrap())
        });
        visual.simulate_keystrokes("end");
        input.read_with(visual, |i, _| assert_eq!(i.cursor(), end + 4));
        reader.update_in(visual, |_, window, cx| {
            input.update(cx, |i, cx| {
                let eof = original.encode_utf16().count();
                i.replace_and_mark_text_in_range(Some(eof..eof), "中", Some(0..1), window, cx);
                assert_eq!(i.marked_text_range(window, cx), Some(eof..eof + 1));
                assert_eq!(i.value().as_ref(), format!("{original}中"));
                i.unmark_text(window, cx);
            })
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| r.toggle_live_preview(window, cx));
        #[cfg(target_os = "macos")]
        visual.simulate_keystrokes("cmd-z");
        #[cfg(not(target_os = "macos"))]
        visual.simulate_keystrokes("ctrl-z");
        visual.run_until_parked();
        input.read_with(visual, |i, _| assert_eq!(i.value().as_ref(), original));
    }
}
