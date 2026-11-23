use super::*;

#[test]
fn linked_from_titles_prefer_h1_then_frontmatter_then_name() {
    let dir = std::env::temp_dir().join(format!("okilum-381-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, body: &str| {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    };
    let h1 = write(
        "a.md",
        "---\ntitle: \"Front\"\n---\n```\n# not a title\n```\n# Real title\n",
    );
    let front = write("b.md", "---\ntitle: Front only\n---\nText.\n");
    let plain = write("c.md", "Just text.\n");
    assert_eq!(display_title(&h1).as_deref(), Some("Real title"));
    assert_eq!(display_title(&front).as_deref(), Some("Front only"));
    assert_eq!(
        display_title(&plain),
        None,
        "positive control: file name fallback"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A point a few pixels inside the first text line of the centred
/// Reader column (docs/design/reader.md: ≤740px, left gutter ≤48px, 44px top).
fn reader_text_origin(document: Bounds<Pixels>) -> Point<Pixels> {
    let column = document.size.width.min(px(READER_MAX_WIDTH));
    point(
        document.left() + (document.size.width - column) / 2. + px(48. + 8.),
        document.top() + px(44. + 20.),
    )
}
use ::core::prelude::v1::test;
#[test]
fn delayed_parse_and_old_list_cannot_satisfy_landing() {
    for attempt in 0..99 {
        assert_eq!(
            reader_landing_state(true, true, 0, 8, attempt),
            ReaderLanding::Waiting
        );
        assert_eq!(
            reader_landing_state(true, false, 100, 8, attempt),
            ReaderLanding::Cancelled
        );
    }
    assert_eq!(
        reader_landing_state(true, true, 3, 8, 99),
        ReaderLanding::Failed
    );
    assert_eq!(
        reader_landing_state(true, true, 9, 8, 99),
        ReaderLanding::Ready
    );
    assert_eq!(
        reader_landing_state(false, true, 9, 8, 10),
        ReaderLanding::Cancelled
    );
}
#[gpui::test]
fn table_alias_fixture_uses_one_three_column_grid(cx: &mut gpui::TestAppContext) {
    struct TableView {
        source: String,
        captured: Arc<std::sync::Mutex<Option<gpui_component::text::TableData>>>,
    }
    impl Render for TableView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let captured = self.captured.clone();
            div().w(px(600.)).child(
                TextView::markdown("table-fixture", self.source.clone())
                    .style(reader_text_style(cx.theme()))
                    .table_actions(move |table, _, _| {
                        *captured.lock().unwrap() = Some(table.clone());
                        div()
                            .id("table-end")
                            .debug_selector(|| "table-end".into())
                            .child("After table")
                    }),
            )
        }
    }
    cx.update(crate::test_init);
    let source = include_str!("../../../fixtures/reader/table-alias-columns.md");
    let vault = Vault::from_note_paths([
        "notes/alpha.md".into(),
        "notes/beta.md".into(),
        "notes/gamma.md".into(),
    ]);
    let source = okilum_core::render::rewrite_source_links(source, &vault, "table.md");
    let captured = Arc::new(std::sync::Mutex::new(None));
    let (_, visual) = cx.add_window_view(|_, _| TableView {
        source,
        captured: captured.clone(),
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let table = captured
        .lock()
        .unwrap()
        .clone()
        .expect("table actually rendered");
    assert_eq!(table.headers.len(), 3);
    assert_eq!(table.rows.len(), 4);
    assert!(table.rows.iter().all(|row| row.len() == 3));
    assert_eq!(table.rows[0][0], "First alias Second alias");
    assert_eq!(table.rows[2], ["Missing cells", "", ""]);
    assert_eq!(table.rows[3][2], "Kept third column.");
    assert!(!table.markdown.contains("Discarded fourth"));
    assert!(
        visual
            .debug_bounds("table-content-frame")
            .unwrap()
            .size
            .height
            > px(100.),
        "long text wraps within the shared grid"
    );
}

#[gpui::test]
fn escape_in_preview_and_source_preserves_docked_and_overlay_panels(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let fixture = std::env::temp_dir().join(format!("okilum-escape483-{}", uuid::Uuid::new_v4()));
    let root = fixture.join("vault");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("note.md"), "# Note\n\nKeep reading.\n").unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("note.md".into()),
                    session_directory: Some(fixture.join("state")),
                    index_dir: Some(fixture.join("index")),
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
    let reader = reader.unwrap();
    visual.run_until_parked();
    for width in [1366., 700.] {
        visual.simulate_resize(size(px(width), px(768.)));
        visual.run_until_parked();
        for source in [false, true]
            .into_iter()
            .filter(|source| !source || cfg!(any(unix, windows)))
        {
            for panel in [reader_layout::Panel::Notes, reader_layout::Panel::Backlinks] {
                reader.update_in(visual, |reader, window, cx| {
                    assert_eq!(
                        reader.pane.current_rel, "note.md",
                        "loaded note positive control"
                    );
                    reader.panels.open(panel);
                    if reader.pane.editing.is_some() != source {
                        reader.toggle_source(window, cx);
                    }
                    if !source {
                        reader
                            .pane
                            .content
                            .read(cx)
                            .focus_handle()
                            .clone()
                            .focus(window, cx);
                    }
                    cx.notify();
                });
                visual.run_until_parked();
                let before = reader.read_with(visual, |reader, _| {
                    assert!(
                        reader.panels.visible(panel, width),
                        "open panel positive control"
                    );
                    (
                        reader.panels.visible(reader_layout::Panel::Notes, width),
                        reader
                            .panels
                            .visible(reader_layout::Panel::Backlinks, width),
                    )
                });
                let focus = visual.update(|window, cx| window.focused(cx).unwrap());
                visual.simulate_keystrokes("escape escape");
                visual.run_until_parked();
                reader.update_in(visual, |reader, window, _| {
                    assert_eq!(
                        before,
                        (
                            reader.panels.visible(reader_layout::Panel::Notes, width),
                            reader
                                .panels
                                .visible(reader_layout::Panel::Backlinks, width)
                        )
                    );
                    if !source {
                        assert!(focus.is_focused(window), "Escape preserves Reader focus");
                    }
                    assert!(reader.pane.editing.is_none(), "Escape returns to Reader");
                });
            }
        }
    }
    std::fs::remove_dir_all(fixture).unwrap();
}

#[gpui::test]
fn wheel_scrolls_both_document_margins_without_double_scrolling_text(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let root = std::env::temp_dir().join(format!("okilum-scroll377-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("long.md"),
        "Paragraph for scrolling.\n\n".repeat(160),
    )
    .unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("long.md".into()),
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
    visual.simulate_resize(size(px(1800.), px(860.)));
    visual.run_until_parked();
    view.update_in(visual, |v, _, cx| {
        v.panels = Default::default();
        cx.notify();
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let document = visual.debug_bounds("reader-document").unwrap();
    let column = visual.debug_bounds("reader-column").unwrap();
    assert!(
        column.left() - document.left() > px(100.),
        "real left margin"
    );
    assert!(
        document.right() - column.right() > px(100.),
        "real right margin"
    );
    let mut positions = Vec::new();
    for x in [
        column.left() + px(100.),
        document.left() + px(20.),
        document.right() - px(20.),
    ] {
        view.update_in(visual, |v, _, cx| {
            v.pane
                .content
                .read(cx)
                .list_state()
                .scroll_to(ListOffset::default());
            cx.notify();
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.simulate_event(ScrollWheelEvent {
            position: point(x, document.top() + px(120.)),
            delta: ScrollDelta::Pixels(point(px(0.), px(-80.))),
            ..Default::default()
        });
        visual.run_until_parked();
        let offset = view.read_with(visual, |v, cx| {
            v.pane.content.read(cx).list_state().logical_scroll_top()
        });
        assert!(
            offset.item_ix > 0 || offset.offset_in_item > px(0.),
            "wheel positive control"
        );
        positions.push((offset.item_ix, offset.offset_in_item));
    }
    assert_eq!(
        positions[0], positions[1],
        "left margin matches one text wheel event"
    );
    assert_eq!(
        positions[0], positions[2],
        "right margin matches one text wheel event"
    );

    // At compact widths the panels cover document margins. A wheel over
    // either overlay must not reach the document behind it. Closing the
    // panel at the same pointer position is the positive control.
    visual.simulate_resize(size(px(900.), px(860.)));
    for panel in [reader_layout::Panel::Notes, reader_layout::Panel::Backlinks] {
        for open in [true, false] {
            view.update_in(visual, |v, _, cx| {
                v.panels = Default::default();
                if open {
                    v.panels.open(panel);
                }
                v.pane
                    .content
                    .read(cx)
                    .list_state()
                    .scroll_to(ListOffset::default());
                cx.notify();
            });
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
            let document = visual.debug_bounds("reader-document").unwrap();
            let column = visual.debug_bounds("reader-column").unwrap();
            let x = if panel == reader_layout::Panel::Notes {
                document.left() + px(20.)
            } else {
                document.right() - px(20.)
            };
            let position = point(x, document.top() + px(120.));
            assert!(!column.contains(&position), "pointer is over a margin");
            if open {
                let selector = if panel == reader_layout::Panel::Notes {
                    "reader-notes-panel"
                } else {
                    "reader-backlinks-panel"
                };
                assert!(visual.debug_bounds(selector).unwrap().contains(&position));
            }
            visual.simulate_event(ScrollWheelEvent {
                position,
                delta: ScrollDelta::Pixels(point(px(0.), px(-80.))),
                ..Default::default()
            });
            visual.run_until_parked();
            let offset = view.read_with(visual, |v, cx| {
                v.pane.content.read(cx).list_state().logical_scroll_top()
            });
            let moved = offset.item_ix > 0 || offset.offset_in_item > px(0.);
            assert_eq!(moved, !open, "overlay {panel:?}, open={open}");
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui::test]
fn outline_rows_contain_text_at_every_heading_level(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let root = std::env::temp_dir().join(format!("okilum-outline417-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let source = (0..36)
        .map(|i| {
            format!(
                "{} Section — Раздел {}\n\nText.\n\n",
                "#".repeat(i % 6 + 1),
                i + 1
            )
        })
        .collect::<String>();
    std::fs::write(root.join("headings.md"), source).unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("headings.md".into()),
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
    visual.run_until_parked();
    view.update(visual, |v, cx| {
        assert_eq!(v.pane.outline.len(), 36, "long outline positive control");
        v.panels.open(reader_layout::Panel::Backlinks);
        cx.notify();
    });
    for height in [700., 1000.] {
        visual.simulate_resize(size(px(1400.), px(height)));
        visual.run_until_parked();
        // The first six rendered rows exercise H1-H6, Latin and Cyrillic,
        // while the remaining headings force the outline height limit.
        for (ix, (row_id, text_id)) in [
            ("reader-outline-row-0", "reader-outline-text-0"),
            ("reader-outline-row-1", "reader-outline-text-1"),
            ("reader-outline-row-2", "reader-outline-text-2"),
            ("reader-outline-row-3", "reader-outline-text-3"),
            ("reader-outline-row-4", "reader-outline-text-4"),
            ("reader-outline-row-5", "reader-outline-text-5"),
        ]
        .into_iter()
        .enumerate()
        {
            let row = visual.debug_bounds(row_id).unwrap();
            let text = visual.debug_bounds(text_id).unwrap();
            assert!(text.size.height > px(10.), "text must actually be laid out");
            assert!(
                row.top() <= text.top()
                    && row.bottom() >= text.bottom()
                    && row.left() <= text.left()
                    && row.right() >= text.right(),
                "H{} at window height {height}: row {row:?} must contain text {text:?}",
                ix + 1,
            );
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui::test]
fn the_outline_builds_only_the_rows_in_view(cx: &mut gpui::TestAppContext) {
    // #1004: «On this page» built a row, with its own click listener, for every heading on
    // every redrawn frame, so a note with hundreds of headings made every scroll frame
    // slower. Only the rows in view are built.
    const HEADINGS: usize = 436;
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    std::fs::create_dir(&root).unwrap();
    let source = (0..HEADINGS)
        .map(|i| format!("{} Section {i}\n\nText.\n\n", "#".repeat(i % 3 + 1)))
        .collect::<String>();
    std::fs::write(root.join("many.md"), source).unwrap();
    std::fs::write(root.join("few.md"), "# One\n\n## Two\n").unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("many.md".into()),
                    session_directory: Some(fixture.path().join("state")),
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
    visual.run_until_parked();
    visual.simulate_resize(size(px(1366.), px(768.)));
    view.update(visual, |v, cx| {
        v.panels.open(reader_layout::Panel::Backlinks);
        cx.notify();
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert_eq!(
            v.pane.outline.len(),
            HEADINGS,
            "positive control: all headings exist"
        );
        assert_eq!(v.outline_list.item_count(), HEADINGS);
    });
    // `debug_bounds` takes a `&'static str`.
    let name = |prefix: &str, ix: usize| -> &'static str {
        Box::leak(format!("{prefix}-{ix}").into_boxed_str())
    };
    let built = |visual: &mut VisualTestContext| -> Vec<usize> {
        (0..HEADINGS)
            .filter(|ix| {
                visual
                    .debug_bounds(name("reader-outline-row", *ix))
                    .is_some()
            })
            .collect()
    };
    let first = built(visual);
    assert!(
        first.contains(&0),
        "positive control: the first row is drawn"
    );
    assert!(
        (5..=40).contains(&first.len()),
        "{} of {HEADINGS} rows built for one screen",
        first.len()
    );
    assert!(
        !first.contains(&(HEADINGS - 1)),
        "the last row is not built"
    );
    // #2814: with «Linked from» empty the list is no longer cut at 60 %;
    // it takes the height «Linked from» does not need and stays in view.
    let list = visual.debug_bounds("reader-outline-list").unwrap();
    assert!(
        list.size.height > px(768. * 0.6 + 1.),
        "the list uses the free height: {list:?}"
    );
    let linked = visual.debug_bounds("right-linked-from-heading").unwrap();
    assert!(
        linked.bottom() <= px(768.),
        "«Linked from» stays in view: {linked:?}"
    );
    let row = visual.debug_bounds("reader-outline-row-0").unwrap();
    assert_eq!(row.size.height, px(OUTLINE_ROW_HEIGHT));

    // Scrolling brings other rows into view (the list really works).
    view.update(visual, |v, cx| {
        v.outline_list.scroll_by(px(3000.));
        cx.notify();
    });
    visual.run_until_parked();
    let scrolled = built(visual);
    assert!(
        !scrolled.is_empty() && !scrolled.contains(&0),
        "{scrolled:?}"
    );
    assert!(scrolled.len() <= 40);

    // A click on a row still jumps to its heading.
    let target = scrolled[scrolled.len() / 2];
    let at = visual
        .debug_bounds(name("reader-outline-row", target))
        .unwrap();
    let block = view.read_with(visual, |v, _| v.pane.outline[target].target.block);
    visual.simulate_click(at.center(), Modifiers::default());
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(300));
    visual.run_until_parked();
    view.read_with(visual, |v, cx| {
        assert_eq!(
            v.pane
                .content
                .read(cx)
                .list_state()
                .logical_scroll_top()
                .item_ix,
            block
        );
    });

    // Another note resets the list to its own headings.
    view.update_in(visual, |v, window, cx| {
        v.open_note("few.md", None, window, cx)
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert_eq!(v.pane.outline.len(), 2);
        assert_eq!(v.outline_list.item_count(), 2);
    });
    assert_eq!(built(visual), vec![0, 1]);
}

#[gpui::test]
fn reader_outline_follows_scroll_jumps_to_headings_and_clears_without_headings(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let root =
        std::env::temp_dir().join(format!("okilum-reader348-outline-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let filler = "Filler paragraph for scrolling.\n\n".repeat(40);
    let source = format!(
            "# Title\n\n{filler}## Alpha\n\n{filler}### Alpha detail\n\n{filler}```md\n# not a heading\n```\n\nSetext\n------\n\n{filler}## Alpha\n\n{filler}"
        );
    std::fs::write(root.join("long.md"), &source).unwrap();
    std::fs::write(root.join("plain.md"), "Just text, no headings.\n").unwrap();
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
    visual.simulate_resize(size(px(1400.), px(860.)));
    visual.run_until_parked();
    // Real top-level headings in source order: fenced examples excluded,
    // Setext included, repeated labels kept as separate entries.
    let outline = view.read_with(visual, |v, _| {
        v.pane
            .outline
            .iter()
            .map(|h| (h.text.clone(), h.level))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        outline,
        [
            ("Title".to_string(), 1),
            ("Alpha".to_string(), 2),
            ("Alpha detail".to_string(), 3),
            ("Setext".to_string(), 2),
            ("Alpha".to_string(), 2),
        ]
    );
    view.read_with(visual, |v, cx| assert_eq!(v.current_section(cx), Some(0)));
    // Jumping to the second «Alpha» lands on that exact block, and the
    // highlight follows the rendered scroll position, not the label.
    let block = view.read_with(visual, |v, _| v.pane.outline[4].target.block);
    view.update_in(visual, |v, _, cx| v.scroll_to_block(block, cx));
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(300));
    visual.run_until_parked();
    view.read_with(visual, |v, cx| {
        assert_eq!(
            v.pane
                .content
                .read(cx)
                .list_state()
                .logical_scroll_top()
                .item_ix,
            block
        );
        assert_eq!(v.current_section(cx), Some(4));
    });
    // A note without headings clears the previous outline.
    view.update_in(visual, |v, window, cx| {
        v.open_note("plain.md", None, window, cx)
    });
    visual.run_until_parked();
    view.read_with(visual, |v, cx| {
        assert_eq!(v.pane.current_rel, "plain.md");
        assert!(v.pane.outline.is_empty());
        assert_eq!(v.current_section(cx), None);
    });
    let _ = std::fs::remove_dir_all(root);
}

#[gpui::test]
fn wide_table_persistent_cues_follow_scroll_and_expand(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let root = std::env::temp_dir().join(format!("okilum-reader418-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    // #379 wraps headings: force overflow beyond the per-column floors
    // instead of relying on long headings staying on one line.
    let source = format!(
        "|{}|\n|{}|\n|{}|\n",
        (0..24)
            .map(|i| format!("Long column heading {i}"))
            .collect::<Vec<_>>()
            .join("|"),
        ["---"; 24].join("|"),
        [" "; 24].join("|")
    );
    std::fs::write(root.join("wide.md"), source).unwrap();
    std::fs::write(root.join("narrow.md"), "| A | B |\n|---|---|\n| 1 | 2 |\n").unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("wide.md".into()),
                    ..Opts::default()
                },
                window,
                cx,
            )
        });
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    let reader = reader.unwrap();
    visual.simulate_resize(size(px(1500.), px(1000.)));
    visual.run_until_parked();
    for _ in 0..3 {
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.run_until_parked();
    }
    let track = visual
        .debug_bounds("table-scroll-track")
        .expect("overflow positive control");
    let thumb = visual.debug_bounds("table-scroll-thumb").unwrap();
    assert!(thumb.size.width < track.size.width);
    assert!((thumb.left() - track.left()).abs() < px(1.));
    assert!(visual.debug_bounds("table-shadow-right").is_some());
    assert!(visual.debug_bounds("table-shadow-left").is_none());
    let badge = visual
        .debug_bounds("expand-table-badge")
        .expect("badge visible without hovering table");
    visual.simulate_click(badge.center(), Modifiers::default());
    reader.read_with(visual, |v, _| assert!(v.pane.table_overlay.is_some()));
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    for delta in [-100., -10000.] {
        visual.simulate_event(ScrollWheelEvent {
            position: point(track.center().x, track.top() - px(15.)),
            delta: ScrollDelta::Pixels(point(px(delta), px(0.))),
            ..Default::default()
        });
        for _ in 0..3 {
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.run_until_parked();
        }
        let moved = visual.debug_bounds("table-scroll-thumb").unwrap();
        assert!(
            moved.left() > thumb.left(),
            "horizontal wheel actually moved content"
        );
        assert!(visual.debug_bounds("table-shadow-left").is_some());
        if delta == -100. {
            assert!(visual.debug_bounds("table-shadow-right").is_some());
        } else {
            assert!(visual.debug_bounds("table-shadow-right").is_none());
            assert!((moved.right() - track.right()).abs() < px(2.));
        }
    }
    reader.update_in(visual, |v, window, cx| {
        v.open_note("narrow.md", None, window, cx)
    });
    visual.run_until_parked();
    for _ in 0..3 {
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.run_until_parked();
    }
    for selector in [
        "table-scroll-track",
        "table-shadow-left",
        "table-shadow-right",
        "expand-table-badge",
    ] {
        assert!(
            visual.debug_bounds(selector).is_none(),
            "narrow table has no {selector}"
        );
    }
    let _ = std::fs::remove_dir_all(root);
}

#[gpui::test]
fn table_overlay_occludes_manager_bounds_and_restores_them(cx: &mut gpui::TestAppContext) {
    use std::cell::{Cell, RefCell};
    // Exercise the real Reader overlay and shared paint-time registry on
    // every platform; no WKWebView or production T3 connection is needed.
    struct Probe {
        reader: Entity<Reader>,
        clear: Rc<Cell<Option<Bounds<Pixels>>>>,
        regions: Rc<RefCell<Vec<Bounds<Pixels>>>>,
    }
    let manager = Bounds::new(point(px(1000.), px(100.)), size(px(350.), px(650.)));
    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let clear = self.clear.clone();
            let regions = self.regions.clone();
            let manager = Bounds::new(point(px(1000.), px(100.)), size(px(350.), px(650.)));
            div()
                .size_full()
                .relative()
                .child(self.reader.clone())
                .child(
                    canvas(
                        |_, _, _| (),
                        move |_, (), window, cx| {
                            let overlays = gpui_base::GlobalState::native_overlays(window, cx);
                            *regions.borrow_mut() = overlays.to_vec();
                            clear.set(reader_manager_occlusion::uncovered_bounds(
                                manager, overlays,
                            ));
                        },
                    )
                    .absolute()
                    .inset_0(),
                )
        }
    }
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.md"), "# A\n").unwrap();
    let clear = Rc::new(Cell::new(None));
    let regions = Rc::new(RefCell::new(Vec::new()));
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root),
                    note: Some("a.md".into()),
                    index_dir: Some(temp.path().join("cache")),
                    session_directory: Some(temp.path().join("state")),
                    panel_settings_override: Some(temp.path().join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(view.clone());
        let probe = cx.new(|_| Probe {
            reader: view,
            clear: clear.clone(),
            regions: regions.clone(),
        });
        Root::new(probe, window, cx)
    });
    let view = reader.unwrap();
    visual.simulate_resize(size(px(1400.), px(960.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert_eq!(
        clear.get(),
        Some(manager),
        "positive control: Manager area is initially available"
    );
    view.update_in(visual, |v, window, cx| {
        v.open_table_overlay("| x | y |\n|---|---|\n|1|2|\n", window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        regions
            .borrow()
            .iter()
            .any(|r| r.intersect(&manager) == manager),
        "real Table scrim must register over the Manager area"
    );
    assert_eq!(
        clear.get(),
        None,
        "Manager native view must hide beneath Table"
    );
    view.update_in(visual, |v, window, cx| v.close_table_overlay(window, cx));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert_eq!(
        clear.get(),
        Some(manager),
        "closing Table restores Manager on the next paint"
    );
}

#[gpui::test]
fn reader_table_overlay_opens_closes_and_leaves_on_navigation(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let root = std::env::temp_dir().join(format!("okilum-reader368-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("a.md"),
        "# A\n\n| x | y |\n|---|---|\n| 1 | 2 |\n",
    )
    .unwrap();
    std::fs::write(root.join("b.md"), "# B\n").unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("a.md".into()),
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
    visual.run_until_parked();
    let document = view.read_with(visual, |v, _| v.pane.content.entity_id());
    view.update_in(visual, |v, window, cx| {
        v.open_table_overlay("| x | y |\n|---|---|\n| 1 | 2 |\n", window, cx);
        assert!(
            v.pane.table_overlay.is_some(),
            "positive control: overlay opens"
        );
        // Escape closes the overlay before anything else; the document
        // underneath is the same entity, so its position is untouched.
        v.dismiss(window, cx);
        assert!(v.pane.table_overlay.is_none());
        assert_eq!(v.pane.content.entity_id(), document);
        v.open_table_overlay("| x |\n|---|\n| 1 |\n", window, cx);
        v.open_note("b.md", None, window, cx);
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert_eq!(v.pane.current_rel, "b.md");
        assert!(
            v.pane.table_overlay.is_none(),
            "navigation from the overlay closes it"
        );
    });
    let _ = std::fs::remove_dir_all(root);
}

#[gpui::test]
fn reader_properties_parse_on_open_and_links_navigate(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let root = std::env::temp_dir().join(format!("okilum-reader386-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("plan.md"),
        "---\ntype: Note\nrelated: \"[[menu]]\"\nmissing: \"[[nowhere]]\"\n_id: x\n---\n# Plan\n",
    )
    .unwrap();
    std::fs::write(root.join("menu.md"), "# Menu\n").unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("plan.md".into()),
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
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        let props = v.pane.properties.as_ref().expect("valid frontmatter");
        let keys: Vec<&str> = props.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, ["type", "related", "missing", "_id"]);
        assert_eq!(
            reader_properties::summary(props),
            "Note · 2 relations",
            "hidden key not counted, both links are relations"
        );
        // An unresolved relation says so and stays put (positive control
        // for the navigation below).
        v.open_property_link("nowhere", window, cx);
        assert_eq!(v.pane.current_rel, "plan.md");
        assert!(v.link_notice.is_some());
        v.open_property_link("menu", window, cx);
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert_eq!(v.pane.current_rel, "menu.md");
        assert!(
            v.pane.properties.as_ref().unwrap().is_empty(),
            "menu has none"
        );
    });
    let _ = std::fs::remove_dir_all(root);
}

#[gpui::test]
fn one_non_utf8_note_does_not_block_links_between_readable_notes(cx: &mut gpui::TestAppContext) {
    // #1120: the vault finished loading; one CP1251 note stays reported as unreadable, and
    // wikilinks between the other notes still open instead of saying "still loading".
    use okilum_core::document_links::prepared::LinkStatus;
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("Target.md"), "# Target\n\n[[Other vault]]\n").unwrap();
    std::fs::write(root.join("Other vault.md"), "# Other vault\n\n[[Target]]\n").unwrap();
    let legacy: Vec<u8> = [
        &b"# Legacy encoding\n\n"[..],
        &[
            0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2, 0x20, 0xEC, 0xE8, 0xF0, 0x0A,
        ],
    ]
    .concat();
    assert!(
        std::str::from_utf8(&legacy).is_err(),
        "the fixture is really not UTF-8"
    );
    std::fs::write(root.join("Non-UTF8.md"), &legacy).unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("Target.md".into()),
                    index_dir: Some(fixture.path().join("index")),
                    session_directory: Some(fixture.path().join("state")),
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
    visual.run_until_parked();
    let url = view.read_with(visual, |v, _| {
        assert!(v.vault.inventory_scanned, "the vault finished loading");
        assert_eq!(
            v.vault.unreadable.len(),
            1,
            "one item stays reported as unreadable"
        );
        assert!(!v.vault.inventory_complete);
        assert_eq!(v.pane.current_rel, "Target.md");
        let link = v
            .pane
            .link_identities
            .iter()
            .find(|link| link.target == "Other vault")
            .expect("the wikilink is prepared");
        let state = v
            .pane
            .prepared_links
            .get(&link.url)
            .expect("its state is published");
        assert_eq!(
            (state.status, state.reason.as_str()),
            (LinkStatus::Resolved, "Open document"),
            "not \"unavailable while the vault is loading\""
        );
        link.url.clone()
    });
    let entity = view.downgrade();
    visual.update(|window, cx| reader_link_navigation::handle_link(&entity, &url, window, cx));
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert_eq!(v.pane.current_rel, "Other vault.md")
    });
}

/// #1313: leaving a note for the vault home emptied the backlinks but not the cards'
/// ranges, and the next frame of «Linked from» indexed an empty list (a crash).
#[gpui::test]
fn the_vault_home_empties_linked_from_and_a_stale_card_draws_nothing(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("Target.md"), "# Target\n\nLinked to.\n").unwrap();
    std::fs::write(root.join("Source.md"), "# Source\n\nsee [[Target]]\n").unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("Target.md".into()),
                    index_dir: Some(fixture.path().join("index")),
                    session_directory: Some(fixture.path().join("state")),
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
    visual.run_until_parked();
    visual.simulate_resize(size(px(1366.), px(768.)));
    view.update_in(visual, |v, window, cx| {
        v.panel_settings = None;
        v.toggle_panel(reader_layout::Panel::Backlinks, window, cx);
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| assert_eq!(v.pane.backlink_groups.len(), 1));
    assert!(
        visual.debug_bounds("bl-card-0").is_some(),
        "positive control: the card is drawn"
    );

    // To the vault home, then a width change redraws the panel (the night QA steps).
    view.update_in(visual, |v, window, cx| v.show_empty_vault(window, cx));
    visual.run_until_parked();
    visual.simulate_resize(size(px(683.), px(768.)));
    visual.run_until_parked();
    visual.simulate_resize(size(px(1366.), px(768.)));
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert!(v.pane.backlinks.is_empty());
        assert!(
            v.pane.backlink_groups.is_empty(),
            "the cards went with the backlinks"
        );
        assert_eq!(v.backlinks_list.item_count(), 0);
    });

    // Even a pair out of step draws no card instead of crashing.
    view.update(visual, |v, cx| {
        v.pane.backlink_groups = std::iter::once(0..1).collect();
        v.backlinks_list.reset_with_uniform_height(1, px(58.));
        cx.notify();
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("bl-card-0").is_none());
}

#[gpui::test]
fn the_linked_from_list_builds_only_the_cards_in_view(cx: &mut gpui::TestAppContext) {
    // #1135: a hub note with thousands of backlinks built every card on every frame, so
    // typing anywhere in the Reader redrew all of them. Only the cards in view are built.
    const NOTES: usize = 400;
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("Hub.md"), "# Hub\n\nWhere everything points.\n").unwrap();
    for ix in 0..NOTES {
        // One note links five times: its card shows three places and «Show 2 more».
        let body = if ix == 7 {
            "# Many\n\n[[Hub]] one\n\n[[Hub]] two\n\n[[Hub]] three\n\n[[Hub]] four\n\n[[Hub]] five\n"
                    .to_owned()
        } else {
            format!("# Note {ix}\n\nsee [[Hub]] for the overview\n")
        };
        std::fs::write(root.join(format!("Note {ix:03}.md")), body).unwrap();
    }
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("Hub.md".into()),
                    index_dir: Some(fixture.path().join("index")),
                    session_directory: Some(fixture.path().join("state")),
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
    visual.run_until_parked();
    visual.simulate_resize(size(px(1366.), px(768.)));
    view.update_in(visual, |v, window, cx| {
        v.panel_settings = None;
        v.toggle_panel(reader_layout::Panel::Backlinks, window, cx);
    });
    visual.run_until_parked();
    let (groups, many) = view.read_with(visual, |v, _| {
        let many = v
            .pane
            .backlink_groups
            .iter()
            .position(|g| v.pane.backlinks[g.start].path == "Note 007.md")
            .expect("the note with five places is a group");
        assert_eq!(v.backlinks_list.item_count(), v.pane.backlink_groups.len());
        (v.pane.backlink_groups.len(), many)
    });
    assert_eq!(
        groups, NOTES,
        "one card per linking note, counted before drawing"
    );
    // `debug_bounds` takes a `&'static str`.
    let name = |prefix: &str, ix: usize| -> &'static str {
        Box::leak(format!("{prefix}-{ix}").into_boxed_str())
    };
    let built = |visual: &mut VisualTestContext| -> Vec<usize> {
        (0..groups)
            .filter(|ix| visual.debug_bounds(name("bl-card", *ix)).is_some())
            .collect()
    };
    let first = built(visual);
    assert!(
        first.contains(&0),
        "positive control: the first card is drawn"
    );
    assert!(
        (3..=40).contains(&first.len()),
        "{} of {groups} cards built for one screen",
        first.len()
    );
    assert!(!first.contains(&(groups - 1)), "the last card is not built");

    // Scrolling brings others into view and drops the first ones (the list really works).
    // The list does not ask for a redraw itself when scrolled from outside.
    view.update(visual, |v, cx| {
        v.backlinks_list.scroll_by(px(3000.));
        cx.notify();
    });
    visual.run_until_parked();
    let scrolled = built(visual);
    assert!(
        !scrolled.is_empty() && !scrolled.contains(&0),
        "{scrolled:?}"
    );
    assert!(scrolled.len() <= 40);

    // A background refresh that finds the same notes keeps the position; another note starts
    // from the top.
    view.update(visual, |v, cx| {
        let same = v.pane.backlinks.clone();
        v.set_backlinks(same, false);
        cx.notify();
    });
    visual.run_until_parked();
    let kept = built(visual);
    assert!(
        !kept.is_empty() && !kept.contains(&0),
        "scroll lost by a refresh: {kept:?}"
    );
    view.update(visual, |v, cx| {
        let same = v.pane.backlinks.clone();
        v.set_backlinks(same, true);
        cx.notify();
    });
    visual.run_until_parked();
    assert!(built(visual).contains(&0), "a new note starts at the top");

    // «Show 2 more» on a card makes that card taller without building the others.
    // The card sits at the foot of the first screen: bring it to the top so its «Show 2 more»
    // row is inside the window and can be clicked.
    view.update(visual, |v, cx| {
        v.backlinks_list.scroll_to(ListOffset {
            item_ix: many,
            offset_in_item: px(0.),
        });
        cx.notify();
    });
    visual.run_until_parked();
    let before = visual
        .debug_bounds(name("bl-card", many))
        .expect("the card with five places is in view");
    let more = visual
        .debug_bounds(name("bl-more", many))
        .expect("it offers «Show 2 more»");
    visual.simulate_click(more.center(), Modifiers::default());
    visual.run_until_parked();
    let after = visual.debug_bounds(name("bl-card", many)).unwrap();
    assert!(
        after.size.height > before.size.height,
        "{before:?} -> {after:?}"
    );
    assert!(visual.debug_bounds(name("bl-more", many)).is_none());
    assert!(built(visual).len() <= 40);
}

#[gpui::test]
fn hover_and_navigation_agree_in_a_vault_with_a_non_utf8_note_also_on_a_warm_start(
    cx: &mut gpui::TestAppContext,
) {
    // #1188: in a vault with one CP1251 note the hover said «Link unavailable.» while the
    // click opened the other note. The hover reads the prepared state of the link and the
    // tooltip reads the presentation state, so both are checked, on the note opened first and
    // on the one the click lands on, on a cold start and again on the warm start that follows.
    use okilum_core::document_links::prepared::LinkStatus;
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    std::fs::create_dir(&root).unwrap();
    // The shape of the vault of the report: front matter everywhere, a note per folder entry
    // linking both Alpha and Beta, so each of them is linked from thousands of places.
    let note = |body: &str| format!("---\ntype: Note\n---\n# {body}\n");
    std::fs::write(root.join("Alpha.md"), note("Alpha\n\n[[Beta]]")).unwrap();
    std::fs::write(root.join("Beta.md"), note("Beta\n\n[[Alpha]]")).unwrap();
    std::fs::write(
        root.join("@index.md"),
        note("Gate home\n\n[[Alpha]] [[Beta]]"),
    )
    .unwrap();
    for ix in 0..300 {
        let folder = root.join("Notes").join(format!("Group {:02}", ix / 100));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join(format!("Note {ix:04}.md")),
            note(&format!(
                "Note {ix:04}\n\nMeeting {ix}. [[Alpha]] [[Beta]]\n\nMeeting content."
            )),
        )
        .unwrap();
    }
    std::fs::write(
        root.join("Non-UTF8.md"),
        [
            &b"# Legacy\n\n"[..],
            &[0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2, 0x0A],
        ]
        .concat(),
    )
    .unwrap();
    for launch in ["cold", "warm"] {
        let mut reader = None;
        let (window, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Alpha.md".into()),
                        index_dir: Some(fixture.path().join("index")),
                        session_directory: Some(fixture.path().join("state")),
                        ..Opts::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let _ = window;
        let view = reader.unwrap();
        visual.run_until_parked();
        view.read_with(visual, |v, _| {
            assert_eq!(v.vault.unreadable.len(), 1, "{launch}: one item unreadable");
            let link = v
                .pane
                .link_identities
                .iter()
                .find(|link| link.target == "Beta")
                .unwrap_or_else(|| panic!("{launch}: the link is prepared"));
            let state = v
                .pane
                .prepared_links
                .get(&link.url)
                .unwrap_or_else(|| panic!("{launch}: no state for the link"));
            assert_eq!(
                (state.status, state.reason.as_str()),
                (LinkStatus::Resolved, "Open document"),
                "{launch}: the state the hover reads"
            );
            assert!(
                reader_hover::resolved_target(
                    &link.url,
                    &v.pane.prepared_links,
                    &v.pane.current_rel
                )
                .is_some(),
                "{launch}: the hover has a note to preview"
            );
        });
        // The tooltip is painted from the presentation states, the click reads the
        // prepared ones: both must agree, on the note the click lands on as well.
        let first = view.read_with(visual, |v, _| {
            let link = v
                .pane
                .link_identities
                .iter()
                .find(|l| l.target == "Beta")
                .unwrap();
            let shown = v.pane.link_presentations.get(&link.url);
            assert_eq!(
                shown.map(|s| (s.status, s.reason.clone())),
                Some((LinkStatus::Resolved, "Open document".into())),
                "{launch}: the tooltip state in Alpha"
            );
            link.url.clone()
        });
        let entity = view.downgrade();
        visual
            .update(|window, cx| reader_link_navigation::handle_link(&entity, &first, window, cx));
        visual.run_until_parked();
        view.read_with(visual, |v, _| {
            assert_eq!(
                v.pane.current_rel, "Beta.md",
                "{launch}: the click opened Beta"
            );
            let link = v
                .pane
                .link_identities
                .iter()
                .find(|l| l.target == "Alpha")
                .unwrap_or_else(|| panic!("{launch}: Beta's link is prepared"));
            for (name, states) in [
                ("tooltip", &v.pane.link_presentations),
                ("click", &v.pane.prepared_links),
            ] {
                let state = states
                    .get(&link.url)
                    .unwrap_or_else(|| panic!("{launch}: no {name} state in Beta"));
                assert_eq!(
                    (state.status, state.reason.as_str()),
                    (LinkStatus::Resolved, "Open document"),
                    "{launch}: the {name} state in Beta"
                );
            }
            assert!(
                reader_hover::resolved_target(
                    &link.url,
                    &v.pane.prepared_links,
                    &v.pane.current_rel
                )
                .is_some(),
                "{launch}: Beta's hover has a note to preview"
            );
        });
        visual.update(|window, _| window.remove_window());
        visual.run_until_parked();
    }
}

/// #1508: the sidebar is the folder tree only. Recent and Pinned keep their
/// stores (the title bar reads them) but never come back as sidebar rows.
#[gpui::test]
fn tree_mode_is_the_folder_tree_only_even_with_recent_and_pins(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| Reader::new(Opts::default(), window, cx));
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = reader.unwrap();
    visual.run_until_parked();
    // #1512: no Folders row either; every sidebar item is a tree row.
    let not_tree = |v: &Reader| {
        v.sidebar_items()
            .iter()
            .filter(|item| !matches!(item, SideItem::Tree(_)))
            .count()
    };
    let entries = |v: &Reader| {
        v.sidebar_items()
            .iter()
            .filter(|item| matches!(item, SideItem::Entry { .. }))
            .count()
    };
    view.update(visual, |v, _| {
        v.loading = None;
        v.vault = Arc::new(Vault::from_note_paths(["Work/Plan.md".to_string()]));
        v.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
        v.sync_tree();
        v.sidebar.recent.clear();
        v.sidebar.pinned.clear();
        assert_eq!(not_tree(v), 0);
        // Filled stores still add nothing to the sidebar.
        v.sidebar.record_open("Work/Plan.md", 0);
        v.sidebar.toggle_pin("Work/Plan.md");
        v.sidebar.toggle_pin("Work");
        assert_eq!(not_tree(v), 0);
        assert_eq!(entries(v), 0);
        assert_eq!(v.sidebar.pinned, ["Work/Plan.md", "Work"]);
        assert_eq!(v.sidebar.recent.len(), 1);
        // Positive control: the tree rows are there.
        assert!(v
            .sidebar_items()
            .iter()
            .any(|item| matches!(item, SideItem::Tree(row) if row.path == "Work")));
    });
}

/// #1350: an empty tree says so in the sidebar's one hint; an empty vault
/// shows no hint at all.
#[gpui::test]
fn empty_sidebar_tree_hint(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| Reader::new(Opts::default(), window, cx));
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = reader.unwrap();
    visual.run_until_parked();
    let hints = |v: &Reader| -> Vec<&'static str> {
        v.sidebar_items()
            .into_iter()
            .filter_map(|item| match item {
                SideItem::Empty(text) => Some(text),
                _ => None,
            })
            .collect()
    };
    view.update(visual, |v, _| {
        v.loading = None;
        v.sidebar.recent.clear();
        v.sidebar.pinned.clear();
        v.vault = Arc::new(Vault::from_note_paths(Vec::<String>::new()));
        assert!(hints(v).is_empty(), "an empty vault shows no hints");
        v.vault = Arc::new(Vault::from_note_paths(["Work/Plan.md".to_string()]));
        assert_eq!(hints(v), ["Notes and folders appear here."]);
        // Positive control: a populated tree drops its hint.
        v.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
        v.sync_tree();
        assert!(hints(v).is_empty());
    });
}

#[gpui::test]
fn recursive_folder_actions_preserve_scroll_and_take_keyboard_focus(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| Reader::new(Opts::default(), window, cx));
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = reader.unwrap();
    visual.simulate_resize(size(px(1400.), px(860.)));
    visual.run_until_parked();
    view.update_in(visual, |v, _, cx| {
        v.loading = None;
        v.vault = Arc::new(Vault::from_note_paths(
            (0..1000).map(|i| format!("Big/Sub/Note {i}.md")),
        ));
        v.pane.current_rel = "Big/Sub/Note 0.md".into();
        v.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
        v.panels.open(reader_layout::Panel::Notes);
        v.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
        v.sync_tree();
        v.tree.set_subtree("Big", true);
        cx.notify();
    });
    visual.run_until_parked();
    // Start below the top, with keyboard focus in the document rather
    // than the tree. A mouse subtree action must establish both anchors.
    view.update_in(visual, |v, window, cx| {
        v.pane
            .content
            .read(cx)
            .focus_handle()
            .clone()
            .focus(window, cx);
        assert!(!v.tree_focus.is_focused(window));
        v.tree_scroll
            .scroll_to_item_strict(800, ScrollStrategy::Top);
        cx.notify();
    });
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        assert!(v.tree_scroll.0.borrow().base_handle.offset().y < px(-1000.));
        v.set_folder_subtree("Big", false, window, cx);
        assert!(v.tree_focus.is_focused(window));
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert_eq!(v.tree.rows.len(), 1);
        assert_eq!(v.tree_scroll.0.borrow().base_handle.offset().y, px(0.));
    });
    visual.simulate_keystrokes("alt-right");
    visual.run_until_parked();
    view.read_with(visual, |v, _| assert_eq!(v.tree.rows.len(), 1002));
    view.update_in(visual, |v, _, cx| {
        v.tree_scroll
            .scroll_to_item_strict(800, ScrollStrategy::Top);
        cx.notify();
    });
    visual.run_until_parked();
    view.update(visual, |v, cx| {
        assert!(v.tree_scroll.0.borrow().base_handle.offset().y < px(-1000.));
        v.collapse_folders(cx);
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert_eq!(v.tree_scroll.0.borrow().base_handle.offset().y, px(0.));
    });
    // Reveal current also clears a Folders fold saved by an older build.
    view.update_in(visual, |v, window, cx| {
        v.sidebar.collapsed.insert(reader_sidebar::Section::Folders);
        v.focus_current_folder(window, cx);
        assert_eq!(v.tree.cursor.as_deref(), Some("Big/Sub/Note 0.md"));
        assert!(!v
            .sidebar
            .collapsed
            .contains(&reader_sidebar::Section::Folders));
    });
}

