use super::*;
use ::core::prelude::v1::test;
use gpui_component::WindowExt as _;

const FILES: [(&str, &str); 5] = [
    ("A.md", "# Alpha\n\nalpha body\n"),
    ("B.md", "# Beta\n\nbeta body\n"),
    ("Docs/C.md", "# Gamma\n\ngamma body\n"),
    ("notes.txt", "plain notes\n"),
    ("Folder/inner.md", "# Inner\n"),
];

fn vault(fixture: &Path) -> PathBuf {
    let root = fixture.join("vault");
    for (path, body) in FILES {
        std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
        std::fs::write(root.join(path), body).unwrap();
    }
    root
}

fn init(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
}

/// One Reader window on `root`, its state in `fixture` so a second launch
/// with the same arguments is a relaunch.
fn launch<'a>(
    cx: &'a mut TestAppContext,
    fixture: &Path,
    root: &Path,
    note: &str,
) -> (Entity<Reader>, &'a mut VisualTestContext) {
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.to_path_buf()),
                    note: Some(note.into()),
                    index_dir: Some(fixture.join("index")),
                    session_directory: Some(fixture.join("state")),
                    panel_settings_override: Some(fixture.join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    visual.simulate_resize(size(px(1400.), px(900.)));
    visual.run_until_parked();
    (reader.unwrap(), visual)
}

/// Bounds from a freshly drawn frame: cached views do not replay debug bounds.
fn bounds(visual: &mut VisualTestContext, selector: &'static str) -> Option<Bounds<Pixels>> {
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    visual.debug_bounds(selector)
}

fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let target = bounds(visual, selector).unwrap_or_else(|| panic!("{selector} is drawn"));
    visual.simulate_click(target.center(), Modifiers::default());
    visual.run_until_parked();
}

/// Chooses Pin or Unpin in the open document's ⋯ menu. From no selection,
/// Up selects the last item (Close), then Move to Trash, then Pin.
fn pin_from_document_menu(visual: &mut VisualTestContext) {
    click(visual, "document-more");
    visual.simulate_keystrokes("up up up enter");
    visual.run_until_parked();
}

fn open(reader: &Entity<Reader>, visual: &mut VisualTestContext, rel: &str) {
    reader.update_in(visual, |r, window, cx| r.open_note(rel, None, window, cx));
    visual.run_until_parked();
    reader.read_with(visual, |r, _| assert_eq!(r.selected_file(), rel));
}

