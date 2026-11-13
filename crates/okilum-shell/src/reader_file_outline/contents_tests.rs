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
        .map(|row| match row {
            tree::Row::Node { index, depth, .. } => format!(
                "{}{} {}",
                "  ".repeat(depth - 1),
                tree::row_key(doc, *index),
                tree::row_value(doc, *index)
            ),
            tree::Row::Group { depth, .. } => {
                let (key, value) = tree::group_label(doc, row);
                format!("{}[{key}] {value}", "  ".repeat(depth - 1))
            }
        })
        .collect()
}

fn find_row(doc: &Document, rows: &[tree::Row], key: &str) -> tree::Row {
    rows.iter()
        .find(
            |row| matches!(row, tree::Row::Node { index, .. } if tree::row_key(doc, *index) == key),
        )
        .cloned()
        .unwrap_or_else(|| panic!("row {key}"))
}

/// Open a node by its visible row, as the chevron does.
fn open(doc: &Document, expansion: &mut tree::Expansion, key: &str) {
    let row = find_row(doc, &tree::visible_rows(doc, expansion), key);
    if !expansion.row_open(&row) {
        expansion.toggle(&row);
    }
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
        open(&d, &mut expansion, "a");
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
        expansion.toggle(&rows[1]);
        let opened = tree::visible_rows(&d, &expansion);
        assert!(
            opened.len() - rows.len() <= tree::GROUP,
            "{count}: no click adds more than 100 rows"
        );
    }
    let keys: Vec<String> = (0..150).map(|i| format!("\"/v1/k{i:03}\":{i}")).collect();
    let d = doc(&format!("{{\"paths\":{{{}}}}}", keys.join(",")));
    let mut expansion = tree::Expansion::default();
    open(&d, &mut expansion, "paths");
    let shown = labels(&d, &tree::visible_rows(&d, &expansion));
    assert_eq!(shown[1], "  [/v1/k000 … /v1/k099] 1–100");
    assert_eq!(shown[2], "  [/v1/k100 … /v1/k149] 101–150");
}

#[test]
fn contents2814_group_labels_cost_no_child_walk() {
    let items: Vec<String> = (0..200_000).map(|i| i.to_string()).collect();
    let d = doc(&format!("{{\"a\":[{}]}}", items.join(",")));
    let mut expansion = tree::Expansion::default();
    open(&d, &mut expansion, "a");
    let rows = tree::visible_rows(&d, &expansion);
    assert_eq!(rows.len(), 1 + 20, "positive: 20 groups of 10,000");
    d.child_steps.store(0, std::sync::atomic::Ordering::Relaxed);
    let shown = labels(&d, &rows);
    assert_eq!(shown[20], "  [190,000 … 199,999] ");
    assert_eq!(
        d.child_steps.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "drawing group rows walks no children"
    );
}

#[test]
fn contents2814_children_of_an_opened_group_start_closed() {
    let objects: Vec<String> = (0..150)
        .map(|i| {
            let fields: Vec<String> = (0..20).map(|f| format!("\"f{f}\":{i}")).collect();
            format!("{{{}}}", fields.join(","))
        })
        .collect();
    let d = doc(&format!("{{\"a\":[{}]}}", objects.join(",")));
    let mut expansion = tree::Expansion::default();
    open(&d, &mut expansion, "a");
    let rows = tree::visible_rows(&d, &expansion);
    expansion.toggle(&rows[1]);
    let opened = tree::visible_rows(&d, &expansion);
    assert_eq!(
        opened.len() - rows.len(),
        100,
        "one click adds exactly the group's 100 rows"
    );
}

#[test]
fn contents2814_repeated_keys_open_and_close_independently() {
    let d = doc("{\"x\":{\"a\":1},\"x\":{\"b\":2}}");
    let mut expansion = tree::Expansion::default();
    let rows = tree::visible_rows(&d, &expansion);
    assert_eq!(labels(&d, &rows), ["x 1 key", "  a 1", "x 1 key", "  b 2"]);
    expansion.toggle(&rows[0]);
    assert_eq!(
        labels(&d, &tree::visible_rows(&d, &expansion)),
        ["x 1 key", "x 1 key", "  b 2"],
        "closing the first x leaves the second one open"
    );
}

