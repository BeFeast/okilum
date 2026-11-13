//! Contents from the viewer's tree (#2814 9a): rows, groups, expansion,
//! states and navigation in every view.
use super::*;
use ::core::prelude::v1::test;
use reader_code_file::View as CodeView;

fn doc(text: &str) -> Arc<Document> {
    crate::source_presentation::code_languages::install();
    Arc::new(Document::for_file(text.to_owned(), "json", true))
}

fn labels(doc: &Document, rows: &[tree::Row]) -> Vec<String> {
    rows.iter()
        .map(|row| match *row {
            tree::Row::Node { index, depth } => format!(
                "{}{} {}",
                "  ".repeat(depth - 1),
                tree::row_key(doc, index),
                tree::row_value(doc, index)
            ),
            tree::Row::Group {
                parent,
                first,
                last,
                depth,
            } => {
                let (key, value) = tree::group_label(doc, parent, first, last);
                format!("{}[{key}] {value}", "  ".repeat(depth - 1))
            }
        })
        .collect()
}

fn find_row(doc: &Document, rows: &[tree::Row], key: &str) -> tree::Row {
    *rows
        .iter()
        .find(
            |row| matches!(row, tree::Row::Node { index, .. } if tree::row_key(doc, *index) == key),
        )
        .unwrap_or_else(|| panic!("row {key}"))
}

#[test]
fn contents2814_two_levels_open_counts_short_values_and_plain_ordinals() {
    let routes: Vec<String> = (0..26).map(|i| format!("{{\"id\":{i}}}")).collect();
    let d = doc(&format!(
        "{{\"upstreams\":{{\"accounts\":{{\"retry\":{{\"count\":3}},\"tls\":true}}}},\"routes\":[{}],\"name\":\"a fairly long gateway name\",\"empty\":{{}},\"list\":[\"x\",null]}}",
        routes.join(",")
    ));
    let rows = tree::visible_rows(&d, &tree::Expansion::default());
    assert_eq!(
        labels(&d, &rows),
        [
            "upstreams 1 key",
            "  accounts 2 keys",
            "    retry 1 key",
            "    tls true",
            "routes 26 items",
            "name \"a fairly long g…",
            "empty { }",
            "list 2 items",
            "  0 \"x\"",
            "  1 null",
        ],
        "two levels open; more than 20 children start closed; 0 not [0]"
    );
    let retry = (0..d.structure.len())
        .find(|&i| d.key_text(i) == Some("retry"))
        .unwrap();
    assert_eq!(tree::row_tooltip(&d, retry), "upstreams › accounts › retry");
}

#[test]
fn contents2814_groups_of_100_and_10000_with_a_bounded_click() {
    for (count, groups) in [(101, 2), (1204, 13), (10_001, 2)] {
        let items: Vec<String> = (0..count).map(|i| i.to_string()).collect();
        let d = doc(&format!("{{\"a\":[{}]}}", items.join(",")));
        let mut expansion = tree::Expansion::default();
        let a = (0..d.structure.len())
            .find(|&i| d.key_text(i) == Some("a"))
            .unwrap();
        expansion.set(tree::path_key(&d, a), true, tree::default_open(&d, a));
        let rows = tree::visible_rows(&d, &expansion);
        assert_eq!(
            rows.len(),
            1 + groups,
            "{count}: one group row per 100 (10,000)"
        );
        let shown = labels(&d, &rows);
        if count == 1204 {
            assert_eq!(shown[1], "  [0 … 99] ");
            assert_eq!(shown[2], "  [100 … 199] ");
            assert_eq!(shown[13], "  [1,200 … 1,203] ");
        }
        // Opening any group adds at most 100 rows.
        let tree::Row::Group {
            parent,
            first,
            last,
            ..
        } = rows[1]
        else {
            panic!("group row");
        };
        expansion.set(tree::group_key(&d, parent, first, last), true, false);
        let opened = tree::visible_rows(&d, &expansion);
        assert!(
            opened.len() - rows.len() <= tree::GROUP,
            "{count}: no click adds more than 100 rows"
        );
    }
    let keys: Vec<String> = (0..150).map(|i| format!("\"/v1/k{i:03}\":{i}")).collect();
    let d = doc(&format!("{{\"paths\":{{{}}}}}", keys.join(",")));
    let mut expansion = tree::Expansion::default();
    let paths = (0..d.structure.len())
        .find(|&i| d.key_text(i) == Some("paths"))
        .unwrap();
    expansion.set(tree::path_key(&d, paths), true, false);
    let shown = labels(&d, &tree::visible_rows(&d, &expansion));
    assert_eq!(shown[1], "  [/v1/k000 … /v1/k099] 1–100");
    assert_eq!(shown[2], "  [/v1/k100 … /v1/k149] 101–150");
}

