//! Reader-integration test for `reader_obsidian` block references and
//! footnotes (extracted from `okilum-shell-text::reader_obsidian` so the text
//! crate stays independent of `Reader`, #3235).
use super::*;
use ::core::prelude::v1::test;

#[gpui::test]
fn reader_lands_block_references_and_footnotes(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let root = std::env::temp_dir().join(format!("okilum-651-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let filler = "Filler paragraph for scrolling.\n\n".repeat(60);
    std::fs::write(
        root.join("long.md"),
        format!(
            "# Long\n\nClaim.[^1]\n\n{filler}Target paragraph. ^target\n\n{filler}[^1]: Source.\n"
        ),
    )
    .unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("long.md".into()),
                    ..Opts::default()
                },
                window,
                cx,
            )
        });
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = reader.unwrap();
    let settle = |visual: &mut gpui::VisualTestContext| {
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(300));
        visual.run_until_parked();
    };
    settle(visual);
    let top = |visual: &mut gpui::VisualTestContext| {
        view.read_with(visual, |v, cx| {
            v.pane
                .content
                .read(cx)
                .list_state()
                .logical_scroll_top()
                .item_ix
        })
    };
    assert_eq!(top(visual), 0, "positive control: opens at the top");
    // Find-in-note sees rendered text: the block ID is not in it.
    let matches = |visual: &mut gpui::VisualTestContext, query: &'static str| {
        view.update(visual, |v, cx| {
            v.pane
                .content
                .update(cx, |s, cx| s.set_search_query(query, cx));
        });
        visual.run_until_parked();
        view.read_with(visual, |v, cx| v.pane.content.read(cx).search_status().1)
    };
    assert_eq!(matches(visual, "Target paragraph."), 1, "positive control");
    assert_eq!(matches(visual, "^target"), 0, "the block ID is hidden");
    assert_eq!(matches(visual, "!--"), 0, "the marker is not shown either");
    assert_eq!(matches(visual, ""), 0);
    let (target, definition, reference) = view.read_with(visual, |v, _| {
        let inventory = okilum_core::document_links::HeadingInventory::new(&v.pane.note_source);
        (
            inventory.locate("^target").unwrap().block,
            okilum_core::obsidian::footnote_block(&v.pane.note_source, "1").unwrap(),
            okilum_core::obsidian::footnote_reference_block(&v.pane.note_source, "1").unwrap(),
        )
    });
    assert_eq!(reference, 1);
    assert!(target > 30 && definition > target);
    view.update_in(visual, |v, window, cx| {
        v.open_note_at("long.md", None, Some("^target"), window, cx)
    });
    settle(visual);
    assert_eq!(top(visual), target, "[[long#^target]] lands on the block");
    let weak = view.downgrade();
    visual.update(|window, cx| handle_link(&weak, "okilum://footnote/1", window, cx));
    settle(visual);
    // The footnote is the last block: the list clamps at its end rather
    // than lifting it to the top, so it lands in view, not at the top.
    let landed = top(visual);
    assert!(
        landed > target && landed <= definition,
        "a reference scrolls to its footnote ({landed})"
    );
    view.read_with(visual, |v, _| assert_eq!(v.link_notice, None));
    visual.update(|window, cx| handle_link(&weak, "okilum://footnote-back/1", window, cx));
    settle(visual);
    assert_eq!(top(visual), reference, "the back-link returns to the text");
    view.update_in(visual, |v, window, cx| {
        v.open_note_at("long.md", None, Some("^missing"), window, cx)
    });
    settle(visual);
    view.read_with(visual, |v, _| {
        assert_eq!(
            v.link_notice.as_deref(),
            Some("No block with that ID exists in the current document.")
        );
    });
    std::fs::remove_dir_all(root).unwrap();
}