#[test]
fn contents2814_expansion_is_keyed_by_path_and_survives_a_rebuild() {
    let before = doc("{\"a\":{\"b\":{\"c\":{\"d\":1}}},\"z\":1}");
    let mut expansion = tree::Expansion::default();
    open(&before, &mut expansion, "c");
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
                // No rows (Table view, or no structure): only the heading and
                // its empty line remain; data files have no «Linked from» (#2885).
                view.read_with(visual, |v, _| assert!(!v.has_entries(), "{file}"));
                assert!(visual.debug_bounds("file-outline-list").is_none(), "{file}");
                assert!(
                    visual.debug_bounds("file-linked-from-heading").is_none(),
                    "{file}"
                );
            }
        }
    }
}

#[gpui::test]
fn contents2814_rows_and_their_document_change_together(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let (view, visual) = cx.add_window_view(|_, _| View::new());
    let first = doc("{\"a\":1,\"b\":2}");
    view.update(visual, |v, cx| {
        v.snapshot.active = true;
        v.apply_source("a.json".into(), ContentsSource::Ready(first), cx);
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| assert_eq!(shown(v), ["a 1", "b 2"]));
    // A rebuilt document whose rows have not been computed yet.
    let second = doc("{\"new\":{\"x\":1},\"a\":1,\"b\":2}");
    view.update(visual, |v, cx| {
        v.apply_source("a.json".into(), ContentsSource::Ready(second), cx);
        assert_eq!(shown(v), ["a 1", "b 2"], "old rows keep their own document");
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert_eq!(shown(v), ["new 1 key", "  x 1", "a 1", "b 2"])
    });
}

#[gpui::test]
fn contents2814_reading_timer_restarts_for_each_file(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let (view, visual) = cx.add_window_view(|_, _| View::new());
    view.update(visual, |v, cx| {
        v.snapshot.active = true;
        v.apply_source("a.json".into(), ContentsSource::Loading, cx);
    });
    visual.executor().advance_clock(Duration::from_millis(200));
    visual.run_until_parked();
    view.update(visual, |v, cx| {
        v.apply_source("b.json".into(), ContentsSource::Loading, cx)
    });
    visual.executor().advance_clock(Duration::from_millis(150));
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert!(!v.has_entries(), "b.json has waited only 150 ms")
    });
    visual.executor().advance_clock(Duration::from_millis(200));
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert!(v.has_entries(), "b.json after its own 300 ms")
    });
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
    view.update(visual, |v, cx| v.toggle_snapshot_row(&row, cx));
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

#[gpui::test]
fn contents2814_a_click_on_old_rows_reaches_the_same_node_in_a_newer_document(
    cx: &mut TestAppContext,
) {
    // The viewer already holds the newer parse; the shown rows still index
    // the older one (their rebuild has not landed yet).
    let (reader, visual, _root) = reader_with(
        cx,
        &[("live.json", "{\"new\":{\"x\":1},\"a\":1,\"b\":2}")],
        "live.json",
    );
    let code = code(&reader, visual);
    let old = doc("{\"a\":1,\"b\":2}");
    let b = (0..old.structure.len())
        .find(|&i| old.key_text(i) == Some("b"))
        .unwrap();
    code.update_in(visual, |p, w, cx| p.set_view(CodeView::Tree, w, cx));
    settle(visual);
    code.update_in(visual, |p, w, cx| p.reveal_contents_node(&old, b, w, cx));
    code.read_with(visual, |p, _| {
        let raw = p.document_snapshot().unwrap();
        let selected = p.tree_selection_for_tests().unwrap();
        assert_ne!(
            selected, b,
            "positive: the index differs in the newer document"
        );
        assert_eq!(
            raw.key_text(selected),
            Some("b"),
            "the clicked node, not its old index"
        );
    });
    // Repeated keys keep their occurrence; a vanished node is ignored.
    let first = doc("{\"x\":1,\"x\":2}");
    let second = doc("{\"y\":0,\"x\":1,\"x\":2}");
    let twice = (0..first.structure.len())
        .filter(|&i| first.key_text(i) == Some("x"))
        .nth(1)
        .unwrap();
    let mapped = first.node_in(twice, &second).unwrap();
    assert_eq!(second.scalar(mapped).unwrap().1, "2");
    assert_eq!(doc("{\"gone\":1}").node_in(1, &doc("{\"other\":1}")), None);
}