#[test]
fn contents2814_expansion_is_keyed_by_path_and_survives_a_rebuild() {
    let before = doc("{\"a\":{\"b\":{\"c\":{\"d\":1}}},\"z\":1}");
    let mut expansion = tree::Expansion::default();
    let c = find_row(&before, &tree::visible_rows(&before, &expansion), "c");
    let tree::Row::Node { index, .. } = c else {
        panic!()
    };
    expansion.set(
        tree::path_key(&before, index),
        true,
        tree::default_open(&before, index),
    );
    assert!(labels(&before, &tree::visible_rows(&before, &expansion))
        .iter()
        .any(|row| row.trim() == "d 1"));
    // A rebuilt document with a new key in front: indices move, paths do not.
    let after = doc("{\"new\":0,\"a\":{\"b\":{\"c\":{\"d\":1}}},\"z\":1}");
    assert!(labels(&after, &tree::visible_rows(&after, &expansion))
        .iter()
        .any(|row| row.trim() == "d 1"));
}

fn reader_with<'a>(
    cx: &'a mut TestAppContext,
    files: &[(&str, &str)],
    open: &str,
) -> (Entity<Reader>, &'a mut VisualTestContext, tempfile::TempDir) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let root = tempfile::tempdir().unwrap();
    for (name, text) in files {
        std::fs::write(root.path().join(name), text).unwrap();
    }
    let open = open.to_owned();
    let path = root.path().to_path_buf();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut reader = Reader::new(
                Opts {
                    vault: Some(path.clone()),
                    note: Some(open.clone()),
                    index_dir: Some(path.join(".index")),
                    ..Default::default()
                },
                window,
                cx,
            );
            reader.panels.open(reader_layout::Panel::Backlinks);
            reader
        });
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1400.), px(800.)));
    settle(visual);
    (reader.unwrap(), visual, root)
}

fn settle(visual: &mut VisualTestContext) {
    for _ in 0..3 {
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(350));
        visual.run_until_parked();
    }
}

fn outline(reader: &Entity<Reader>, visual: &VisualTestContext) -> Entity<View> {
    reader.read_with(visual, |reader, cx| {
        reader.side_panels[1]
            .as_ref()
            .unwrap()
            .read(cx)
            .file_outline
            .clone()
            .unwrap()
    })
}

fn code(
    reader: &Entity<Reader>,
    visual: &VisualTestContext,
) -> Entity<reader_code_file::CodePreview> {
    reader.read_with(visual, |r, _| {
        r.file_preview.as_ref().unwrap().code.clone().unwrap()
    })
}

fn shown(view: &View) -> Vec<String> {
    let doc = view.snapshot.doc.clone().expect("snapshot rows");
    labels(&doc, &view.snapshot.rows)
}

#[gpui::test]
fn contents2814_json_rows_states_and_absent_table(cx: &mut TestAppContext) {
    let long = format!(
        "{{\"head\":\"{}\",\"tail\":{{\"x\":1}}}}",
        "x".repeat(300_000)
    );
    let (reader, visual, _root) = reader_with(
        cx,
        &[
            ("config.json", "{\"first\":{\"child\":true},\"second\":2}"),
            ("big.json", &long),
            ("bad.json", "{\"a\":1,}"),
            ("array.json", "[{\"a\":1},{\"a\":2}]"),
            ("plain.txt", "text\n"),
        ],
        "config.json",
    );
    let view = outline(&reader, visual);
    assert!(visual.debug_bounds("file-contents-heading").is_some());
    view.read_with(visual, |v, _| {
        assert_eq!(shown(v), ["first 1 key", "  child true", "second 2"])
    });
    for (file, expect) in [
        ("big.json", Some("rows")),
        ("bad.json", Some("invalid")),
        ("array.json", None),
        ("plain.txt", None),
    ] {
        reader.update_in(visual, |r, w, cx| r.open_note(file, None, w, cx));
        settle(visual);
        match expect {
            Some("rows") => {
                assert!(
                    visual.debug_bounds("file-contents-heading").is_some(),
                    "{file}"
                );
                view.read_with(visual, |v, _| {
                    assert_eq!(
                        shown(v)[..2],
                        [
                            "head \"xxxxxxxxxxxxxxx…".to_owned(),
                            "tail 1 key".to_owned()
                        ],
                        "a long-line file gets Contents"
                    )
                });
            }
            Some(_) => {
                assert!(
                    visual.debug_bounds("file-contents-heading").is_some(),
                    "{file}"
                );
                view.read_with(visual, |v, _| {
                    assert_eq!(
                        v.snapshot.state,
                        SnapshotState::Invalid("No outline: the file is not valid JSON.")
                    )
                });
                assert!(visual.debug_bounds("file-outline-state").is_some());
            }
            None => {
                assert!(
                    visual.debug_bounds("file-contents-heading").is_none(),
                    "{file}: no section (Table view, or no structure)"
                );
                assert!(visual.debug_bounds("file-linked-from-heading").is_some());
            }
        }
    }
}