/// #774: no colour draws nothing; a colour draws a 2 px line at the top
/// of every window of that vault and a dot at the vault name, without
/// moving the header. Another vault's window stays plain.
#[gpui::test]
fn vault_color_draws_line_and_dot_in_every_window_of_that_vault(cx: &mut gpui::TestAppContext) {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    let other = fixture.path().join("other");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&other).unwrap();
    let state = fixture.path().join("state");
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
        reader_ui_state::install(&state, cx);
    });
    let open = |vault: &Path, cx: &mut gpui::TestAppContext| {
        let mut reader = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| Reader::new(Opts::default(), window, cx));
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        let view = reader.unwrap();
        visual.simulate_resize(size(px(1200.), px(700.)));
        view.update(&mut visual, |v, cx| {
            v.loading = None;
            v.vault_root = vault.to_owned();
            v.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
            v.panels.open(reader_layout::Panel::Notes);
            cx.notify();
        });
        visual.run_until_parked();
        visual
    };
    let mut first = open(&root, cx);
    let mut second = open(&root, cx);
    let mut unrelated = open(&other, cx);
    for visual in [&mut first, &mut second, &mut unrelated] {
        assert!(visual.debug_bounds("vault-accent-line").is_none());
        assert!(visual.debug_bounds("sidebar-vault-dot").is_none());
    }
    let title_before = first.debug_bounds("sidebar-vault-title").unwrap();
    let actions = |visual: &mut gpui::VisualTestContext| {
        SidebarAction::available()
            .iter()
            .map(|action| {
                visual
                    .debug_bounds(action.id())
                    .unwrap_or_else(|| panic!("{}", action.id()))
            })
            .collect::<Vec<_>>()
    };
    let actions_before = actions(&mut first);
    first
        .update(|_, cx| reader_ui_state::set_vault_color(&root, Some(brand::VaultColor::Teal), cx));
    for visual in [&mut first, &mut second, &mut unrelated] {
        visual.run_until_parked();
    }
    for visual in [&mut first, &mut second] {
        let line = visual.debug_bounds("vault-accent-line").unwrap();
        assert_eq!(line.origin.y, px(0.));
        assert_eq!(line.size.height, px(VAULT_ACCENT_LINE));
        assert_eq!(line.size.width, px(1200.));
        let dot = visual.debug_bounds("sidebar-vault-dot").unwrap();
        let title = visual.debug_bounds("sidebar-vault-title").unwrap();
        assert!(dot.right() <= title.left(), "dot sits before the name");
    }
    let title_after = first.debug_bounds("sidebar-vault-title").unwrap();
    assert_eq!(
        title_after.origin.y, title_before.origin.y,
        "the line is drawn over the header, not above it"
    );
    assert_eq!(
        title_after.left() - title_before.left(),
        px(VAULT_DOT_SLOT),
        "the dot takes exactly its slot before the name"
    );
    assert_eq!(
        actions(&mut first),
        actions_before,
        "header actions do not shift"
    );
    assert!(
        unrelated.debug_bounds("vault-accent-line").is_none(),
        "positive control: only this vault's windows change"
    );
    first.update(|_, cx| reader_ui_state::set_vault_color(&root, None, cx));
    first.run_until_parked();
    assert!(first.debug_bounds("vault-accent-line").is_none());
    assert!(first.debug_bounds("sidebar-vault-dot").is_none());
}

