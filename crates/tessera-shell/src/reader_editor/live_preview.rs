//! Opt-in presentation over the existing exact Reader editor and FileEditor.
use super::*;
use crate::source_presentation::{CachedProvider, ProjectionColors};
use std::cell::Cell;

#[derive(Default)]
pub(super) struct LivePreview {
    pub enabled: bool,
    pub(super) restore_after_find: bool,
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
        muted: palette.text_muted,
    }
}

impl Reader {
    /// Opt-in, cross-platform diagnostics. Sampling reads state only: no notify/draw.
    pub(super) fn start_editor_layout_diagnostics(
        &self,
        input: Entity<EditorState>,
        cx: &mut Context<Self>,
    ) {
        if std::env::var("TESSERA_EDITOR_LAYOUT_DIAGNOSTICS").as_deref() != Ok("1") {
            return;
        }
        let Some(trace) = self
            .loading
            .as_ref()
            .and_then(|load| load.opts.diagnostics.clone())
        else {
            return;
        };
        input.update(cx, |input, _| input.enable_layout_diagnostics());
        cx.spawn(async move |reader, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let keep = reader.update(cx, |reader, cx| {
                    let Some(editing) = reader.editing.as_ref().filter(|e| e.input == input) else {
                        return false;
                    };
                    let editor = input.read(cx);
                    let Some(counts) = editor.layout_diagnostics() else { return false; };
                    let scroll = editor.scroll_offset();
                    let bounds = editor.input_bounds();
                    trace.event("editor_layout_sample", serde_json::json!({
                        "document": editor.source_stamp().document,
                        "generation": editor.source_stamp().generation,
                        "presentation_epoch": editor.presentation_epoch(),
                        "live": editing.live_preview.enabled,
                        "classification_in_flight": editing.live_preview.in_flight.get(),
                        "layout_calls": counts.layout_calls,
                        "metric_changes": counts.metric_changes,
                        "provider_applies": counts.provider_applies,
                        "projection_composes": counts.projection_composes,
                        "active_changes": counts.active_changes,
                        "scroll": [f32::from(scroll.x), f32::from(scroll.y)],
                        "bounds": [f32::from(bounds.origin.x), f32::from(bounds.origin.y),
                            f32::from(bounds.size.width), f32::from(bounds.size.height)],
                        "caret": editor.cursor_layout().map(|(b, _)| [f32::from(b.origin.x),
                            f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height)]),
                    }));
                    true
                }).unwrap_or(false);
                if !keep { break; }
            }
        }).detach();
    }

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

    pub(crate) fn render_live_preview_control(
        &self,
        labels: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
        use gpui_component::button::ButtonGroup;
        let live = editing.live_preview.enabled;
        ButtonGroup::new("edit-presentation")
            .children([
                Button::new("reader-live-preview")
                    .ghost()
                    .small()
                    .icon(IconName::Eye)
                    .h(px(28.))
                    .when(!labels, |b| b.w(px(28.)))
                    .when(labels, |button| button.label("Live Preview"))
                    .accessibility_label("Live Preview")
                    .tooltip(if limited {
                        SharedString::from("Live Preview uses Source for this note")
                    } else {
                        reader_shortcuts::hint("Live Preview", &OpenLivePreview, cx)
                    })
                    .debug_selector(|| "reader-live-preview".into())
                    .selected(live)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if !live {
                            this.toggle_live_preview(window, cx);
                        }
                    })),
                Button::new("reader-source")
                    .ghost()
                    .small()
                    .icon(Icon::default().path("icons/code-xml.svg"))
                    .h(px(28.))
                    .when(!labels, |b| b.w(px(28.)))
                    .when(labels, |button| button.label("Source"))
                    .accessibility_label("Source")
                    .tooltip("Markdown source")
                    .debug_selector(|| "reader-source".into())
                    .selected(!live)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(editing) = &mut this.editing {
                            editing.live_preview.restore_after_find = false;
                        }
                        if live {
                            this.toggle_live_preview(window, cx);
                        }
                    })),
            ])
            .into_any_element()
    }

    pub(crate) fn set_live_preview(
        &mut self,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(editing) = &mut self.editing {
            editing.live_preview.restore_after_find = false;
            if editing.live_preview.enabled != enabled {
                self.toggle_live_preview(window, cx);
            }
        }
    }

    pub(crate) fn toggle_live_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let colors = projection_colors(cx);
        let Some(editing) = &mut self.editing else {
            return;
        };
        editing.live_preview.restore_after_find = false;
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
    #[cfg(unix)]
    #[gpui::test]
    fn mode_segments_follow_labels_preference_and_switch_editor_presentation(
        cx: &mut TestAppContext,
    ) {
        let fixture = tempfile::tempdir().unwrap();
        let vault = fixture.path().join("Personal knowledge — 研究");
        std::fs::create_dir(&vault).unwrap();
        std::fs::write(vault.join("Note.md"), "# Note\n\nBody text\n").unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
            reader_ui_state::install(&fixture.path().join("state"), cx);
            cx.set_global(reader_history::TestSessionDirectory(
                fixture.path().join("history"),
            ));
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(vault.clone()),
                        note: Some("Note.md".into()),
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
        visual.simulate_resize(size(px(1400.), px(900.)));
        visual.run_until_parked();
        let icon_width = visual.debug_bounds("reader-edit").unwrap().size.width;
        visual.update(|_, cx| reader_ui_state::set_toolbar_labels(true, cx));
        visual.run_until_parked();
        let edit = visual.debug_bounds("reader-edit").unwrap();
        assert!(
            edit.size.width > icon_width,
            "labels change the actual rendered control"
        );
        visual.simulate_click(edit.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| assert!(reader.editing.is_some()));
        assert!(
            visual.debug_bounds("source-save").is_none(),
            "clean source needs no Save glyph"
        );
        let live = visual.debug_bounds("reader-live-preview").unwrap();
        visual.simulate_click(live.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.editing.as_ref().unwrap().live_preview.enabled)
        });
        let source = visual.debug_bounds("reader-source").unwrap();
        visual.simulate_click(source.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(!reader.editing.as_ref().unwrap().live_preview.enabled)
        });
        // The pencil is Edit, never Rename, and toggles back to Reader (#871).
        let pencil = visual.debug_bounds("reader-edit").unwrap();
        visual.simulate_click(pencil.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.editing.is_none());
            assert!(reader.renaming.is_none());
        });
        let pencil = visual.debug_bounds("reader-edit").unwrap();
        visual.simulate_click(pencil.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.editing.is_some(), "pencil enters the editor");
            assert!(!reader.source_live_preview());
        });
        reader.update_in(visual, |reader, window, cx| {
            reader.set_live_preview(true, window, cx)
        });
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.editing.is_none());
            assert!(
                reader.ui_state.live_preview,
                "Reader retains the last editor mode"
            );
        });
        reader.update_in(visual, |reader, _, cx| {
            assert!(reader.leave_source(cx), "repeated leave is a no-op");
            assert!(reader.ui_state.live_preview);
        });
        let pencil = visual.debug_bounds("reader-edit").unwrap();
        visual.simulate_click(pencil.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.editing.is_some(), "pencil re-enters the editor");
            assert!(reader.source_live_preview());
        });
        let read = visual.debug_bounds("reader-read").unwrap();
        visual.simulate_click(read.center(), Modifiers::default());
        visual.run_until_parked();
        visual.simulate_resize(size(px(480.), px(900.)));
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx)
        });
        visual.run_until_parked();
        assert_eq!(
            visual.debug_bounds("reader-edit").unwrap().size.width,
            icon_width,
            "optional labels yield to glyphs in a narrow editor"
        );
        visual.simulate_keystrokes("f2");
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.renaming.as_ref().unwrap().in_header)
        });
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        let title = visual.debug_bounds("reader-document-root").unwrap();
        visual.simulate_click(title.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(
                reader.renaming.as_ref().unwrap().in_header,
                "title click renames"
            );
        });
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        let header = visual.debug_bounds("document-header").unwrap();
        for control in [
            "reader-history-back",
            "reader-history-forward",
            "reader-read",
            "reader-edit",
            "reader-live-preview",
            "reader-source",
            "document-more",
        ] {
            let bounds = visual.debug_bounds(control).unwrap();
            assert!(
                bounds.left() >= header.left() && bounds.right() <= header.right(),
                "{control} stays inside narrow header"
            );
        }
        visual.simulate_resize(size(px(1100.), px(900.)));
        reader.update_in(visual, |reader, _, cx| {
            reader.panel_widths.notes = 200.;
            reader.panels.open(reader_layout::Panel::Notes);
            cx.notify();
        });
        visual.run_until_parked();
        let panel = visual.debug_bounds("reader-notes-panel").unwrap();
        let menu = visual.debug_bounds("sidebar-actions").unwrap();
        let folders = visual.debug_bounds("folders-actions").unwrap();
        assert!(
            menu.right() < panel.right(),
            "menu={menu:?}, panel={panel:?}"
        );
        assert!(
            folders.right() < panel.right(),
            "Folders actions never clip at 200px"
        );
        assert!(
            visual.debug_bounds("sidebar-new-note").is_none(),
            "long vault name takes precedence over actions"
        );
    }

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
    fn save_echo_batches_preserve_editor_projection_and_viewport(cx: &mut TestAppContext) {
        let original = format!(
            "# Echo\n\n{}",
            "**Bold** wrapped paragraph with [[target|label]].\n\n".repeat(80)
        );
        let (reader, visual, dir) = fixture(cx, &original);
        reader.update_in(visual, |r, window, cx| {
            r.toggle_live_preview(window, cx);
            r.editing.as_ref().unwrap().input.update(cx, |input, cx| {
                input.enable_layout_diagnostics();
                input.set_value(format!("{original}Saved\n"), window, cx);
            });
            assert!(r.save_source(cx));
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, _, cx| {
            r.editing.as_ref().unwrap().input.update(cx, |input, cx| {
                input.set_scroll_offset(point(px(0.), px(-120.)), cx);
            });
        });
        visual.run_until_parked();
        let snapshot = |r: &Reader, cx: &App| {
            let e = r.editing.as_ref().unwrap();
            let input = e.input.read(cx);
            (
                e.input.entity_id(),
                input.source_stamp(),
                input.presentation_epoch(),
                input.selected_range(),
                input.scroll_offset(),
                input.value().to_string(),
                input.layout_diagnostics().unwrap().provider_applies,
            )
        };
        let before = reader.read_with(visual, snapshot);
        let accepted = reader.read_with(visual, |r, _| {
            r.editing
                .as_ref()
                .unwrap()
                .live_preview
                .accepted
                .clone()
                .unwrap()
        });
        assert!(
            before.4.y < px(0.),
            "positive control: genuinely scrolled editor"
        );
        for _ in 0..3 {
            // Same bytes, new metadata: includes sync/save echo that must still be indexed.
            let path = dir.path().join("vault/note.md");
            let bytes = std::fs::read(&path).unwrap();
            std::fs::write(&path, bytes).unwrap();
            reader.update_in(visual, |r, window, cx| {
                r.apply_vault_changes(
                    tessera_core::Changes {
                        changed: ["note.md".to_owned()].into(),
                        directories: [String::new()].into(),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            visual.run_until_parked();
            assert_eq!(reader.read_with(visual, snapshot), before);
            reader.read_with(visual, |r, _| {
                assert!(Arc::ptr_eq(
                    &accepted,
                    r.editing
                        .as_ref()
                        .unwrap()
                        .live_preview
                        .accepted
                        .as_ref()
                        .unwrap()
                ))
            });
        }
        // Different bytes must not be hidden by the echo guard.
        std::fs::write(dir.path().join("vault/note.md"), "# Real external change\n").unwrap();
        reader.update_in(visual, |r, window, cx| {
            r.apply_vault_changes(
                tessera_core::Changes {
                    changed: ["note.md".to_owned()].into(),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        visual.run_until_parked();
        let after = reader.read_with(visual, snapshot);
        assert_ne!(after.1, before.1);
        assert_eq!(after.5, "# Real external change\n");
    }

    #[gpui::test]
    fn layout_counter_sampling_does_not_request_presentation_changes(cx: &mut TestAppContext) {
        let (reader, visual, _) = fixture(cx, ORIGINAL);
        reader.update_in(visual, |r, window, cx| {
            r.editing
                .as_ref()
                .unwrap()
                .input
                .update(cx, |input, _| input.enable_layout_diagnostics());
            r.toggle_live_preview(window, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            let input = r.editing.as_ref().unwrap().input.read(cx);
            let counts = input.layout_diagnostics().unwrap();
            assert!(counts.provider_applies > 0 && counts.projection_composes > 0);
            let epoch = input.presentation_epoch();
            for _ in 0..100 {
                assert_eq!(input.layout_diagnostics(), Some(counts));
                assert_eq!(input.presentation_epoch(), epoch);
            }
        });
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
    fn closing_find_restores_live_preview_but_preserves_source(cx: &mut TestAppContext) {
        let (reader, visual, _) = fixture(cx, ORIGINAL);
        reader.update_in(visual, |r, window, cx| r.toggle_live_preview(window, cx));
        visual.run_until_parked();
        let input = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone());
        let stamp = input.read_with(visual, |input, _| input.source_stamp());
        // The menu action, including repeated activation, must remember Live Preview.
        reader.update_in(visual, |r, window, cx| {
            r.open_find(window, cx);
            r.open_find(window, cx);
            assert!(!r.editing.as_ref().unwrap().live_preview.enabled);
            assert!(input.read(cx).search_session().open);
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            assert!(!input.read(cx).search_session().open);
            assert!(r.editing.as_ref().unwrap().live_preview.enabled);
            assert_eq!(input.read(cx).source_stamp(), stamp);
            assert_eq!(input.read(cx).value().as_ref(), ORIGINAL);
        });
        // The toolbar close uses this same EditorState close notification.
        reader.update_in(visual, |r, window, cx| r.open_find(window, cx));
        input.update(visual, |input, cx| input.close_search(cx));
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(r.editing.as_ref().unwrap().live_preview.enabled)
        });
        reader.update_in(visual, |r, window, cx| {
            r.toggle_live_preview(window, cx);
            r.open_find(window, cx);
        });
        input.update(visual, |input, cx| input.close_search(cx));
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(!r.editing.as_ref().unwrap().live_preview.enabled)
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
            r.set_live_preview(true, window, cx);
            assert!(
                r.source_live_preview(),
                "classify the next note in Live Preview"
            );
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
