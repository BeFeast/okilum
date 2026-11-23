use super::*;
use ::core::prelude::v1::test;

#[gpui::test]
fn document_keys_scroll_after_click_and_compact_sidebar_selection(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("Start.md"), "# Start").unwrap();
    std::fs::write(
        root.join("Long.md"),
        (0..200)
            .map(|i| format!("Paragraph {i}. Enough text to scroll.\n\n"))
            .collect::<String>(),
    )
    .unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let r = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("Long.md".into()),
                    index_dir: Some(temp.path().join("index")),
                    session_directory: Some(temp.path().join("state")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(r.clone());
        Root::new(r, window, cx)
    });
    let reader = reader.unwrap();
    let position = |visual: &mut gpui::VisualTestContext| {
        reader.read_with(visual, |r, cx| {
            let top = r.pane.content.read(cx).list_state().logical_scroll_top();
            (top.item_ix, f32::from(top.offset_in_item))
        })
    };
    for width in [1400., 664.] {
        visual.simulate_resize(size(px(width), px(800.)));
        visual.run_until_parked();
        if width < 1000. {
            reader.update_in(visual, |r, window, cx| {
                r.open_note("Start.md", None, window, cx)
            });
            visual.run_until_parked();
            reader.update_in(visual, |r, window, cx| {
                r.reveal_in_tree("Long.md", window, cx);
                let row = r
                    .tree
                    .rows
                    .iter()
                    .find(|row| row.path == "Long.md")
                    .unwrap()
                    .clone();
                r.activate_tree_row(&row, window, cx);
            });
            visual.run_until_parked();
            reader.update_in(visual, |r, window, cx| {
                assert!(!r.panels.visible(reader_layout::Panel::Notes, width));
                assert!(r.pane.content.read(cx).focus_handle().is_focused(window));
            });
        } else {
            let bounds = reader.read_with(visual, |r, cx| r.pane.content.read(cx).bounds());
            visual.simulate_click(bounds.center(), Modifiers::default());
            visual.run_until_parked();
        }
        let before = position(visual);
        // The action itself must reject another focus owner, even when
        // dispatched directly instead of through its narrow key context.
        reader.update_in(visual, |r, window, cx| {
            r.tree_focus.focus(window, cx);
            r.scroll_reader_key(1., true, window, cx);
            r.pane
                .content
                .read(cx)
                .focus_handle()
                .clone()
                .focus(window, cx);
        });
        visual.run_until_parked();
        assert_eq!(
            before,
            position(visual),
            "tree focus cannot scroll the note"
        );
        visual.simulate_keystrokes("down");
        visual.run_until_parked();
        let after_line = position(visual);
        assert_ne!(before, after_line, "Down scrolls at width {width}");
        visual.simulate_keystrokes("pagedown");
        visual.run_until_parked();
        let after_page = position(visual);
        assert_ne!(after_line, after_page, "Page Down scrolls at width {width}");
        visual.simulate_keystrokes("pageup");
        visual.run_until_parked();
        assert_ne!(after_page, position(visual));
    }
}