/// #1474/#1508: the sidebar is a header plus the tree. Hundreds of pins
/// add no section, and scrolling never changes the sidebar's geometry.
#[gpui::test]
fn sidebar_scroll_preserves_height_and_has_no_other_sections(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| Reader::new(Opts::default(), window, cx));
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = reader.unwrap();
    visual.simulate_resize(size(px(1400.), px(700.)));
    visual.run_until_parked();
    view.update(visual, |v, cx| {
        v.loading = None;
        v.vault = Arc::new(Vault::from_note_paths(
            (0..300).map(|i| format!("Note {i}.md")),
        ));
        v.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
        v.panels.open(reader_layout::Panel::Notes);
        v.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
        v.sync_tree();
        v.sidebar.pinned = (0..200).map(|i| format!("Note {i}.md")).collect();
        v.sidebar.record_open("Note 0.md", 1);
        cx.notify();
    });
    visual.run_until_parked();
    let geometry = |visual: &mut gpui::VisualTestContext| {
        ["reader-notes-panel", "sidebar-header", "sidebar-tree-body"]
            .map(|selector| visual.debug_bounds(selector).expect(selector))
    };
    let before = geometry(visual);
    for gone in [
        "sidebar-header-Folders",
        "sidebar-header-Recent",
        "sidebar-header-Pinned",
        "sidebar-header-Inbox",
        "sidebar-header-Projects",
        "sidebar-body-Recent",
        "sidebar-body-Pinned",
        "sidebar-sections-toggle",
    ] {
        assert!(visual.debug_bounds(gone).is_none(), "{gone} is retired");
    }
    // #2278: Vault Index owns a fixed action row below the mode header.
    let index_header = visual.debug_bounds("tree-index-header").unwrap();
    assert_eq!(
        index_header.top(),
        visual.debug_bounds("sidebar-mode-header").unwrap().bottom()
    );
    assert_eq!(before[2].top(), index_header.bottom());
    assert!(before[2].bottom() >= before[0].bottom() - px(16.));
    view.update(visual, |v, cx| {
        v.tree_scroll
            .scroll_to_item_strict(150, ScrollStrategy::Top);
        cx.notify();
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert!(
            v.tree_scroll.0.borrow().base_handle.offset().y < px(-100.),
            "real scroll positive control"
        )
    });
    assert_eq!(geometry(visual), before);
    view.update(visual, |v, cx| {
        v.tree_scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
        cx.notify();
    });
    visual.run_until_parked();
    assert_eq!(geometry(visual), before);
}

