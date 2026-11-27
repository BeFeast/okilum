//! Re-exports archive viewer + keeps Reader-hosted tests (#996, #3238).
use super::*;
#[cfg(test)]
use okilum_shell_viewers::reader_archive::size;
pub(crate) use okilum_shell_viewers::reader_archive::*;

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn code2809_zip_entry_has_one_measured_compact_bar_and_view_menu(cx: &mut TestAppContext) {
        use reader_viewer_chrome::tests::{fixture, measured_bounds};
        cx.executor().allow_parking();
        let (reader, visual, temp) = fixture(cx, "note.md");
        let mut zip = zip::ZipWriter::new(
            std::fs::File::create(temp.path().join("vault/header.zip")).unwrap(),
        );
        zip.start_file("data.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut zip, b"{\"a\":{\"b\":1}}").unwrap();
        zip.finish().unwrap();
        reader.update_in(visual, |reader, window, cx| {
            reader.preview_file("header.zip", window, cx)
        });
        visual.run_until_parked();
        let archive = reader.read_with(visual, |reader, _| {
            reader
                .pane
                .file_preview
                .as_ref()
                .unwrap()
                .archive
                .clone()
                .unwrap()
        });
        archive.update_in(visual, |archive, window, cx| {
            archive.open_entry(0, window, cx)
        });
        visual.run_until_parked();
        let started = std::time::Instant::now();
        while archive.read_with(visual, |archive, cx| !matches!(&archive.entry, Some(EntryView::Code(code)) if code.read(cx).document_snapshot().is_some())) {
            visual.run_until_parked(); assert!(started.elapsed() < Duration::from_secs(30)); std::thread::sleep(Duration::from_millis(10));
        }
        for width in [1100., 700., 460.] {
            visual.simulate_resize(gpui::size(px(width), px(800.)));
            visual.run_until_parked();
            let bar = measured_bounds(visual, "archive-entry-bar").unwrap();
            assert_eq!(bar.size.height, px(48.));
            for id in ["archive-entry-crumb", "code-view-menu", "code-compact-more"] {
                let item = visual
                    .debug_bounds(id)
                    .unwrap_or_else(|| panic!("required {id}"));
                assert!(
                    item.left() >= bar.left()
                        && item.right() <= bar.right()
                        && item.top() >= bar.top()
                        && item.bottom() <= bar.bottom(),
                    "{id} outside the only entry bar: {item:?} vs {bar:?}"
                );
            }
            let find = visual
                .debug_bounds("viewer-find")
                .or_else(|| visual.debug_bounds("viewer-find-slot"))
                .expect("required compact Find slot");
            assert!(find.left() >= bar.left() && find.right() <= bar.right());
            if bar.size.width < px(600.) {
                let icon = visual
                    .debug_bounds("viewer-find")
                    .expect("narrow entry Find is an icon");
                let switch = visual.debug_bounds("code-view-menu").unwrap();
                visual.simulate_click(icon.center(), Modifiers::default());
                visual.run_until_parked();
                let bar = measured_bounds(visual, "archive-entry-bar").unwrap();
                let field = visual
                    .debug_bounds("viewer-find-field")
                    .expect("the same Find field opened over compact row");
                assert!(field.left() >= bar.left() && field.right() <= bar.right());
                assert!(
                    field.size.width >= px(240.),
                    "compact overlay leaves room for query and steps"
                );
                assert!(
                    visual.debug_bounds("viewer-find-query").unwrap().size.width >= px(64.),
                    "typed query stays visible beside Find controls"
                );
                assert_eq!(visual.debug_bounds("code-view-menu").unwrap(), switch);
                visual.simulate_keystrokes("escape");
                visual.run_until_parked();
            }
        }
        let button = measured_bounds(visual, "code-view-menu").unwrap();
        visual.simulate_click(button.center(), Modifiers::default());
        visual.run_until_parked();
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            assert!(!gpui_base::GlobalState::native_overlays(window, cx).is_empty());
        });
        visual.simulate_keystrokes("down down down enter");
        visual.run_until_parked();
        archive.read_with(visual, |archive, cx| {
            let Some(EntryView::Code(code)) = &archive.entry else {
                panic!("code entry");
            };
            assert_eq!(
                code.read(cx).document_snapshot().unwrap().text.as_ref(),
                "{\"a\":{\"b\":1}}",
                "Raw menu action selected the unformatted snapshot"
            );
        });
    }

    #[gpui::test]
    fn code_entry_click_retains_keyboard_selection_and_copy(cx: &mut TestAppContext) {
        entry_click_copy(cx, "source.rs");
    }

    #[gpui::test]
    fn table_entry_click_retains_keyboard_cell_copy(cx: &mut TestAppContext) {
        entry_click_copy(cx, "data.csv");
    }

    fn entry_click_copy(cx: &mut TestAppContext, name: &str) {
        use reader_viewer_chrome::tests::{fixture, measured_bounds};
        cx.executor().allow_parking();
        let (reader, visual, temp) = fixture(cx, "note.md");
        let mut zip = zip::ZipWriter::new(
            std::fs::File::create(temp.path().join("vault/focus.zip")).unwrap(),
        );
        for (name, text) in [
            ("source.rs", "alpha beta gamma\n"),
            ("data.csv", "id,value\nfirst,1\nsecond,2\n"),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut zip, text.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        reader.update_in(visual, |reader, window, cx| {
            reader.preview_file("focus.zip", window, cx)
        });
        visual.run_until_parked();
        let archive = reader.read_with(visual, |reader, _| {
            reader
                .pane
                .file_preview
                .as_ref()
                .unwrap()
                .archive
                .clone()
                .unwrap()
        });
        let index = archive.read_with(visual, |archive, _| {
            archive
                .listing
                .as_ref()
                .unwrap()
                .entries
                .iter()
                .position(|entry| entry.name == name)
                .unwrap()
        });
        archive.update_in(visual, |archive, window, cx| {
            archive.open_entry(index, window, cx)
        });
        visual.run_until_parked();
        let at = if name == "source.rs" {
            let input = archive.read_with(visual, |archive, cx| {
                match archive.entry.as_ref().unwrap() {
                    EntryView::Code(code) => code.read(cx).input().unwrap(),
                    _ => panic!("code entry"),
                }
            });
            let bounds = input.read_with(visual, |input, _| input.text_bounds().unwrap());
            bounds.origin + point(px(8.), px(8.))
        } else {
            measured_bounds(visual, "delimited-cell-1-0")
                .unwrap()
                .center()
        };
        visual.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into())));
        visual.simulate_click(at, Modifiers::default());
        visual.run_until_parked();
        if name == "source.rs" {
            visual.simulate_keystrokes(if cfg!(target_os = "macos") {
                "cmd-a"
            } else {
                "ctrl-a"
            });
        }
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-c"
        } else {
            "ctrl-c"
        });
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some(if name == "source.rs" {
                    "alpha beta gamma\n"
                } else {
                    "first"
                }),
                "clicking {name} retains the child's keyboard Copy handler"
            )
        });
    }

    #[gpui::test]
    fn entry_compact_bar_fits_460_pixels_and_partial_status_stays_inside_the_pane(
        cx: &mut TestAppContext,
    ) {
        use reader_viewer_chrome::tests::{fixture, measured_bounds};
        let (reader, visual, temp) = fixture(cx, "note.md");
        let mut zip = zip::ZipWriter::new(
            std::fs::File::create(temp.path().join("vault/release-0.9.4.zip")).unwrap(),
        );
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("package.json", options).unwrap();
        std::io::Write::write_all(&mut zip, b"{\"name\":\"entry-fixture\"}\n").unwrap();
        zip.start_file("gateway.log", options).unwrap();
        std::io::Write::write_all(&mut zip, &b"INFO ready\n".repeat(7000)).unwrap();
        zip.finish().unwrap();
        reader.update_in(visual, |reader, window, cx| {
            reader.preview_file("release-0.9.4.zip", window, cx);
            reader.maximize_note(window, cx);
        });
        visual.simulate_resize(gpui::size(px(460.), px(800.)));
        visual.run_until_parked();
        let archive = reader.read_with(visual, |reader, _| {
            reader
                .pane
                .file_preview
                .as_ref()
                .unwrap()
                .archive
                .clone()
                .unwrap()
        });
        for name in ["package.json", "gateway.log"] {
            let index = archive.read_with(visual, |archive, _| {
                archive
                    .listing
                    .as_ref()
                    .unwrap()
                    .entries
                    .iter()
                    .position(|entry| entry.name == name)
                    .unwrap()
            });
            archive.update_in(visual, |archive, window, cx| {
                archive.open_entry(index, window, cx)
            });
            visual.run_until_parked();
            let pane = measured_bounds(visual, "archive-entry").unwrap();
            let bar = measured_bounds(visual, "archive-entry-bar").unwrap();
            let crumb = measured_bounds(visual, "archive-entry-crumb").unwrap();
            assert_eq!(bar.size, gpui::size(px(460.), px(48.)));
            assert_eq!(bar.top(), pane.top());
            assert!(crumb.size.width >= px(120.));
            assert!(crumb.left() >= bar.left() && crumb.right() <= bar.right());
            if name == "gateway.log" {
                let open = measured_bounds(visual, "plain-file-open").unwrap();
                assert!(open.left() >= bar.left() && open.right() <= bar.right());
                let status = measured_bounds(visual, "plain-file-status-line").unwrap();
                let archive_status = measured_bounds(visual, "file-status-line").unwrap();
                assert_eq!(status.size.height, px(28.));
                assert!(status.top() >= bar.bottom() && status.bottom() <= pane.bottom());
                assert!(status.bottom() <= archive_status.top());
            }
        }
    }

    #[gpui::test]
    fn a_selected_entry_fills_the_space_under_the_list(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), "# Note\n").unwrap();
        let mut zip =
            zip::ZipWriter::new(std::fs::File::create(root.join("redesign.zip")).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        for i in 0..74 {
            zip.start_file(format!("src/components/file{i:02}.tsx"), options)
                .unwrap();
            std::io::Write::write_all(&mut zip, b"export const C = () => null;\n").unwrap();
        }
        zip.start_file("src/types/empty-state.d.ts", options)
            .unwrap();
        for i in 0..40 {
            std::io::Write::write_all(
                &mut zip,
                format!("export interface EmptyStateProps{i} {{\n  title: string;\n}}\n")
                    .as_bytes(),
            )
            .unwrap();
        }
        zip.finish().unwrap();
        let state = fixture.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
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
                        index_dir: Some(fixture.path().join("index")),
                        session_directory: Some(state.clone()),
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
        visual.simulate_resize(gpui::size(px(1440.), px(900.)));
        visual.run_until_parked();
        let open = |visual: &mut VisualTestContext| {
            reader.update_in(visual, |r, window, cx| {
                r.preview_file("redesign.zip", window, cx)
            });
            visual.run_until_parked();
            let archive = reader.read_with(visual, |r, _| {
                r.pane
                    .file_preview
                    .as_ref()
                    .unwrap()
                    .archive
                    .clone()
                    .expect("archive view")
            });
            let index = archive.read_with(visual, |a, _| {
                a.listing
                    .as_ref()
                    .expect("listing")
                    .entries
                    .iter()
                    .position(|e| e.name.ends_with(".d.ts"))
                    .unwrap()
            });
            archive.update_in(visual, |a, window, cx| a.open_entry(index, window, cx));
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        };
        open(visual);
        let slot = visual.debug_bounds("archive-entry").expect("entry slot");
        let code = visual
            .debug_bounds("reader-code-file")
            .expect("code preview");
        // 15 lines at the 13 px code size need well over 250 px.
        assert!(
            f32::from(code.size.height) > 300.,
            "the code preview collapsed: {code:?} in {slot:?}"
        );
        let bar = visual
            .debug_bounds("archive-entry-bar")
            .expect("entry compact bar");
        assert_eq!(bar.size.height, px(48.));
        assert!(
            (code.top() - bar.bottom()).abs() <= px(1.),
            "code starts directly below its compact bar"
        );
        assert!(
            (code.bottom() - slot.bottom()).abs() <= px(1.),
            "code still fills the remaining pane"
        );

        // The list starts at 40% of the split, the preview at 60%.
        let pane = visual.debug_bounds("zip-entry-pane").expect("entry pane");
        let table = visual.debug_bounds("archive-table").expect("entry table");
        let share = f32::from(pane.size.height) / f32::from(pane.size.height + table.size.height);
        assert!(
            (0.55..0.65).contains(&share),
            "initial preview share {share}"
        );
        // A real drag of the handle moves the boundary, and it is remembered
        // for the next archive view and the next launch (#1149).
        let handle = visual.debug_bounds("zip-entry-handle").expect("handle");
        let target = handle.center() + point(px(0.), px(150.));
        visual.simulate_mouse_down(handle.center(), MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(
            handle.center() + point(px(0.), px(20.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.simulate_mouse_move(target, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(target, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let dragged = visual.debug_bounds("zip-entry-pane").unwrap().size.height;
        let expected = pane.size.height - px(150.);
        assert!(
            (dragged - expected).abs() < px(8.),
            "the preview follows the handle: {dragged:?}, expected {expected:?}"
        );
        reader.update_in(visual, |r, window, cx| {
            r.open_note("note.md", None, window, cx)
        });
        visual.run_until_parked();
        open(visual);
        let reopened = visual.debug_bounds("zip-entry-pane").unwrap().size.height;
        assert!(
            (reopened - dragged).abs() < px(1.),
            "the next archive view keeps it: {reopened:?}"
        );
        visual
            .executor()
            .advance_clock(std::time::Duration::from_secs(1));
        visual.run_until_parked();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(state.join("reader-ui.json")).unwrap()).unwrap();
        let saved = saved["split_sizes"]["zip-entry"].as_f64().unwrap() as f32;
        assert!(
            (saved - f32::from(dragged)).abs() < 1.,
            "saved for the next launch: {saved}"
        );
        // Back to the initial share for the small-window check.
        archive_split_reset(&reader, visual);

        // A small window: the list starts at 40%, the preview keeps room.
        visual.simulate_resize(gpui::size(px(1000.), px(560.)));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let view = visual.debug_bounds("archive-view").expect("archive view");
        let table = visual.debug_bounds("archive-table").expect("entry table");
        let code = visual
            .debug_bounds("reader-code-file")
            .expect("code preview");
        assert!(
            f32::from(table.size.height) <= f32::from(view.size.height) * 0.4 + 1.,
            "the table is capped: {table:?} in {view:?}"
        );
        assert!(
            f32::from(table.size.height) < ROW_HEIGHT * 12.,
            "positive control: the cap, not the 12-row height, applies here: {table:?}"
        );
        assert!(
            f32::from(code.size.height) > 80.,
            "the preview keeps room: {code:?}"
        );
    }

    fn archive_split_reset(reader: &Entity<Reader>, visual: &mut VisualTestContext) {
        let archive = reader.read_with(visual, |r, _| {
            r.pane
                .file_preview
                .as_ref()
                .unwrap()
                .archive
                .clone()
                .unwrap()
        });
        archive.update(visual, |a, cx| {
            a.entry_split = reader_split::Split::new(
                "zip-entry-reset",
                reader_split::Initial::Share(ENTRY_SHARE),
                ENTRY_MIN_HEIGHT,
                ROW_HEIGHT * LIST_MIN_ROWS,
            );
            cx.notify();
        });
    }

    #[test]
    fn supported_archives_are_eligible() {
        for path in [
            "a.zip",
            "dir/B.ZIP",
            "a.tar",
            "a.tar.gz",
            "dir/B.TAR.GZ",
            "a.tgz",
            "a.tar.bz2",
            "a.tbz2",
            "a.tar.xz",
            "a.txz",
            "a.tar.zst",
            "a.tzst",
            "a.7z",
            "dir/B.7Z",
        ] {
            assert!(eligible(path), "{path}");
        }
        for path in [
            "a.zip.md",
            "a.tar.gz.md",
            "a.gz",
            "a.bz2",
            "a.xz",
            "a.zst",
            "app.log.gz",
            ".tar.gz",
            ".zip",
            "zip",
            "a.pdf",
        ] {
            assert!(!eligible(path), "{path}");
        }
    }

    #[gpui::test]
    fn tar_gz_archive_lists_entries_and_previews_entry(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (reader, visual, temp) = reader_viewer_chrome::tests::fixture(cx, "note.md");
        let tar_gz_path = temp.path().join("vault/release.tar.gz");
        {
            let file = std::fs::File::create(&tar_gz_path).unwrap();
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            let data = b"# Release notes\nhello from tar.gz\n";
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(1_730_000_000);
            header.set_cksum();
            builder
                .append_data(&mut header, "docs/notes.md", &data[..])
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        reader.update_in(visual, |reader, window, cx| {
            reader.preview_file("release.tar.gz", window, cx)
        });
        visual.run_until_parked();
        let archive = reader.read_with(visual, |reader, _| {
            reader
                .pane
                .file_preview
                .as_ref()
                .unwrap()
                .archive
                .clone()
                .expect("tar.gz opens in archive viewer")
        });
        archive.read_with(visual, |archive, _| {
            let listing = archive.listing.as_ref().expect("tar.gz listing loaded");
            assert_eq!(listing.files, 1);
            assert_eq!(listing.entries[0].name, "docs/notes.md");
            assert!(!listing.entries[0].packed_known);
        });
        archive.update_in(visual, |archive, window, cx| {
            archive.open_entry(0, window, cx)
        });
        visual.run_until_parked();
        archive.read_with(visual, |archive, _| {
            assert!(
                matches!(archive.entry, Some(EntryView::Text(_))),
                "tar.gz markdown entry previews in text viewer"
            );
        });
    }

    #[gpui::test]
    fn header_find_filters_entries_and_enter_and_escape_return_to_the_entry_list(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let (reader, visual, temp) = reader_viewer_chrome::tests::fixture(cx, "note.md");
        let mut zip = zip::ZipWriter::new(
            std::fs::File::create(temp.path().join("vault/archive.zip")).unwrap(),
        );
        for name in ["package.json", "README.md"] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut zip, b"{}").unwrap();
        }
        zip.finish().unwrap();
        reader.update_in(visual, |reader, window, cx| {
            reader.preview_file("archive.zip", window, cx)
        });
        visual.run_until_parked();
        let archive = reader.read_with(visual, |reader, _| {
            reader
                .pane
                .file_preview
                .as_ref()
                .unwrap()
                .archive
                .clone()
                .unwrap()
        });
        assert_eq!(
            archive.read_with(visual, |archive, _| archive.rows.len()),
            2
        );
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-f"
        } else {
            "ctrl-f"
        });
        visual.run_until_parked();
        visual.simulate_input("package");
        visual.run_until_parked();
        archive.read_with(visual, |archive, cx| {
            assert_eq!(archive.filter.read(cx).value().to_string(), "package");
            assert_eq!(
                archive.rows.len(),
                1,
                "header query actually narrows archive entries"
            );
            assert_eq!(
                archive.listing.as_ref().unwrap().entries[archive.rows[0]].name,
                "package.json"
            );
        });
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        visual.update(|window, cx| {
            assert!(archive.read(cx).list_focus.is_focused(window));
            assert_eq!(
                archive.read(cx).filter.read(cx).value().to_string(),
                "package"
            );
        });
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-f"
        } else {
            "ctrl-f"
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        visual.update(|window, cx| {
            let archive = archive.read(cx);
            assert_eq!(archive.rows.len(), 2, "one Escape restores every entry");
            assert!(archive.filter.read(cx).value().is_empty());
            assert!(archive.list_focus.is_focused(window));
        });
    }

    #[test]
    fn sizes_and_dos_times_are_human_readable() {
        assert_eq!(size(999, Kind::File), "999 B");
        assert_eq!(size(1_500, Kind::File), "1.5 KB");
        assert_eq!(size(2_500_000, Kind::File), "2.5 MB");
        assert_eq!(size(10, Kind::Directory), "");
        let t = archive::Modified {
            year: 2024,
            month: 10,
            day: 9,
            hour: 7,
            minute: 5,
            second: 0,
        };
        assert_eq!(when(t), "2024-10-09 07:05");
    }
}