#[gpui::test]
fn linked2885_data_files_hide_linked_from_and_notes_keep_it(cx: &mut TestAppContext) {
    let (reader, visual, _root) = reader_with(
        cx,
        &[
            ("note.md", "# Note\n\nText.\n"),
            ("data.json", "{\"a\":1}"),
            ("feed.xml", "<feed><title>x</title></feed>"),
            ("api.har", "{\"log\":{\"version\":\"1.2\",\"entries\":[]}}"),
            ("gateway.log", "2026-11-10T12:00:00Z INFO ready\n"),
        ],
        "note.md",
    );
    assert!(
        visual.debug_bounds("right-linked-from-heading").is_some(),
        "a Markdown note keeps «Linked from»"
    );
    for (file, keeps) in [
        ("data.json", false),
        ("feed.xml", false),
        ("api.har", false),
        ("gateway.log", false),
    ] {
        reader.update_in(visual, |r, w, cx| r.open_note(file, None, w, cx));
        settle(visual);
        reader.read_with(visual, |r, _| assert_eq!(r.selected_file(), file));
        assert_eq!(
            visual.debug_bounds("file-linked-from-heading").is_some(),
            keeps,
            "{file}: «Linked from» only where it can have backlinks"
        );
        if !keeps {
            assert!(
                visual.debug_bounds("file-contents-heading").is_some(),
                "{file}: Contents takes the panel"
            );
        }
    }
}

#[gpui::test]
fn linked2885_a_linked_data_file_keeps_linked_from(cx: &mut TestAppContext) {
    let (reader, visual, _root) = reader_with(
        cx,
        &[
            ("note.md", "# Note\n\nSee data.json for the numbers.\n"),
            ("data.json", "{\"a\":1}"),
            ("other.json", "{\"b\":2}"),
        ],
        "data.json",
    );
    // The vault's link graph resolves note targets only today, so a link to a
    // JSON file is not in `Vault::backlinks`; the panel rule is exercised with
    // the Reader's backlink state as the graph would set it.
    reader.update(visual, |r, cx| {
        r.set_backlinks(
            vec![okilum_core::vault::Backlink {
                path: "note.md".into(),
                title: "Note".into(),
                context: "See data.json for the numbers.".into(),
                link: "data.json".into(),
                ambiguous: false,
                property: None,
            }],
            true,
        );
        cx.notify();
    });
    settle(visual);
    assert!(visual.debug_bounds("file-contents-heading").is_some());
    assert!(
        visual.debug_bounds("file-linked-from-heading").is_some(),
        "a linked JSON file shows «Linked from»"
    );
    assert!(
        visual.debug_bounds("backlinks-list").is_some(),
        "with its note"
    );
    reader.update_in(visual, |r, w, cx| r.open_note("other.json", None, w, cx));
    settle(visual);
    reader.read_with(visual, |r, _| {
        assert!(r.backlinks.is_empty(), "positive: unlinked")
    });
    assert!(visual.debug_bounds("file-contents-heading").is_some());
    assert!(
        visual.debug_bounds("file-linked-from-heading").is_none(),
        "an unlinked JSON shows only Contents"
    );
}