/// Pins and recents survive a relaunch; nothing of the retired section
/// state comes back (#1508). A Folders fold saved by an older build hides
/// nothing: the root entries are always listed (#1512).
#[gpui::test]
fn sidebar_pins_survive_relaunch_and_a_saved_folders_fold_hides_nothing(
    cx: &mut gpui::TestAppContext,
) {
    use reader_sidebar::Section;
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    std::fs::create_dir(&root).unwrap();
    for i in 0..20 {
        std::fs::write(root.join(format!("Note {i:03}.md")), "# Note").unwrap();
    }
    for launch in 0..2 {
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Note 010.md".into()),
                        index_dir: Some(fixture.path().join("cache")),
                        session_directory: Some(fixture.path().join("state")),
                        panel_settings_override: Some(fixture.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = entity.unwrap();
        visual.simulate_resize(size(px(1400.), px(960.)));
        visual.run_until_parked();
        if launch == 0 {
            view.update(visual, |v, cx| {
                v.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
                v.panels.open(reader_layout::Panel::Notes);
                v.toggle_pin("Note 001.md", cx);
                v.toggle_section(Section::Folders, cx);
            });
            visual.run_until_parked();
        }
        view.read_with(visual, |v, _| {
            assert!(v.sidebar.is_pinned("Note 001.md"), "launch {launch}");
            assert!(
                v.sidebar.recent.iter().any(|(p, _)| p == "Note 010.md"),
                "launch {launch}"
            );
            assert!(
                v.sidebar_items()
                    .iter()
                    .any(|item| matches!(item, SideItem::Tree(row) if row.path == "Note 001.md")),
                "launch {launch}: the root entries stay listed"
            );
        });
        view.update_in(visual, |_, window, _| window.remove_window());
        visual.run_until_parked();
    }
}

/// A folder opened without a vault (#1512): the same one-row header, no
/// Folders row or count, and Collapse all never hides the file list.
#[gpui::test]
fn folder_view_header_actions_and_collapse_keep_the_file_list(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("folder");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("Note.md"), "# Note").unwrap();
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    open_path: Some(root.join("Note.md")),
                    session_directory: Some(temp.path().join("state")),
                    panel_settings_override: Some(temp.path().join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        entity = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = entity.unwrap();
    visual.run_until_parked();
    visual.simulate_resize(size(px(1400.), px(960.)));
    view.update(visual, |v, cx| {
        assert!(v.single_file, "exercise the non-vault launch path");
        v.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
        v.panels.open(reader_layout::Panel::Notes);
        v.panel_widths.set(reader_layout::Panel::Notes, 320.);
        v.load_quick_folder(String::new(), cx);
        cx.notify();
    });
    visual.run_until_parked();
    let visible = |visual: &mut gpui::VisualTestContext| {
        view.read_with(visual, |v, _| {
            v.sidebar_items()
                .iter()
                .any(|item| matches!(item, SideItem::Tree(_)))
        })
    };
    assert!(visible(visual), "positive control: populated file list");
    for gone in [
        "sidebar-header-Folders",
        "section-label-Folders",
        "section-count-Folders",
    ] {
        assert!(visual.debug_bounds(gone).is_none(), "{gone} is retired");
    }
    view.read_with(visual, |v, _| {
        assert_eq!(
            v.sidebar_name_menu(),
            [SidebarNameItem::SwitchVault, SidebarNameItem::RevealVault],
            "no vault, so no Vault settings"
        );
    });
    let title = visual.debug_bounds("sidebar-vault-title").unwrap();
    for action in SidebarAction::available() {
        let bounds = visual
            .debug_bounds(action.id())
            .unwrap_or_else(|| panic!("missing action {action:?}"));
        assert!(
            title.right() <= bounds.left(),
            "the name never covers {action:?}"
        );
    }
    let collapse = visual.debug_bounds("sidebar-collapse-all").unwrap();
    visual.simulate_click(collapse.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visible(visual), "Collapse all keeps the file list");
    view.read_with(visual, |v, _| {
        assert!(!v.sidebar.is_collapsed(reader_sidebar::Section::Folders))
    });
}

/// #2278/#2280: Vault Index and the four fixed actions share one row
/// below the vault/mode header. Hovering and toggling never move them;
/// there is no Folders section between that row and the root entries.
#[gpui::test]
fn sidebar_header_is_one_row_with_fixed_actions_and_no_folders_row(cx: &mut gpui::TestAppContext) {
    use reader_sidebar::Section;
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    std::fs::create_dir_all(root.join("Folder")).unwrap();
    std::fs::write(root.join("Folder/Note.md"), "# Note").unwrap();
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("Folder/Note.md".into()),
                    index_dir: Some(temp.path().join("cache")),
                    session_directory: Some(temp.path().join("state")),
                    panel_settings_override: Some(temp.path().join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        entity = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = entity.unwrap();
    visual.run_until_parked();
    visual.simulate_resize(size(px(1400.), px(960.)));
    view.update(visual, |v, cx| {
        v.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
        v.panels.open(reader_layout::Panel::Notes);
        v.panel_widths.set(reader_layout::Panel::Notes, 320.);
        cx.notify();
    });
    visual.run_until_parked();
    let ids: &[&str] = if cfg!(any(unix, windows)) {
        &[
            "sidebar-new-note",
            "sidebar-daily-note",
            "sidebar-new-folder",
            "sidebar-collapse-all",
            "sidebar-show-hidden",
        ]
    } else {
        &["sidebar-collapse-all", "sidebar-show-hidden"]
    };
    assert_eq!(
        SidebarAction::available()
            .iter()
            .map(|action| action.id())
            .collect::<Vec<_>>(),
        ids
    );
    let slots = |visual: &mut gpui::VisualTestContext| {
        ids.iter()
            .map(|id| {
                visual
                    .debug_bounds(id)
                    .unwrap_or_else(|| panic!("missing action {id}"))
            })
            .collect::<Vec<_>>()
    };
    assert!(
        visual.debug_bounds("sidebar-actions").is_none(),
        "no ⋯ at 320 px"
    );
    // Move the pointer well away from the sidebar first.
    visual.simulate_mouse_move(point(px(1000.), px(600.)), None, Modifiers::default());
    visual.run_until_parked();
    let idle = slots(visual);
    for pair in idle.windows(2) {
        assert!(pair[0].right() <= pair[1].left(), "fixed order: {idle:?}");
        assert_eq!(pair[0].size, pair[1].size, "equal slots: {idle:?}");
        assert_eq!(pair[0].center().y, pair[1].center().y, "one row: {idle:?}");
    }
    assert_eq!(idle[0].size.width, px(28.));
    // The vault menu remains above the independent Vault Index action row.
    let header = visual.debug_bounds("tree-index-header").unwrap();
    let mode_header = visual.debug_bounds("sidebar-mode-header").unwrap();
    let title = visual.debug_bounds("tree-vault-index").unwrap();
    assert_eq!(header.size.height, px(32.));
    assert_eq!(header.top(), mode_header.bottom());
    assert!(visual.debug_bounds("sidebar-vault-title").unwrap().bottom() <= header.top());
    assert!(
        title.left() <= header.left() + px(16.),
        "name is left-aligned"
    );
    assert!(title.right() <= idle[0].left());
    assert!(idle.last().unwrap().right() <= header.right());
    for bounds in idle.iter().chain([&title]) {
        assert!(bounds.top() >= header.top() && bounds.bottom() <= header.bottom());
    }
    view.read_with(visual, |v, _| {
        assert_eq!(
            v.sidebar_name_menu(),
            [
                SidebarNameItem::SwitchVault,
                SidebarNameItem::RevealVault,
                SidebarNameItem::VaultSettings,
            ]
        );
    });
    assert_eq!(
        SidebarNameItem::SwitchVault.label().as_ref(),
        "Open folder…"
    );
    assert_eq!(
        SidebarNameItem::RevealVault.label().as_ref(),
        platform::labels::Os::CURRENT.reveal_named("vault")
    );
    // No Folders row: the first root entry sits right under the header.
    for gone in [
        "sidebar-header-Folders",
        "section-label-Folders",
        "folders-collapse-all",
        "toggle-hidden-files",
    ] {
        assert!(visual.debug_bounds(gone).is_none(), "{gone} is retired");
    }
    view.read_with(visual, |v, _| {
        assert!(matches!(
            v.sidebar_items().first(),
            Some(SideItem::Tree(row)) if row.path == "Folder" && row.depth == 0
        ));
    });
    let first = visual.debug_bounds("tree-row-Folder").unwrap();
    assert!(first.top() >= header.bottom() && first.top() < header.bottom() + px(4.));
    visual.simulate_mouse_move(title.center(), None, Modifiers::default());
    visual.run_until_parked();
    assert_eq!(slots(visual), idle, "hovering the name moves nothing");
    visual.simulate_mouse_move(idle[2].center(), None, Modifiers::default());
    visual.run_until_parked();
    assert_eq!(slots(visual), idle, "hovering an action moves nothing");
    // Collapse all folds the tree; the root entries stay listed.
    let collapse = visual.debug_bounds("sidebar-collapse-all").unwrap();
    visual.simulate_click(collapse.center(), Modifiers::default());
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert!(!v.tree.rows.iter().any(|row| row.path == "Folder/Note.md"));
        assert!(v.tree.rows.iter().any(|row| row.path == "Folder"));
    });
    assert!(visual.debug_bounds("sidebar-tree-body").is_some());
    // Show hidden is a toggle that keeps its slot.
    let hidden = visual.debug_bounds("sidebar-show-hidden").unwrap();
    visual.simulate_click(hidden.center(), Modifiers::default());
    visual.run_until_parked();
    view.read_with(visual, |v, _| assert!(v.sidebar.show_hidden));
    assert_eq!(slots(visual), idle, "toggling Show hidden moves nothing");
    visual.simulate_click(hidden.center(), Modifiers::default());
    visual.run_until_parked();
    view.read_with(visual, |v, _| assert!(!v.sidebar.show_hidden));
    // The retired «Folders only» key does nothing to the sidebar now.
    view.update_in(visual, |v, window, cx| {
        v.pane
            .content
            .read(cx)
            .focus_handle()
            .clone()
            .focus(window, cx);
    });
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-shift-left"
    } else {
        "ctrl-shift-left"
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert!(!v.sidebar.is_collapsed(Section::Folders))
    });
    assert!(visual.debug_bounds("sidebar-sections-toggle").is_none());
    assert_eq!(
        std::fs::read_to_string(root.join("Folder/Note.md")).unwrap(),
        "# Note"
    );
}

