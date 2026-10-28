//! Real document/outline navigation in both local-only folder and vault modes.
use crate::*;
use ::core::prelude::v1::test;

fn settle(visual: &mut gpui::VisualTestContext) {
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(300));
    visual.run_until_parked();
}

fn return_to_section(cx: &mut gpui::TestAppContext, folder: bool, history: bool) {
    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
    });
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("notes");
    std::fs::create_dir(&root).unwrap();
    let source = (1..=4)
        .map(|n| {
            format!(
                "## Section {n}\n\n{}",
                "A paragraph long enough to provide a real reading viewport.\n\n".repeat(35)
            )
        })
        .collect::<String>();
    std::fs::write(root.join("A.md"), &source).unwrap();
    std::fs::write(root.join("B.md"), &source).unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: (!folder).then(|| root.clone()),
                    open_path: folder.then(|| root.join("A.md")),
                    note: (!folder).then(|| "A.md".into()),
                    single_file: folder,
                    session_directory: Some(fixture.path().join("state")),
                    index_dir: Some(fixture.path().join("index")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = reader.unwrap();
    visual.simulate_resize(size(px(1400.), px(860.)));
    settle(visual);
    view.update(visual, |v, cx| {
        assert_eq!(v.single_file, folder, "positive control: actual open mode");
        assert_eq!(v.current_rel, "A.md");
        assert_eq!(v.outline.len(), 4);
        assert_eq!(v.current_section(cx), Some(0));
        v.panels.open(reader_layout::Panel::Backlinks);
        cx.notify();
    });
    settle(visual);
    // Use the real Contents click listener, rather than assigning an active section.
    let third = visual.debug_bounds("reader-outline-row-2").unwrap();
    visual.simulate_click(third.center(), Modifiers::default());
    settle(visual);
    view.read_with(visual, |v, cx| {
        assert_eq!(
            v.current_section(cx),
            Some(2),
            "positive control: section 3 selected"
        );
    });
    // Preserve an intra-block offset too, not just the heading number.
    view.update(visual, |v, cx| {
        let block = v.outline[2].target.block;
        v.content.read(cx).list_state().scroll_to(ListOffset {
            item_ix: block + 1,
            offset_in_item: px(7.),
        });
        cx.notify();
    });
    settle(visual);
    let position = view.read_with(visual, |v, cx| {
        assert_eq!(v.current_section(cx), Some(2));
        let position = v.content.read(cx).list_state().logical_scroll_top();
        assert!(position.item_ix > v.outline[2].target.block);
        assert_ne!(position.offset_in_item, px(0.));
        position
    });
    let open_row = |v: &mut Reader, window: &mut Window, cx: &mut Context<Reader>, rel: &str| {
        v.sync_tree();
        let row = v
            .tree
            .rows
            .iter()
            .find(|row| row.path == rel)
            .unwrap()
            .clone();
        v.activate_tree_row(&row, window, cx);
    };
    view.update_in(visual, |v, window, cx| open_row(v, window, cx, "B.md"));
    settle(visual);
    view.read_with(visual, |v, cx| {
        assert_eq!(v.current_rel, "B.md");
        assert_eq!(
            v.current_section(cx),
            Some(0),
            "new note starts at its own top"
        );
    });
    view.update_in(visual, |v, window, cx| {
        if history {
            v.history_move(-1, window, cx);
        } else {
            open_row(v, window, cx, "A.md");
        }
    });
    settle(visual);
    view.read_with(visual, |v, cx| {
        assert_eq!(v.current_rel, "A.md");
        assert_eq!(
            v.current_section(cx),
            Some(2),
            "RETURN1827: section 3 must remain active"
        );
        let restored = v.content.read(cx).list_state().logical_scroll_top();
        assert_eq!(restored.item_ix, position.item_ix);
        assert_eq!(restored.offset_in_item, position.offset_in_item);
    });
    if history {
        view.update_in(visual, |v, window, cx| v.history_move(1, window, cx));
        settle(visual);
        view.read_with(visual, |v, cx| {
            assert_eq!(v.current_rel, "B.md");
            assert_eq!(v.current_section(cx), Some(0));
        });
    }
    // Explicit heading destinations still take precedence over the remembered place.
    view.update_in(visual, |v, window, cx| {
        v.open_note_at("A.md", None, Some("Section 2"), window, cx);
    });
    settle(visual);
    view.read_with(visual, |v, cx| {
        assert_eq!(
            v.current_section(cx),
            Some(1),
            "explicit link destination wins"
        );
    });
    // An explicit bare deep link also has a destination: the document top.
    // It must not inherit either the remembered viewport or the heading above.
    view.update_in(visual, |v, window, cx| {
        v.open_link(
            "A.md",
            okilum_core::deep_link::Position::default(),
            window,
            cx,
        );
    });
    settle(visual);
    view.read_with(visual, |v, cx| {
        assert_eq!(
            v.current_section(cx),
            Some(0),
            "a bare deep link opens at the top"
        );
        assert!(v.navigation.flash.is_none(), "a bare deep link stays quiet");
    });
}

#[gpui::test]
fn folder_reopening_from_file_list_restores_section_and_position(cx: &mut gpui::TestAppContext) {
    return_to_section(cx, true, false);
}

#[gpui::test]
fn vault_reopening_from_file_list_restores_section_and_position(cx: &mut gpui::TestAppContext) {
    return_to_section(cx, false, false);
}

#[gpui::test]
fn folder_back_forward_restores_section_and_position(cx: &mut gpui::TestAppContext) {
    return_to_section(cx, true, true);
}

#[gpui::test]
fn vault_back_forward_restores_section_and_position(cx: &mut gpui::TestAppContext) {
    return_to_section(cx, false, true);
}