#[gpui::test]
fn contents2814_yaml_toml_xml_rows_states_and_where_you_are(cx: &mut TestAppContext) {
    let compose = format!(
        "version: \"3.9\"\nservices:\n  gateway:\n    image: okilum/gateway\n    ports:\n      - \"8080:80\"\n      - \"8443:443\"\n  accounts:\n    image: okilum/accounts\n{}volumes: {{}}\n",
        "  # padding\n".repeat(200)
    );
    let (reader, visual, _root) = reader_with(
        cx,
        &[
            ("docker-compose.yml", compose.as_str()),
            ("Cargo.toml", "[package]\nname = \"a\"\n\n[[bin]]\nname = \"x\"\n\n[[bin]]\nname = \"y\"\n"),
            ("feed.xml", "<feed>\n  <title>Release notes</title>\n  <entry id=\"r-0.9.4\"><title>x</title></entry>\n</feed>\n"),
            ("broken.yaml", "a: [1, 2\nb: 3\n"),
            ("broken.toml", "[a]\nb = 1\nc = \n[d]\ne = 2\n"),
        ],
        "docker-compose.yml",
    );
    let view = outline(&reader, visual);
    view.read_with(visual, |v, _| {
        let rows = shown(v);
        assert_eq!(rows[0], "version \"3.9\"");
        assert!(rows.contains(&"services 2 keys".to_owned()), "{rows:?}");
        assert!(rows.iter().any(|r| r.trim() == "ports 2 items"), "{rows:?}");
    });
    // Where you are: the viewer's top line is in «accounts»; the mark sits on it.
    let code = code(&reader, visual);
    let accounts = view.read_with(visual, |v, _| {
        let doc = v.snapshot.doc.clone().unwrap();
        (0..doc.structure.len())
            .find(|&i| doc.key_text(i) == Some("accounts"))
            .unwrap()
    });
    view.update_in(visual, |v, w, cx| v.jump_snapshot(accounts, w, cx));
    settle(visual);
    visual.update(|w, cx| w.draw(cx).clear(cx));
    settle(visual);
    view.read_with(visual, |v, _| {
        let doc = v.snapshot.doc.clone().unwrap();
        let row = v.snapshot.current.expect("a marked row");
        let tree::Row::Node { index, .. } = &v.snapshot.rows[row] else {
            panic!("node row")
        };
        assert_eq!(
            doc.key_text(*index),
            Some("accounts"),
            "the clicked node is where you are"
        );
    });
    code.read_with(visual, |p, _| {
        assert_eq!(p.active_view(), CodeView::Raw, "no view switch")
    });
    for (file, first) in [
        ("Cargo.toml", "package 1 key"),
        ("feed.xml", "feed 2 elements"),
    ] {
        reader.update_in(visual, |r, w, cx| r.open_note(file, None, w, cx));
        settle(visual);
        view.read_with(visual, |v, _| assert_eq!(shown(v)[0], first, "{file}"));
    }
    view.read_with(visual, |v, _| {
        let rows = shown(v);
        assert!(
            rows.contains(&"  title Release notes".to_owned()),
            "{rows:?}"
        );
        assert!(rows.contains(&"  entry r-0.9.4".to_owned()), "{rows:?}");
    });
    reader.update_in(visual, |r, w, cx| r.open_note("Cargo.toml", None, w, cx));
    settle(visual);
    view.read_with(visual, |v, _| {
        assert!(shown(v).contains(&"bin 2 items".to_owned()))
    });
    reader.update_in(visual, |r, w, cx| r.open_note("broken.yaml", None, w, cx));
    settle(visual);
    view.read_with(visual, |v, _| {
        assert_eq!(
            v.snapshot.state,
            SnapshotState::Invalid("No outline: the file is not valid YAML.")
        )
    });
    reader.update_in(visual, |r, w, cx| r.open_note("broken.toml", None, w, cx));
    settle(visual);
    view.read_with(visual, |v, _| {
        assert_eq!(
            v.snapshot.state,
            SnapshotState::Ready,
            "TOML has no verdict"
        );
        assert!(
            shown(v).iter().any(|r| r.starts_with("d ")),
            "the tolerant rows stay"
        );
    });
}

/// The document index of top-level key `key`.
fn key_index(view: &Entity<View>, visual: &VisualTestContext, key: &str) -> usize {
    view.read_with(visual, |v, _| {
        let doc = v.snapshot.doc.clone().unwrap();
        (0..doc.structure.len())
            .find(|&i| doc.key_text(i) == Some(key))
            .unwrap()
    })
}

fn where_you_are(view: &Entity<View>, visual: &mut VisualTestContext, index: usize) -> tree::Row {
    view.update_in(visual, |v, w, cx| v.jump_snapshot(index, w, cx));
    settle(visual);
    visual.update(|w, cx| w.draw(cx).clear(cx));
    settle(visual);
    view.read_with(visual, |v, _| {
        let row = v.snapshot.current.expect("a marked row");
        v.snapshot.rows[row].clone()
    })
}