/// #1756: Reveal moved to the note toolbar; only Show hidden can spill.
#[test]
fn narrow_sidebar_spills_only_show_hidden() {
    let all = &SidebarAction::ALL;
    let min = SIDEBAR_NAME_MIN;
    let slot = SIDEBAR_ACTION_SLOT;
    assert_eq!(sidebar_visible_actions(400., 120., all), 5);
    assert_eq!(sidebar_visible_actions(min + 5. * slot, 300., all), 5);
    assert_eq!(sidebar_visible_actions(min + 5. * slot - 1., 300., all), 4);
    assert_eq!(sidebar_visible_actions(60., min, all), 4);
    assert!(all[4].spills());
    assert!(!all[..4].iter().any(|action| action.spills()));
    for dot in [0., VAULT_DOT_SLOT] {
        let room = 280. - 24. - dot - 2. * SIDEBAR_NAME_GAP;
        assert_eq!(sidebar_visible_actions(room, 200., all), 5, "dot {dot}");
    }
}

/// #2216: an old/open Notes flag must not recreate a Project folder browser.
#[gpui::test]
fn project_workspace_suppresses_stale_folder_panel_and_its_layout_width(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    std::fs::create_dir_all(root.join("Work/Proj/docs")).unwrap();
    std::fs::write(root.join("Work/Proj/Plan.md"), "# Plan").unwrap();
    std::fs::write(root.join("Other.md"), "# Other").unwrap();
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("Work/Proj/Plan.md".into()),
                    index_dir: Some(temp.path().join("cache")),
                    session_directory: Some(temp.path().join("state")),
                    panel_settings_override: Some(temp.path().join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        entity = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = entity.unwrap();
    visual.simulate_resize(size(px(1400.), px(960.)));
    view.update(visual, |v, cx| {
        v.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
        v.panels.open(reader_layout::Panel::Notes);
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("sidebar-tree-body").is_some(),
        "Files positive control"
    );
    view.update(visual, |v, cx| {
        v.tree.scope_to("Work/Proj");
        v.project_navigation.active = Some(okilum_t3::projects::Project {
            folder: v.vault_root.join("Work/Proj"),
            name: "Proj".into(),
            kind: okilum_t3::projects::Kind::Code,
            environment: None,
            manager_project: None,
            manager: okilum_t3::projects::Manager::NotStarted,
            appearance: Default::default(),
        });
        // Leave Notes enabled, as a stale restore or another command can.
        assert!(v.panels.notes);
        cx.notify();
    });
    visual.run_until_parked();
    for width in [1400., 800., 600.] {
        visual.simulate_resize(size(px(width), px(960.)));
        visual.run_until_parked();
        assert!(visual.debug_bounds("reader-notes-panel").is_none());
        assert!(visual.debug_bounds("sidebar-header").is_none());
        assert!(visual.debug_bounds("sidebar-tree-body").is_none());
        assert!(visual.debug_bounds("reader-files").is_some());
        assert!(visual.debug_bounds("title-switcher").is_some());
        assert!(visual.debug_bounds("title-recent").is_some());
        assert!(visual.debug_bounds("title-pinned").is_some());
        assert!(visual.debug_bounds("reader-search").is_some());
        view.read_with(visual, |v, _| {
            assert!(v.panels.notes, "presentation must not rewrite raw state");
            assert_eq!(
                v.workspace_panels().widths(&v.panel_widths, width).notes,
                0.
            );
        });
        let document = visual.debug_bounds("reader-document").unwrap();
        assert_eq!(document.left(), px(0.), "no empty sidebar gutter");
    }
}

#[gpui::test]
fn typed_projects_do_not_reappear_as_sidebar_sections(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    let first = "Work/Projects/One/@index.md";
    let done = "Home/Projects/Done/@index.md";
    let source = "---\ntype: project\nstatus: active\n---\n# One\n- [ ] Next\n";
    for path in [first, done] {
        std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
    }
    std::fs::write(root.join(first), source).unwrap();
    std::fs::write(root.join(done), source.replace("active", "closed")).unwrap();
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some(first.into()),
                    index_dir: Some(temp.path().join("cache")),
                    session_directory: Some(temp.path().join("state")),
                    panel_settings_override: Some(temp.path().join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        entity = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = entity.unwrap();
    visual.simulate_resize(size(px(1400.), px(960.)));
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        v.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
        v.panels.open(reader_layout::Panel::Notes);
        v.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
        v.sync_tree();
        cx.notify();
        let _ = window;
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert_eq!(
            v.projects.rows().len(),
            2,
            "typed project inventory positive control"
        );
        assert!(v
            .sidebar_items()
            .iter()
            .all(|item| matches!(item, SideItem::Tree(_))));
    });
}

#[gpui::test]
fn folder_click_selects_double_click_and_open_key_open_its_index_and_the_chevron_only_toggles(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    for dir in ["Areas/apertune/specs", "Areas/plain", "Areas/other"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(root.join("Start.md"), "# Start\n").unwrap();
    std::fs::write(root.join("Areas/apertune/@index.md"), "# Apertune\n").unwrap();
    std::fs::write(root.join("Areas/apertune/Alpha.md"), "# Alpha\n").unwrap();
    std::fs::write(root.join("Areas/apertune/specs/Spec.md"), "# Spec\n").unwrap();
    std::fs::write(root.join("Areas/plain/Note.md"), "# Note\n").unwrap();
    std::fs::write(root.join("Areas/other/@Index.md"), "# Other\n").unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("Start.md".into()),
                    index_dir: Some(temp.path().join("cache")),
                    session_directory: Some(temp.path().join("state")),
                    panel_settings_override: Some(temp.path().join("panels.json")),
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
    visual.simulate_resize(size(px(1400.), px(860.)));
    visual.run_until_parked();
    view.update(visual, |v, cx| {
        v.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
        v.panels.open(reader_layout::Panel::Notes);
        cx.notify();
    });
    visual.run_until_parked();
    let row = |visual: &mut gpui::VisualTestContext, path: &str| {
        view.update_in(visual, |v, _, _| {
            v.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
            v.sync_tree();
            v.tree.rows.iter().find(|r| r.path == path).cloned()
        })
    };
    // (open note, tree selection, highlighted row), after the tree has
    // caught up with the open note.
    let state = |visual: &mut gpui::VisualTestContext| {
        view.update_in(visual, |v, _, _| {
            v.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
            v.sync_tree();
            (
                v.pane.current_rel.clone(),
                v.tree.cursor.clone(),
                v.tree_current(),
            )
        })
    };
    let click = |visual: &mut gpui::VisualTestContext, selector: &'static str| {
        view.update(visual, |_, cx| cx.notify());
        visual.run_until_parked();
        visual.update(|window, cx| window.simulate_next_frame(cx));
        let bounds = visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is rendered"));
        visual.simulate_click(bounds.center(), gpui::Modifiers::default());
        visual.run_until_parked();
    };
    // A real double click: a first click, then a second with click_count 2.
    let double_click = |visual: &mut gpui::VisualTestContext, selector: &'static str| {
        view.update(visual, |_, cx| cx.notify());
        visual.run_until_parked();
        visual.update(|window, cx| window.simulate_next_frame(cx));
        let at = visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is rendered"))
            .center();
        visual.simulate_click(at, gpui::Modifiers::default());
        visual.run_until_parked();
        visual.simulate_event(gpui::MouseDownEvent {
            position: at,
            modifiers: gpui::Modifiers::default(),
            button: gpui::MouseButton::Left,
            click_count: 2,
            first_mouse: false,
        });
        visual.simulate_event(gpui::MouseUpEvent {
            position: at,
            modifiers: gpui::Modifiers::default(),
            button: gpui::MouseButton::Left,
            click_count: 2,
        });
        visual.run_until_parked();
    };
    let some = |path: &str| Some(path.to_owned());
    let want =
        |note: &str, cursor: &str, lit: &str| (note.to_owned(), some(cursor), lit.to_owned());

    // One click on a folder only selects it: no expand, nothing opens.
    click(visual, "tree-row-Areas");
    assert!(
        !row(visual, "Areas").unwrap().expanded,
        "one click never expands"
    );
    assert_eq!(state(visual), want("Start.md", "Areas", "Start.md"));
    view.update_in(visual, |v, window, _| {
        assert!(v.tree_focus.is_focused(window))
    });

    // A double click on a folder without an index note only expands it.
    double_click(visual, "tree-row-Areas");
    assert!(row(visual, "Areas").unwrap().expanded);
    assert_eq!(state(visual), want("Start.md", "Areas", "Start.md"));

    // One click on a folder with an index: selected, still collapsed,
    // the index not opened.
    click(visual, "tree-row-Areas/apertune");
    assert!(!row(visual, "Areas/apertune").unwrap().expanded);
    assert_eq!(
        state(visual),
        want("Start.md", "Areas/apertune", "Start.md")
    );

    // A double click on it: expands, opens the index, and the folder
    // keeps the selection, the highlight and keyboard focus.
    double_click(visual, "tree-row-Areas/apertune");
    assert!(row(visual, "Areas/apertune").unwrap().expanded);
    let index = "Areas/apertune/@index.md";
    assert_eq!(
        state(visual),
        want(index, "Areas/apertune", "Areas/apertune")
    );
    view.update_in(visual, |v, window, _| {
        assert!(v.tree_focus.is_focused(window))
    });

    // The index row stays listed without Show hidden files: first in its
    // folder (above the subfolder), flagged for its muted style and icon.
    view.read_with(visual, |v, _| {
        assert!(!v.tree.show_hidden());
        let paths: Vec<_> = v.tree.rows.iter().map(|r| r.path.as_str()).collect();
        let folder = paths.iter().position(|p| *p == "Areas/apertune").unwrap();
        assert_eq!(
            paths[folder + 1..folder + 4],
            [index, "Areas/apertune/specs", "Areas/apertune/Alpha.md"]
        );
        let rows: Vec<_> = v.tree.rows.iter().filter(|r| r.index).collect();
        assert_eq!(rows.len(), 1, "only the open folder's index is listed");
        assert_eq!(rows[0].path, index);
        assert!(!rows[0].hidden);
        // Positive control: an ordinary note is not an index row.
        assert!(
            !v.tree
                .rows
                .iter()
                .find(|r| r.path == "Areas/apertune/Alpha.md")
                .unwrap()
                .index
        );
    });

    // One click on an expanded folder neither collapses it nor opens
    // its index.
    view.update_in(visual, |v, window, cx| {
        v.open_note("Start.md", None, window, cx)
    });
    visual.run_until_parked();
    click(visual, "tree-row-Areas/apertune");
    assert!(
        row(visual, "Areas/apertune").unwrap().expanded,
        "one click never collapses"
    );
    assert_eq!(
        state(visual),
        want("Start.md", "Areas/apertune", "Start.md")
    );
    // A double click on the expanded folder collapses it and opens
    // nothing; the next one expands it and opens the index.
    double_click(visual, "tree-row-Areas/apertune");
    assert!(!row(visual, "Areas/apertune").unwrap().expanded);
    assert_eq!(
        state(visual),
        want("Start.md", "Areas/apertune", "Start.md")
    );
    double_click(visual, "tree-row-Areas/apertune");
    assert!(row(visual, "Areas/apertune").unwrap().expanded);
    assert_eq!(
        state(visual),
        want(index, "Areas/apertune", "Areas/apertune")
    );

    // The chevron only toggles: nothing opens, the selection stays.
    view.update(visual, |v, _| v.tree.cursor = some("Areas/plain"));
    click(visual, "folder-disclosure-Areas/apertune");
    assert!(!row(visual, "Areas/apertune").unwrap().expanded);
    click(visual, "folder-disclosure-Areas/other");
    assert!(row(visual, "Areas/other").unwrap().expanded);
    assert_eq!(state(visual), want(index, "Areas/plain", "Areas/apertune"));
    click(visual, "folder-disclosure-Areas/apertune");
    assert!(row(visual, "Areas/apertune").unwrap().expanded);
    assert_eq!(state(visual).1, some("Areas/plain"));

    // Reveal current with the index opened through its folder: the folder.
    view.update_in(visual, |v, window, cx| v.focus_current_folder(window, cx));
    visual.run_until_parked();
    assert_eq!(state(visual).1, some("Areas/apertune"));

    // Clicking the index row itself selects and highlights that row, and
    // Reveal current keeps it.
    click(visual, "tree-row-Areas/apertune/@index.md");
    assert_eq!(state(visual), want(index, index, index));
    view.update_in(visual, |v, window, cx| v.focus_current_folder(window, cx));
    visual.run_until_parked();
    assert_eq!(state(visual), want(index, index, index));
    // One click on the folder moves only the selection; the open index
    // row keeps the highlight.
    click(visual, "tree-row-Areas/apertune");
    assert_eq!(state(visual), want(index, "Areas/apertune", index));
    // A double click collapses the folder: the index stays open and the
    // folder takes the highlight from the hidden index row.
    double_click(visual, "tree-row-Areas/apertune");
    assert!(!row(visual, "Areas/apertune").unwrap().expanded);
    assert_eq!(
        state(visual),
        want(index, "Areas/apertune", "Areas/apertune")
    );
    double_click(visual, "tree-row-Areas/apertune");
    assert!(row(visual, "Areas/apertune").unwrap().expanded);
    assert_eq!(
        state(visual),
        want(index, "Areas/apertune", "Areas/apertune")
    );
    // An index opened any other way (here a link-style open after
    // another note) stands on its folder too.
    click(visual, "tree-row-Areas/apertune/@index.md");
    assert_eq!(state(visual).1, some(index));
    view.update_in(visual, |v, window, cx| {
        v.open_note("Start.md", None, window, cx)
    });
    visual.run_until_parked();
    assert_eq!(state(visual).0, "Start.md");
    view.update_in(visual, |v, window, cx| v.open_note(index, None, window, cx));
    visual.run_until_parked();
    assert_eq!(
        state(visual),
        want(index, "Areas/apertune", "Areas/apertune")
    );

    // Keyboard: ←/→ only collapse and expand; Open (⌘↓, Ctrl+↓ elsewhere)
    // is the double click (here an index spelled `@Index.md`). Enter
    // stays Rename on a row (#1099).
    let open = if cfg!(target_os = "macos") {
        "cmd-down"
    } else {
        "ctrl-down"
    };
    click(visual, "tree-row-Areas/plain");
    assert_eq!(state(visual).1, some("Areas/plain"));
    assert!(!row(visual, "Areas/plain").unwrap().expanded);
    // Reveal current folded «Areas/other» away again.
    assert!(!row(visual, "Areas/other").unwrap().expanded);
    view.update(visual, |v, _| v.tree.cursor = some("Areas/other"));
    visual.simulate_keystrokes("right");
    visual.run_until_parked();
    assert!(row(visual, "Areas/other").unwrap().expanded);
    visual.simulate_keystrokes("left");
    visual.run_until_parked();
    assert!(!row(visual, "Areas/other").unwrap().expanded);
    visual.simulate_keystrokes("right");
    visual.run_until_parked();
    assert!(row(visual, "Areas/other").unwrap().expanded);
    assert_eq!(
        state(visual),
        want(index, "Areas/other", "Areas/apertune"),
        "arrows open nothing"
    );
    // Open on an expanded folder collapses it and opens nothing.
    visual.simulate_keystrokes(open);
    visual.run_until_parked();
    assert!(!row(visual, "Areas/other").unwrap().expanded);
    assert_eq!(state(visual), want(index, "Areas/other", "Areas/apertune"));
    // Open on a collapsed one expands it and opens its index.
    visual.simulate_keystrokes(open);
    visual.run_until_parked();
    assert!(row(visual, "Areas/other").unwrap().expanded);
    let other = "Areas/other/@Index.md";
    assert_eq!(state(visual), want(other, "Areas/other", "Areas/other"));
    // Open again collapses it; the index stays open.
    visual.simulate_keystrokes(open);
    visual.run_until_parked();
    assert!(!row(visual, "Areas/other").unwrap().expanded);
    assert_eq!(state(visual), want(other, "Areas/other", "Areas/other"));
    // From another open note, Open on the collapsed folder expands it
    // and opens its index.
    view.update_in(visual, |v, window, cx| {
        v.open_note("Start.md", None, window, cx)
    });
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        v.tree.cursor = some("Areas/other");
        v.tree_focus.focus(window, cx);
    });
    visual.simulate_keystrokes(open);
    visual.run_until_parked();
    assert!(row(visual, "Areas/other").unwrap().expanded);
    assert_eq!(state(visual), want(other, "Areas/other", "Areas/other"));
}

