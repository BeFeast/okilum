//! App-level `Reader` glue and Reader-integration tests for `okilum_shell_logs::reader_log`
//! (#3239, epic #3241). The standalone log viewer and remote/virtual log sources
//! live in `crates/okilum-shell-logs`.
use super::*;
pub(crate) use okilum_shell_logs::reader_log::{
    bind_keys, chrome, http, remote, view_stack, ClearLogFilters, CopyRawLine, ExtendNextRecord,
    ExtendPreviousRecord, FocusLogFilter, InvestigationBack, LogData, LogDocument, LogView,
    NextMark, OpenLogColumns, OpenLogExport, OpenLogFiles, OpenLogLevels, OpenLogSavedViews,
    OpenLogTimeRange, PreviousMark, RowText, SelectNextRecord, SelectPreviousRecord,
    ToggleLogFollow, ToggleLogQuery, ToggleMark,
};

pub(crate) mod virtual_sources {
    pub(crate) use okilum_shell_logs::reader_log::virtual_sources::*;

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::reader_log::{LogData, LogView};
        use crate::*;
        use ::core::prelude::v1::test;
        use gpui::TestAppContext;
        use okilum_core::log::chips::{FilterChip, FilterMode, FilterValue};
        use okilum_core::log::query::FieldRef;
        use std::time::Instant;

        #[gpui::test]
        fn toolbar2808_embedded_okilog_plans_from_its_docked_pane(cx: &mut TestAppContext) {
            cx.executor().allow_parking();
            let (reader, visual, _dir) = reader_viewer_chrome::tests::fixture(cx, "view.okilog");
            visual.simulate_resize(size(px(1100.), px(900.)));
            reader.update(visual, |reader, cx| {
                reader.panels.open(reader_layout::Panel::Notes);
                reader.panels.open(reader_layout::Panel::Backlinks);
                cx.notify();
            });
            let bounds = reader_viewer_chrome::tests::measured_bounds;
            let panel = reader.read_with(visual, |reader, _| {
                reader
                    .pane
                    .file_preview
                    .as_ref()
                    .unwrap()
                    .virtual_log
                    .clone()
                    .unwrap()
            });
            let start =
                bounds(visual, "virtual-start").expect("real embedded .okilog criteria loaded");
            visual.simulate_click(start.center(), Modifiers::default());
            let deadline = Instant::now() + Duration::from_secs(3);
            let view = loop {
                visual.run_until_parked();
                if let Some(view) = panel.read_with(visual, |panel, _| panel.view.clone()) {
                    break view;
                }
                assert!(
                    Instant::now() < deadline,
                    "real .okilog selection reaches LogView"
                );
                std::thread::sleep(Duration::from_millis(1));
            };
            let pane = bounds(visual, "reader-log").unwrap();
            assert!(
                pane.size.width < px(700.),
                "positive control: pane is narrower than the 1100 px window"
            );
            for id in ["reader-notes-panel", "reader-backlinks-panel"] {
                assert!(
                    bounds(visual, id).is_some(),
                    "both actual panels are docked"
                );
            }
            for id in [
                "viewer-find-field",
                "log-level",
                "log-time",
                "log-query-toggle",
                "log-more",
            ] {
                let control = bounds(visual, id).expect("required compact-header control");
                assert!(
                    control.left() >= pane.left() && control.right() <= pane.right(),
                    "{id} {control:?} stays inside the measured embedded pane {pane:?}"
                );
            }
            let more = bounds(visual, "log-more").unwrap();
            assert!(
                (more.right() - (pane.right() - px(12.))).abs() <= px(1.),
                "More stays pinned to the pane's trailing padding"
            );
            for id in ["log-saved", "log-columns-menu", "log-export", "log-files"] {
                if let Some(control) = bounds(visual, id) {
                    assert!(
                        control.left() >= pane.left() && control.right() <= pane.right(),
                        "visible {id} stays inside the measured embedded pane"
                    );
                }
            }
            view.update_in(visual, |view, window, cx| {
                let chip = FilterChip::new(
                    FieldRef::Key("status".into()),
                    vec![FilterValue::string("502")],
                    FilterMode::Include,
                );
                view.add_chip(chip, window, cx);
            });
            assert!(
                bounds(visual, "log-chip-count").is_some(),
                "chips fold from pane width, even though the window is wider than 900 px"
            );
            assert!(bounds(visual, "log-chip-0").is_none());
        }

        fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
            let dir = tempfile::tempdir().unwrap();
            let vault = dir.path().join("vault");
            std::fs::create_dir_all(vault.join("logs")).unwrap();
            std::fs::write(vault.join("logs/a.log"), "first\n").unwrap();
            std::fs::write(vault.join("logs/b.log"), "second\n").unwrap();
            std::fs::write(
                vault.join("view.okilog"),
                r#"{"version":1,"name":"Combined","vault_root":true,"glob":"logs/*.log"}"#,
            )
            .unwrap();
            let path = vault.join("view.okilog");
            (dir, vault, path)
        }

        #[gpui::test]
        fn file_open_routes_definitions_inline_without_starting_selection_and_keeps_normal_logs(
            cx: &mut TestAppContext,
        ) {
            cx.executor().allow_parking();
            cx.update(crate::test_init);
            let (dir, vault, path) = fixture();
            cx.update(|cx| {
                reader_ui_state::install(&dir.path().join("preferences"), cx);
                cx.set_global(reader_history::TestSessionDirectory(
                    dir.path().join("history"),
                ));
            });
            std::fs::write(vault.join("home.md"), "# Home\n").unwrap();
            std::fs::write(vault.join("broken.okilog"), "not JSON").unwrap();
            let mut reader = None;
            let (_, visual) = cx.add_window_view(|window, cx| {
                let view = cx.new(|cx| {
                    Reader::new(
                        Opts {
                            vault: Some(vault.clone()),
                            note: Some("home.md".into()),
                            index_dir: Some(dir.path().join("index")),
                            session_directory: Some(dir.path().join("state")),
                            panel_settings_override: Some(dir.path().join("panels.json")),
                            ..Default::default()
                        },
                        window,
                        cx,
                    )
                });
                reader = Some(view.clone());
                Root::new(view, window, cx)
            });
            visual.simulate_resize(size(px(1400.), px(900.)));
            visual.run_until_parked();
            let reader = reader.unwrap();
            reader.update_in(visual, |reader, window, cx| {
                reader.open_note("view.okilog", None, window, cx)
            });
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                visual.run_until_parked();
                if reader.read_with(visual, |reader, cx| {
                    reader
                        .pane
                        .file_preview
                        .as_ref()
                        .and_then(|preview| preview.virtual_log.as_ref())
                        .is_some_and(|panel| panel.read(cx).loaded.is_some())
                }) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "definition must load through the real Reader file route"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            reader.read_with(visual, |reader, cx| {
                assert_eq!(reader.selected_file(), "view.okilog");
                let preview = reader.pane.file_preview.as_ref().unwrap();
                assert!(preview.log.is_none() && preview.code.is_none() && preview.text.is_none());
                let panel = preview.virtual_log.as_ref().unwrap().read(cx);
                assert!(panel.embedded);
                assert_eq!(
                    panel.loaded.as_ref().unwrap(),
                    &path.canonicalize().unwrap()
                );
                assert!(
                    panel.worker.is_none() && !panel.active,
                    "file-open never selects sources or installs a source-root watch before Start"
                );
                assert_eq!(panel.generation, 0);
                assert!(panel.view.is_none() && panel.pending.is_none());
            });
            assert!(visual.debug_bounds("virtual-definition-view").is_some());
            // The ordinary Source command must acquire the same writer, not conflict
            // with the typed view's lock or bypass the Reader editor/recovery contract.
            reader.update_in(visual, |reader, window, cx| {
                reader.toggle_source(window, cx);
                assert!(
                    reader.pane.editing.is_some(),
                    "explicit .okilog Source must open"
                );
                let panel = reader
                    .pane
                    .file_preview
                    .as_ref()
                    .unwrap()
                    .virtual_log
                    .as_ref()
                    .unwrap()
                    .read(cx);
                assert!(panel.session.is_none());
                reader.toggle_source(window, cx);
                assert!(reader.pane.editing.is_none());
            });
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                visual.run_until_parked();
                if reader.read_with(visual, |reader, cx| {
                    reader
                        .pane
                        .file_preview
                        .as_ref()
                        .unwrap()
                        .virtual_log
                        .as_ref()
                        .unwrap()
                        .read(cx)
                        .session
                        .is_some()
                }) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "returning Read reopens canonical Save"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            let inline = reader.read_with(visual, |reader, _| {
                reader
                    .pane
                    .file_preview
                    .as_ref()
                    .unwrap()
                    .virtual_log
                    .as_ref()
                    .unwrap()
                    .clone()
            });
            inline.update(visual, |panel, _| panel.watch = false);
            let start = visual.debug_bounds("virtual-start").unwrap();
            visual.simulate_click(start.center(), Modifiers::default());
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                visual.run_until_parked();
                if inline.read_with(visual, |panel, _| panel.view.is_some()) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "positive control: explicit Start must select the embedded source"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            inline.read_with(visual, |panel, cx| {
                assert!(panel.generation > 0);
                assert_eq!(panel.view.as_ref().unwrap().read(cx).total(), 2);
            });
            std::fs::remove_file(&path).unwrap();
            reader.update(visual, |reader, cx| {
                reader.pane.file_preview.as_mut().unwrap().unavailable =
                    Some("Deleted original".into());
                cx.notify();
            });
            visual.run_until_parked();
            assert!(
                visual.debug_bounds("virtual-definition-view").is_some(),
                "deleted original must not hide the retained criteria form"
            );
            assert!(visual.debug_bounds("reader-file-unavailable").is_none());
            drop(inline);
            reader.update_in(visual, |reader, window, cx| {
                reader.preview_file("broken.okilog", window, cx)
            });
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                visual.run_until_parked();
                if reader.read_with(visual, |reader, cx| {
                    reader
                        .pane
                        .file_preview
                        .as_ref()
                        .unwrap()
                        .virtual_log
                        .as_ref()
                        .unwrap()
                        .read(cx)
                        .error
                }) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "malformed definition must remain a visible typed error"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            visual.update(|window, cx| window.draw(cx).clear(cx));
            reader.read_with(visual, |reader, cx| {
                let preview = reader.pane.file_preview.as_ref().unwrap();
                let panel = preview.virtual_log.as_ref().unwrap().read(cx);
                assert!(panel.loaded.is_none() && panel.worker.is_none());
                assert_eq!(
                    PathBuf::from(panel.path.read(cx).value().as_str()),
                    vault.join("broken.okilog").canonicalize().unwrap()
                );
                assert!(!panel.message.is_empty());
            });
            reader.update_in(visual, |reader, window, cx| {
                reader.preview_file("logs/a.log", window, cx)
            });
            visual.run_until_parked();
            reader.read_with(visual, |reader, _| {
                let preview = reader.pane.file_preview.as_ref().unwrap();
                assert!(
                    preview.log.is_some() && preview.virtual_log.is_none(),
                    "ordinary log route stays intact"
                );
            });
        }
    }
}