#[gpui::test]
fn contents2814_where_you_are_inside_a_closed_group_marks_the_group(cx: &mut TestAppContext) {
    let text: String = (0..250).map(|i| format!("k{i:03}: {i}\n")).collect();
    let (reader, visual, _root) = reader_with(cx, &[("keys.yaml", text.as_str())], "keys.yaml");
    let view = outline(&reader, visual);
    let k050 = key_index(&view, visual, "k050");
    view.read_with(visual, |v, _| {
        assert!(
            v.snapshot
                .rows
                .iter()
                .all(|row| matches!(row, tree::Row::Group { parent: 0, .. })),
            "positive: 250 top-level keys list only groups"
        )
    });
    // Plain scrolling: the closed group holding the top node carries the
    // mark and nothing opens (a caret move would open the path).
    let code = code(&reader, visual);
    let tree::Row::Group { first, last, .. } = scrolled_to(&view, &code, visual, 50) else {
        panic!("the closed group holding k050 carries the mark")
    };
    assert_eq!((first, last), (0, 99));
    let tree::Row::Group { first, last, .. } = scrolled_to(&view, &code, visual, 150) else {
        panic!("the closed group holding k150 carries the mark")
    };
    assert_eq!((first, last), (100, 199));
    // Opened, the group lists the node itself, and the node takes the mark.
    let group = view.read_with(visual, |v, _| v.snapshot.rows[0].clone());
    view.update(visual, |v, cx| v.toggle_snapshot_row(&group, cx));
    settle(visual);
    let tree::Row::Node { index, .. } = scrolled_to(&view, &code, visual, 50) else {
        panic!("k050 is listed once its group is open")
    };
    assert_eq!(index, k050);
}

#[gpui::test]
fn contents2814_a_newer_parse_costs_viewport_samples_no_child_walk(cx: &mut TestAppContext) {
    let keys: Vec<String> = (0..20_000).map(|i| format!("\"k{i:05}\":{i}")).collect();
    let text = format!("{{\n{}\n}}\n", keys.join(",\n"));
    let (reader, visual, _root) = reader_with(cx, &[("big.json", text.as_str())], "big.json");
    let view = outline(&reader, visual);
    let near_end = key_index(&view, visual, "k19990");
    let clicked = where_you_are(&view, visual, near_end);
    assert!(
        matches!(clicked, tree::Row::Node { index, .. } if index == near_end),
        "positive: the click opened the groups and marks the node near the end, \
         though the jump's scroll lands a frame later (clamped at the end)"
    );
    // The viewer now holds a newer parse than the shown rows (same text, a
    // separate document): exactly the state while new rows are built.
    let raw = code(&reader, visual)
        .read_with(visual, |p, _| p.document_raw_for_contents())
        .expect("the viewer's document");
    let shown_doc = doc(&text);
    assert!(!Arc::ptr_eq(&raw, &shown_doc));
    let before = view.update(visual, |v, cx| {
        v.snapshot.doc = Some(shown_doc.clone());
        let before = v.snapshot.current;
        raw.child_steps
            .store(0, std::sync::atomic::Ordering::Relaxed);
        shown_doc
            .child_steps
            .store(0, std::sync::atomic::Ordering::Relaxed);
        for _ in 0..30 {
            v.sync_current(cx);
        }
        before
    });
    let steps = raw.child_steps.load(std::sync::atomic::Ordering::Relaxed)
        + shown_doc
            .child_steps
            .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(steps, 0, "30 viewport samples walked {steps} children");
    view.read_with(visual, |v, _| {
        assert_eq!(
            v.snapshot.current, before,
            "the mark waits for the new rows"
        )
    });
}

fn paint(visual: &mut VisualTestContext) {
    visual.update(|w, cx| w.draw(cx).clear(cx));
    settle(visual);
}

fn marked(view: &Entity<View>, visual: &VisualTestContext) -> tree::Row {
    view.read_with(visual, |v, _| {
        let row = v.snapshot.current.expect("a marked row");
        v.snapshot.rows[row].clone()
    })
}

fn marked_key(view: &Entity<View>, visual: &VisualTestContext) -> String {
    view.read_with(visual, |v, _| {
        let doc = v.snapshot.doc.clone().unwrap();
        let row = v.snapshot.current.expect("a marked row");
        match &v.snapshot.rows[row] {
            tree::Row::Node { index, .. } => doc.key_text(*index).unwrap_or("").to_owned(),
            tree::Row::Group { .. } => "<group>".to_owned(),
        }
    })
}

/// Scroll the viewer's text so `line` is on top; the caret does not move.
fn scrolled_to(
    view: &Entity<View>,
    code: &Entity<reader_code_file::CodePreview>,
    visual: &mut VisualTestContext,
    line: usize,
) -> tree::Row {
    let input = code.read_with(visual, |p, _| p.input().unwrap());
    input.update(visual, |input, cx| input.scroll_line_to_top(line, cx));
    paint(visual);
    marked(view, visual)
}

fn caret_at(
    code: &Entity<reader_code_file::CodePreview>,
    visual: &mut VisualTestContext,
    at: usize,
) {
    let input = code.read_with(visual, |p, _| p.input().unwrap());
    input.update(visual, |input, cx| input.set_selected_range(at..at, cx));
    paint(visual);
}