/// #2789: Pin from the file's ⋯ menu puts the file in the row above the
/// document; it stays there while other notes and files open, a click opens
/// it, unsaved edits follow ordinary navigation, and the pins come back after
/// a relaunch.
///
/// Without the feature the first failing assertion is «Pin from the note
/// menu pins A.md»: the ⋯ menu has no Pin item, so Up Up Up Enter chooses
/// another item and nothing is pinned; with only the old store the row
/// assertions (`reader-pin-row`) fail as nothing draws it.
#[gpui::test]
fn pin_from_file_menu_keeps_the_file_in_the_row_across_navigation_and_relaunch(
    cx: &mut TestAppContext,
) {
    init(cx);
    let fixture = tempfile::tempdir().unwrap();
    let root = vault(fixture.path());
    {
        let (reader, visual) = launch(cx, fixture.path(), &root, "A.md");
        assert!(
            bounds(visual, "reader-pin-row").is_none(),
            "control: no pins, no row"
        );
        assert!(
            bounds(visual, "document-header").is_some(),
            "note header drawn"
        );

        // Pin the open note from its ⋯ menu.
        pin_from_document_menu(visual);
        reader.read_with(visual, |r, _| {
            assert!(
                r.sidebar.is_pinned("A.md"),
                "Pin from the note menu pins A.md"
            );
            assert_eq!(r.selected_file(), "A.md", "pinning opens nothing");
        });
        let row = bounds(visual, "reader-pin-row").expect("the pinned row is drawn");
        let chip = bounds(visual, "reader-pin-A.md").expect("A.md is in the row");
        let header = bounds(visual, "document-header").unwrap();
        assert!(
            row.bottom() <= header.top() + px(0.5),
            "row sits above the header"
        );
        assert!(row.contains(&chip.center()));
        assert_eq!(row.size.height, px(PIN_ROW_HEIGHT));

        // Navigate elsewhere: the pin stays one click away.
        open(&reader, visual, "B.md");
        assert!(
            bounds(visual, "reader-pin-A.md").is_some(),
            "A stays pinned on B"
        );
        open(&reader, visual, "notes.txt");
        assert!(
            bounds(visual, "reader-pin-A.md").is_some(),
            "A stays on a file"
        );

        // A plain file is pinned from the file viewer's ⋯ menu.
        pin_from_document_menu(visual);
        reader.read_with(visual, |r, _| {
            assert!(r.sidebar.is_pinned("notes.txt"), "Pin from the file menu");
            assert_eq!(
                r.pinned_files()
                    .iter()
                    .map(|p| p.path.as_str())
                    .collect::<Vec<_>>(),
                ["A.md", "notes.txt"],
                "pin order"
            );
        });

        // A click on a pin opens it; the row is the same afterwards.
        click(visual, "reader-pin-A.md");
        reader.read_with(visual, |r, _| assert_eq!(r.selected_file(), "A.md"));
        click(visual, "reader-pin-notes.txt");
        reader.read_with(visual, |r, _| assert_eq!(r.selected_file(), "notes.txt"));

        // Unsaved edits: leaving B through a pin is ordinary navigation, so
        // the edit is saved as on any other switch and nothing is discarded.
        #[cfg(any(unix, windows))]
        {
            open(&reader, visual, "B.md");
            reader.update_in(visual, |r, window, cx| {
                r.toggle_source(window, cx);
                r.set_live_preview(false, window, cx);
            });
            visual.run_until_parked();
            visual.simulate_input("pinned draft sentinel ");
            visual.run_until_parked();
            reader.read_with(visual, |r, cx| {
                assert!(r.source_is_dirty(cx), "control: dirty")
            });
            assert!(
                !std::fs::read_to_string(root.join("B.md"))
                    .unwrap()
                    .contains("pinned draft sentinel"),
                "control: the edit is not on disk yet"
            );
            click(visual, "reader-pin-A.md");
            let on_disk = std::fs::read_to_string(root.join("B.md"))
                .unwrap()
                .contains("pinned draft sentinel");
            let kept = reader.read_with(visual, |r, cx| {
                r.selected_file() == "B.md" && r.source_is_dirty(cx)
            });
            assert!(on_disk || kept, "the unsaved edit is saved or still open");
            if on_disk {
                reader.read_with(visual, |r, _| assert_eq!(r.selected_file(), "A.md"));
            }
        }

        // The Pinned menu (#1517) reads the same pins.
        reader.read_with(visual, |r, _| {
            let listed: Vec<_> = r
                .nav_entries(reader_title_nav::Menu::Pinned, reader_sidebar::now())
                .into_iter()
                .map(|entry| entry.path)
                .collect();
            assert!(listed.contains(&"A.md".to_owned()), "{listed:?}");
        });
        visual.update(|window, _| window.remove_window());
        visual.run_until_parked();
    }

    // Relaunch on the same vault and state: the pins and the row are back.
    let (reader, visual) = launch(cx, fixture.path(), &root, "B.md");
    reader.read_with(visual, |r, _| {
        assert!(r.sidebar.is_pinned("A.md") && r.sidebar.is_pinned("notes.txt"));
    });
    assert!(
        bounds(visual, "reader-pin-row").is_some(),
        "row after relaunch"
    );
    assert!(bounds(visual, "reader-pin-A.md").is_some());
    click(visual, "reader-pin-A.md");
    reader.read_with(visual, |r, _| assert_eq!(r.selected_file(), "A.md"));

    // Unpin from the open note's ⋯ menu, then from the row's ×.
    pin_from_document_menu(visual);
    reader.read_with(visual, |r, _| {
        assert!(!r.sidebar.is_pinned("A.md"), "Unpin")
    });
    assert!(bounds(visual, "reader-pin-A.md").is_none());
    click(visual, "reader-unpin-notes.txt");
    reader.read_with(visual, |r, _| assert!(r.sidebar.pinned.is_empty()));
    assert!(
        bounds(visual, "reader-pin-row").is_none(),
        "no pins, no row"
    );
    reader.read_with(visual, |r, _| assert_eq!(r.selected_file(), "A.md"));
}