/// #1512 (Oleg): a double click on a folder row toggles it. A collapsed
/// folder expands and opens its index; an expanded one collapses, its
/// index stays open and the folder selected. One click only selects:
/// the first click of a double click does not wait for the second.
#[gpui::test]
fn double_click_on_a_folder_name_toggles_the_folder(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    std::fs::create_dir_all(root.join("Areas/apertune/specs")).unwrap();
    std::fs::write(root.join("Start.md"), "# Start\n").unwrap();
    std::fs::write(root.join("Areas/apertune/@index.md"), "# Apertune\n").unwrap();
    std::fs::write(root.join("Areas/apertune/Alpha.md"), "# Alpha\n").unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("Start.md".into()),
                    index_dir: Some(temp.path().join("cache")),
                    session_directory: Some(temp.path().join("state")),
                    panel_settings_override: Some(temp.path().join("panels.json")),
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
    visual.simulate_resize(size(px(1400.), px(860.)));
    visual.run_until_parked();
    view.update(visual, |v, cx| {
        v.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
        v.panels.open(reader_layout::Panel::Notes);
        cx.notify();
    });
    visual.run_until_parked();
    let expanded = |visual: &mut gpui::VisualTestContext, path: &str| {
        view.update_in(visual, |v, _, _| {
            v.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
            v.sync_tree();
            v.tree
                .rows
                .iter()
                .find(|r| r.path == path)
                .unwrap_or_else(|| panic!("{path} is listed"))
                .expanded
        })
    };
    let state = |visual: &mut gpui::VisualTestContext| {
        view.update_in(visual, |v, _, _| {
            v.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
            v.sync_tree();
            (v.pane.current_rel.clone(), v.tree.cursor.clone())
        })
    };
    let bounds = |visual: &mut gpui::VisualTestContext, selector: &'static str| {
        view.update(visual, |_, cx| cx.notify());
        visual.run_until_parked();
        visual.update(|window, cx| window.simulate_next_frame(cx));
        visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is rendered"))
            .center()
    };
    let click = |visual: &mut gpui::VisualTestContext, selector: &'static str| {
        let at = bounds(visual, selector);
        visual.simulate_click(at, gpui::Modifiers::default());
        visual.run_until_parked();
    };
    // A real double click: a first click, then a second with click_count 2.
    let double_click = |visual: &mut gpui::VisualTestContext, selector: &'static str| {
        let at = bounds(visual, selector);
        visual.simulate_click(at, gpui::Modifiers::default());
        visual.run_until_parked();
        visual.simulate_event(gpui::MouseDownEvent {
            position: at,
            modifiers: gpui::Modifiers::default(),
            button: gpui::MouseButton::Left,
            click_count: 2,
            first_mouse: false,
        });
        visual.simulate_event(gpui::MouseUpEvent {
            position: at,
            modifiers: gpui::Modifiers::default(),
            button: gpui::MouseButton::Left,
            click_count: 2,
        });
        visual.run_until_parked();
    };
    let folder = "Areas/apertune";
    let index = "Areas/apertune/@index.md";
    let opened = || (index.to_owned(), Some(folder.to_owned()));
    double_click(visual, "tree-row-Areas");
    assert!(expanded(visual, "Areas"));
    assert!(!expanded(visual, folder));

    // One click selects at once and does nothing else.
    click(visual, "tree-row-Areas/apertune");
    assert!(!expanded(visual, folder), "one click never expands");
    assert_eq!(
        state(visual),
        ("Start.md".to_owned(), Some(folder.to_owned()))
    );

    // Collapsed: a double click expands and opens the index.
    double_click(visual, "tree-row-Areas/apertune");
    assert!(expanded(visual, folder), "a collapsed folder expands");
    assert_eq!(state(visual), opened());

    // Expanded: a double click collapses; the index stays open and the
    // selection stays on the folder.
    double_click(visual, "tree-row-Areas/apertune");
    assert!(!expanded(visual, folder), "an expanded folder collapses");
    assert_eq!(state(visual), opened());
    view.update_in(visual, |v, window, _| {
        assert!(v.tree_focus.is_focused(window))
    });

    // And back again.
    double_click(visual, "tree-row-Areas/apertune");
    assert!(expanded(visual, folder));
    assert_eq!(state(visual), opened());

    // One click on the expanded folder still never collapses it.
    click(visual, "tree-row-Areas/apertune");
    assert!(expanded(visual, folder), "a single click never collapses");
    assert_eq!(state(visual), opened());

    // A folder without an index toggles the same way and opens nothing.
    double_click(visual, "tree-row-Areas");
    assert!(!expanded(visual, "Areas"));
    assert_eq!(state(visual), (index.to_owned(), Some("Areas".to_owned())));
    double_click(visual, "tree-row-Areas");
    assert!(expanded(visual, "Areas"));
}

#[gpui::test]
fn reader_folder_tree_reveals_current_and_panels_survive_selection(cx: &mut gpui::TestAppContext) {
    let state = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
        // The tree follows the open note only with auto-reveal on (#1762).
        reader_ui_state::install(state.path(), cx);
        reader_ui_state::set_tree_auto_reveal(true, cx);
    });
    let root = std::env::temp_dir().join(format!("okilum-reader348-tree-{}", uuid::Uuid::new_v4()));
    for dir in ["Work/Projects/Cafe", "Home/Empty"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(root.join("Work/Projects/Cafe/Plan.md"), "# Plan\n\nText.\n").unwrap();
    std::fs::write(root.join("Work/Projects/Cafe/Menu.md"), "# Menu\n").unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("Work/Projects/Cafe/Plan.md".into()),
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
    visual.simulate_resize(size(px(1400.), px(860.)));
    view.update_in(visual, |v, window, cx| {
        v.panel_settings = None;
        v.toggle_panel(reader_layout::Panel::Notes, window, cx);
    });
    visual.run_until_parked();
    let paths = |visual: &mut gpui::VisualTestContext| {
        view.read_with(visual, |v, _| {
            v.tree
                .rows
                .iter()
                .map(|r| r.path.clone())
                .collect::<Vec<_>>()
        })
    };
    // Real folders, current ancestry expanded, the empty folder retained.
    assert_eq!(
        paths(visual),
        [
            "Home",
            "Work",
            "Work/Projects",
            "Work/Projects/Cafe",
            "Work/Projects/Cafe/Menu.md",
            "Work/Projects/Cafe/Plan.md",
        ]
    );
    // Docked: choosing a note keeps the navigator open.
    view.update_in(visual, |v, window, cx| {
        let row = v.tree.rows[4].clone();
        v.activate_tree_row(&row, window, cx);
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert_eq!(v.pane.current_rel, "Work/Projects/Cafe/Menu.md");
        assert!(v.panels.visible(
            reader_layout::Panel::Notes,
            f32::from(v.body_bounds.size.width)
        ));
    });
    // Folder rows toggle; the expanded Home folder shows its empty child.
    view.update_in(visual, |v, window, cx| {
        let row = v.tree.rows[0].clone();
        v.activate_tree_row(&row, window, cx);
    });
    visual.run_until_parked();
    assert!(paths(visual).contains(&"Home/Empty".to_string()));
    // Automatic compact entry hides the dock; explicitly reopening it
    // creates an overlay, and choosing a note dismisses it (#677).
    visual.simulate_resize(size(px(800.), px(860.)));
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        assert!(!v.panels.visible(
            reader_layout::Panel::Notes,
            f32::from(v.body_bounds.size.width)
        ));
        v.focus_sidebar_search(window, cx);
        v.select_panel_note(
            reader_layout::Panel::Notes,
            "Work/Projects/Cafe/Plan.md",
            None,
            window,
            cx,
        );
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert_eq!(v.pane.current_rel, "Work/Projects/Cafe/Plan.md");
        assert!(!v.panels.visible(
            reader_layout::Panel::Notes,
            f32::from(v.body_bounds.size.width)
        ));
    });
    view.update_in(visual, |v, window, cx| v.focus_sidebar_search(window, cx));
    visual.run_until_parked();
    assert!(visual.debug_bounds("reader-notes-panel").is_some());
    view.update_in(visual, |v, window, cx| {
        let row = v
            .tree
            .rows
            .iter()
            .find(|r| r.path == "Work/Projects/Cafe/Menu.md")
            .unwrap()
            .clone();
        v.activate_tree_row(&row, window, cx);
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("reader-notes-panel").is_none());
    view.update_in(visual, |v, window, cx| {
        assert_eq!(v.pane.current_rel, "Work/Projects/Cafe/Menu.md");
        assert!(v.pane.content.read(cx).focus_handle().is_focused(window));
        assert!(v.panels.notes, "wide sidebar preference survives selection");
        v.open_note("Work/Projects/Cafe/Plan.md", None, window, cx);
    });
    // Breadcrumbs (#366): with the sidebar closed, a folder crumb
    // shows the tree, selects and expands the folder.
    visual.simulate_resize(size(px(1400.), px(860.)));
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        v.close_panel(reader_layout::Panel::Notes, window, cx);
        v.tree.toggle("Work/Projects");
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert!(!v.panels.visible(
            reader_layout::Panel::Notes,
            f32::from(v.body_bounds.size.width)
        ));
        assert!(!v.tree.rows.iter().any(|r| r.path == "Work/Projects/Cafe"));
    });
    view.update_in(visual, |v, window, cx| {
        v.reveal_in_tree("Work/Projects", window, cx)
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert!(v.panels.visible(
            reader_layout::Panel::Notes,
            f32::from(v.body_bounds.size.width)
        ));
        assert_eq!(v.tree.cursor.as_deref(), Some("Work/Projects"));
        assert!(v.tree.rows.iter().any(|r| r.path == "Work/Projects/Cafe"));
    });
    // The current-note crumb reveals the note itself.
    view.update_in(visual, |v, window, cx| {
        let current = v.pane.current_rel.clone();
        v.reveal_in_tree(&current, window, cx)
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert_eq!(v.tree.cursor.as_deref(), Some("Work/Projects/Cafe/Plan.md"));
    });
    // A single glyph search lives in the app header, independent of sidebar width.
    let header_button = visual.debug_bounds("sidebar-search").unwrap();
    let panel = visual.debug_bounds("reader-notes-panel").unwrap();
    assert!(header_button.bottom() <= panel.top() + px(READER_HEADER_HEIGHT));
    assert!(visual.debug_bounds("sidebar-search-shortcut").is_none());
    assert!(visual.debug_bounds("reader-close-notes-preserve").is_none());
    view.update(visual, |v, cx| {
        v.panel_widths.notes = 200.;
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("sidebar-search").is_some());
    assert!(visual.debug_bounds("sidebar-search-shortcut").is_none());
    view.update(visual, |v, cx| {
        v.panel_widths.notes = 280.;
        cx.notify();
    });
    visual.run_until_parked();
    let saved_panels = view.read_with(visual, |v, _| v.panels.clone());
    visual.simulate_resize(size(px(800.), px(860.)));
    visual.run_until_parked();
    for side in [reader_layout::Panel::Notes, reader_layout::Panel::Backlinks] {
        view.update(visual, |v, cx| {
            v.panels.open(side);
            cx.notify();
        });
        visual.run_until_parked();
        let backdrop = visual
            .debug_bounds("reader-panel-backdrop")
            .expect("overlay positive control");
        assert!(visual
            .debug_bounds("reader-close-backlinks-preserve")
            .is_none());
        visual.simulate_click(
            point(backdrop.center().x, backdrop.top() + px(80.)),
            Modifiers::default(),
        );
        visual.run_until_parked();
        view.read_with(visual, |v, _| assert!(!v.panels.visible(side, 800.)));
        view.update(visual, |v, cx| {
            v.panels.open(side);
            cx.notify();
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        view.read_with(visual, |v, _| assert!(v.panels.visible(side, 800.)));
        if side == reader_layout::Panel::Notes {
            // #773: the old chord now splits the view and no longer
            // toggles the sidebar; pressing it again unsplits.
            let split_key = if cfg!(target_os = "macos") {
                "cmd-\\"
            } else {
                "ctrl-\\"
            };
            visual.simulate_keystrokes(split_key);
            visual.run_until_parked();
            view.read_with(visual, |v, _| {
                assert!(v.panels.visible(side, 800.));
                assert!(
                    v.split.is_some(),
                    "positive control: the chord split the view"
                );
            });
            visual.simulate_keystrokes(split_key);
            visual.run_until_parked();
            view.read_with(visual, |v, _| assert!(v.split.is_none()));
        }
        visual.simulate_keystrokes(if side == reader_layout::Panel::Notes {
            NOTES_KEY
        } else {
            BACKLINKS_KEY
        });
        visual.run_until_parked();
        view.read_with(visual, |v, _| assert!(!v.panels.visible(side, 800.)));
    }
    view.update(visual, |v, cx| {
        v.panels = saved_panels;
        cx.notify();
    });
    visual.simulate_resize(size(px(1400.), px(860.)));
    visual.run_until_parked();
    // The sidebar entry opens quick open; searching never filters the tree.
    let before = paths(visual);
    let button = visual
        .debug_bounds("sidebar-search")
        .expect("search button rendered");
    visual.simulate_click(button.center(), Modifiers::default());
    view.update_in(visual, |v, window, cx| {
        assert!(v.quick_open.open);
        assert!(v
            .quick_open
            .input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window));
        v.quick_open
            .input
            .update(cx, |input, cx| input.set_value("Menu", window, cx));
    });
    visual.run_until_parked();
    assert_eq!(paths(visual), before);
    view.update_in(visual, |v, window, cx| v.close_quick_open(window, cx));
    // Keyboard activation shares the same palette; merely focusing the
    // sidebar does not open it.
    view.update_in(visual, |v, window, cx| {
        v.focus_sidebar_search(window, cx);
        assert!(!v.quick_open.open);
    });
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        assert!(v.quick_open.open);
        v.close_quick_open(window, cx);
    });
    visual.simulate_keystrokes("ctrl-k");
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        assert!(v.quick_open.open);
        v.close_quick_open(window, cx);
    });
    // Sections (#369): Recent remembers opens, pins toggle, and fresh
    // notes without incoming links are computed into Inbox.
    visual.run_until_parked();
    view.update_in(visual, |v, _, cx| {
        assert_eq!(
            v.sidebar.recent.first().map(|(p, _)| p.as_str()),
            Some("Work/Projects/Cafe/Plan.md")
        );
        assert!(v
            .sidebar
            .recent
            .iter()
            .any(|(p, _)| p == "Work/Projects/Cafe/Menu.md"));
        assert!(matches!(v.sidebar_items().first(), Some(SideItem::Tree(_))));
        v.toggle_pin("Work/Projects", cx);
        assert!(v.sidebar.is_pinned("Work/Projects"));
        let inbox: Vec<_> = v
            .inbox
            .iter()
            .map(|i| (i.path.as_str(), i.reason))
            .collect();
        assert!(
            inbox.contains(&(
                "Work/Projects/Cafe/Plan.md",
                reader_sidebar::InboxReason::NoIncomingLinks
            )),
            "fresh unlinked note in Inbox: {inbox:?}"
        );
        // A Folders fold saved by an older build hides nothing (#1512).
        v.toggle_section(reader_sidebar::Section::Folders, cx);
        assert!(v
            .sidebar_items()
            .iter()
            .any(|item| matches!(item, SideItem::Tree(..))));
        v.toggle_section(reader_sidebar::Section::Folders, cx);
    });
    // Find keeps docked panels open and Escape clears/closes it.
    visual.simulate_resize(size(px(1400.), px(860.)));
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        v.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
        v.panels.open(reader_layout::Panel::Notes);
        v.panels.open(reader_layout::Panel::Backlinks);
        let width = f32::from(v.body_bounds.size.width);
        assert!(v.panels.visible(reader_layout::Panel::Notes, width));
        v.open_find(window, cx);
        assert!(v.panels.visible(reader_layout::Panel::Notes, width));
        assert!(v.panels.visible(reader_layout::Panel::Backlinks, width));
        v.pane
            .find_input
            .update(cx, |input, cx| input.set_value("Text", window, cx));
    });
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        assert!(v.pane.find_open, "positive control: find is open");
        v.dismiss(window, cx);
        assert!(v.pane.find_open);
        assert!(v.pane.find_input.read(cx).value().is_empty());
        v.dismiss(window, cx);
        assert!(!v.pane.find_open);
    });
    view.update_in(visual, |v, window, cx| {
        // Find marks follow the find field the same way.
        v.open_find(window, cx);
        v.pane
            .find_input
            .update(cx, |input, cx| input.set_value("Text", window, cx));
        v.run_find(window, cx);
    });
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        assert!(v.pane.content.read(cx).search_status().1 > 0);
        v.pane
            .find_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        v.run_find(window, cx);
        assert_eq!(v.pane.content.read(cx).search_status().1, 0);
        v.close_find(window, cx);
    });
    let _ = std::fs::remove_dir_all(root);
}