fn listed(view: &Entity<View>, visual: &VisualTestContext, key: &str) -> bool {
    view.read_with(visual, |v, _| {
        shown(v)
            .iter()
            .any(|row| row.trim().starts_with(&format!("{key} ")))
    })
}

/// Lines: 1-30 `a00`-`a29`, 31 `big` (31 children, so it starts closed),
/// 32-61 `c00`-`c29`, 62 `target`, 64-123 `p00`-`p59`. 92 top-level keys:
/// no groups at the top.
fn caret_fixture() -> String {
    let mut text = String::from("{\n");
    for i in 0..30 {
        text += &format!("  \"a{i:02}\": {i},\n");
    }
    text += "  \"big\": {\n";
    for i in 0..30 {
        text += &format!("    \"c{i:02}\": {i},\n");
    }
    text += "    \"target\": 99\n  },\n";
    for i in 0..60 {
        text += &format!("  \"p{i:02}\": {i},\n");
    }
    text += "  \"tail\": 0\n}\n";
    text
}

fn node_start(view: &Entity<View>, visual: &VisualTestContext, key: &str) -> usize {
    view.read_with(visual, |v, _| {
        let doc = v.snapshot.doc.clone().unwrap();
        let index = (0..doc.structure.len())
            .find(|&i| doc.key_text(i) == Some(key))
            .unwrap();
        doc.structure[index].range.start
    })
}

#[gpui::test]
fn contents2814_a_caret_move_opens_the_path_and_scrolling_never_opens(cx: &mut TestAppContext) {
    let text = caret_fixture();
    let (reader, visual, _root) = reader_with(cx, &[("caret.json", text.as_str())], "caret.json");
    let view = outline(&reader, visual);
    let code = code(&reader, visual);
    paint(visual);
    assert!(
        !listed(&view, visual, "target"),
        "positive: big starts closed"
    );
    // Scrolling: the closed ancestor carries the mark; nothing opens.
    scrolled_to(&view, &code, visual, 62);
    assert_eq!(
        marked_key(&view, visual),
        "big",
        "scrolled into big: big carries the mark"
    );
    assert!(
        !listed(&view, visual, "target"),
        "scrolling never opens rows"
    );
    // A caret move: the path opens and the node itself is marked.
    let target = node_start(&view, visual, "target");
    caret_at(&code, visual, target);
    assert!(listed(&view, visual, "target"), "the caret opened big");
    assert_eq!(marked_key(&view, visual), "target");
    view.read_with(visual, |v, _| {
        let row = v.snapshot.current.unwrap();
        // In pixels: the whole row lies inside the viewport.
        let top = v.list.logical_scroll_top();
        let height = v.list.viewport_bounds().size.height.as_f32();
        let row_height = OUTLINE_ROW_HEIGHT;
        let viewport_top = top.item_ix as f32 * row_height + top.offset_in_item.as_f32();
        let (row_top, row_bottom) = (row as f32 * row_height, (row + 1) as f32 * row_height);
        assert!(
            row > 40,
            "positive: the row starts below a first screen of rows ({row})"
        );
        assert!(
            viewport_top <= row_top && row_bottom <= viewport_top + height + 0.5,
            "the row is on screen: rows {row_top}..{row_bottom} in {viewport_top}..{}",
            viewport_top + height
        );
    });
    // Scrolling again: the top node leads; opened rows stay open.
    scrolled_to(&view, &code, visual, 69);
    assert_eq!(
        marked_key(&view, visual),
        "p05",
        "the top of the view after scrolling"
    );
    assert!(
        listed(&view, visual, "target"),
        "scrolling never closes rows either"
    );
    // Selection wins over scroll when both change in one step.
    let c05 = node_start(&view, visual, "c05");
    let input = code.read_with(visual, |p, _| p.input().unwrap());
    input.update(visual, |input, cx| {
        input.set_selected_range(c05..c05, cx);
        input.scroll_line_to_top(90, cx);
    });
    paint(visual);
    assert_eq!(
        marked_key(&view, visual),
        "c05",
        "selection wins over scroll"
    );
}