#[gpui::test]
fn contents2814_reading_line_waits_300_ms(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let (view, visual) = cx.add_window_view(|_, _| View::new());
    view.update(visual, |v, cx| {
        v.snapshot.active = true;
        v.apply_source("a.json".into(), ContentsSource::Loading, cx);
        assert!(!v.has_entries(), "nothing before 300 ms");
    });
    visual.executor().advance_clock(Duration::from_millis(250));
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert!(!v.has_entries(), "still nothing at 250 ms")
    });
    visual.executor().advance_clock(Duration::from_millis(100));
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert!(
            v.has_entries(),
            "heading and «Reading structure…» after about 300 ms"
        );
        assert_eq!(v.snapshot.state, SnapshotState::Loading { shown: true });
    });
}

#[gpui::test]
fn contents2814_click_goes_to_the_node_in_every_view_without_switching(cx: &mut TestAppContext) {
    let source = "{\n  \"a\": {\n    \"b\": {\n      \"c\": 1\n    }\n  },\n  \"z\": 2\n}\n";
    let (reader, visual, _root) = reader_with(cx, &[("nav.json", source)], "nav.json");
    let view = outline(&reader, visual);
    let code = code(&reader, visual);
    let c = view.read_with(visual, |v, _| {
        let doc = v.snapshot.doc.clone().unwrap();
        (0..doc.structure.len())
            .find(|&i| doc.key_text(i) == Some("c"))
            .unwrap()
    });
    for target in [CodeView::Raw, CodeView::Tree, CodeView::Formatted] {
        code.update_in(visual, |p, w, cx| p.set_view(target, w, cx));
        settle(visual);
        view.update_in(visual, |v, w, cx| v.jump_snapshot(c, w, cx));
        settle(visual);
        code.read_with(visual, |p, cx| {
            assert_eq!(p.active_view(), target, "the click never switches the view");
            let doc = p.document_snapshot().unwrap();
            if target == CodeView::Tree {
                assert_eq!(
                    p.tree_selection_for_tests(),
                    Some(c),
                    "Tree selects the node"
                );
            } else {
                let input = p.input().unwrap().read(cx);
                let caret = input.selected_range();
                let start = doc.structure[c].range.start;
                assert_eq!(caret, start..start, "{target:?}: caret at the node start");
            }
        });
    }
}

#[gpui::test]
fn contents2814_open_rows_survive_view_switch_and_panel_close(cx: &mut TestAppContext) {
    let _cache = reader_side_panel::tests::CachedPanels::on();
    let source = "{\"a\":{\"b\":{\"c\":{\"d\":1}}},\"z\":1}";
    let (reader, visual, _root) = reader_with(cx, &[("keep.json", source)], "keep.json");
    let view = outline(&reader, visual);
    let row = view.read_with(visual, |v, _| {
        find_row(v.snapshot.doc.as_ref().unwrap(), &v.snapshot.rows, "c")
    });
    view.update(visual, |v, cx| v.toggle_snapshot_row(row, cx));
    settle(visual);
    let has_d = |visual: &mut VisualTestContext| {
        view.read_with(visual, |v, _| shown(v).iter().any(|r| r.trim() == "d 1"))
    };
    assert!(has_d(visual), "positive: the chevron opened c");
    let code = code(&reader, visual);
    code.update_in(visual, |p, w, cx| p.set_view(CodeView::Raw, w, cx));
    settle(visual);
    assert!(has_d(visual), "a view switch keeps open rows");
    reader.update(visual, |r, cx| {
        r.panels.close(reader_layout::Panel::Backlinks);
        cx.notify();
    });
    settle(visual);
    reader.update(visual, |r, cx| {
        r.panels.open(reader_layout::Panel::Backlinks);
        cx.notify();
    });
    settle(visual);
    assert!(
        has_d(visual),
        "closing and reopening the panel keeps open rows"
    );
}