#[gpui::test]
fn reader_compact_clamp_preserves_nonzero_document_position(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let root =
        std::env::temp_dir().join(format!("okilum-reader321-scroll-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    // With the test font this is tall at the correct document width but
    // fits a transient full-window width, exposing ListState's top clamp.
    let source = format!(
        "# Wrapped selection\n\n{}\n",
        (0..400)
            .map(|i| format!("word{i:03}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    std::fs::write(root.join("wrapped.md"), &source).unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("wrapped.md".into()),
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
    visual.simulate_resize(size(px(1600.), px(860.)));
    view.update_in(visual, |v, window, cx| {
        v.panel_settings = None;
        v.panel_widths = reader_layout::Widths {
            notes: 662.8242,
            backlinks: 355.79297,
        };
        v.toggle_panel(reader_layout::Panel::Backlinks, window, cx);
        v.toggle_panel(reader_layout::Panel::Notes, window, cx);
    });
    visual.run_until_parked();
    view.update_in(visual, |v, _, cx| {
        v.pane.content.read(cx).list_state().scroll_by(px(180.));
        cx.notify();
    });
    visual.run_until_parked();
    let before = view.read_with(visual, |v, cx| {
        let p = v.pane.content.read(cx).list_state().logical_scroll_top();
        (
            v.pane.content.entity_id(),
            v.pane.current_rel.clone(),
            v.vault_root.clone(),
            v.pane.navigation.history.clone(),
            p.item_ix,
            p.offset_in_item,
        )
    });
    assert!(
        before.4 > 0 || before.5 > px(0.),
        "nonzero position positive control"
    );
    let wide = visual.debug_bounds("reader-document").unwrap();
    visual.simulate_resize(size(px(600.), px(860.)));
    visual.run_until_parked();
    assert!(visual.debug_bounds("reader-notes-panel").is_none());
    view.update_in(visual, |v, window, cx| v.focus_sidebar_search(window, cx));
    visual.run_until_parked();
    assert_eq!(
        visual
            .debug_bounds("reader-notes-panel")
            .unwrap()
            .size
            .width,
        px(552.)
    );
    view.read_with(visual, |v, cx| {
        let position = v.pane.content.read(cx).list_state().logical_scroll_top();
        assert_eq!(
            (position.item_ix, position.offset_in_item),
            (before.4, before.5)
        );
    });
    visual.simulate_resize(size(px(1600.), px(860.)));
    visual.run_until_parked();
    let after = view.read_with(visual, |v, cx| {
        assert_eq!(v.panel_widths.notes, 662.8242);
        assert_eq!(v.panel_widths.backlinks, 355.79297);
        assert_eq!(v.pane.note_source, source);
        let p = v.pane.content.read(cx).list_state().logical_scroll_top();
        (
            v.pane.content.entity_id(),
            v.pane.current_rel.clone(),
            v.vault_root.clone(),
            v.pane.navigation.history.clone(),
            p.item_ix,
            p.offset_in_item,
        )
    });
    assert_eq!(visual.debug_bounds("reader-document").unwrap(), wide);
    assert_eq!(
        after, before,
        "settled compact roundtrip must preserve document position"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui::test]
fn reader_wrapped_selection_survives_notes_reflow_and_invalidates_on_new_input(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let root = std::env::temp_dir().join(format!("okilum-reader321-wrap-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let source = (0..600)
        .map(|ix| format!("word{ix:03} "))
        .collect::<String>();
    assert!(!source.contains('\n'), "one paragraph must wrap visually");
    std::fs::write(root.join("wrapped.md"), &source).unwrap();
    std::fs::write(
        root.join("replacement.md"),
        "Replacement document has different content.",
    )
    .unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("wrapped.md".into()),
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
    visual.simulate_resize(size(px(1366.), px(768.)));
    view.update_in(visual, |v, window, cx| {
        v.panel_settings = None;
        v.panel_widths = reader_layout::Widths::default();
        v.toggle_panel(reader_layout::Panel::Notes, window, cx);
    });
    visual.run_until_parked();
    let document = visual.debug_bounds("reader-document").unwrap();
    let origin = reader_text_origin(visual.debug_bounds("reader-column").unwrap());
    let first = origin;
    let later = origin + point(px(0.), px(70.));
    let select = |visual: &mut gpui::VisualTestContext, start: Point<Pixels>| {
        let end = start + point(px(300.), px(0.));
        visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
    };
    select(visual, first);
    let first_text = view.read_with(visual, |v, cx| v.pane.content.read(cx).selected_text());
    assert!(!first_text.trim().is_empty());
    select(visual, later);
    let selected = view.read_with(visual, |v, cx| v.pane.content.read(cx).selected_text());
    assert!(
        !selected.trim().is_empty(),
        "later wrapped-line positive control"
    );
    assert_eq!(
        source.matches(selected.trim()).count(),
        1,
        "unique selection: {selected:?}"
    );
    let selected_range = source.find(selected.trim()).unwrap()
        ..source.find(selected.trim()).unwrap() + selected.trim().len();
    assert!(
        selected_range.start > source.find(first_text.trim()).unwrap() + first_text.trim().len(),
        "selection is on a later visual line"
    );
    let entity = view.read_with(visual, |v, _| v.pane.content.entity_id());
    let anchor = visual.debug_bounds("reader-notes-splitter-anchor").unwrap();
    let start = point(anchor.left(), anchor.top() + px(140.));
    visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    view.read_with(visual, |v, cx| {
        assert_eq!(v.pane.content.read(cx).selected_text(), selected)
    });
    visual.simulate_mouse_move(
        start + point(px(35.), px(0.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    visual.simulate_mouse_move(
        start + point(px(200.), px(0.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    visual.simulate_mouse_up(
        start + point(px(200.), px(0.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("reader-document").unwrap().size.width < document.size.width - px(100.)
    );
    view.read_with(visual, |v, cx| {
        assert_eq!(
            v.pane.content.read(cx).selected_text(),
            selected,
            "settled wrapped reflow after real Notes resize"
        )
    });
    for _ in 0..2 {
        let control = visual.debug_bounds("reader-notes-preserve").unwrap();
        visual.simulate_click(control.center(), Modifiers::default());
        visual.run_until_parked();
        view.read_with(visual, |v, cx| {
            let current = v.pane.content.read(cx).selected_text();
            assert_eq!(current, selected, "settled registration after Notes toggle");
            assert_eq!(
                source.find(current.trim()).unwrap()
                    ..source.find(current.trim()).unwrap() + current.trim().len(),
                selected_range
            );
            assert_eq!(v.pane.content.entity_id(), entity);
        });
    }
    for distance in [80., -80.] {
        let before = view.update_in(visual, |v, _, cx| {
            let list = v.pane.content.read(cx).list_state();
            let before = list.logical_scroll_top();
            list.scroll_by(px(distance));
            cx.notify();
            before
        });
        visual.run_until_parked();
        view.read_with(visual, |v, cx| {
            let after = v.pane.content.read(cx).list_state().logical_scroll_top();
            assert_ne!(
                (after.item_ix, after.offset_in_item),
                (before.item_ix, before.offset_in_item),
                "scroll positive control"
            );
            assert_eq!(
                v.pane.content.read(cx).selected_text(),
                selected,
                "settled scroll projection preserves range"
            );
        });
    }
    // A new ordinary gesture must replace the settled byte range.
    let document = visual.debug_bounds("reader-column").unwrap();
    select(visual, reader_text_origin(document));
    let next = view.read_with(visual, |v, cx| v.pane.content.read(cx).selected_text());
    assert!(!next.trim().is_empty());
    assert_ne!(next, selected);
    assert_eq!(source.matches(next.trim()).count(), 1);
    view.update_in(visual, |v, window, cx| {
        v.open_note("replacement.md", None, window, cx)
    });
    visual.run_until_parked();
    view.read_with(visual, |v, cx| {
        assert!(
            v.pane.content.read(cx).selected_text().is_empty(),
            "content replacement invalidates selection"
        )
    });
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui::test]
fn reader_actual_large_heading_navigation_uses_new_parse_and_restores_back(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let root = std::env::temp_dir().join(format!("okilum-reader314-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let start = format!(
        "# Origin\n\n{}[Target](target.md#Landing)\n",
        (0..30)
            .map(|ix| format!("{ix:02} unique paragraph.\n\n"))
            .collect::<String>()
    );
    let target = format!(
        "# Target\n\n{}## Landing\n\n{}",
        "Preceding content deliberately exceeds asynchronous parsing threshold.\n\n".repeat(100),
        "Following content.\n\n".repeat(100)
    );
    assert!(target.len() > 4096);
    std::fs::write(root.join("start.md"), &start).unwrap();
    std::fs::write(root.join("target.md"), &target).unwrap();
    let mut reader = None;
    let (_root_view, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("start.md".into()),
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
    view.update_in(visual, |v, _, _| v.panel_settings = None);
    visual.run_until_parked();
    let origin = ListOffset {
        item_ix: 5,
        offset_in_item: px(7.),
    };
    view.update_in(visual, |v, _, cx| {
        v.pane.content.read(cx).list_state().scroll_to(origin);
        cx.notify();
    });
    visual.run_until_parked();
    let (old, origin) = view.update_in(visual, |v, _, cx| {
        assert!(
            v.pane.content.read(cx).list_state().item_count() > 5,
            "old-list positive control"
        );
        (
            v.pane.content.entity_id(),
            v.pane.content.read(cx).list_state().logical_scroll_top(),
        )
    });
    // The actual rendered topology, both native drag directions, selection
    // and the document identity must survive docking/overlay transitions.
    visual.simulate_resize(size(px(1366.), px(768.)));
    view.update_in(visual, |v, window, cx| {
        v.toggle_panel(reader_layout::Panel::Notes, window, cx);
        v.toggle_panel(reader_layout::Panel::Backlinks, window, cx);
    });
    visual.run_until_parked();
    let left = visual.debug_bounds("reader-notes-panel").unwrap();
    let document = visual.debug_bounds("reader-document").unwrap();
    let right = visual.debug_bounds("reader-backlinks-panel").unwrap();
    assert!(left.right() <= document.left());
    assert!(document.right() <= right.left());
    assert!(document.size.width >= px(reader_layout::DOCUMENT_MIN_WIDTH));
    // Select a measured visible paragraph, not a fixed y-coordinate that
    // can land in the inter-paragraph gap after layout changes.
    let text_start = view.read_with(visual, |v, cx| {
        let list = v.pane.content.read(cx).list_state();
        let block = list
            .bounds_for_item(list.logical_scroll_top().item_ix + 1)
            .unwrap();
        block.origin + point(px(4.), px(10.))
    });
    let text_end = text_start + point(px(85.), px(0.));
    visual.simulate_mouse_down(text_start, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(text_end, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_up(text_end, MouseButton::Left, Modifiers::default());
    visual.run_until_parked();
    let selection = view.read_with(visual, |v, cx| v.pane.content.read(cx).selected_text());
    assert!(
        !selection.is_empty() && selection.len() < 100,
        "partial-selection positive control: {selection:?}"
    );
    assert_eq!(
        start.matches(selection.trim()).count(),
        1,
        "selected substring identifies one source range, not repeated text"
    );
    // Probe both edges while the splitters are still mounted. An inside
    // press preserves the pre-existing selection; an adjacent outside
    // press must retain ordinary clearing behavior.
    for anchor in [
        "reader-notes-splitter-anchor",
        "reader-backlinks-splitter-anchor",
    ] {
        let edge = visual
            .debug_bounds(anchor)
            .expect("live splitter anchor")
            .left();
        for dx in [-3.75, 3.75] {
            let inside = point(edge + px(dx), left.bottom() - px(20.));
            visual.simulate_mouse_down(inside, MouseButton::Left, Modifiers::default());
            view.read_with(visual, |v, cx| {
                assert_eq!(
                    v.pane.content.read(cx).selected_text(),
                    selection,
                    "inside {anchor} {dx}"
                )
            });
            visual.simulate_mouse_up(inside, MouseButton::Left, Modifiers::default());
        }
        for dx in [-4.25, 4.25] {
            let outside = point(edge + px(dx), left.bottom() - px(20.));
            visual.simulate_mouse_down(outside, MouseButton::Left, Modifiers::default());
            view.read_with(visual, |v, cx| {
                assert!(
                    v.pane.content.read(cx).selected_text().is_empty(),
                    "outside live splitter {dx}"
                )
            });
            visual.simulate_mouse_up(outside, MouseButton::Left, Modifiers::default());
            // Restore the fixture viewport for the next independent
            // boundary case; ordinary outside clicks may move focus/scroll.
            view.update_in(visual, |v, _, cx| {
                v.pane.content.read(cx).list_state().scroll_to(origin);
                cx.notify();
            });
            visual.run_until_parked();
            visual.simulate_mouse_down(text_start, MouseButton::Left, Modifiers::default());
            visual.simulate_mouse_move(text_end, MouseButton::Left, Modifiers::default());
            visual.simulate_mouse_up(text_end, MouseButton::Left, Modifiers::default());
            view.read_with(visual, |v, cx| {
                assert_eq!(
                    v.pane.content.read(cx).selected_text(),
                    selection,
                    "reselect after {anchor} {dx}"
                )
            });
        }
    }
    for (panel, start, delta) in [
        (
            reader_layout::Panel::Notes,
            point(left.right() - px(2.), left.top() + px(100.)),
            70.,
        ),
        (
            reader_layout::Panel::Backlinks,
            point(right.left(), right.top() + px(100.)),
            -70.,
        ),
    ] {
        visual.simulate_mouse_move(start, None, Modifiers::default());
        visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        view.read_with(visual, |v, cx| {
            assert_eq!(
                v.pane.content.read(cx).selected_text(),
                selection,
                "after MouseDown {panel:?}"
            )
        });
        visual.simulate_mouse_move(
            start + point(px(delta / 3.), px(0.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.run_until_parked();
        visual.simulate_mouse_move(
            start + point(px(delta), px(0.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.run_until_parked();
        visual.simulate_mouse_up(
            start + point(px(delta), px(0.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.run_until_parked();
        view.read_with(visual, |v, cx| {
            assert_eq!(
                v.pane.content.read(cx).selected_text(),
                selection,
                "selection retained through actual drag"
            );
            assert!(
                v.panel_widths.get(panel) > 320.,
                "real side-specific drag: {panel:?} width {}",
                v.panel_widths.get(panel)
            )
        });
    }
    // Real mouse title-bar toggles must not clear or replace the range.
    for id in [
        "reader-notes-preserve",
        "reader-notes-preserve",
        "reader-backlinks-preserve",
        "reader-backlinks-preserve",
    ] {
        let control = visual.debug_bounds(id).expect("visible Reader control");
        visual.simulate_mouse_down(control.center(), MouseButton::Left, Modifiers::default());
        view.read_with(visual, |v, cx| {
            assert_eq!(
                v.pane.content.read(cx).selected_text(),
                selection,
                "control MouseDown {id}, {control:?}"
            )
        });
        visual.simulate_mouse_up(control.center(), MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        view.read_with(visual, |v, cx| {
            assert_eq!(
                v.pane.content.read(cx).selected_text(),
                selection,
                "mouse control {id}"
            )
        });
    }
    for width in [666., 1366., 640., 1366., 1000., 1366.] {
        visual.simulate_resize(size(px(width), px(720.)));
        visual.run_until_parked();
        let document = visual.debug_bounds("reader-document").unwrap();
        if width < 1000. {
            let state = view.read_with(visual, |v, _| (v.body_bounds, v.panels.clone()));
            assert!(
                visual.debug_bounds("reader-notes-panel").is_none(),
                "compact state: {state:?}"
            );
            assert!(visual.debug_bounds("reader-backlinks-panel").is_none());
            assert_eq!(document.left(), state.0.left());
            assert_eq!(document.right(), state.0.right());
            assert!(state.1.notes && state.1.backlinks, "wide choices retained");
        } else {
            let right = visual.debug_bounds("reader-backlinks-panel").unwrap();
            let left = visual.debug_bounds("reader-notes-panel").unwrap();
            assert!(left.right() <= document.left());
            assert!(document.right() <= right.left());
            assert!(document.size.width >= px(479.9));
        }
        view.read_with(visual, |v, cx| {
            assert_eq!(v.pane.content.entity_id(), old);
            assert_eq!(v.pane.content.read(cx).selected_text(), selection);
        });
    }
    // Switch the compact overlay to Notes and verify the opposite edge.
    visual.simulate_resize(size(px(640.), px(720.)));
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| v.focus_sidebar_search(window, cx));
    visual.run_until_parked();
    let left = visual.debug_bounds("reader-notes-panel").unwrap();
    let document = visual.debug_bounds("reader-document").unwrap();
    assert_eq!(left.left(), document.left());
    assert!(visual.debug_bounds("reader-backlinks-panel").is_none());
    view.read_with(visual, |v, cx| {
        assert_eq!(v.pane.content.read(cx).selected_text(), selection)
    });
    visual.simulate_resize(size(px(1366.), px(768.)));
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        v.close_panel(reader_layout::Panel::Notes, window, cx);
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("reader-notes-panel").is_none());
    assert!(visual.debug_bounds("reader-backlinks-panel").is_some());
    view.read_with(visual, |v, cx| {
        assert_eq!(v.pane.content.read(cx).selected_text(), selection)
    });
    // Lose the release of a control gesture; a subsequent ordinary press
    // must still clear selection after the panel has been dismissed.
    let close = visual.debug_bounds("reader-backlinks-preserve").unwrap();
    visual.simulate_mouse_down(close.center(), MouseButton::Left, Modifiers::default());
    view.read_with(visual, |v, cx| {
        assert_eq!(v.pane.content.read(cx).selected_text(), selection)
    });
    view.update_in(visual, |v, window, cx| {
        v.close_panel(reader_layout::Panel::Backlinks, window, cx);
        v.panel_widths = reader_layout::Widths::default();
        v.pane.content.read(cx).list_state().scroll_to(origin);
    });
    visual.run_until_parked();
    // Removed controls leave no stale exemption. An ordinary document
    // MouseDown at the former splitter position still clears selection.
    visual.simulate_click(
        point(left.right() - px(2.), left.top() + px(100.)),
        Modifiers::default(),
    );
    visual.run_until_parked();
    view.read_with(visual, |v, cx| {
        assert!(
            v.pane.content.read(cx).selected_text().is_empty(),
            "ordinary document click clears selection"
        )
    });
    // #321: toggling UI panels must not replace the parsed document or its
    // navigation/scroll state. Closing must not leave focus in a hidden control.
    let history_before = view.read_with(visual, |v, _| v.pane.navigation.history.clone());
    view.update_in(visual, |v, window, cx| {
        v.toggle_panel(reader_layout::Panel::Notes, window, cx);
    });
    visual.run_until_parked();
    view.update_in(visual, |v, window, _| {
        assert!(v.sidebar_search_focus.is_focused(window));
        assert_eq!(v.pane.content.entity_id(), old);
        assert_eq!(v.pane.navigation.history, history_before);
    });
    for (width, height) in [(640., 720.), (1366., 768.), (640., 720.)] {
        visual.simulate_resize(size(px(width), px(height)));
        visual.run_until_parked();
        if width < reader_layout::DOCK_MIN_WIDTH {
            view.update_in(visual, |v, window, cx| {
                assert!(!v.panels.visible(reader_layout::Panel::Notes, width));
                assert!(!v.sidebar_search_focus.is_focused(window));
                assert!(v.pane.content.read(cx).focus_handle().is_focused(window));
                v.focus_sidebar_search(window, cx);
            });
            visual.run_until_parked();
        }
        view.update_in(visual, |v, window, _| {
            assert_eq!(v.pane.content.entity_id(), old);
            assert_eq!(v.pane.navigation.history, history_before);
            assert!(v.panels.visible(
                reader_layout::Panel::Notes,
                f32::from(v.body_bounds.size.width)
            ));
            assert!(v.sidebar_search_focus.is_focused(window));
        });
    }
    // Drive the production drag handle, not a direct state setter.
    let preference = root.with_extension("layout.json");
    let handle = view.update_in(visual, |v, _, _| {
        v.panel_settings = Some(preference.clone());
        point(
            v.body_bounds.left()
                + px(reader_layout::displayed_width(
                    v.panel_widths.notes,
                    f32::from(v.body_bounds.size.width),
                ))
                - px(2.),
            v.body_bounds.top() + px(100.),
        )
    });
    visual.simulate_mouse_move(handle, None, Modifiers::default());
    visual.simulate_mouse_down(handle, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(
        handle + point(px(20.), px(0.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    visual.run_until_parked();
    visual.simulate_mouse_move(
        handle + point(px(80.), px(0.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    visual.run_until_parked();
    visual.simulate_mouse_up(
        handle + point(px(80.), px(0.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    visual.run_until_parked();
    view.update_in(visual, |v, _, _| {
        assert!(
            v.panel_widths.notes > reader_layout::DEFAULT_PANEL_WIDTH + 40.,
            "real drag changed width"
        );
        assert!(v.resizing_panel.is_none());
        assert_eq!(
            reader_layout::Widths::load(&preference).notes,
            v.panel_widths.notes
        );
        assert_eq!(v.pane.content.entity_id(), old);
        assert_eq!(v.pane.navigation.history, history_before);
    });
    // Close during a real drag must not let ordinary motion resize a
    // reopened/different panel after its release was lost.
    let handle = view.read_with(visual, |v, _| {
        point(
            v.body_bounds.left() + px(v.panel_widths.notes) - px(2.),
            v.body_bounds.top() + px(100.),
        )
    });
    visual.simulate_mouse_down(handle, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(
        handle + point(px(20.), px(0.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        assert!(v.resizing_panel.is_some(), "active drag positive control");
        #[cfg(target_os = "linux")]
        assert!(
            window.captured_hitbox().is_some(),
            "native capture positive control"
        );
        v.close_panel(reader_layout::Panel::Notes, window, cx);
        assert!(v.resizing_panel.is_none());
        v.toggle_panel(reader_layout::Panel::Backlinks, window, cx);
    });
    visual.run_until_parked();
    visual.simulate_mouse_move(point(px(100.), px(300.)), None, Modifiers::default());
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        assert_eq!(v.panel_widths.backlinks, reader_layout::DEFAULT_PANEL_WIDTH);
        assert!(v.resizing_panel.is_none());
        #[cfg(target_os = "linux")]
        assert!(
            window.captured_hitbox().is_none(),
            "closing a panel releases its pointer"
        );
        cx.stop_active_drag(window);
        v.toggle_panel(reader_layout::Panel::Notes, window, cx);
    });
    visual.run_until_parked();
    std::fs::remove_file(preference).unwrap();
    visual.simulate_keystrokes(NOTES_KEY);
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        assert_eq!(
            v.panels.dismiss_target(f32::from(v.body_bounds.size.width)),
            reader_layout::Panel::Closed
        );
        assert!(v.pane.content.read(cx).focus_handle().is_focused(window));
        assert_eq!(v.pane.content.entity_id(), old);
        let position = v.pane.content.read(cx).list_state().logical_scroll_top();
        assert_eq!(position.item_ix, origin.item_ix);
        assert_eq!(position.offset_in_item, origin.offset_in_item);
        v.toggle_panel(reader_layout::Panel::Backlinks, window, cx);
    });
    visual.run_until_parked();
    visual.simulate_keystrokes(crate::BACKLINKS_KEY);
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        assert!(v.pane.content.read(cx).focus_handle().is_focused(window));
        assert_eq!(v.pane.content.entity_id(), old);
        assert_eq!(v.pane.navigation.history, history_before);
        v.open_note_at("target.md", None, Some("Landing"), window, cx);
        assert_eq!(
            v.pane.content.entity_id(),
            old,
            "pending preparation keeps the current document"
        );
    });
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(300));
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        assert_eq!(v.pane.current_rel, "target.md");
        assert_eq!(
            v.pane
                .content
                .read(cx)
                .list_state()
                .logical_scroll_top()
                .item_ix,
            101
        );
        v.open_note_at("start.md", None, Some("Missing"), window, cx);
        assert_eq!(
            v.pane.current_rel, "target.md",
            "missing heading preserves document"
        );
    });
    visual.run_until_parked();
    view.update_in(visual, |v, _, _| {
        assert_eq!(v.pane.current_rel, "target.md");
        assert!(v.link_notice.is_some());
    });
    visual.simulate_keystrokes("alt-left");
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(300));
    visual.run_until_parked();
    view.update_in(visual, |v, _, cx| {
        assert_eq!(v.pane.current_rel, "start.md");
        let actual = v.pane.content.read(cx).list_state().logical_scroll_top();
        assert_eq!(actual.item_ix, origin.item_ix);
        assert_eq!(actual.offset_in_item, origin.offset_in_item);
    });
    // A later user focus change during parsing must survive the landing retry.
    view.update_in(visual, |v, window, cx| {
        v.open_note_at("target.md", None, Some("Landing"), window, cx);
        v.focus_sidebar_search(window, cx);
    });
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(300));
    visual.run_until_parked();
    view.update_in(visual, |v, window, _| {
        assert!(v.sidebar_search_focus.is_focused(window));
    });
    // Selecting from a compact sidebar dismisses it (#677) and leaves
    // immediate history shortcuts live, with no search/focus workaround.
    let before = view.read_with(visual, |v, _| {
        v.panels.dismiss_target(f32::from(v.body_bounds.size.width))
    });
    assert_eq!(before, reader_layout::Panel::Notes, "positive control");
    view.update_in(visual, |v, window, cx| {
        v.select_panel_note(reader_layout::Panel::Notes, "start.md", None, window, cx);
    });
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        assert_eq!(
            v.panels.dismiss_target(f32::from(v.body_bounds.size.width)),
            reader_layout::Panel::Closed
        );
        assert!(v.pane.content.read(cx).focus_handle().is_focused(window));
    });
    visual.simulate_keystrokes("alt-left");
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(300));
    visual.run_until_parked();
    view.update_in(visual, |v, _, cx| {
        assert_eq!(v.pane.current_rel, "target.md");
        assert_eq!(
            v.pane
                .content
                .read(cx)
                .list_state()
                .logical_scroll_top()
                .item_ix,
            101
        );
    });
    visual.simulate_keystrokes("alt-right");
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(300));
    visual.run_until_parked();
    view.update_in(visual, |v, _, _| assert_eq!(v.pane.current_rel, "start.md"));
    visual.simulate_keystrokes("ctrl-k");
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        assert!(v.quick_open.open);
        assert!(v
            .quick_open
            .input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window));
    });
    #[cfg(not(target_os = "macos"))]
    visual.simulate_keystrokes("ctrl-f");
    #[cfg(target_os = "macos")]
    visual.simulate_keystrokes("cmd-f");
    visual.run_until_parked();
    view.update_in(visual, |v, window, cx| {
        assert_eq!(
            v.panels.dismiss_target(f32::from(v.body_bounds.size.width)),
            reader_layout::Panel::Closed
        );
        assert!(v.pane.find_open);
        assert!(v
            .pane
            .find_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window));
    });
    #[cfg(target_os = "macos")]
    {
        view.update_in(visual, |v, window, cx| v.focus_sidebar_search(window, cx));
        visual.simulate_keystrokes("cmd-f");
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert_eq!(
                v.panels.dismiss_target(f32::from(v.body_bounds.size.width)),
                reader_layout::Panel::Closed
            );
            assert!(v.pane.find_open);
            assert!(v
                .pane
                .find_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window));
        });
    }
    assert_eq!(
        std::fs::read(root.join("start.md")).unwrap(),
        start.as_bytes()
    );
    std::fs::remove_dir_all(root).unwrap();
}