#[gpui::test]
fn contents2814_find_hits_and_tree_selection_become_where_you_are(cx: &mut TestAppContext) {
    let text = caret_fixture();
    let (reader, visual, _root) = reader_with(cx, &[("find.json", text.as_str())], "find.json");
    let view = outline(&reader, visual);
    let code = code(&reader, visual);
    paint(visual);
    let find = |visual: &mut VisualTestContext, query: &str| {
        let query = query.to_owned();
        let code = code.clone();
        visual.update(move |w, cx| {
            let target = code.read(cx).find_target(code.clone(), cx);
            target.set_query(&query, w, cx);
        });
        settle(visual);
        paint(visual);
    };
    // Raw: the Find hit becomes the current row and its path opens.
    find(visual, "target");
    assert!(listed(&view, visual, "target"), "the Find hit opened big");
    assert_eq!(marked_key(&view, visual), "target");
    // Tree: close big in Contents, then a Find hit selects a Tree row.
    let big = view.read_with(visual, |v, _| {
        find_row(&v.snapshot.doc.clone().unwrap(), &v.snapshot.rows, "big")
    });
    view.update(visual, |v, cx| v.toggle_snapshot_row(&big, cx));
    settle(visual);
    assert!(!listed(&view, visual, "c07"), "positive: big closed again");
    code.update_in(visual, |p, w, cx| p.set_view(CodeView::Tree, w, cx));
    settle(visual);
    paint(visual);
    find(visual, "c07");
    assert!(listed(&view, visual, "c07"), "the Tree Find hit opened big");
    assert_eq!(marked_key(&view, visual), "c07");
    // A click in Contents selects the Tree row, and the mark follows it.
    let c12 = key_index(&view, visual, "c12");
    view.update_in(visual, |v, w, cx| v.jump_snapshot(c12, w, cx));
    settle(visual);
    paint(visual);
    code.read_with(visual, |p, _| {
        assert_eq!(p.tree_selection_for_tests(), Some(c12))
    });
    assert_eq!(marked_key(&view, visual), "c12");
}

#[gpui::test]
fn contents2814_a_page_jump_is_scrolling_and_a_click_on_another_page_is_a_caret_move(
    cx: &mut TestAppContext,
) {
    // One line over 64 KB: Raw shows it in pages.
    let keys: Vec<String> = (0..20_000).map(|i| format!("\"k{i:05}\":{i}")).collect();
    let text = format!("{{{}}}", keys.join(","));
    let (reader, visual, _root) = reader_with(cx, &[("line.json", text.as_str())], "line.json");
    let view = outline(&reader, visual);
    let code = code(&reader, visual);
    code.update_in(visual, |p, w, cx| p.set_view(CodeView::Raw, w, cx));
    settle(visual);
    paint(visual);
    // A click on a row whose node is on the last page places the caret there.
    let near_end = key_index(&view, visual, "k19990");
    assert!(
        matches!(where_you_are(&view, visual, near_end), tree::Row::Node { index, .. } if index == near_end),
        "a caret placed on another page is a caret move"
    );
    // The position scrubber loads page 1 with the caret at its start.
    let pages = code.update_in(visual, |p, w, cx| p.show_page_for_tests(1, w, cx));
    assert!(pages > 2, "positive: the fixture is paged ({pages})");
    paint(visual);
    let tree::Row::Group { first, .. } = marked(&view, visual) else {
        panic!("page 1's first node sits in the closed first group: nothing opened")
    };
    assert_eq!(first, 0);
}

/// Frames without moving the clock: a caret move's own scroll lands, and a
/// scroll right after it happens inside any time window.
fn paint_now(visual: &mut VisualTestContext) {
    for _ in 0..3 {
        visual.update(|w, cx| w.draw(cx).clear(cx));
        visual.run_until_parked();
    }
}

#[gpui::test]
fn contents2814_scrolling_right_after_a_caret_move_follows_the_top(cx: &mut TestAppContext) {
    let text = caret_fixture();
    let (reader, visual, _root) = reader_with(cx, &[("soon.json", text.as_str())], "soon.json");
    let view = outline(&reader, visual);
    let code = code(&reader, visual);
    paint_now(visual);
    let input = code.read_with(visual, |p, _| p.input().unwrap());
    let a10 = node_start(&view, visual, "a10");
    input.update(visual, |input, cx| input.set_selected_range(a10..a10, cx));
    paint_now(visual);
    assert_eq!(
        marked_key(&view, visual),
        "a10",
        "positive: the caret leads"
    );
    // The user scrolls at once, with no time passing.
    input.update(visual, |input, cx| input.scroll_line_to_top(100, cx));
    paint_now(visual);
    assert_eq!(
        marked_key(&view, visual),
        "p36",
        "a scroll right after a caret move is scrolling"
    );
}