impl Reader {
    /// Shows an already indexed log as the quick viewer's selection.
    pub(crate) fn mount_log(
        &mut self,
        document: LogDocument,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rel = document.rel.clone();
        let mut preview = match reader_files::FilePreview::load(&self.vault_root, &rel) {
            Ok(preview) => preview,
            Err(error) => {
                reader_toast::error(format!("Cannot show this log: {error:#}"), window, cx);
                return;
            }
        };
        let filters = self
            .pane
            .file_preview
            .as_ref()
            .filter(|previous| previous.path == document.file.path())
            .and_then(|previous| previous.log.as_ref())
            .map(|previous| previous.read(cx).filter_view_state());
        let view = cx.new(|cx| {
            let mut view = LogView::ready(document, window, cx).without_header();
            if let Some(filters) = filters {
                view.restore_filter_view(filters, window, cx);
            }
            view
        });
        view.read(cx).focus_handle().clone().focus(window, cx);
        preview.log = Some(view);
        self.pane.file_preview = Some(preview);
        self.pane.navigation.history = vec![rel.clone()];
        self.pane.navigation.history_positions = vec![ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        }];
        self.pane.navigation.history_ix = 0;
        self.pane.find_open = false;
        self.link_notice = None;
        self.show_window_title(Some(rel.to_string()), window, cx);
        self.record_usable_document(cx);
        cx.notify();
    }

    /// The header is a sibling of LogView. Preserve user bindings from its
    /// focus chain too, without handling a command twice after LogView did.
    #[inline(never)]
    fn log_header_command(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        invoke: impl FnOnce(&mut LogView, &mut Window, &mut Context<LogView>),
    ) {
        let Some(view) = self
            .pane
            .file_preview
            .as_ref()
            .and_then(|preview| preview.log.clone())
        else {
            return;
        };
        if !view.read(cx).focus.contains_focused(window, cx) {
            view.update(cx, |view, cx| invoke(view, window, cx));
        }
    }

    pub(crate) fn render_log_preview(
        &self,
        view: &Entity<LogView>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        v_flex()
            .id("reader-log-preview")
            .key_context("ReaderFile")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|reader, _: &FocusLogFilter, window, cx| {
                reader.log_header_command(window, cx, |view, window, cx| {
                    view.focus_filter(window, cx)
                })
            }))
            .on_action(cx.listener(|reader, _: &CopyRawLine, window, cx| {
                reader.log_header_command(window, cx, |view, window, cx| view.copy_raw(window, cx))
            }))
            .on_action(cx.listener(|reader, _: &ToggleLogQuery, window, cx| {
                reader.log_header_command(window, cx, |view, window, cx| {
                    view.toggle_query(window, cx)
                })
            }))
            .on_action(cx.listener(|reader, _: &ToggleLogFollow, window, cx| {
                reader.log_header_command(window, cx, |view, _, cx| view.toggle_follow(cx))
            }))
            .on_action(cx.listener(|reader, _: &ClearLogFilters, window, cx| {
                reader.log_header_command(window, cx, |view, window, cx| {
                    view.clear_filters(window, cx)
                })
            }))
            .on_action(cx.listener(|reader, _: &OpenLogLevels, window, cx| {
                reader.log_header_command(window, cx, |view, window, cx| {
                    view.show_menu(chrome::MenuKind::Level, window, cx)
                })
            }))
            .on_action(cx.listener(|reader, _: &OpenLogTimeRange, window, cx| {
                reader.log_header_command(window, cx, |view, window, cx| {
                    view.show_menu(chrome::MenuKind::Time, window, cx)
                })
            }))
            .on_action(cx.listener(|reader, _: &OpenLogSavedViews, window, cx| {
                reader.log_header_command(window, cx, |view, window, cx| {
                    view.show_menu(chrome::MenuKind::Saved, window, cx)
                })
            }))
            .on_action(cx.listener(|reader, _: &OpenLogColumns, window, cx| {
                reader.log_header_command(window, cx, |view, window, cx| {
                    view.show_menu(chrome::MenuKind::Columns, window, cx)
                })
            }))
            .on_action(cx.listener(|reader, _: &OpenLogExport, window, cx| {
                reader.log_header_command(window, cx, |view, window, cx| {
                    view.show_menu(chrome::MenuKind::Export, window, cx)
                })
            }))
            .on_action(cx.listener(|reader, _: &OpenLogFiles, window, cx| {
                reader.log_header_command(window, cx, |view, window, cx| {
                    view.show_menu(chrome::MenuKind::Files, window, cx)
                })
            }))
            .size_full()
            .min_h_0()
            .child(self.render_file_header_viewport(window, cx))
            .child(view.clone())
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use okilum_core::log::chips::{FilterChip, FilterMode, FilterValue};
    use okilum_core::log::correlation;
    use okilum_core::log::export::ExportFormat;
    use okilum_core::log::query::FieldRef;
    use okilum_core::log::saved_query::SavedQuery;
    use okilum_core::log::source_link::{self, Open};
    use okilum_core::log::tail::ResetReason;
    use okilum_core::log::{
        ansi, filter, Format, Level, LogFile, LogFilter, Query, TimeWindow, ValueKind,
    };
    use okilum_shell_logs::reader_log::{
        automatic_regex, match_counter, regex_error_summary, row_text, row_text_message,
        status_parts, ExportScope, Snapshot, SourceData, State, DETAIL_HEADER_HEIGHT,
        DETAIL_HEIGHT, MIN_LIST_ROWS, ROW_HEIGHT, WIDE_COLUMN_WIDTH,
    };
    include!("reader_log/filter_tests.rs");
    include!("reader_log/investigation_tests.rs");
    include!("reader_log/chrome_tests.rs");
    use ::core::prelude::v1::test;

    // GPUI's TestPlatform::quit is a no-op. Explicit shutdown also runs the
    // VisualTestContext::into_mut cleanup callbacks; otherwise separate tests
    // retain whole windows/snapshots and exhaust the real process-wide pool.
    struct TestShutdown(TestAppContext);
    impl Drop for TestShutdown {
        fn drop(&mut self) {
            self.0.quit();
        }
    }

    fn log(text: &str) -> (tempfile::TempDir, LogFile) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.jsonl");
        std::fs::write(&path, text).unwrap();
        let file = LogFile::open(&path).unwrap();
        (dir, file)
    }

    fn log_view(reader: &Entity<Reader>, visual: &mut VisualTestContext) -> Entity<LogView> {
        reader.read_with(visual, |reader, _| {
            reader
                .pane
                .file_preview
                .as_ref()
                .unwrap()
                .log
                .clone()
                .unwrap()
        })
    }

    /// #1156: the detail pane is resized with the handle above it, keeps
    /// the rows' and its header's minimums, collapses on a double-click and
    /// keeps its height for the next log and the next launch.
    #[gpui::test]
    fn detail_pane_resizes_collapses_and_remembers_its_height(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let directory = tempfile::tempdir().unwrap();
        let (root, state) = (
            directory.path().join("vault"),
            directory.path().join("state"),
        );
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(root.join("start.md"), "# Start").unwrap();
        let text =
            "{\"level\":\"info\",\"msg\":\"first\"}\n{\"level\":\"warn\",\"msg\":\"second\"}\n";
        std::fs::write(root.join("events.jsonl"), text).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            crate::bind_keys(cx);
            reader_ui_state::install(&state, cx);
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
                        index_dir: Some(directory.path().join("index")),
                        session_directory: Some(state.clone()),
                        panel_settings_override: Some(directory.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.simulate_resize(gpui::size(px(1440.), px(900.)));
        visual.run_until_parked();
        let open_log = |visual: &mut VisualTestContext| {
            reader.update_in(visual, |reader, window, cx| {
                let row = reader
                    .tree
                    .rows
                    .iter()
                    .find(|row| row.path == "events.jsonl")
                    .unwrap()
                    .clone();
                reader.activate_tree_row(&row, window, cx);
            });
            visual.run_until_parked();
            let view = log_view(&reader, visual);
            view.update_in(visual, |view, window, cx| view.select(0, true, window, cx));
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        };
        let pane = |visual: &mut VisualTestContext| {
            visual
                .debug_bounds("log-detail-pane")
                .expect("detail pane shown")
        };
        open_log(visual);
        let before = pane(visual);
        assert!(
            (before.size.height - px(DETAIL_HEIGHT)).abs() < px(1.),
            "positive control: the default height, {before:?}"
        );
        // Drag the handle up until the pane fills about 60% of the split.
        let handle = visual.debug_bounds("log-detail-handle").unwrap();
        let split_bottom = before.bottom();
        let target = point(handle.center().x, split_bottom - px(540.));
        visual.simulate_mouse_down(handle.center(), MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(
            handle.center() - point(px(0.), px(20.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.simulate_mouse_move(target, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(target, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let dragged = pane(visual);
        assert!(
            (dragged.size.height - px(540.)).abs() < px(HANDLE_TOLERANCE),
            "the pane follows the handle: {dragged:?}"
        );
        // Not past the rows' minimum.
        let rows_minimum = px(ROW_HEIGHT * MIN_LIST_ROWS);
        let view = log_view(&reader, visual);
        view.update(visual, |view, cx| {
            view.detail_split.drag_to(5_000., 600.);
            cx.notify();
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let tallest = pane(visual);
        assert!(
            tallest.size.height <= px(600. - reader_split::HANDLE_HEIGHT) - rows_minimum + px(1.),
            "the rows keep five lines: {tallest:?}"
        );
        view.update(visual, |view, cx| {
            view.detail_split.drag_to(540., 900.);
            cx.notify();
        });
        // A double-click collapses the pane to its header, another restores it.
        let double_click = |visual: &mut VisualTestContext| {
            let at = visual.debug_bounds("log-detail-handle").unwrap().center();
            for click_count in [1, 2] {
                visual.simulate_event(MouseDownEvent {
                    position: at,
                    button: MouseButton::Left,
                    click_count,
                    ..Default::default()
                });
                visual.simulate_event(MouseUpEvent {
                    position: at,
                    button: MouseButton::Left,
                    click_count,
                    ..Default::default()
                });
            }
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        };
        double_click(visual);
        assert!(
            (pane(visual).size.height - px(DETAIL_HEADER_HEIGHT)).abs() < px(1.),
            "collapsed to the header"
        );
        double_click(visual);
        assert!(
            (pane(visual).size.height - px(540.)).abs() < px(HANDLE_TOLERANCE),
            "restored"
        );
        // Remembered for the next log view and written for the next launch.
        reader.update_in(visual, |reader, window, cx| {
            reader.open_note("start.md", None, window, cx)
        });
        visual.run_until_parked();
        open_log(visual);
        assert!(
            (pane(visual).size.height - px(540.)).abs() < px(HANDLE_TOLERANCE),
            "the reopened log keeps the height"
        );
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(state.join("reader-ui.json")).unwrap()).unwrap();
        let saved = saved["split_sizes"]["log-detail"].as_f64().unwrap() as f32;
        assert!(
            (saved - 540.).abs() < HANDLE_TOLERANCE,
            "saved for the next launch: {saved}"
        );
    }

    /// The handle's hit area is a few pixels tall; the drop point lands
    /// somewhere inside it.
    const HANDLE_TOLERANCE: f32 = 8.;

    #[gpui::test]
    fn vault_tree_logs_publish_records_copy_and_return_to_notes(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        cx.update(|cx| {
            crate::test_init(cx);
            crate::bind_keys(cx);
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("start.md"), "# Start").unwrap();
        let text = "{\"level\":\"info\",\"msg\":\"first\"}\nnot json\n{\"level\":\"error\",\"msg\":\"second\"}\n";
        for name in ["events.log", "events.jsonl"] {
            std::fs::write(root.join(name), text).unwrap();
        }
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
                        index_dir: Some(directory.path().join("index")),
                        session_directory: Some(directory.path().join("state")),
                        panel_settings_override: Some(directory.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        for name in ["events.log", "events.jsonl"] {
            reader.update_in(visual, |reader, window, cx| {
                assert!(!reader.single_file && reader.searcher.is_some());
                let row = reader
                    .tree
                    .rows
                    .iter()
                    .find(|row| row.path == name)
                    .unwrap()
                    .clone();
                assert_eq!(row.kind, okilum_core::vault::EntryKind::Attachment);
                reader.activate_tree_row(&row, window, cx);
                let preview = reader.pane.file_preview.as_ref().unwrap();
                assert!(preview.log.is_some());
                assert!(preview.text.is_none());
                assert_eq!(reader.selected_file(), name);
            });
            visual.run_until_parked();
            let view = log_view(&reader, visual);
            view.read_with(visual, |view, _| {
                let file = view
                    .file()
                    .expect("vault log completed background indexing");
                assert_eq!(file.index().len(), 3);
                assert_eq!(file.index().get(0).unwrap().level(), Level::Info);
                assert_eq!(file.index().get(2).unwrap().level(), Level::Error);
            });
            // Tree activation retains navigator focus; a row selection gives
            // the log its normal keyboard context before Copy.
            view.update_in(visual, |view, window, cx| view.select(1, true, window, cx));
            visual.simulate_keystrokes(if cfg!(target_os = "macos") {
                "cmd-c"
            } else {
                "ctrl-c"
            });
            visual.read(|cx| {
                assert_eq!(
                    cx.read_from_clipboard().unwrap().text().unwrap(),
                    "not json"
                )
            });
            reader.update_in(visual, |reader, window, cx| {
                reader.open_note("start.md", None, window, cx)
            });
            visual.run_until_parked();
            reader.read_with(visual, |reader, _| {
                assert!(reader.pane.file_preview.is_none());
                assert_eq!(reader.pane.current_rel, "start.md");
            });
            assert_eq!(std::fs::read_to_string(root.join(name)).unwrap(), text);
        }
    }

    #[gpui::test]
    fn a_log_opens_in_the_quick_viewer_and_copies_its_raw_line(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        cx.update(|cx| {
            crate::test_init(cx);
            crate::bind_keys(cx);
        });
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("logs")).unwrap();
        let root = directory.path().join("logs").canonicalize().unwrap();
        let state = directory.path().join("state");
        let text = "{\"level\":\"info\",\"msg\":\"first\"}\nnot json\n{\"level\":\"error\",\"msg\":\"second\"}\n";
        std::fs::write(root.join("app.jsonl"), text).unwrap();
        std::fs::write(root.join("older.log"), "{\"level\":\"warn\"}\n").unwrap();
        std::fs::write(root.join("notes.md"), "# Notes").unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(root.join("app.jsonl")),
                        // An open folder containing the log must not take it
                        // down the Markdown path.
                        reusable_roots: vec![root.clone()],
                        session_directory: Some(state.clone()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.single_file && reader.searcher.is_none());
            assert_eq!(reader.selected_file(), "app.jsonl");
            assert!(reader.pane.file_preview.as_ref().unwrap().text.is_none());
            assert_eq!(reader.pane.navigation.history, ["app.jsonl"]);
            assert!(reader.vault.entries.iter().any(|e| e.path == "older.log"));
        });
        let view = log_view(&reader, visual);
        view.read_with(visual, |view, _| {
            assert_eq!(view.file().unwrap().index().len(), 3);
            assert_eq!(view.selected, None);
        });
        // Keyboard bindings reach the focused view.
        visual.simulate_keystrokes("down down");
        view.read_with(visual, |view, _| assert_eq!(view.selected, Some(1)));
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-c"
        } else {
            "ctrl-c"
        });
        reader.update_in(visual, |_, _, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "not json"
            );
        });
        assert_eq!(
            crate::reader_history::ReadingHistory::quick_document(&state, &root)
                .unwrap()
                .as_deref(),
            Some("app.jsonl"),
            "the log is what a restart reopens"
        );
        // A sibling log in the quick viewer is indexed off the UI thread.
        reader.update_in(visual, |reader, window, cx| {
            reader.open_note("older.log", None, window, cx)
        });
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.pane.file_preview.as_ref().unwrap().text.is_none());
        });
        let sibling = log_view(&reader, visual);
        sibling.read_with(visual, |view, _| {
            assert_eq!(view.file().unwrap().index().len(), 1);
        });
        reader.update_in(visual, |reader, window, cx| {
            reader.open_note("notes.md", None, window, cx)
        });
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.pane.file_preview.is_none());
            assert_eq!(reader.pane.current_rel, "notes.md");
        });
        assert_eq!(
            std::fs::read_to_string(root.join("app.jsonl")).unwrap(),
            text
        );
        assert!(!root.join(".okilum-index").exists());
    }

    #[gpui::test]
    fn queued_note_open_wins_over_the_initial_log_publication(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        cx.update(|cx| {
            crate::test_init(cx);
            crate::bind_keys(cx);
        });
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("files");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("app.log"), "initial log\n").unwrap();
        std::fs::write(root.join("notes.md"), "# Queued note\n").unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(root.join("app.log")),
                        session_directory: Some(fixture.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader.update(cx, |reader, _| {
                assert!(!reader.loading.as_ref().unwrap().published);
                assert!(reader.pane.file_preview.is_none());
                reader.pane.queued_open_note = Some("notes.md".into());
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(
                reader.pane.queued_open_note.is_none(),
                "publication consumed the request"
            );
            assert_eq!(reader.selected_file(), "notes.md");
            assert!(
                reader.pane.file_preview.is_none(),
                "the initial log cannot replace the newer note"
            );
            assert!(reader.document_ready());
        });
        assert_eq!(
            std::fs::read_to_string(root.join("app.log")).unwrap(),
            "initial log\n"
        );
    }

    fn open_log<'a>(
        text: &str,
        cx: &'a mut TestAppContext,
    ) -> (tempfile::TempDir, Entity<Reader>, &'a mut VisualTestContext) {
        open_named("app.jsonl", text.as_bytes(), cx)
    }

    /// Opens a file named `name` holding `bytes`, as a path given on launch.
    fn open_named<'a>(
        name: &str,
        bytes: &[u8],
        cx: &'a mut TestAppContext,
    ) -> (tempfile::TempDir, Entity<Reader>, &'a mut VisualTestContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            crate::bind_keys(cx);
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join(name), bytes).unwrap();
        let state = root.join(".state");
        // Admit fixture state before the initial shallow inventory, so Follow
        // measures source appends rather than a newly created sibling folder.
        std::fs::create_dir_all(&state).unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(root.join(name)),
                        session_directory: Some(state.clone()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        visual.run_until_parked();
        (directory, entity.unwrap(), visual)
    }

    #[gpui::test]
    fn issue2778_quick_log_open_shows_encoding_refusal(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let bytes: Vec<_> = [0xff, 0xfe]
            .into_iter()
            .chain(
                "hello i am a file\n"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes),
            )
            .collect();
        let (directory, reader, visual) = open_named("utf-16le.log", &bytes, cx);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        reader.read_with(visual, |reader, _| {
            let loading = reader.loading.as_ref().unwrap();
            assert!(!loading.active);
            assert_eq!(loading.phase, "UTF-16LE log encoding is not supported. Open a UTF-8 copy; the original file is unchanged.");
            assert!(reader.pane.file_preview.is_none(), "no RAW rows were published");
        });
        assert!(
            reader_viewer_chrome::tests::measured_bounds(visual, "reader-loading-phase").is_some()
        );
        assert_eq!(
            std::fs::read(directory.path().join("utf-16le.log")).unwrap(),
            bytes
        );
    }

    #[gpui::test]
    fn issue2778_embedded_log_refuses_on_reopen_and_keeps_utf8(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let (directory, reader, visual) = open_named("supported.log", b"hello i am a file\n", cx);
        let path = directory.path().join("utf-16le.log");
        let bytes: Vec<_> = [0xff, 0xfe]
            .into_iter()
            .chain(
                "hello i am a file\n"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes),
            )
            .collect();
        std::fs::write(&path, &bytes).unwrap();
        for _ in 0..2 {
            reader.update_in(visual, |reader, window, cx| {
                reader.open_note("utf-16le.log", None, window, cx);
            });
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
            log_view(&reader, visual).read_with(visual, |view, _| {
                assert!(matches!(view.state, State::Failed(_)));
                assert_eq!(view.visible_len(), 0);
                assert_eq!(view.status(), "Cannot show this log: UTF-16LE log encoding is not supported. Open a UTF-8 copy; the original file is unchanged.");
            });
            assert!(reader_viewer_chrome::tests::measured_bounds(visual, "log-status").is_some());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            reader.update_in(visual, |reader, window, cx| {
                reader.open_note("supported.log", None, window, cx);
            });
            visual.run_until_parked();
            log_view(&reader, visual).read_with(visual, |view, _| {
                assert_eq!(view.visible_len(), 1);
                assert_eq!(view.file().unwrap().raw(0), Some(&b"hello i am a file"[..]));
            });
        }
    }

    #[gpui::test]
    fn a_rotated_gzip_log_opens_decoded_and_without_follow(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        // Packed by the system gzip; the text is the pack's 649-line access log.
        let packed = include_bytes!("../../okilum-core/tests/fixtures/log/names/access.log.1.gz");
        let (_directory, reader, visual) = open_named("app.log.1.gz", packed, cx);
        let view = log_view(&reader, visual);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        view.read_with(visual, |view, _| {
            let file = view.file().unwrap();
            assert!(file.is_compressed());
            assert_eq!(file.index().format(), Format::AccessLog);
            assert_eq!(view.visible_len(), 649);
        });
        assert!(
            reader_viewer_chrome::tests::measured_bounds(visual, "log-follow").is_none(),
            "a compressed log is not followed"
        );
    }

    #[gpui::test]
    fn a_glog_file_name_opens_in_the_log_view(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let name = "ordgw.trade-gw-01.god.log.INFO.20261010-060400.4411";
        let text = include_bytes!(
            "../../okilum-core/tests/fixtures/log/names/ordgw.trade-gw-01.god.log.INFO.20261010-060400.4411"
        );
        let (_directory, reader, visual) = open_named(name, text, cx);
        let view = log_view(&reader, visual);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        view.read_with(visual, |view, _| {
            assert_eq!(view.file().unwrap().index().format(), Format::Glog);
            assert_eq!(view.visible_len(), 26);
        });
        // Positive control for the test above: an uncompressed log has Follow.
        assert!(reader_viewer_chrome::tests::measured_bounds(visual, "log-follow").is_some());
    }

    fn shown(view: &Entity<LogView>, visual: &mut VisualTestContext) -> Vec<usize> {
        view.read_with(visual, |view, _| {
            (0..view.visible_len())
                .map(|row| view.entry_at(row).unwrap())
                .collect()
        })
    }

    fn counter(view: &Entity<LogView>, visual: &mut VisualTestContext) -> Option<String> {
        view.read_with(visual, |view, _| {
            match_counter(
                view.rows.as_ref().map(|rows| rows.len()),
                view.data().unwrap().index().len(),
                view.filtering,
            )
        })
    }

    #[gpui::test]
    fn filters_narrow_rows_in_the_background_and_keep_the_selection(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let text = concat!(
            r#"{"ts":"2025-10-01T00:00:00Z","level":"info","msg":"boot","status":200}"#,
            "\n",
            r#"{"ts":"2025-10-01T00:50:00Z","level":"error","msg":"Cache miss","status":500}"#,
            "\n",
            "panicked at main.rs\n",
            r#"{"ts":"2025-10-01T01:00:00Z","msg":"no level","status":500}"#,
            "\n",
        );
        let (directory, reader, visual) = open_log(text, cx);
        let view = log_view(&reader, visual);
        assert_eq!(shown(&view, visual), [0, 1, 2, 3]);
        assert_eq!(counter(&view, visual), None, "no counter without a filter");

        // The keyboard shortcut reaches the filter field, and typing filters.
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-f"
        } else {
            "ctrl-f"
        });
        visual.simulate_input("CACHE");
        visual.run_until_parked();
        view.read_with(visual, |view, _| assert_eq!(view.filter.text, "CACHE"));
        assert_eq!(shown(&view, visual), [1]);
        assert_eq!(counter(&view, visual).as_deref(), Some("1 of 4"));

        // Clearing returns every row and hides the counter.
        view.update_in(visual, |view, window, cx| view.clear_filters(window, cx));
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [0, 1, 2, 3]);
        view.read_with(visual, |view, cx| {
            assert!(view.text_input.read(cx).value().is_empty());
            assert!(!view.filter.is_active());
        });

        // Level: lines without a level stay unless the toggle hides them.
        view.update(visual, |view, cx| {
            view.update_filter(|filter| filter.level.min = Some(Level::Error), cx)
        });
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [1, 2, 3]);
        assert_eq!(counter(&view, visual).as_deref(), Some("3 of 4"));
        view.update(visual, |view, cx| {
            view.update_filter(|filter| filter.level.without_level = false, cx)
        });
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [1]);

        // Selection and arrow keys walk the filtered rows.
        view.update(visual, |view, cx| {
            view.update_filter(|filter| filter.level = Default::default(), cx)
        });
        visual.run_until_parked();
        view.update_in(visual, |view, window, cx| view.select(3, true, window, cx));
        view.update_in(visual, |view, window, cx| {
            view.add_field_term(
                "status",
                FilterValue::Value {
                    kind: ValueKind::Number,
                    text: "500".into(),
                },
                window,
                cx,
            )
        });
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [1, 3]);
        view.read_with(visual, |view, _| {
            assert_eq!(view.selected, Some(3), "the chip's own record stays");
            assert_eq!(view.row_of(3), Some(1));
        });
        visual.simulate_keystrokes("up");
        view.read_with(visual, |view, _| assert_eq!(view.selected, Some(1)));

        // Time window from the newest record; a selection it hides is dropped.
        view.update(visual, |view, cx| {
            view.update_filter(|filter| filter.time = TimeWindow::Last5Minutes, cx)
        });
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [3]);
        view.read_with(visual, |view, _| assert_eq!(view.selected, None));

        // Nothing matches: no zero count, a quiet empty state instead.
        view.update(visual, |view, cx| {
            view.set_text("nothing like it".into(), cx)
        });
        visual.run_until_parked();
        assert!(shown(&view, visual).is_empty());
        assert_eq!(counter(&view, visual), None);
        view.update_in(visual, |view, window, cx| {
            view.replace_query(String::new(), window, cx)
        });
        view.update(visual, |view, cx| view.set_text(String::new(), cx));
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [3], "only the time window is left");
        assert_eq!(
            std::fs::read_to_string(directory.path().join("app.jsonl")).unwrap(),
            text
        );
    }

    /// #1158: the query bar filters in the background, a typo shows its
    /// column and keeps the last valid rows, a detail-pane field narrows the
    /// same query, and applied queries are kept per file.
    #[gpui::test]
    fn query_bar_filters_reports_typos_and_remembers_queries(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let text = concat!(
            r#"{"level":"info","msg":"boot","status":200,"pod":"checkout-1"}"#,
            "\n",
            r#"{"level":"warn","msg":"slow","status":200,"pod":"checkout-2"}"#,
            "\n",
            r#"{"level":"error","msg":"upstream","status":503,"pod":"checkout-2","err":"timeout"}"#,
            "\n",
            r#"{"level":"error","msg":"bad gateway","status":502,"pod":"payments-1"}"#,
            "\n",
        );
        let state = tempfile::tempdir().unwrap();
        cx.update(|cx| reader_ui_state::install(state.path(), cx));
        let (_directory, reader, visual) = open_log(text, cx);
        let view = log_view(&reader, visual);
        let type_query = |visual: &mut VisualTestContext, query: &str| {
            view.update_in(visual, |view, window, cx| {
                view.query_input
                    .update(cx, |input, cx| input.set_value(query, window, cx));
                view.set_query(query.into(), cx);
            });
            visual.run_until_parked();
        };
        assert_eq!(shown(&view, visual), [0, 1, 2, 3], "positive control");
        type_query(visual, "level>=warn and status>=500");
        assert_eq!(shown(&view, visual), [2, 3]);
        assert_eq!(counter(&view, visual).as_deref(), Some("2 of 4"));
        type_query(visual, "exists(err)");
        assert_eq!(shown(&view, visual), [2]);

        // A typo: the error names its column, the rows stay as they were.
        type_query(visual, "exists(err) and pod ~ checkout");
        view.read_with(visual, |view, _| {
            let (column, _) = view.query_error.clone().expect("a parse error");
            assert_eq!(column, 21, "{:?}", view.query_error);
        });
        assert_eq!(shown(&view, visual), [2], "the last valid query stays");
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let error = visual
            .debug_bounds("log-query-error")
            .expect("the error shows in the query field");
        let field = visual.debug_bounds("log-query").expect("query field");
        assert!(field.contains(&error.center()), "inside the field");

        // A detail-pane field narrows the same query.
        type_query(visual, "level>=error");
        assert_eq!(shown(&view, visual), [2, 3]);
        view.update_in(visual, |view, window, cx| {
            view.add_field_term("pod", FilterValue::string("payments-1"), window, cx)
        });
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [3]);
        view.read_with(visual, |view, cx| {
            assert_eq!(
                view.query_input.read(cx).value().as_ref(),
                r#"level >= error and typed(pod, string, "payments-1")"#
            );
            assert!(view.query_error.is_none());
        });
        // The same field again changes nothing.
        view.update_in(visual, |view, window, cx| {
            view.add_field_term("pod", FilterValue::string("payments-1"), window, cx)
        });
        view.read_with(visual, |view, cx| {
            assert_eq!(
                view.query_input.read(cx).value().as_ref(),
                r#"level >= error and typed(pod, string, "payments-1")"#
            );
        });

        // History: newest first, applied by the menu, kept for the file.
        view.update(visual, |view, cx| view.remember_query(cx));
        type_query(visual, "status=200");
        view.update(visual, |view, cx| view.remember_query(cx));
        let key = view.read_with(visual, |view, _| view.history_key().unwrap());
        let history = visual.update(|_, cx| reader_ui_state::log_queries(&key, cx));
        assert_eq!(
            history,
            [
                "status = 200",
                r#"level >= error and typed(pod, string, "payments-1")"#
            ]
        );
        view.update_in(visual, |view, window, cx| {
            view.replace_query(history[1].clone(), window, cx)
        });
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [3]);
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(state.path().join("reader-ui.json")).unwrap())
                .unwrap();
        assert_eq!(saved["log_queries"][&key][0], "status = 200");

        // Clear filters empties the query and its error.
        view.update_in(visual, |view, window, cx| view.clear_filters(window, cx));
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [0, 1, 2, 3]);
        view.read_with(visual, |view, cx| {
            assert!(view.query_input.read(cx).value().is_empty());
            assert!(view.filter.query.is_none());
        });
    }

    /// #1159 acceptance, simulated: a writer appends every poll interval for
    /// 600 polls (150 s at 4 polls/s) while a query is active, and the file
    /// is moved and recreated halfway, as logrotate does. Every poll the
    /// rows are exactly the matching records written so far, in file order,
    /// none twice and none missing, and the view stays pinned.
    #[gpui::test]
    fn follow_mode_stays_exact_over_many_polls_and_a_rotation(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        use std::io::Write as _;
        let line = |n: usize| {
            let level = if n.is_multiple_of(10) { "warn" } else { "info" };
            format!("{{\"level\":\"{level}\",\"msg\":\"tick {n}\"}}\n")
        };
        let (directory, reader, visual) = open_log(&line(1), cx);
        let path = directory.path().canonicalize().unwrap().join("app.jsonl");
        let view = log_view(&reader, visual);
        view.update(visual, |view, cx| view.toggle_follow(cx));
        visual.run_until_parked();
        view.update(visual, |view, cx| view.set_query("level>=warn".into(), cx));
        visual.run_until_parked();
        let mut written = 1;
        for poll in 0..600usize {
            if poll == 300 {
                std::fs::rename(&path, path.with_extension("jsonl.1")).unwrap();
                std::fs::write(&path, "").unwrap();
            }
            // 2–3 records per 250 ms: 10 records a second.
            let burst = if poll.is_multiple_of(2) { 3 } else { 2 };
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            for _ in 0..burst {
                written += 1;
                file.write_all(line(written).as_bytes()).unwrap();
            }
            drop(file);
            visual.executor().advance_clock(Duration::from_millis(250));
            visual.run_until_parked();
            if poll % 50 == 49 || poll == 299 || poll == 300 || poll == 599 {
                // The rows are every warn record written so far, in order:
                // after the rotation the earlier file is listed first under
                // its own name (#1160), so record n is still row n - 1.
                let expected: Vec<usize> = (1..=written)
                    .filter(|n| n.is_multiple_of(10))
                    .map(|n| n - 1)
                    .collect();
                assert_eq!(shown(&view, visual), expected, "poll {poll}");
                view.read_with(visual, |view, _| {
                    let follow = view.follow.as_ref().unwrap();
                    assert!(follow.pinned && follow.unseen == 0, "poll {poll}");
                    assert_eq!(
                        follow.reset.map(|reset| reset.0),
                        (poll >= 300).then_some(ResetReason::Rotated),
                        "poll {poll}"
                    );
                });
            }
        }
        assert!(
            written > 1_200,
            "positive control: {written} records written"
        );
    }

    /// #1159: follow mode shows appended records, filters them as they
    /// arrive, pauses when scrolled up with an "N new" pill that resumes it,
    /// and announces a rotation and a truncation instead of a silent reset.
    #[gpui::test]
    fn follow_mode_appends_filters_pauses_and_announces_resets(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        use std::io::Write as _;
        let line =
            |level: &str, msg: &str| format!("{{\"level\":\"{level}\",\"msg\":\"{msg}\"}}\n");
        let text: String = (0..60)
            .map(|i| line("info", &format!("tick {i}")))
            .collect();
        let (directory, reader, visual) = open_log(&text, cx);
        let path = directory.path().canonicalize().unwrap().join("app.jsonl");
        let append = |text: &str| {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            file.write_all(text.as_bytes()).unwrap();
        };
        let tick = |visual: &mut VisualTestContext| {
            visual.executor().advance_clock(Duration::from_millis(600));
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        };
        let view = log_view(&reader, visual);
        let len = |visual: &mut VisualTestContext| view.read_with(visual, |v, _| v.visible_len());

        // Positive control: without follow, an append is not shown.
        append(&line("error", "early"));
        tick(visual);
        assert_eq!(len(visual), 60);

        view.update(visual, |view, cx| view.toggle_follow(cx));
        visual.run_until_parked();
        tick(visual);
        assert_eq!(len(visual), 61, "follow reads the file afresh");
        view.read_with(visual, |view, _| {
            assert!(view.following());
            assert!(view.follow.as_ref().unwrap().pinned);
        });

        // A query applies to records as they arrive.
        view.update(visual, |view, cx| view.set_query("level>=warn".into(), cx));
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [60]);
        append(&(line("warn", "w1") + &line("info", "quiet") + &line("error", "e1")));
        tick(visual);
        assert_eq!(
            shown(&view, visual),
            [60, 61, 63],
            "appended rows are filtered"
        );

        // Scrolling up pauses; new records are counted on a pill.
        view.update(visual, |view, cx| view.set_query(String::new(), cx));
        visual.run_until_parked();
        tick(visual);
        assert_eq!(len(visual), 64);
        let list = visual.debug_bounds("reader-log").unwrap();
        visual.simulate_event(ScrollWheelEvent {
            position: list.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(600.))),
            ..Default::default()
        });
        visual.run_until_parked();
        tick(visual);
        view.read_with(visual, |view, _| {
            assert!(!view.follow.as_ref().unwrap().pinned, "scrolled up: paused")
        });
        append(&(line("info", "n1") + &line("info", "n2") + &line("info", "n3")));
        tick(visual);
        assert_eq!(len(visual), 67);
        view.read_with(visual, |view, _| {
            assert_eq!(view.follow.as_ref().unwrap().unseen, 3)
        });
        let pill = visual
            .debug_bounds("log-follow-new")
            .expect("the N new pill shows");
        visual.simulate_click(pill.center(), Modifiers::default());
        visual.run_until_parked();
        tick(visual);
        view.read_with(visual, |view, _| {
            let follow = view.follow.as_ref().unwrap();
            assert!(follow.pinned && follow.unseen == 0, "resumed");
        });
        assert!(visual.debug_bounds("log-follow-new").is_none());

        // Rotation: move and recreate; the new file is followed and the
        // earlier records stay listed under their own name (#1160).
        std::fs::rename(&path, path.with_extension("jsonl.1")).unwrap();
        std::fs::write(&path, line("info", "fresh 1") + &line("warn", "fresh 2")).unwrap();
        tick(visual);
        tick(visual);
        assert_eq!(len(visual), 69, "67 earlier records and the new file's 2");
        view.read_with(visual, |view, _| {
            assert_eq!(view.source_count(), 2);
            assert_eq!(view.source_name(1), "app.jsonl (before rotation)");
            // The earlier records come first, then the new file's.
            assert_eq!(view.locate(0).unwrap().0, 1);
            assert_eq!(view.locate(68).unwrap(), (0, 1));
        });
        let reset = visual
            .debug_bounds("log-follow-reset")
            .expect("a separator");
        assert!(reset.size.height > px(0.));
        view.read_with(visual, |view, _| {
            assert_eq!(
                view.follow.as_ref().unwrap().reset,
                Some((ResetReason::Rotated, 67))
            );
        });

        // Truncation of the followed file keeps its two records as well.
        std::fs::write(&path, line("info", "after truncate")).unwrap();
        tick(visual);
        assert_eq!(len(visual), 70);
        view.read_with(visual, |view, _| {
            assert_eq!(view.source_name(2), "app.jsonl (before truncation)");
        });
        view.read_with(visual, |view, _| {
            assert_eq!(
                view.follow.as_ref().unwrap().reset.map(|r| r.0),
                Some(ResetReason::Truncated)
            );
        });

        // Follow off: appends wait until it is on again.
        view.update(visual, |view, cx| view.toggle_follow(cx));
        append(&line("info", "later"));
        tick(visual);
        assert_eq!(len(visual), 70);
        view.update(visual, |view, cx| view.toggle_follow(cx));
        visual.run_until_parked();
        tick(visual);
        assert_eq!(len(visual), 71);
    }

    /// #1282: a merged view with a query stays on its newest match while
    /// following, from an empty result through appends and a rotation.
    #[gpui::test]
    fn a_followed_merged_view_with_a_query_stays_at_the_newest_match(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        use std::io::Write as _;
        let line = |n: u32, level: &str, msg: &str| {
            format!(
                "{{\"ts\":\"2025-10-01T{:02}:{:02}:{:02}Z\",\"level\":\"{level}\",\"msg\":\"{msg}\"}}\n",
                n / 3600,
                n / 60 % 60,
                n % 60
            )
        };
        let lines = |from: u32, step: u32, count: u32, level: &str| -> String {
            (0..count)
                .map(|i| line(from + i * step, level, &format!("r{}", from + i * step)))
                .collect()
        };
        // Nothing matches level>=warn at first.
        let (directory, reader, visual) = open_log(&line(1, "info", "a1"), cx);
        let root = directory.path().canonicalize().unwrap();
        let a = root.join("app.jsonl");
        let b = root.join("b.jsonl");
        std::fs::write(&b, line(2, "info", "b2")).unwrap();
        let append = |path: &Path, text: &str| {
            let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
            file.write_all(text.as_bytes()).unwrap();
        };
        let tick = |visual: &mut VisualTestContext| {
            visual.executor().advance_clock(Duration::from_millis(600));
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        };
        // Exactly one poll: the one that reads the new records. A writer that
        // never stops leaves no quiet poll after it to pin the view again.
        let poll = |visual: &mut VisualTestContext| {
            visual
                .executor()
                .advance_clock(okilum_core::log::tail::LOCAL_POLL_INTERVAL);
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        };
        let view = log_view(&reader, visual);
        // (the list overflows, it is scrolled to the end)
        let place = |visual: &mut VisualTestContext| {
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.update(|window, cx| window.draw(cx).clear(cx));
            view.read_with(visual, |view, _| {
                let handle = view.scroll.0.borrow();
                let offset = handle.base_handle.offset();
                let max = handle.base_handle.max_offset();
                (max.y > px(0.), -offset.y >= max.y - px(2.))
            })
        };
        view.update(visual, |view, cx| view.add_files(vec![b.clone()], cx));
        visual.run_until_parked();
        view.update(visual, |view, cx| view.set_query("level>=warn".into(), cx));
        visual.run_until_parked();
        assert_eq!(view.read_with(visual, |view, _| view.visible_len()), 0);
        view.update(visual, |view, cx| view.toggle_follow(cx));
        visual.run_until_parked();
        tick(visual);

        // Appends to both files, interleaved in time so the merge is rebuilt
        // and refiltered: far more matches than fit.
        append(&a, &lines(10, 2, 60, "warn"));
        append(&b, &lines(11, 2, 60, "error"));
        poll(visual);
        assert_eq!(view.read_with(visual, |view, _| view.visible_len()), 120);
        assert_eq!(
            place(visual),
            (true, true),
            "newest match in view after appends"
        );

        // Rotation of the added file, then more records in both files.
        std::fs::rename(&b, b.with_extension("jsonl.1")).unwrap();
        std::fs::write(&b, lines(200, 2, 5, "warn")).unwrap();
        tick(visual);
        tick(visual);
        append(&a, &lines(211, 2, 5, "warn"));
        append(&b, &lines(210, 2, 5, "error"));
        poll(visual);
        assert_eq!(view.read_with(visual, |view, _| view.visible_len()), 135);
        view.read_with(visual, |view, _| {
            assert!(view.following() && view.follow.as_ref().unwrap().pinned)
        });
        assert_eq!(
            place(visual),
            (true, true),
            "newest match in view after rotation"
        );
    }

    /// #1160: follow covers every file of a merged view. A newer record
    /// lands at the end in place, an older one rebuilds the time order, the
    /// query applies to both, and a rotated added file keeps its records.
    #[gpui::test]
    fn follow_covers_every_file_of_a_merged_view(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        use std::io::Write as _;
        let line = |second: u32, level: &str, msg: &str| {
            format!(
                "{{\"ts\":\"2025-10-01T00:00:{second:02}Z\",\"level\":\"{level}\",\"msg\":\"{msg}\"}}\n"
            )
        };
        let (directory, reader, visual) =
            open_log(&(line(1, "info", "a1") + &line(3, "warn", "a3")), cx);
        let root = directory.path().canonicalize().unwrap();
        let a = root.join("app.jsonl");
        let b = root.join("b.jsonl");
        std::fs::write(&b, line(2, "info", "b2") + &line(4, "error", "b4")).unwrap();
        let append = |path: &Path, text: &str| {
            let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
            file.write_all(text.as_bytes()).unwrap();
        };
        let tick = |visual: &mut VisualTestContext| {
            visual.executor().advance_clock(Duration::from_millis(600));
            visual.run_until_parked();
        };
        let messages = |visual: &mut VisualTestContext, view: &Entity<LogView>| {
            view.read_with(visual, |view, _| {
                (0..view.visible_len())
                    .map(|row| {
                        let id = view.entry_at(row).unwrap();
                        let record = String::from_utf8_lossy(view.raw_at(id).unwrap()).into_owned();
                        record
                            .split("\"msg\":\"")
                            .nth(1)
                            .unwrap()
                            .trim_end_matches("\"}")
                            .to_owned()
                    })
                    .collect::<Vec<_>>()
            })
        };
        let view = log_view(&reader, visual);
        view.update(visual, |view, cx| view.add_files(vec![b.clone()], cx));
        visual.run_until_parked();
        assert_eq!(messages(visual, &view), ["a1", "b2", "a3", "b4"]);
        view.update(visual, |view, cx| view.toggle_follow(cx));
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert!(view.following());
            assert!(
                matches!(view.extra[0].data, SourceData::Tail(_)),
                "b is followed too"
            );
        });

        // Newer records in either file land at the end.
        append(&b, &line(5, "warn", "b5"));
        tick(visual);
        append(&a, &line(6, "info", "a6"));
        tick(visual);
        assert_eq!(
            messages(visual, &view),
            ["a1", "b2", "a3", "b4", "b5", "a6"]
        );

        // The query applies to appended records of every file.
        view.update(visual, |view, cx| view.set_query("level>=warn".into(), cx));
        visual.run_until_parked();
        assert_eq!(messages(visual, &view), ["a3", "b4", "b5"]);
        append(&b, &line(7, "error", "b7"));
        tick(visual);
        assert_eq!(messages(visual, &view), ["a3", "b4", "b5", "b7"]);
        // An older record is placed in time order, not at the end.
        append(&a, &line(0, "warn", "a0 late"));
        tick(visual);
        assert_eq!(messages(visual, &view), ["a0 late", "a3", "b4", "b5", "b7"]);

        // Rotation of the added file keeps its records under their own name.
        view.update(visual, |view, cx| view.set_query(String::new(), cx));
        visual.run_until_parked();
        std::fs::rename(&b, b.with_extension("jsonl.1")).unwrap();
        std::fs::write(&b, line(8, "info", "b8 new file")).unwrap();
        tick(visual);
        tick(visual);
        view.read_with(visual, |view, _| {
            let names: Vec<String> = (0..view.source_count())
                .map(|s| view.source_name(s))
                .collect();
            assert!(
                names.contains(&"b.jsonl (before rotation)".to_owned()),
                "{names:?}"
            );
        });
        let shown = messages(visual, &view);
        for msg in ["b2", "b4", "b5", "b7", "b8 new file"] {
            assert!(shown.contains(&msg.to_owned()), "{msg} in {shown:?}");
        }
        assert_eq!(shown.last().map(String::as_str), Some("b8 new file"));
    }

    /// #1160 acceptance over the sample pack: the Kubernetes stream plus the
    /// kubelet and Apache logs read in one time order, a query covers every
    /// file, hiding a file keeps the selection when its record stays shown,
    /// and a time window ends at the newest record of all files.
    #[gpui::test]
    fn several_files_read_in_time_order_and_filter_together(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let samples =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../okilum-core/tests/fixtures/log/samples");
        let names = [
            "kubernetes/kubernetes-fluentbit.ndjson",
            "kubernetes/kubelet.jsonl",
            "apache/apache-access.logfmt",
            "apache/apache-error.jsonl",
        ];
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let paths: Vec<PathBuf> = names
            .iter()
            .map(|name| {
                let target = root.join(Path::new(name).file_name().unwrap());
                std::fs::copy(samples.join(name), &target).unwrap();
                target
            })
            .collect();
        cx.update(|cx| {
            crate::test_init(cx);
            crate::bind_keys(cx);
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(paths[0].clone()),
                        session_directory: Some(root.join(".state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        let view = log_view(&reader, visual);
        let alone = view.read_with(visual, |view, _| view.total());
        view.update(visual, |view, cx| view.add_files(paths[1..].to_vec(), cx));
        visual.run_until_parked();
        let files: Vec<LogFile> = paths
            .iter()
            .map(|path| LogFile::open(path).unwrap())
            .collect();
        let records: usize = files.iter().map(|file| file.index().len()).sum();
        view.read_with(visual, |view, _| {
            assert_eq!(view.source_count(), 4);
            assert_eq!(view.total(), records);
            assert!(records > alone, "positive control: the files joined");
            // Time order across files: no timestamped row is older than one above it.
            let mut last = i64::MIN;
            let mut from_each = [false; 4];
            for id in 0..view.total() {
                let (source, entry) = view.locate(id).unwrap();
                from_each[source] = true;
                if let Some(ts) = view
                    .source(source)
                    .unwrap()
                    .index()
                    .get(entry)
                    .unwrap()
                    .timestamp()
                {
                    assert!(ts >= last, "row {id} goes back in time");
                    last = ts;
                }
            }
            assert_eq!(from_each, [true; 4], "rows of every file");
        });

        // A query over every file: the union of each file's own matches.
        let per_file = |filter: &LogFilter| -> usize {
            files
                .iter()
                .map(|file| {
                    filter::evaluate_range(
                        filter,
                        file.index(),
                        file.bytes(),
                        0..file.index().len(),
                    )
                    .len()
                })
                .sum()
        };
        view.update(visual, |view, cx| view.set_query("level>=error".into(), cx));
        visual.run_until_parked();
        let errors = per_file(&LogFilter {
            query: Some(Query::parse("level>=error").unwrap()),
            ..Default::default()
        });
        assert!(errors > 0);
        assert_eq!(view.read_with(visual, |view, _| view.visible_len()), errors);
        let shown = shown(&view, visual);
        assert!(
            shown.windows(2).all(|pair| pair[0] < pair[1]),
            "rows stay in time order"
        );

        // Hiding a file keeps the selection when its record stays shown.
        let kubelet_row = view.read_with(visual, |view, _| {
            (0..view.total())
                .find(|&id| view.locate(id).unwrap().0 == 1)
                .unwrap()
        });
        view.update(visual, |view, cx| view.set_query(String::new(), cx));
        visual.run_until_parked();
        view.update_in(visual, |view, window, cx| {
            view.select(kubelet_row, true, window, cx)
        });
        let access = files[2].index().len();
        view.update(visual, |view, cx| view.set_source_enabled(2, false, cx));
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert_eq!(view.total(), records - access);
            let selected = view.selected.expect("the kubelet record stays selected");
            assert_eq!(view.locate(selected).unwrap().0, 1);
        });
        // Hiding the selected record's file drops the selection.
        view.update(visual, |view, cx| view.set_source_enabled(1, false, cx));
        visual.run_until_parked();
        view.read_with(visual, |view, _| assert_eq!(view.selected, None));
        view.update(visual, |view, cx| {
            view.set_source_enabled(1, true, cx);
            view.set_source_enabled(2, true, cx);
        });
        visual.run_until_parked();

        // The time window ends at the newest record of all files.
        let newest = files
            .iter()
            .filter_map(|file| {
                file.index()
                    .entries()
                    .iter()
                    .filter_map(|e| e.timestamp())
                    .max()
            })
            .max();
        view.update(visual, |view, cx| {
            view.update_filter(|filter| filter.time = TimeWindow::Last5Minutes, cx)
        });
        visual.run_until_parked();
        let windowed = per_file(&LogFilter {
            time: TimeWindow::Last5Minutes,
            window_end: newest,
            ..Default::default()
        });
        assert_eq!(
            view.read_with(visual, |view, _| view.visible_len()),
            windowed
        );
        assert!(
            windowed < records,
            "positive control: the window hides older rows"
        );

        // One stripe and file name per row while several files are shown.
        visual.update(|window, cx| window.draw(cx).clear(cx));
        view.read_with(visual, |view, _| {
            assert!(view.merge.is_some());
            assert!(!view.following() && view.extra.len() == 3);
        });
    }

    /// #1160: log files dropped on the view join it and other files are
    /// named, not opened; extra log paths from the command line open with
    /// the first.
    #[gpui::test]
    fn dropped_and_command_line_logs_join_the_view(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let line = |msg: &str| {
            format!(
                "{{\"ts\":\"2025-10-01T00:00:0{}Z\",\"msg\":\"{msg}\"}}\n",
                msg.len() % 10
            )
        };
        let (directory, reader, visual) = open_log(&line("first"), cx);
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("second.jsonl"), line("second") + &line("again")).unwrap();
        std::fs::write(root.join("notes.md"), "# Notes\n").unwrap();
        let view = log_view(&reader, visual);
        assert_eq!(view.read_with(visual, |view, _| view.source_count()), 1);
        let center = visual.debug_bounds("reader-log").unwrap().center();
        let paths = ExternalPaths(
            [root.join("second.jsonl"), root.join("notes.md")]
                .into_iter()
                .collect(),
        );
        visual.simulate_event(FileDropEvent::Entered {
            position: center,
            paths,
        });
        visual.simulate_event(FileDropEvent::Submit { position: center });
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert_eq!(view.source_count(), 2, "the log joined, the note did not");
            assert!(view.has_file(&root.join("second.jsonl")));
            assert_eq!(view.total(), 3);
        });
        // Dropping it again adds nothing.
        visual.simulate_event(FileDropEvent::Entered {
            position: center,
            paths: ExternalPaths([root.join("second.jsonl")].into_iter().collect()),
        });
        visual.simulate_event(FileDropEvent::Submit { position: center });
        visual.run_until_parked();
        assert_eq!(view.read_with(visual, |view, _| view.source_count()), 2);
        drop(directory);

        // Command line: `okilum a.jsonl b.jsonl` opens both in one view.
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("a.jsonl"), line("a")).unwrap();
        std::fs::write(root.join("b.jsonl"), line("bb") + &line("bbb")).unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(root.join("a.jsonl")),
                        extra_logs: vec![root.join("b.jsonl")],
                        session_directory: Some(root.join(".state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        visual.run_until_parked();
        let view = log_view(&entity.unwrap(), visual);
        view.read_with(visual, |view, _| {
            assert_eq!(view.source_count(), 2);
            assert_eq!(view.total(), 3);
            assert!(view.merge.is_some());
        });
    }

    #[gpui::test]
    fn a_followed_stack_trace_grows_its_row_and_is_filtered_again(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        use std::io::Write as _;
        let line =
            |level: &str, msg: &str| format!("{{\"level\":\"{level}\",\"msg\":\"{msg}\"}}\n");
        let (directory, reader, visual) = open_log(&line("info", "start"), cx);
        let path = directory.path().canonicalize().unwrap().join("app.jsonl");
        let append = |text: &str| {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            file.write_all(text.as_bytes()).unwrap();
        };
        let tick = |visual: &mut VisualTestContext| {
            visual.executor().advance_clock(Duration::from_millis(600));
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        };
        let view = log_view(&reader, visual);
        view.update(visual, |view, cx| view.toggle_follow(cx));
        visual.run_until_parked();
        tick(visual);
        // The word is only in a frame that has not been written yet.
        view.update(visual, |view, cx| view.set_text("Pool.java".into(), cx));
        visual.run_until_parked();
        append(
            &(line("error", "request failed") + "\tat com.example.Handler.run(Handler.java:12)\n"),
        );
        tick(visual);
        assert!(
            shown(&view, visual).is_empty(),
            "positive control: no match yet"
        );
        append("\tat com.example.Pool.take(Pool.java:88)\n");
        tick(visual);
        assert_eq!(shown(&view, visual), [1], "the grown record matches now");
        view.update(visual, |view, cx| view.set_text(String::new(), cx));
        visual.run_until_parked();
        tick(visual);
        assert_eq!(shown(&view, visual), [0, 1], "still two entries, not four");
        view.read_with(visual, |view, _| {
            assert_eq!(row_text(view.data().unwrap(), 1).lines, 3)
        });
    }

    /// #1165 acceptance on the sample pack's AddressSanitizer log: the
    /// frame `order_book.cpp:212:23` is a link in the selected entry, and
    /// opening it asks the chosen editor for that file, line and column.
    #[gpui::test]
    fn asan_frames_open_in_the_chosen_editor(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let sample = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../okilum-core/tests/fixtures/log/samples/cpp/ordgw_tests-asan.log");
        let text = std::fs::read_to_string(sample).unwrap();
        let state = tempfile::tempdir().unwrap();
        cx.update(|cx| reader_ui_state::install(state.path(), cx));
        let (_directory, reader, visual) = open_log(&text, cx);
        let view = log_view(&reader, visual);
        // The entry holding the frame, whether a record's continuation or a
        // raw block (#1161).
        let (id, location) = view.read_with(visual, |view, _| {
            (0..view.total())
                .find_map(|id| {
                    let raw = String::from_utf8_lossy(view.raw_at(id)?).into_owned();
                    let text = String::from_utf8_lossy(&ansi::strip(raw.as_bytes())).into_owned();
                    source_link::find(&text)
                        .into_iter()
                        .find(|l| l.path.ends_with("order_book.cpp") && l.line == 212)
                        .map(|location| (id, location))
                })
                .expect("the ASan frame is found")
        });
        assert_eq!(location.column, Some(23));
        assert_eq!(location.path, "/src/ordgw/src/md/order_book.cpp");
        view.update_in(visual, |view, window, cx| view.select(id, true, window, cx));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        // The detail pane draws the frames as links.
        assert!(
            visual.debug_bounds("log-detail-frames").is_some()
                || visual.debug_bounds("log-detail-raw").is_some(),
            "the frames are shown"
        );
        // Positive control: nothing was opened yet.
        assert_eq!(
            view.read_with(visual, |view, _| view.last_open.clone()),
            None
        );
        view.update_in(visual, |view, window, cx| {
            view.open_location(&location, window, cx)
        });
        assert_eq!(
            view.read_with(visual, |view, _| view.last_open.clone()),
            Some(Open::Url(
                "vscode://file/src/ordgw/src/md/order_book.cpp:212:23".into()
            ))
        );
        // A custom command gets the same place, word by word.
        visual.update(|_, cx| {
            reader_ui_state::set_log_editor(
                &source_link::Editor::Custom("myedit --goto {file}:{line}:{col}".into()),
                cx,
            )
        });
        view.update_in(visual, |view, window, cx| {
            view.open_location(&location, window, cx)
        });
        assert_eq!(
            view.read_with(visual, |view, _| view.last_open.clone()),
            Some(Open::Command(
                [
                    "myedit",
                    "--goto",
                    "/src/ordgw/src/md/order_book.cpp:212:23"
                ]
                .map(String::from)
                .to_vec()
            ))
        );
        // A build made elsewhere: the vault maps /src/ordgw to a local
        // checkout, and the frame opens there (#1165).
        let checkout = _directory.path().canonicalize().unwrap().join("checkout");
        let root = _directory.path().canonicalize().unwrap();
        visual.update(|_, cx| {
            reader_ui_state::set_log_editor(&source_link::Editor::VsCode, cx);
            reader_ui_state::set_source_maps(
                &root,
                vec![source_link::PathMap {
                    from: "/src/ordgw".into(),
                    to: checkout.clone(),
                }],
                cx,
            );
        });
        view.update_in(visual, |view, window, cx| {
            view.open_location(&location, window, cx)
        });
        let expected = checkout.join("src/md/order_book.cpp");
        assert_eq!(
            view.read_with(visual, |view, _| view.last_open.clone()),
            Some(Open::Url(format!(
                "vscode://file{}:212:23",
                expected.to_string_lossy()
            )))
        );
        // Kept for the next launch.
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(state.path().join("reader-ui.json")).unwrap())
                .unwrap();
        assert_eq!(
            saved["source_maps"][root.to_string_lossy().as_ref()][0]["from"],
            "/src/ordgw"
        );
    }

    /// #1166 acceptance: with ingress and app logs merged, choosing a
    /// record's request id shows only that request's rows from both files,
    /// and Back returns to the previous filter.
    #[gpui::test]
    fn show_this_request_filters_every_file_and_back_restores(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../okilum-core/tests/fixtures/log/correlation");
        let ingress = std::fs::read_to_string(fixtures.join("ingress-nginx.jsonl")).unwrap();
        let (directory, reader, visual) = open_log(&ingress, cx);
        let root = directory.path().canonicalize().unwrap();
        let app = root.join("checkout-api.jsonl");
        std::fs::copy(fixtures.join("checkout-api.jsonl"), &app).unwrap();
        let view = log_view(&reader, visual);
        view.update(visual, |view, cx| view.add_files(vec![app.clone()], cx));
        visual.run_until_parked();
        let messages = |visual: &mut VisualTestContext| {
            view.read_with(visual, |view, _| {
                (0..view.visible_len())
                    .map(|row| {
                        let id = view.entry_at(row).unwrap();
                        let raw = String::from_utf8_lossy(view.raw_at(id).unwrap()).into_owned();
                        let msg = raw.split("\"msg\": \"").nth(1).unwrap();
                        msg[..msg.find('"').unwrap()].to_owned()
                    })
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(
            messages(visual).len(),
            3 + 7,
            "positive control: both files"
        );
        // A query first, so Back has something to return to.
        view.update(visual, |view, cx| view.set_query("level>=info".into(), cx));
        visual.run_until_parked();
        let before = messages(visual);
        // Select the ingress record of the failed POST and show its request.
        let id = view.read_with(visual, |view, _| {
            (0..view.total())
                .find(|&id| {
                    String::from_utf8_lossy(view.raw_at(id).unwrap()).contains("POST /checkout 502")
                })
                .unwrap()
        });
        view.update_in(visual, |view, window, cx| view.select(id, true, window, cx));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let button = visual
            .debug_bounds("log-show-request")
            .expect("the record offers its request");
        visual.simulate_click(button.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            messages(visual),
            [
                "POST /checkout 502",
                "handling POST /checkout",
                "payment backend timeout"
            ],
            "only that request, from both files, in time order"
        );
        view.read_with(visual, |view, _| {
            assert_eq!(view.investigation.frames.len(), 1);
        });
        // Back returns to the previous filter.
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let back = visual
            .debug_bounds("log-request-back")
            .expect("Back is offered");
        visual.simulate_click(back.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(messages(visual), before);
        view.read_with(visual, |view, cx| {
            assert!(view.investigation.frames.is_empty());
            assert_eq!(view.query_input.read(cx).value().as_ref(), "level>=info");
        });
    }

    /// #1167 acceptance: the Apache access sample with columns status,
    /// duration_us, path and client is readable at 1440 px, without the
    /// detail pane, and the columns are kept for the file.
    #[gpui::test]
    fn apache_access_reads_in_columns_at_1440(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let sample = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../okilum-core/tests/fixtures/log/samples/apache/apache-access.logfmt");
        let text = std::fs::read_to_string(sample).unwrap();
        let state = tempfile::tempdir().unwrap();
        cx.update(|cx| reader_ui_state::install(state.path(), cx));
        let (_directory, reader, visual) = open_log(&text, cx);
        visual.simulate_resize(gpui::size(px(1440.), px(900.)));
        visual.run_until_parked();
        let view = log_view(&reader, visual);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        // Built-in headers remain available for point-and-click filtering even
        // before the user chooses any additional field columns (#2389).
        assert!(
            visual.debug_bounds("log-columns").is_some(),
            "built-in filter headers"
        );
        assert!(
            visual.debug_bounds("log-column-0").is_none(),
            "positive control: no field columns yet"
        );
        view.read_with(visual, |view, cx| assert!(view.columns(cx).is_empty()));
        // One column from the detail pane with a real click, the rest by key.
        view.update_in(visual, |view, window, cx| view.select(0, true, window, cx));
        visual.run_until_parked();
        let status_field = view.read_with(visual, |view, _| {
            let record = view.data().unwrap().record(0).unwrap();
            record
                .fields
                .iter()
                .position(|f| f.key == "status")
                .unwrap()
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let toggle = visual
            .debug_bounds(Box::leak(
                format!("log-toggle-column-{status_field}").into_boxed_str(),
            ))
            .expect("the field offers a column");
        visual.simulate_click(toggle.center(), Modifiers::default());
        visual.run_until_parked();
        view.update(visual, |view, cx| {
            for key in ["duration_us", "path", "client"] {
                view.toggle_column(key, cx);
            }
        });
        // Without the detail pane.
        view.update(visual, |view, cx| {
            view.detail_split.toggle_collapsed();
            cx.notify();
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let titles: Vec<_> = (0..4)
            .map(|at| {
                visual
                    .debug_bounds(Box::leak(format!("log-column-{at}").into_boxed_str()))
                    .unwrap()
            })
            .collect();
        let log = visual.debug_bounds("reader-log").unwrap();
        // In order, inside the view, with room left for the message.
        assert!(titles
            .windows(2)
            .all(|pair| pair[0].right() <= pair[1].left()));
        assert!(
            titles[3].right() + px(200.) < log.right(),
            "the message keeps room: {:?} in {log:?}",
            titles[3]
        );
        view.read_with(visual, |view, cx| {
            assert_eq!(
                view.columns(cx),
                ["status", "duration_us", "path", "client"]
            );
        });
        // Kept for the file, for the next launch.
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        let key = view.read_with(visual, |view, _| view.history_key().unwrap());
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(state.path().join("reader-ui.json")).unwrap())
                .unwrap();
        assert_eq!(saved["log_columns"][&key][2], "path");
        // Hiding from the header.
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let remove = visual.debug_bounds("log-column-0").unwrap();
        view.update(visual, |view, cx| view.toggle_column("status", cx));
        visual.run_until_parked();
        let _ = remove;
        view.read_with(visual, |view, cx| {
            assert_eq!(view.columns(cx), ["duration_us", "path", "client"]);
        });
    }

    /// #1167: a column is resized by dragging its header edge, rows follow,
    /// and the width is kept; the Columns menu offers the records' fields.
    #[gpui::test]
    fn columns_resize_by_drag_and_the_menu_lists_fields(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let sample = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../okilum-core/tests/fixtures/log/samples/apache/apache-access.logfmt");
        let text = std::fs::read_to_string(sample).unwrap();
        let state = tempfile::tempdir().unwrap();
        cx.update(|cx| reader_ui_state::install(state.path(), cx));
        let (_directory, reader, visual) = open_log(&text, cx);
        visual.simulate_resize(gpui::size(px(1440.), px(900.)));
        visual.run_until_parked();
        let view = log_view(&reader, visual);
        let names = view.read_with(visual, |view, _| view.field_names());
        for key in ["status", "duration_us", "path", "client", "req_id"] {
            assert!(names.iter().any(|name| name == key), "{key} in {names:?}");
        }
        view.update(visual, |view, cx| {
            for key in ["status", "path", "client"] {
                view.toggle_column(key, cx);
            }
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let before = visual.debug_bounds("log-column-1").unwrap();
        assert!((before.size.width - px(WIDE_COLUMN_WIDTH)).abs() < px(1.));
        let handle = visual.debug_bounds("log-column-resize-1").unwrap();
        let from = handle.center();
        let to = from + point(px(100.), px(0.));
        visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(
            from + point(px(10.), px(0.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.simulate_mouse_move(to, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let after = visual.debug_bounds("log-column-1").unwrap();
        assert!(
            (after.size.width - before.size.width - px(100.)).abs() < px(6.),
            "the column follows the drag: {before:?} -> {after:?}"
        );
        // The next column moved with it: the grid is shared with the rows.
        let client = visual.debug_bounds("log-column-2").unwrap();
        assert!(client.left() >= after.right());
        // Kept for the file.
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        let key = view.read_with(visual, |view, _| view.history_key().unwrap());
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(state.path().join("reader-ui.json")).unwrap())
                .unwrap();
        let width = saved["log_column_widths"][&key]["path"].as_f64().unwrap() as f32;
        assert!((width - f32::from(after.size.width)).abs() < 1., "{width}");
    }

    /// #1168 acceptance on the Kibana sample: the Elasticsearch outage
    /// (05:57:35-38, seven errors) is a red cluster on the bars apart from the
    /// startup FATAL at 05:57:00; dragging over it filters the rows to that
    /// window, Clear brings every row back, and a click on a bar jumps to it.
    #[gpui::test]
    fn kibana_outage_is_a_red_cluster_and_dragging_selects_it(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let sample = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../okilum-core/tests/fixtures/log/samples/kibana/kibana.ndjson");
        let text = std::fs::read_to_string(sample).unwrap();
        let (_directory, reader, visual) = open_log(&text, cx);
        visual.simulate_resize(gpui::size(px(1440.), px(900.)));
        visual.run_until_parked();
        let view = log_view(&reader, visual);
        let settle = |visual: &mut VisualTestContext| {
            for _ in 0..2 {
                visual.update(|window, cx| window.draw(cx).clear(cx));
                visual.run_until_parked();
            }
        };
        settle(visual);
        let histogram = view
            .read_with(visual, |view, _| view.histogram.clone())
            .expect("the bars are counted");
        let total = view.read_with(visual, |view, _| view.total());
        let red = |counts: &[u32; Level::COUNT]| {
            counts[Level::Error as usize] + counts[Level::Fatal as usize]
        };
        let errors: u32 = histogram.buckets().map(|bucket| red(bucket.counts)).sum();
        assert_eq!(errors, 8, "the sample's level>=error records");
        let red_buckets: Vec<usize> = (0..histogram.len())
            .filter(|&at| red(histogram.bucket(at).unwrap().counts) > 0)
            .collect();
        // The startup FATAL stands alone in the first bucket; the outage is
        // one cluster of seven errors after it.
        assert_eq!(red_buckets.first(), Some(&0), "{red_buckets:?}");
        let cluster = &red_buckets[1..];
        let (first, last) = (cluster[0], *cluster.last().unwrap());
        assert!(
            last - first < histogram.len() / 4,
            "a cluster, not spread out: buckets {red_buckets:?}"
        );
        let in_cluster: u32 = (first..=last)
            .map(|at| red(histogram.bucket(at).unwrap().counts))
            .sum();
        assert_eq!(in_cluster, 7, "the outage's errors");
        // Drag over the cluster on the bars, from the middle of its first
        // bar to the middle of its last.
        let bars = visual
            .debug_bounds("log-histogram")
            .expect("the bars are drawn");
        let n = histogram.len() as f32;
        let x = |bucket: f32| bars.left() + bars.size.width * (bucket / n);
        let y = bars.center().y;
        let from = point(x(first as f32 + 0.5), y);
        let to = point(x(last as f32 + 0.5), y);
        visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(
            from + point(px(5.), px(0.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.simulate_mouse_move(to, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        let (time, shown) = view.read_with(visual, |view, _| {
            let shown: Vec<(Option<i64>, Level)> = (0..view.visible_len())
                .map(|row| {
                    let id = view.entry_at(row).unwrap();
                    let (source, entry) = view.locate(id).unwrap();
                    let entry = *view.source(source).unwrap().index().get(entry).unwrap();
                    (entry.timestamp(), entry.level())
                })
                .collect();
            (view.filter.time, shown)
        });
        let (start, end) = histogram.window(first, last).unwrap();
        assert_eq!(
            time,
            TimeWindow::Range {
                from: start,
                to: end
            }
        );
        assert!(shown.len() < total, "the window hides other rows");
        assert!(shown
            .iter()
            .all(|(ts, _)| ts.is_none_or(|ts| (start..=end).contains(&ts))));
        let shown_red = shown
            .iter()
            .filter(|(_, level)| matches!(level, Level::Error | Level::Fatal))
            .count();
        assert_eq!(
            shown_red, 7,
            "the outage is inside the window, the startup FATAL is not"
        );
        settle(visual);
        assert!(visual.debug_bounds("log-histogram-range").is_some());
        let clear = visual.debug_bounds("log-histogram-clear").unwrap();
        visual.simulate_click(clear.center(), Modifiers::default());
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert_eq!(view.filter.time, TimeWindow::All);
            assert_eq!(view.visible_len(), total);
        });
        settle(visual);
        let again = view
            .read_with(visual, |view, _| view.histogram.clone())
            .expect("the bars are counted again");
        assert_eq!(*again, *histogram, "Clear brings the same bars back");
        // A click on a bar selects the first row at or after its start.
        let bucket = histogram.bucket(first).unwrap();
        visual.simulate_click(point(x(first as f32 + 0.5), y), Modifiers::default());
        visual.run_until_parked();
        let selected = view.read_with(visual, |view, _| {
            let (source, entry) = view.locate(view.selected?)?;
            view.source(source)?.index().get(entry)?.timestamp()
        });
        assert!(
            selected.is_some_and(|ts| (bucket.from..bucket.to).contains(&ts)),
            "{selected:?} not in {}..{}",
            bucket.from,
            bucket.to
        );
    }

    /// #1168: in a view of several files the bars count every file, and with a
    /// query only the rows it shows.
    #[gpui::test]
    fn the_bars_count_every_file_of_a_merged_view(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let samples =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../okilum-core/tests/fixtures/log/samples");
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let paths: Vec<PathBuf> = ["kubernetes/kubelet.jsonl", "apache/apache-error.jsonl"]
            .iter()
            .map(|name| {
                let target = root.join(Path::new(name).file_name().unwrap());
                std::fs::copy(samples.join(name), &target).unwrap();
                target
            })
            .collect();
        cx.update(|cx| {
            crate::test_init(cx);
            crate::bind_keys(cx);
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(paths[0].clone()),
                        session_directory: Some(root.join(".state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        let view = log_view(&reader, visual);
        let settle = |visual: &mut VisualTestContext| {
            for _ in 0..2 {
                visual.update(|window, cx| window.draw(cx).clear(cx));
                visual.run_until_parked();
            }
        };
        let counted = |visual: &mut VisualTestContext| {
            view.read_with(visual, |view, _| {
                view.histogram
                    .as_ref()
                    .map(|histogram| (histogram.timed() + histogram.untimed()) as usize)
            })
        };
        settle(visual);
        let alone = view.read_with(visual, |view, _| view.total());
        assert_eq!(counted(visual), Some(alone), "one file");
        view.update(visual, |view, cx| view.add_files(paths[1..].to_vec(), cx));
        visual.run_until_parked();
        settle(visual);
        let total = view.read_with(visual, |view, _| view.total());
        assert!(total > alone, "positive control: the files joined");
        assert_eq!(counted(visual), Some(total), "every record of both files");
        view.update(visual, |view, cx| view.set_query("level>=error".into(), cx));
        visual.run_until_parked();
        settle(visual);
        let shown = view.read_with(visual, |view, _| view.visible_len());
        assert!(shown > 0 && shown < total, "{shown} of {total}");
        assert_eq!(
            counted(visual),
            Some(shown),
            "only the rows the query shows"
        );
    }

    /// #1385: elapsed time between two selected rows of a merged view, from
    /// their parsed timestamps at the files' precision; from one row to now;
    /// a row without a timestamp says so.
    #[gpui::test]
    fn elapsed_time_between_rows_of_two_files_and_since_a_row(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        // api.jsonl writes milliseconds, worker.jsonl microseconds.
        let api = concat!(
            "{\"ts\":\"2025-10-01T00:00:00.120Z\",\"msg\":\"api start\"}\n",
            "{\"msg\":\"api no time\"}\n",
            "{\"ts\":\"2025-10-01T00:03:05.687Z\",\"msg\":\"api end\"}\n",
        );
        let worker = "{\"ts\":\"2025-10-01T00:00:01.354321Z\",\"msg\":\"worker step\"}\n";
        let (directory, reader, visual) = open_named("api.jsonl", api.as_bytes(), cx);
        let other = directory
            .path()
            .canonicalize()
            .unwrap()
            .join("worker.jsonl");
        std::fs::write(&other, worker).unwrap();
        let view = log_view(&reader, visual);
        view.update(visual, |view, cx| view.add_files(vec![other], cx));
        visual.run_until_parked();
        let id = |visual: &mut VisualTestContext, marker: &str| {
            view.read_with(visual, |view, _| {
                (0..view.total())
                    .find(|&id| {
                        view.raw_at(id)
                            .is_some_and(|raw| String::from_utf8_lossy(raw).contains(marker))
                    })
                    .unwrap_or_else(|| panic!("no row with {marker}"))
            })
        };
        let (start, step, end, untimed) = (
            id(visual, "api start"),
            id(visual, "worker step"),
            id(visual, "api end"),
            id(visual, "api no time"),
        );
        assert_eq!(
            view.read_with(visual, |view, _| view.total()),
            4,
            "both files"
        );
        let pick = |visual: &mut VisualTestContext, from: usize, to: Option<usize>| {
            view.update_in(visual, |view, window, cx| {
                view.select(from, false, window, cx);
                if let Some(to) = to {
                    view.extend_to(to, false, window, cx);
                }
            });
            view.read_with(visual, |view, _| {
                view.elapsed_label(1_759_276_812_345_678_901).unwrap().0
            })
        };
        // 00:00:01.354321 − 00:00:00.120 by hand: 1.234321 s, shown at the
        // coarser precision (ms): 1.234 s. Across the two files.
        assert_eq!(pick(visual, start, Some(step)), "\u{394} +1.234 s");
        // Signed by order in the view, whichever end was clicked first.
        assert_eq!(pick(visual, step, Some(start)), "\u{394} +1.234 s");
        // 00:03:05.687 − 00:00:00.120 = 3 min 05.567 s, both in ms.
        assert_eq!(pick(visual, start, Some(end)), "\u{394} +3 min 05.567 s");
        // A row without a timestamp: said, not 0.
        let label = pick(visual, start, Some(untimed));
        assert!(label.ends_with(": no timestamp"), "{label}");
        assert_eq!(pick(visual, untimed, None), "No timestamp");
        // One row: to now (00:00:12.345678901), at the row's ms precision.
        assert_eq!(pick(visual, start, None), "12.225 s ago");
        assert_eq!(pick(visual, step, None), "10.991 s ago");
        // The header shows it.
        view.update_in(visual, |view, window, cx| {
            view.select(start, false, window, cx)
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            visual.debug_bounds("log-elapsed").is_some(),
            "the elapsed time is drawn in the detail header"
        );
    }

    /// #1168: while a log is followed the bars count what arrives, and a
    /// chosen time range stays put as newer records come in.
    #[gpui::test]
    fn the_bars_follow_appends_and_a_range_stays_put(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        use std::io::Write as _;
        let line = |second: u32, level: &str| {
            format!(
                "{{\"ts\":\"2025-10-01T00:{:02}:{:02}Z\",\"level\":\"{level}\"}}\n",
                second / 60,
                second % 60
            )
        };
        let at = |second: i64| (1_759_276_800 + second) * 1_000_000_000;
        let text: String = (0..30).map(|second| line(second, "info")).collect();
        let (directory, reader, visual) = open_log(&text, cx);
        let path = directory.path().canonicalize().unwrap().join("app.jsonl");
        let append = |text: &str| {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            file.write_all(text.as_bytes()).unwrap();
        };
        // A count started by a draw waits for the next second of a followed
        // log, so every step lets more than a second pass.
        let tick = |visual: &mut VisualTestContext| {
            visual.executor().advance_clock(Duration::from_millis(1200));
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.run_until_parked();
        };
        let view = log_view(&reader, visual);
        let len = |visual: &mut VisualTestContext| view.read_with(visual, |v, _| v.visible_len());
        let counted = |visual: &mut VisualTestContext| {
            view.read_with(visual, |view, _| {
                view.histogram
                    .as_ref()
                    .map(|histogram| (histogram.timed() + histogram.untimed()) as usize)
            })
        };
        tick(visual);
        assert_eq!(counted(visual), Some(30));
        view.update(visual, |view, cx| view.toggle_follow(cx));
        visual.run_until_parked();
        tick(visual);
        append(&(line(30, "error") + &line(31, "info")));
        tick(visual);
        tick(visual);
        assert_eq!(len(visual), 32, "follow shows the appended records");
        assert_eq!(counted(visual), Some(32), "and the bars count them");

        // A range is not a window that ends at the newest record.
        view.update(visual, |view, cx| {
            view.update_filter(|f| f.time = TimeWindow::range(at(10), at(20)), cx)
        });
        visual.run_until_parked();
        tick(visual);
        assert_eq!(len(visual), 11, "seconds 10 to 20, both ends included");
        append(&line(50, "info"));
        tick(visual);
        tick(visual);
        assert_eq!(len(visual), 11, "a newer record is outside a fixed range");
        append(&line(15, "error"));
        tick(visual);
        tick(visual);
        assert_eq!(len(visual), 12, "one inside the range is shown");
        assert_eq!(counted(visual), Some(12), "the bars count what is shown");
        view.read_with(visual, |view, _| {
            assert!(
                view.follow.as_ref().unwrap().last_full_run.is_none(),
                "a range never asks for a full run when records arrive"
            );
        });
    }

    #[gpui::test]
    fn regex_entry_shows_errors_saves_and_reports_zero_matches(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let state = tempfile::tempdir().unwrap();
        cx.update(|cx| reader_ui_state::install(state.path(), cx));
        let (directory, reader, visual) = open_log("first line\n  second line\n", cx);
        let view = log_view(&reader, visual);
        view.update_in(visual, |view, window, cx| {
            view.begin_regex(window, cx);
            let editor = view.regex_editor.as_ref().unwrap();
            editor
                .name
                .update(cx, |input, cx| input.set_value("Pipe", window, cx));
            editor
                .pattern
                .update(cx, |input, cx| input.focus(window, cx));
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.simulate_input("(?P<ts>");
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        view.read_with(visual, |view, _| {
            assert!(view
                .regex_editor
                .as_ref()
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("unclosed group"));
        });
        assert!(
            visual.debug_bounds("log-regex-error").is_some(),
            "compile error is drawn while typing"
        );
        view.update_in(visual, |view, window, cx| view.save_regex(window, cx));
        assert!(
            visual.update(|_, cx| reader_ui_state::log_regex_definitions(cx).is_empty()),
            "invalid regex is rejected"
        );
        view.update_in(visual, |view, window, cx| {
            let editor = view.regex_editor.as_ref().unwrap();
            editor.pattern.update(cx, |input, cx| {
                input.set_value("^(?P<msg>never)$", window, cx)
            });
            view.save_regex(window, cx);
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        view.read_with(visual, |view, _| {
            assert!(view.regex_editor.is_none(), "valid regex was saved");
            assert_eq!(view.data().unwrap().index().regex_counts(), Some((0, 2)));
            assert_eq!(view.visible_len(), 2, "unmatched lines stay visible");
            assert_eq!(
                view.status(),
                "Regex “Pipe” matched 0 of 2 lines; 2 unmatched lines shown as text"
            );
        });
        assert!(
            visual.debug_bounds("log-status").is_some(),
            "counts state is drawn"
        );
        visual.simulate_keystrokes("down");
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert_eq!(
                view.selected,
                Some(0),
                "saving returns keyboard control to the log rows"
            );
        });
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(state.path().join("reader-ui.json")).unwrap())
                .unwrap();
        assert_eq!(
            saved["log_regex"]["Pipe"]["pattern"], "^(?P<msg>never)$",
            "durable settings, outside the index"
        );
        assert_eq!(
            visual.update(|_, cx| automatic_regex(cx).unwrap().definition().name.clone()),
            "Pipe"
        );
        // A second saved definition makes automatic selection ambiguous.
        visual.update(|_, cx| {
            assert!(reader_ui_state::save_log_regex(
                okilum_core::log::regex_text::Definition {
                    name: "Another".into(),
                    pattern: "(?P<msg>.*)".into()
                },
                cx
            ));
            assert!(automatic_regex(cx).is_none());
        });
        // Explicitly replacing the parser gives the partial count state and fields.
        let regex = okilum_core::log::regex_text::Definition {
            name: "Partial".into(),
            pattern: "^(?P<worker>first) (?P<msg>.*)$".into(),
        }
        .compile()
        .unwrap();
        view.update(visual, |view, cx| view.apply_regex(Some(regex), cx));
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert_eq!(
                view.status(),
                "Regex “Partial” matched 1 of 2 lines; 1 unmatched line shown as text"
            );
            assert!(
                view.field_names().contains(&"worker".to_owned()),
                "extra named groups are available as columns"
            );
        });
        // A second file must not replace the explicit zero-match state with
        // only the merged view's record count.
        let regex = visual.update(|_, cx| {
            reader_ui_state::log_regex_definitions(cx)
                .into_iter()
                .find(|definition| definition.name == "Pipe")
                .unwrap()
                .compile()
                .unwrap()
        });
        view.update(visual, |view, cx| view.apply_regex(Some(regex), cx));
        visual.run_until_parked();
        let extra = directory.path().join("extra.jsonl");
        std::fs::write(&extra, b"{\"msg\":\"extra\"}\n").unwrap();
        view.update(visual, |view, cx| view.add_files(vec![extra], cx));
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert_eq!(
                view.source_count(),
                2,
                "positive control: the second file opened"
            );
            assert!(view
                .status()
                .contains("Regex “Pipe” matched 0 of 2 lines; 2 unmatched lines shown as text"));
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            visual.debug_bounds("log-status").is_some(),
            "merged counts are drawn"
        );
    }

    /// #1169 acceptance: a filter saved on one log (name, query, levels) is
    /// listed for every log and applies to another file; a name is required,
    /// Enter saves and Escape cancels, and a saved query this version cannot
    /// read is reported without changing the rows.
    #[gpui::test]
    fn a_saved_filter_applies_to_another_log(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let first = concat!(
            r#"{"level":"info","msg":"boot","status":200}"#,
            "\n",
            r#"{"level":"warn","msg":"slow","status":200}"#,
            "\n",
            r#"{"level":"error","msg":"upstream","status":503}"#,
            "\n",
            r#"{"level":"error","msg":"bad gateway","status":502}"#,
            "\n",
        );
        let second = concat!(
            r#"{"level":"info","msg":"ok","status":500}"#,
            "\n",
            r#"{"level":"warn","msg":"slow","status":504}"#,
            "\n",
            r#"{"level":"error","msg":"missing","status":404}"#,
            "\n",
            r#"{"level":"error","msg":"down","status":500}"#,
            "\n",
            r#"{"level":"fatal","msg":"no status"}"#,
            "\n",
        );
        let state = tempfile::tempdir().unwrap();
        cx.update(|cx| reader_ui_state::install(state.path(), cx));
        let query = "level>=warn and status>=500";
        {
            let (_directory, reader, visual) = open_log(first, cx);
            let view = log_view(&reader, visual);
            // Saved views is a header icon at the default desktop width;
            // narrow headers expose the same live menu through overflow.
            visual.simulate_resize(gpui::size(px(1440.), px(900.)));
            visual.update(|window, cx| window.draw(cx).clear(cx));
            assert!(
                reader_viewer_chrome::tests::measured_bounds(visual, "log-saved").is_some(),
                "the Saved button"
            );
            view.update_in(visual, |view, window, cx| {
                view.query_input
                    .update(cx, |input, cx| input.set_value(query, window, cx));
                view.set_query(query.into(), cx);
            });
            visual.run_until_parked();
            assert_eq!(shown(&view, visual), [2, 3], "positive control");
            // The level menu's choice is part of what is saved, too.
            view.update(visual, |view, cx| {
                view.update_filter(|filter| filter.level.min = Some(Level::Warn), cx)
            });
            visual.run_until_parked();
            assert_eq!(shown(&view, visual), [2, 3]);

            // A name is required; the row stays open and says why.
            view.update_in(visual, |view, window, cx| view.begin_saving(window, cx));
            visual.update(|window, cx| window.draw(cx).clear(cx));
            assert!(visual.debug_bounds("log-saving-name").is_some());
            view.update_in(visual, |view, window, cx| view.commit_saving(window, cx));
            visual.update(|window, cx| window.draw(cx).clear(cx));
            assert!(visual.debug_bounds("log-saving-error").is_some());
            assert!(visual.update(|_, cx| reader_ui_state::saved_log_filters(cx).is_empty()));

            // Escape cancels.
            visual.simulate_keystrokes("escape");
            visual.run_until_parked();
            view.read_with(visual, |view, _| {
                assert!(view.saving.is_none(), "cancelled")
            });

            // Enter saves under the typed name.
            view.update_in(visual, |view, window, cx| {
                view.begin_saving(window, cx);
                let input = view.saving.as_ref().unwrap().input.clone();
                input.update(cx, |input, cx| input.set_value("Server errors", window, cx));
            });
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.simulate_keystrokes("enter");
            visual.run_until_parked();
            view.read_with(visual, |view, _| assert!(view.saving.is_none(), "saved"));
            let saved = visual.update(|_, cx| reader_ui_state::saved_log_filters(cx));
            let saved = saved
                .get("server errors")
                .expect("saved under the typed name");
            assert_eq!(saved.min_level, "warn");
            assert_eq!(saved.name, "Server errors");
            assert!(saved.file.is_some(), "new saved views retain their source");
            // Older portable filters remain applicable to any currently open file.
            let mut legacy = saved.clone();
            legacy.file = None;
            visual.update(|_, cx| {
                assert!(reader_ui_state::save_log_filter(legacy, cx));
            });
        }
        // Another log: the filter is listed there and applies to its rows.
        let (_directory, reader, visual) = open_named("other.jsonl", second.as_bytes(), cx);
        let view = log_view(&reader, visual);
        assert_eq!(shown(&view, visual), [0, 1, 2, 3, 4], "positive control");
        view.update_in(visual, |view, window, cx| {
            view.apply_saved("Server errors", window, cx)
        });
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [1, 3]);
        assert_eq!(counter(&view, visual).as_deref(), Some("2 of 5"));
        // The query shows in its canonical spelling, in its field too.
        let canonical = Query::parse(query).unwrap().to_string();
        view.read_with(visual, |view, cx| {
            assert_eq!(view.query_text, canonical);
            assert_eq!(view.query_input.read(cx).value().as_ref(), canonical);
            assert_eq!(view.filter.level.min, Some(Level::Warn));
        });

        // A saved query this version cannot read: reported, rows unchanged.
        visual.update(|_, cx| {
            let broken = SavedQuery {
                name: "Broken".into(),
                query: "status >=".into(),
                ..Default::default()
            };
            assert!(reader_ui_state::save_log_filter(broken, cx));
        });
        view.update_in(visual, |view, window, cx| {
            view.apply_saved("Broken", window, cx)
        });
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [1, 3], "unchanged");

        // Deleted filters are gone for every log.
        visual.update(|_, cx| reader_ui_state::delete_log_filter("server  ERRORS", cx));
        let left: Vec<String> = visual.update(|_, cx| {
            reader_ui_state::saved_log_filters(cx)
                .sorted()
                .iter()
                .map(|query| query.name.clone())
                .collect()
        });
        assert_eq!(left, ["Broken"]);
    }

    /// #1169 acceptance: the 21 shown rows of the Apache sample export as
    /// exactly those raw lines; JSON lines carry the decoded records in file
    /// order; nothing is written over the log itself, and with no rows shown
    /// there is nothing to ask for.
    #[gpui::test]
    fn export_writes_exactly_the_shown_rows_and_never_over_the_log(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let sample = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../okilum-core/tests/fixtures/log/samples/apache/apache-access.logfmt");
        let text = std::fs::read_to_string(sample).unwrap();
        let (directory, reader, visual) = open_named("apache-access.logfmt", text.as_bytes(), cx);
        let root = directory.path().canonicalize().unwrap();
        let view = log_view(&reader, visual);
        view.update(visual, |view, cx| view.set_query("status>=500".into(), cx));
        visual.run_until_parked();
        assert_eq!(view.read_with(visual, |view, _| view.visible_len()), 21);
        let status = |line: &str| {
            line.split(' ')
                .find_map(|token| token.strip_prefix("status=")?.parse::<u32>().ok())
        };
        let expected: String = text
            .lines()
            .filter(|line| status(line).is_some_and(|status| status >= 500))
            .map(|line| format!("{line}\n"))
            .collect();
        assert_eq!(expected.lines().count(), 21, "positive control");
        let export = |visual: &mut VisualTestContext, format: ExportFormat, out: PathBuf| {
            view.update_in(visual, |view, window, cx| {
                view.export_rows(ExportScope::Shown, format, window, cx)
            });
            assert!(visual.did_prompt_for_new_path(), "asks where to save");
            visual.simulate_new_path_selection(move |_| Some(out));
            visual.run_until_parked();
        };
        let raw = root.join("errors.log");
        export(visual, ExportFormat::Raw, raw.clone());
        assert_eq!(std::fs::read_to_string(&raw).unwrap(), expected);

        let json = root.join("errors.jsonl");
        export(visual, ExportFormat::JsonLines, json.clone());
        let rows: Vec<serde_json::Value> = std::fs::read_to_string(&json)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 21);
        let status_of = |row: &serde_json::Value| {
            let field = &row["fields"]["status"];
            field.as_i64().or_else(|| field.as_str()?.parse().ok())
        };
        assert!(
            rows.iter()
                .all(|row| status_of(row).is_some_and(|s| s >= 500)),
            "{}",
            rows[0]
        );
        assert!(
            rows.windows(2)
                .all(|pair| pair[0]["line"].as_u64() < pair[1]["line"].as_u64()),
            "in file order"
        );
        assert!(rows[0].get("source").is_none(), "one file has no source");
        assert!(std::fs::read_dir(&root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tmp")
        }));

        // Never over the log being read.
        let log = root.join("apache-access.logfmt");
        let before = std::fs::read(&log).unwrap();
        export(visual, ExportFormat::Raw, log.clone());
        assert_eq!(std::fs::read(&log).unwrap(), before, "the log is untouched");

        // No rows shown: nothing to ask for.
        view.update(visual, |view, cx| view.set_query("status>=999".into(), cx));
        visual.run_until_parked();
        view.update_in(visual, |view, window, cx| {
            view.export_rows(ExportScope::Shown, ExportFormat::Raw, window, cx)
        });
        assert!(!visual.did_prompt_for_new_path());
    }

    /// #1386: three marked records stay marked through a query, a text
    /// filter, follow and a truncation; next and previous visit exactly them,
    /// wrapping; the marked export writes exactly them; a reopened log has no
    /// marks (they live only while the view is open, Oleg 18.10).
    #[gpui::test]
    fn marks_keep_their_records_and_export_on_their_own(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        use std::io::Write as _;
        let line = |i: usize| {
            let level = if i % 5 == 2 || i == 4 { "warn" } else { "info" };
            format!("{{\"level\":\"{level}\",\"msg\":\"r{i}\"}}\n")
        };
        let text: String = (0..20).map(line).collect();
        let (directory, reader, visual) = open_log(&text, cx);
        let root = directory.path().canonicalize().unwrap();
        let path = root.join("app.jsonl");
        let view = log_view(&reader, visual);
        let tick = |visual: &mut VisualTestContext| {
            visual.executor().advance_clock(Duration::from_millis(600));
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        };
        // Two by the row menu's action, one by the shortcut.
        for id in [2, 7] {
            view.update(visual, |view, cx| view.toggle_mark_at(id, cx));
        }
        view.update_in(visual, |view, window, cx| view.select(12, true, window, cx));
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-f2"
        } else {
            "ctrl-f2"
        });
        visual.run_until_parked();
        let marked_lines = |visual: &mut VisualTestContext| -> Vec<String> {
            view.read_with(visual, |view, _| {
                view.marked_rows()
                    .into_iter()
                    .map(|row| {
                        String::from_utf8_lossy(view.raw_at(view.entry_at(row).unwrap()).unwrap())
                            .trim_end()
                            .to_owned()
                    })
                    .collect()
            })
        };
        let wanted: Vec<String> = [2, 7, 12].map(|i| line(i).trim_end().to_owned()).into();
        assert_eq!(marked_lines(visual), wanted);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            visual.debug_bounds("log-mark-2").is_some(),
            "a dot in the gutter"
        );
        assert!(
            visual.debug_bounds("log-mark-3").is_none(),
            "positive control"
        );

        // A query that keeps them: still the same three, and next/previous
        // visit exactly them, wrapping at both ends.
        view.update(visual, |view, cx| view.set_query("level>=warn".into(), cx));
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [2, 4, 7, 12, 17]);
        assert_eq!(marked_lines(visual), wanted);
        let mut visits = Vec::new();
        for _ in 0..4 {
            view.update_in(visual, |view, window, cx| view.step_mark(true, window, cx));
            visits.push(view.read_with(visual, |view, _| view.selected.unwrap()));
        }
        assert_eq!(visits, [2, 7, 12, 2], "next wraps to the first");
        view.update_in(visual, |view, window, cx| view.step_mark(false, window, cx));
        assert_eq!(
            view.read_with(visual, |view, _| view.selected),
            Some(12),
            "previous wraps"
        );
        // A text filter that hides two of them: only the shown one is visited.
        view.update(visual, |view, cx| view.set_text("r7".into(), cx));
        visual.run_until_parked();
        view.update_in(visual, |view, window, cx| view.step_mark(true, window, cx));
        assert_eq!(view.read_with(visual, |view, _| view.selected), Some(7));
        view.update(visual, |view, cx| {
            view.set_text(String::new(), cx);
            view.set_query(String::new(), cx);
        });
        visual.run_until_parked();
        assert_eq!(marked_lines(visual), wanted, "back when the filter goes");

        // Export «marked»: exactly those records, in order.
        let out = root.join("marked.log");
        view.update_in(visual, |view, window, cx| {
            view.export_rows(ExportScope::Marked, ExportFormat::Raw, window, cx)
        });
        assert!(visual.did_prompt_for_new_path());
        visual.simulate_new_path_selection({
            let out = out.clone();
            move |_| Some(out)
        });
        visual.run_until_parked();
        let expected: String = [2, 7, 12].map(line).concat();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), expected);

        // Follow: new rows come in, the marks stay, next does not land on
        // them, and a jump to a mark pauses follow.
        view.update(visual, |view, cx| view.toggle_follow(cx));
        visual.run_until_parked();
        tick(visual);
        {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            file.write_all((20..25).map(line).collect::<String>().as_bytes())
                .unwrap();
        }
        tick(visual);
        assert_eq!(view.read_with(visual, |view, _| view.visible_len()), 25);
        assert_eq!(marked_lines(visual), wanted);
        view.update_in(visual, |view, window, cx| view.select(24, true, window, cx));
        view.update_in(visual, |view, window, cx| view.step_mark(true, window, cx));
        view.read_with(visual, |view, _| {
            assert_eq!(view.selected, Some(2), "wraps past the new rows");
            assert!(
                !view.follow.as_ref().unwrap().pinned,
                "the jump pauses follow"
            );
        });

        // Truncated while followed: the earlier content keeps its marks under
        // its own name; the new content is not marked.
        std::fs::write(&path, line(100)).unwrap();
        tick(visual);
        tick(visual);
        view.read_with(visual, |view, _| {
            assert!(!view.extra.is_empty(), "the earlier content is kept");
            assert!(view.marks.iter().all(|&(source, _)| source != 0));
        });
        assert_eq!(marked_lines(visual), wanted);

        // Reopened: no marks. Nothing was stored.
        let mut again = None;
        let (_, second) = visual.cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(path.clone()),
                        session_directory: Some(root.join(".state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            again = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        second.run_until_parked();
        let reopened = log_view(&again.unwrap(), second);
        reopened.read_with(second, |view, _| {
            assert!(view.total() > 0, "positive control: it opened");
            assert!(view.marks.is_empty());
        });
    }

    /// #1386: in a merged view a mark stays on its record when the files are
    /// ordered again.
    #[gpui::test]
    fn marks_in_a_merged_view_stay_on_their_records(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let samples =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../okilum-core/tests/fixtures/log/samples");
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let names = ["kubernetes/kubelet.jsonl", "apache/apache-error.jsonl"];
        let paths: Vec<PathBuf> = names
            .iter()
            .map(|name| {
                let target = root.join(Path::new(name).file_name().unwrap());
                std::fs::copy(samples.join(name), &target).unwrap();
                target
            })
            .collect();
        cx.update(|cx| {
            crate::test_init(cx);
            crate::bind_keys(cx);
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(paths[0].clone()),
                        session_directory: Some(root.join(".state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        let view = log_view(&reader, visual);
        view.update(visual, |view, cx| view.add_files(paths[1..].to_vec(), cx));
        visual.run_until_parked();
        // One record of each file.
        let picks: Vec<(usize, usize)> = view.read_with(visual, |view, _| {
            let mut picks = Vec::new();
            for row in 0..view.visible_len() {
                let key = view.locate(view.entry_at(row).unwrap()).unwrap();
                if !picks.iter().any(|&(source, _)| source == key.0) {
                    picks.push(key);
                }
            }
            picks
        });
        assert_eq!(picks.len(), 2, "positive control: two files");
        for &(source, entry) in &picks {
            view.update(visual, |view, cx| {
                let id = view.id_of(source, entry).unwrap();
                view.toggle_mark_at(id, cx);
            });
        }
        let marked = |visual: &mut VisualTestContext| -> Vec<(usize, usize)> {
            view.read_with(visual, |view, _| {
                let mut keys: Vec<_> = view
                    .marked_rows()
                    .into_iter()
                    .map(|row| view.locate(view.entry_at(row).unwrap()).unwrap())
                    .collect();
                keys.sort();
                keys
            })
        };
        let mut wanted = picks.clone();
        wanted.sort();
        assert_eq!(marked(visual), wanted);
        // Hide the added file and show it again: rows are numbered anew.
        view.update(visual, |view, cx| view.set_source_enabled(1, false, cx));
        visual.run_until_parked();
        assert_eq!(
            view.read_with(visual, |view, _| view.marked_rows().len()),
            1
        );
        view.update(visual, |view, cx| view.set_source_enabled(1, true, cx));
        visual.run_until_parked();
        assert_eq!(marked(visual), wanted, "the same two records");
    }

    /// #1387 acceptance: a filtered view with a multi-line stack trace exports
    /// as CSV with the columns column mode shows, in their order, one row per
    /// record; a CSV parser gets the values back.
    #[gpui::test]
    fn csv_export_writes_the_columns_of_column_mode_one_row_per_record(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let text = "\
{\"ts\":\"2025-10-01T00:00:05Z\",\"level\":\"error\",\"msg\":\"cache miss, \\\"cold\\\"\",\"status\":500,\"path\":\"/v1/notes\"}
  at frame one
  at frame two
{\"ts\":\"2025-10-01T00:00:06Z\",\"level\":\"info\",\"msg\":\"ok\",\"status\":200,\"path\":\"/health\"}
{\"ts\":\"2025-10-01T00:00:07Z\",\"level\":\"error\",\"msg\":\"boom\",\"status\":503,\"path\":\"/v1/search\"}
";
        // Column mode keeps its columns in the UI state.
        let state = tempfile::tempdir().unwrap();
        cx.update(|cx| reader_ui_state::install(state.path(), cx));
        let (directory, reader, visual) = open_named("api.jsonl", text.as_bytes(), cx);
        let root = directory.path().canonicalize().unwrap();
        let view = log_view(&reader, visual);
        visual.run_until_parked();
        view.update(visual, |view, cx| {
            // Column mode: path first, then status.
            view.toggle_column("path", cx);
            view.toggle_column("status", cx);
            view.set_query("status>=500".into(), cx);
        });
        visual.run_until_parked();
        assert_eq!(
            view.read_with(visual, |view, cx| view.columns(cx)),
            ["path", "status"],
            "positive control: column mode holds the two columns"
        );
        assert_eq!(view.read_with(visual, |view, _| view.visible_len()), 2);
        let format = view.read_with(visual, |view, cx| ExportFormat::Csv(view.csv_columns(cx)));
        view.update_in(visual, |view, window, cx| {
            view.export_rows(ExportScope::Shown, format, window, cx)
        });
        assert!(visual.did_prompt_for_new_path(), "asks where to save");
        let out = root.join("api-filtered.csv");
        let target = out.clone();
        visual.simulate_new_path_selection(move |_| Some(target));
        visual.run_until_parked();

        let mut reader = csv::ReaderBuilder::new()
            .flexible(false)
            .from_path(&out)
            .expect("the file exists and starts with a header");
        let header: Vec<String> = reader
            .headers()
            .unwrap()
            .iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(header, ["time_utc", "level", "path", "status", "message"]);
        let rows: Vec<Vec<String>> = reader
            .records()
            .map(|row| row.unwrap().iter().map(str::to_owned).collect())
            .collect();
        assert_eq!(
            rows,
            [
                vec![
                    "2025-10-01 00:00:05.000",
                    "error",
                    "/v1/notes",
                    "500",
                    "cache miss, \"cold\"\n  at frame one\n  at frame two"
                ],
                vec![
                    "2025-10-01 00:00:07.000",
                    "error",
                    "/v1/search",
                    "503",
                    "boom"
                ],
            ],
            "the shown rows only, the stack trace in its record's row"
        );
    }

    /// #1387 follow-up (Oleg, 18.10): the CSV export guards formula cells by
    /// default; "Raw values" writes them as they are in the log.
    #[gpui::test]
    fn csv_export_guards_formula_cells_unless_raw_values_is_on(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let text = "\
{\"ts\":\"2025-10-01T00:00:05Z\",\"level\":\"info\",\"msg\":\"-5\"}
{\"ts\":\"2025-10-01T00:00:06Z\",\"level\":\"info\",\"msg\":\"=HYPERLINK(\\\"http://x\\\",\\\"y\\\")\"}
{\"ts\":\"2025-10-01T00:00:07Z\",\"level\":\"info\",\"msg\":\"a=b\"}
";
        let (directory, reader, visual) = open_named("formulas.jsonl", text.as_bytes(), cx);
        let root = directory.path().canonicalize().unwrap();
        let view = log_view(&reader, visual);
        visual.run_until_parked();
        assert_eq!(
            view.read_with(visual, |view, _| view.visible_len()),
            3,
            "positive control: the log is loaded"
        );
        // The messages a CSV export of the shown rows holds, in a file `name`.
        let export = |name: &str, visual: &mut VisualTestContext| -> Vec<String> {
            let format = view.read_with(visual, |view, cx| ExportFormat::Csv(view.csv_columns(cx)));
            view.update_in(visual, |view, window, cx| {
                view.export_rows(ExportScope::Shown, format, window, cx)
            });
            assert!(visual.did_prompt_for_new_path(), "asks where to save");
            let out = root.join(name);
            let target = out.clone();
            visual.simulate_new_path_selection(move |_| Some(target));
            visual.run_until_parked();
            let mut csv = csv::ReaderBuilder::new()
                .flexible(false)
                .from_path(&out)
                .expect("the file exists and starts with a header");
            let at = csv
                .headers()
                .unwrap()
                .iter()
                .position(|name| name == "message")
                .expect("a message column");
            csv.records()
                .map(|row| row.unwrap()[at].to_owned())
                .collect()
        };

        // Default: guarded.
        assert!(!view.read_with(visual, |view, _| view.csv_raw_values));
        assert_eq!(
            export("guarded.csv", visual),
            ["'-5", "'=HYPERLINK(\"http://x\",\"y\")", "a=b"]
        );
        // The switch of the export menu: raw.
        view.update(visual, |view, cx| view.set_csv_raw_values(true, cx));
        assert_eq!(
            export("raw.csv", visual),
            ["-5", "=HYPERLINK(\"http://x\",\"y\")", "a=b"]
        );
        // And back: guarded again.
        view.update(visual, |view, cx| view.set_csv_raw_values(false, cx));
        assert_eq!(
            export("guarded-again.csv", visual),
            ["'-5", "'=HYPERLINK(\"http://x\",\"y\")", "a=b"]
        );
    }

    /// #1169: shift-click and shift-arrows select a run of records, which
    /// copy as their lines and export on their own.
    #[gpui::test]
    fn shift_selection_copies_several_records_and_exports_them(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let lines: Vec<String> = (0..6)
            .map(|i| format!(r#"{{"level":"info","msg":"row {i}"}}"#))
            .collect();
        let text = lines.join("\n") + "\n";
        let (directory, reader, visual) = open_log(&text, cx);
        let root = directory.path().canonicalize().unwrap();
        let view = log_view(&reader, visual);
        let selection =
            |visual: &mut VisualTestContext| view.read_with(visual, |v, _| v.selection());
        let copied = |visual: &mut VisualTestContext| {
            view.update_in(visual, |view, window, cx| view.copy_raw(window, cx));
            visual.read(|cx| cx.read_from_clipboard().unwrap().text().unwrap())
        };
        assert_eq!(selection(visual), None, "positive control");
        view.update_in(visual, |view, window, cx| view.select(1, true, window, cx));
        assert_eq!(selection(visual), Some(1..2));
        assert_eq!(copied(visual), lines[1], "one record: its line, as before");

        // Shift-click on a row further down.
        view.update_in(visual, |view, window, cx| {
            view.extend_to(3, false, window, cx)
        });
        assert_eq!(selection(visual), Some(1..4));
        assert_eq!(copied(visual), lines[1..4].join("\n"), "lines, not a file");
        visual.update(|window, cx| window.draw(cx).clear(cx));

        // Shift-arrows move the end that is not the anchor, past it too.
        visual.simulate_keystrokes("shift-up");
        assert_eq!(selection(visual), Some(1..3));
        visual.simulate_keystrokes("shift-up");
        assert_eq!(
            selection(visual),
            Some(1..2),
            "back to the anchor: one record"
        );
        visual.simulate_keystrokes("shift-up");
        assert_eq!(selection(visual), Some(0..2), "and beyond it");
        visual.simulate_keystrokes("shift-down shift-down shift-down");
        assert_eq!(selection(visual), Some(1..4));

        // A plain arrow or click goes back to one record.
        visual.simulate_keystrokes("down");
        assert_eq!(selection(visual), Some(4..5));
        view.update_in(visual, |view, window, cx| view.select(2, false, window, cx));
        assert_eq!(selection(visual), Some(2..3));

        // The selected records export on their own.
        view.update_in(visual, |view, window, cx| {
            view.extend_to(4, false, window, cx);
            view.export_rows(ExportScope::Selected, ExportFormat::Raw, window, cx)
        });
        let out = root.join("selected.log");
        let target = out.clone();
        visual.simulate_new_path_selection(move |_| Some(target));
        visual.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            lines[2..5]
                .iter()
                .map(|line| format!("{line}\n"))
                .collect::<String>()
        );
    }

    /// #1169: exporting a view of several files writes its rows in the order
    /// shown, each JSON line saying which file it came from.
    #[gpui::test]
    fn export_of_a_merged_view_names_each_source(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let samples =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../okilum-core/tests/fixtures/log/samples");
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let names = ["kubernetes/kubelet.jsonl", "apache/apache-error.jsonl"];
        let paths: Vec<PathBuf> = names
            .iter()
            .map(|name| {
                let target = root.join(Path::new(name).file_name().unwrap());
                std::fs::copy(samples.join(name), &target).unwrap();
                target
            })
            .collect();
        cx.update(|cx| {
            crate::test_init(cx);
            crate::bind_keys(cx);
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(paths[0].clone()),
                        session_directory: Some(root.join(".state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        let view = log_view(&reader, visual);
        view.update(visual, |view, cx| view.add_files(paths[1..].to_vec(), cx));
        visual.run_until_parked();
        let total = view.read_with(visual, |view, _| view.total());
        let expected: Vec<(String, u64)> = view.read_with(visual, |view, _| {
            (0..view.visible_len())
                .map(|row| {
                    let (source, entry) = view.locate(view.entry_at(row).unwrap()).unwrap();
                    let line = view
                        .source(source)
                        .unwrap()
                        .index()
                        .get(entry)
                        .unwrap()
                        .line();
                    (view.source_name(source), line)
                })
                .collect()
        });
        assert_eq!(expected.len(), total);
        view.update_in(visual, |view, window, cx| {
            view.export_rows(ExportScope::Shown, ExportFormat::JsonLines, window, cx)
        });
        let out = root.join("both.jsonl");
        let target = out.clone();
        visual.simulate_new_path_selection(move |_| Some(target));
        visual.run_until_parked();
        let written: Vec<(String, u64)> = std::fs::read_to_string(&out)
            .unwrap()
            .lines()
            .map(|line| {
                let row: serde_json::Value = serde_json::from_str(line).unwrap();
                (
                    row["source"].as_str().expect("a source").to_owned(),
                    row["line"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(written, expected, "every row, in the order shown");
        let sources: std::collections::BTreeSet<_> =
            written.iter().map(|(s, _)| s.clone()).collect();
        assert_eq!(sources.len(), 2, "both files are named");
    }

    #[gpui::test]
    fn bounded_window_scope_is_visible_and_initially_reveals_the_real_end(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let mut text = "older\n".repeat(okilum_core::log::window::WINDOW_LINES + 32);
        text.push_str("FINAL_REAL_ROW_1389\n");
        let (_directory, reader, visual) = open_named("density.log", text.as_bytes(), cx);
        let view = log_view(&reader, visual);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.update(|window, cx| window.draw(cx).clear(cx));
        view.read_with(visual, |view, _| {
            let data = view.data().unwrap();
            assert!(data.window().tail && data.window().offset > 0);
            assert_eq!(
                data.raw(data.index().len() - 1).unwrap(),
                b"FINAL_REAL_ROW_1389"
            );
            let notice = view.window_notice().unwrap();
            for scope in ["search", "filters", "counts", "export", "loaded window"] {
                assert!(notice.contains(scope));
            }
            let handle = view.scroll.0.borrow();
            let max = handle.base_handle.max_offset();
            assert!(max.y > px(0.), "positive: list actually overflows");
            assert!(
                -handle.base_handle.offset().y >= max.y - px(2.),
                "first layout must reveal the final real rows"
            );
        });
        assert!(
            visual.debug_bounds("log-window-scope").is_some(),
            "scope is painted, not only a tooltip"
        );
    }

    #[gpui::test]
    fn sliding_window_clears_evicted_marks_and_rejects_an_older_filter(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        use std::io::Write as _;
        let lines = okilum_core::log::window::WINDOW_LINES;
        let text = "keep\n".repeat(lines);
        let (directory, reader, visual) = open_named("sliding.log", text.as_bytes(), cx);
        let view = log_view(&reader, visual);
        view.update(visual, |view, cx| view.toggle_follow(cx));
        visual.run_until_parked();
        view.update_in(visual, |view, window, cx| {
            assert!(
                matches!(view.state, State::Following { .. }),
                "release the redundant original file snapshot"
            );
            view.select(0, false, window, cx);
            view.toggle_mark(cx);
            assert!(
                view.marks.contains(&(0, 0)),
                "positive: mark existed before eviction"
            );
            view.set_text("keep".into(), cx);
            assert!(view.filtering);
        });
        std::fs::OpenOptions::new()
            .append(true)
            .open(directory.path().join("sliding.log"))
            .unwrap()
            .write_all(b"new\nnew\nnew\n")
            .unwrap();
        let request = view.update(visual, |view, _| {
            view.follow.as_mut().unwrap().tail.begin_poll()
        });
        let read = request.read().unwrap();
        view.update(visual, |view, cx| view.apply_poll(0, Ok(read), cx));
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert!(view.data().unwrap().window().offset > 0);
            assert!(view.marks.is_empty() && view.selected.is_none() && view.anchor.is_none());
            assert_eq!(
                view.visible_len(),
                lines - 3,
                "a stale result cannot publish old window IDs"
            );
            assert!(!view.filtering && !view.filter_again && view.filter_running.is_none());
            let snapshot = view.snapshot_of(0).unwrap();
            let Snapshot::Held(snapshot) = snapshot else {
                panic!("Follow must share a leased snapshot")
            };
            assert!(Arc::ptr_eq(
                &snapshot,
                &view.follow.as_ref().unwrap().tail.snapshot()
            ));
        });
    }

    #[gpui::test]
    fn enabling_follow_after_same_shape_rewrite_invalidates_record_identities(
        cx: &mut TestAppContext,
    ) {
        let _shutdown = TestShutdown(cx.clone());
        let (directory, reader, visual) = open_named("rewrite.log", b"aaa\nbbb\n", cx);
        let view = log_view(&reader, visual);
        view.update_in(visual, |view, window, cx| {
            view.select(0, false, window, cx);
            view.toggle_mark(cx);
            assert!(view.marks.contains(&(0, 0)));
        });
        std::fs::write(directory.path().join("rewrite.log"), b"ccc\nddd\n").unwrap();
        view.update(visual, |view, cx| view.toggle_follow(cx));
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert!(matches!(view.state, State::Following { .. }));
            assert_eq!(view.data().unwrap().raw(0).unwrap(), b"ccc");
            assert!(view.marks.is_empty() && view.selected.is_none() && view.anchor.is_none());
        });
    }

    #[gpui::test]
    fn enabling_follow_invalidates_an_equal_shape_rewritten_additional_source(
        cx: &mut TestAppContext,
    ) {
        let _shutdown = TestShutdown(cx.clone());
        let (directory, reader, visual) = open_named("primary.log", b"primary\n", cx);
        let added = directory.path().join("additional.log");
        std::fs::write(&added, b"aaa\nbbb\n").unwrap();
        let view = log_view(&reader, visual);
        view.update(visual, |view, cx| view.add_files(vec![added.clone()], cx));
        visual.run_until_parked();
        view.update_in(visual, |view, window, cx| {
            let id = (0..view.total())
                .find(|&id| view.locate(id) == Some((1, 0)))
                .unwrap();
            view.select(id, false, window, cx);
            view.toggle_mark(cx);
            assert!(view.marks.contains(&(1, 0)));
        });
        std::fs::write(&added, b"ccc\nddd\n").unwrap();
        view.update(visual, |view, cx| view.toggle_follow(cx));
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert!(matches!(view.extra[0].data, SourceData::Tail(_)));
            assert_eq!(view.source(1).unwrap().raw(0).unwrap(), b"ccc");
            assert!(view.marks.is_empty() && view.selected.is_none() && view.anchor.is_none());
        });
    }

    /// A different view's reservations must not discard successful Follow
    /// history. Reclaim only after a real budget failure, before the next batch.
    #[gpui::test]
    fn rotation_history_reclaims_only_after_a_budget_failure(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let (directory, reader, visual) = open_named("rotation.log", b"before\n", cx);
        let view = log_view(&reader, visual);
        view.update(visual, |view, cx| view.toggle_follow(cx));
        visual.run_until_parked();
        let path = directory.path().join("rotation.log");
        std::fs::rename(&path, directory.path().join("rotation.log.1")).unwrap();
        std::fs::write(&path, b"after\n").unwrap();
        let request = view.update(visual, |view, _| {
            view.follow.as_mut().unwrap().tail.begin_poll()
        });
        let read = request.read().unwrap();
        view.update(visual, |view, cx| view.apply_poll(0, Ok(read), cx));
        visual.run_until_parked();
        view.update(visual, |view, cx| {
            assert_eq!(view.source_count(), 2);
            // Positive no-op control: polling itself never requests eviction.
            drop(view.begin_polls(cx));
            assert_eq!(view.source_count(), 2);
            view.apply_poll(0, Err(anyhow::anyhow!("permission denied")), cx);
            drop(view.begin_polls(cx));
            assert_eq!(view.source_count(), 2, "I/O failure preserves history");
            view.apply_poll(
                0,
                Err(okilum_core::log::window_budget::BudgetBusy.into()),
                cx,
            );
            assert_eq!(view.source_count(), 2, "do not renumber an in-flight batch");
            let requests = view.begin_polls(cx);
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].0, 0);
            assert_eq!(view.source_count(), 1);
            assert_eq!(view.data().unwrap().raw(0).unwrap(), b"after");
            assert!(view.history_limited && view.window_notice().is_some());
        });
    }

    #[gpui::test]
    fn bounded_rotation_history_retains_the_latest_six_segments(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let (directory, reader, visual) = open_named("rotation.log", b"generation-0\n", cx);
        let view = log_view(&reader, visual);
        view.update(visual, |view, cx| view.toggle_follow(cx));
        visual.run_until_parked();
        for generation in 1..=9 {
            std::fs::rename(
                directory.path().join("rotation.log"),
                directory.path().join(format!("rotation.log.{generation}")),
            )
            .unwrap();
            std::fs::write(
                directory.path().join("rotation.log"),
                format!("generation-{generation}\n"),
            )
            .unwrap();
            let request = view.update(visual, |view, _| {
                view.follow.as_mut().unwrap().tail.begin_poll()
            });
            let read = request.read().unwrap();
            view.update(visual, |view, cx| view.apply_poll(0, Ok(read), cx));
            visual.run_until_parked();
        }
        view.read_with(visual, |view, _| {
            let mut retained: Vec<_> = view
                .extra
                .iter()
                .filter_map(|extra| {
                    let SourceData::Segment(segment) = &extra.data else {
                        return None;
                    };
                    Some(String::from_utf8(segment.segment.raw(0).unwrap().to_vec()).unwrap())
                })
                .collect();
            retained.sort();
            assert_eq!(
                retained,
                (3..=8)
                    .map(|n| format!("generation-{n}"))
                    .collect::<Vec<_>>()
            );
            assert_eq!(view.data().unwrap().raw(0).unwrap(), b"generation-9");
            assert!(view.history_limited && view.window_notice().is_some());
        });
    }

    #[gpui::test]
    fn a_late_result_never_replaces_the_latest_filter(cx: &mut TestAppContext) {
        let _shutdown = TestShutdown(cx.clone());
        let mut text = String::new();
        for i in 0..40_000 {
            text.push_str(&format!(
                "{{\"level\":\"{}\",\"msg\":\"m{i}\"}}\n",
                if i % 4 == 0 { "error" } else { "info" }
            ));
        }
        let (_directory, reader, visual) = open_log(&text, cx);
        let view = log_view(&reader, visual);
        // Three changes before any run can finish; only the last may land.
        view.update(visual, |view, cx| {
            view.set_text("m1".into(), cx);
            view.update_filter(|filter| filter.level.min = Some(Level::Error), cx);
            view.set_text("m12".into(), cx);
            assert!(view.filtering);
        });
        visual.run_until_parked();
        let rows = shown(&view, visual);
        let expected: Vec<usize> = (0..40_000)
            .filter(|i| i % 4 == 0 && format!("m{i}").contains("m12"))
            .collect();
        assert!(!expected.is_empty());
        assert_eq!(rows, expected);
        view.read_with(visual, |view, _| assert!(!view.filtering));
        // Removing every filter while a run is pending also wins.
        view.update(visual, |view, cx| {
            view.set_text("m3".into(), cx);
            view.set_text(String::new(), cx);
            view.update_filter(|filter| filter.level = Default::default(), cx);
        });
        visual.run_until_parked();
        assert_eq!(shown(&view, visual).len(), 40_000);
        assert_eq!(counter(&view, visual), None);
    }
}