/// #2789: a pinned file deleted or renamed outside Okilum stays in the row,
/// marked missing, until removed. The Pinned menu leaves it out, and a click
/// on it opens nothing. Pinned folders stay out of the row.
///
/// Without the fix the pruning of a complete inventory drops the pin
/// silently: «the vanished pin stays» fails.
#[gpui::test]
fn a_pin_moved_or_deleted_elsewhere_shows_as_missing_until_removed(cx: &mut TestAppContext) {
    init(cx);
    let fixture = tempfile::tempdir().unwrap();
    let root = vault(fixture.path());
    let (reader, visual) = launch(cx, fixture.path(), &root, "A.md");
    reader.update(visual, |r, cx| {
        assert!(r.vault.inventory_complete, "control: complete inventory");
        r.toggle_pin("Docs/C.md", cx);
        r.toggle_pin("Folder", cx);
        r.toggle_pin("B.md", cx);
    });
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        let pins = r.pinned_files();
        assert_eq!(
            pins.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
            ["Docs/C.md", "B.md"],
            "a pinned folder stays in the Pinned menu only"
        );
        assert!(pins.iter().all(|p| !p.missing), "control: all present");
    });

    // Renamed outside Okilum: the old path is gone from the next inventory.
    std::fs::rename(root.join("Docs/C.md"), root.join("Docs/C renamed.md")).unwrap();
    reader.update_in(visual, |r, window, cx| {
        r.refresh_inventory(okilum_core::Changes::default(), window, cx)
    });
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        assert!(
            r.vault
                .entries
                .iter()
                .any(|e| e.path == "Docs/C renamed.md"),
            "control: the inventory saw the rename"
        );
        assert!(r.sidebar.is_pinned("Docs/C.md"), "the vanished pin stays");
        let missing = r.pinned_files();
        assert!(
            missing.iter().any(|p| p.path == "Docs/C.md" && p.missing),
            "{missing:?}"
        );
        assert!(missing.iter().any(|p| p.path == "B.md" && !p.missing));
        let menu: Vec<_> = r
            .nav_entries(reader_title_nav::Menu::Pinned, reader_sidebar::now())
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        assert!(!menu.contains(&"Docs/C.md".to_owned()), "{menu:?}");
        assert!(menu.contains(&"B.md".to_owned()), "control: {menu:?}");
    });
    assert!(
        bounds(visual, "reader-pin-Docs/C.md").is_some(),
        "shown as missing"
    );
    click(visual, "reader-pin-Docs/C.md");
    reader.read_with(visual, |r, _| assert_eq!(r.selected_file(), "A.md"));
    visual.update(|window, cx| window.clear_notifications(cx));

    // Deleted outside Okilum: the same, and the × removes it.
    std::fs::remove_file(root.join("B.md")).unwrap();
    reader.update_in(visual, |r, window, cx| {
        r.refresh_inventory(okilum_core::Changes::default(), window, cx)
    });
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        assert!(r.pinned_files().iter().all(|p| p.missing));
    });
    click(visual, "reader-unpin-B.md");
    click(visual, "reader-unpin-Docs/C.md");
    reader.read_with(visual, |r, _| {
        assert_eq!(r.sidebar.pinned, ["Folder"], "only the folder pin is left");
        assert!(r.pinned_files().is_empty());
    });
    assert!(bounds(visual, "reader-pin-row").is_none());
}

/// #2789: many pins keep one row of fixed height that never pushes the
/// document off screen; the first pins stay clickable.
#[gpui::test]
fn many_pins_keep_one_usable_row(cx: &mut TestAppContext) {
    init(cx);
    let fixture = tempfile::tempdir().unwrap();
    let root = vault(fixture.path());
    for i in 0..40 {
        std::fs::write(
            root.join(format!("Many {i:02} with a long descriptive name.md")),
            "# Many\n",
        )
        .unwrap();
    }
    let (reader, visual) = launch(cx, fixture.path(), &root, "A.md");
    reader.update(visual, |r, cx| {
        r.toggle_pin("B.md", cx);
        for i in 0..40 {
            r.toggle_pin(&format!("Many {i:02} with a long descriptive name.md"), cx);
        }
    });
    visual.run_until_parked();
    let row = bounds(visual, "reader-pin-row").unwrap();
    let header = bounds(visual, "document-header").unwrap();
    let window = visual.update(|window, _| window.viewport_size());
    assert_eq!(row.size.height, px(PIN_ROW_HEIGHT), "one row");
    assert!(row.right() <= window.width + px(0.5), "row fits the window");
    assert!(header.top() >= row.bottom() - px(0.5) && header.bottom() < window.height);
    click(visual, "reader-pin-B.md");
    reader.read_with(visual, |r, _| assert_eq!(r.selected_file(), "B.md"));
}

/// #2789: a single file opened without its vault has no pins, so no Pin item
/// and no row, even when the folder's saved state lists pins.
#[gpui::test]
fn a_single_file_has_no_pins(cx: &mut TestAppContext) {
    init(cx);
    let fixture = tempfile::tempdir().unwrap();
    let root = vault(fixture.path());
    // Control: the same item exists for a vault.
    {
        let (reader, visual) = launch(cx, fixture.path(), &root, "A.md");
        let weak = reader.downgrade();
        visual.update(|_, cx| assert!(pin_menu_item(&weak, "A.md", cx).is_some()));
        visual.update(|window, _| window.remove_window());
        visual.run_until_parked();
    }
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            Reader::new(
                Opts {
                    open_path: Some(root.join("A.md")),
                    single_file: true,
                    session_directory: Some(fixture.path().join("single-state")),
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
    reader.update(visual, |r, cx| {
        assert!(r.single_file, "control: single-file mode");
        r.sidebar.pinned = vec!["A.md".into()];
        cx.notify();
    });
    let weak = reader.downgrade();
    visual.update(|_, cx| assert!(pin_menu_item(&weak, "A.md", cx).is_none()));
    reader.read_with(visual, |r, _| assert!(r.pinned_files().is_empty()));
    assert!(bounds(visual, "reader-pin-row").is_none());
}