#[gpui::test]
fn contents2814_a_repeated_jump_after_scrolling_away_marks_the_node_again(cx: &mut TestAppContext) {
    let text = caret_fixture();
    let (reader, visual, _root) = reader_with(cx, &[("again.json", text.as_str())], "again.json");
    let view = outline(&reader, visual);
    let code = code(&reader, visual);
    paint(visual);
    // Near the end the jump's scroll is clamped: the top line is not p58's.
    let p58 = key_index(&view, visual, "p58");
    where_you_are(&view, visual, p58);
    assert_eq!(
        marked_key(&view, visual),
        "p58",
        "positive: the jump marks p58"
    );
    scrolled_to(&view, &code, visual, 69);
    assert_eq!(marked_key(&view, visual), "p05", "positive: scrolled away");
    // The same jump again leaves the cursor where it already was.
    where_you_are(&view, visual, p58);
    assert_eq!(
        marked_key(&view, visual),
        "p58",
        "a repeated jump is a caret move"
    );
}

#[gpui::test]
fn contents2814_find_hits_at_the_same_local_offset_on_adjacent_pages(cx: &mut TestAppContext) {
    // 16 bytes per member after `{`: node i starts at 1 + 16i, so nodes 2050
    // and 4098 sit at the same offset within pages 1 and 2 (32 KiB pages).
    let keys: Vec<String> = (0..20_000)
        .map(|i| format!("\"k{i:06}\":{}", 10_000 + i))
        .collect();
    let text = format!("{{{}}}", keys.join(","));
    let (reader, visual, _root) = reader_with(cx, &[("pages.json", text.as_str())], "pages.json");
    let view = outline(&reader, visual);
    let code = code(&reader, visual);
    code.update_in(visual, |p, w, cx| p.set_view(CodeView::Raw, w, cx));
    settle(visual);
    paint(visual);
    let (first, second) = (
        node_start(&view, visual, "k002050"),
        node_start(&view, visual, "k004098"),
    );
    assert_eq!(
        (first / 32_768, second / 32_768, first % 32_768),
        (1, 2, second % 32_768),
        "positive: the same local offset on adjacent pages"
    );
    let find = |visual: &mut VisualTestContext, query: &str| {
        let query = query.to_owned();
        let code = code.clone();
        visual.update(move |w, cx| {
            let target = code.read(cx).find_target(code.clone(), cx);
            target.set_query(&query, w, cx);
        });
        settle(visual);
        paint(visual);
    };
    find(visual, "k002050");
    assert_eq!(
        marked_key(&view, visual),
        "k002050",
        "positive: the first hit"
    );
    find(visual, "k004098");
    assert_eq!(
        marked_key(&view, visual),
        "k004098",
        "the hit on the next page is a caret move"
    );
}

#[gpui::test]
fn contents2814_a_reveal_through_a_huge_container_costs_the_ui_thread_no_child_walk(
    cx: &mut TestAppContext,
) {
    let keys: Vec<String> = (0..50_000)
        .map(|i| format!("    \"k{i:05}\": {i}"))
        .collect();
    let text = format!("{{\n  \"big\": {{\n{}\n  }}\n}}\n", keys.join(",\n"));
    let (reader, visual, _root) = reader_with(cx, &[("huge.json", text.as_str())], "huge.json");
    let view = outline(&reader, visual);
    let code = code(&reader, visual);
    paint(visual);
    let start = node_start(&view, visual, "k49990");
    let doc = view.read_with(visual, |v, _| v.snapshot.doc.clone().unwrap());
    let input = code.read_with(visual, |p, _| p.input().unwrap());
    input.update(visual, |input, cx| {
        input.set_selected_range(start..start, cx)
    });
    // The sample after the caret move, before any worker runs.
    let steps = view.update(visual, |v, cx| {
        doc.child_steps
            .store(0, std::sync::atomic::Ordering::Relaxed);
        v.sync_current(cx);
        doc.child_steps.load(std::sync::atomic::Ordering::Relaxed)
    });
    assert_eq!(
        steps, 0,
        "the UI thread walked {steps} children to open the path"
    );
    paint(visual);
    assert!(
        listed(&view, visual, "k49990"),
        "the worker opened the path"
    );
    assert_eq!(marked_key(&view, visual), "k49990");
}
