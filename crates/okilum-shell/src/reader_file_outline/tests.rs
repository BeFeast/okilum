use super::*;
use ::core::prelude::v1::test;
use gpui_component::input::{Editor, Position};

fn find<'a>(outline: &'a parse::Outline, label: &str) -> &'a parse::Entry {
    outline
        .entries
        .iter()
        .find(|entry| entry.label == label)
        .unwrap_or_else(|| panic!("missing {label:?} in {outline:?}"))
}

fn parent_label<'a>(outline: &'a parse::Outline, entry: &parse::Entry) -> Option<&'a str> {
    entry.parent.map(|ix| outline.entries[ix].label.as_str())
}

#[test]
fn yaml_keys_keep_source_lines_nesting_and_scalar_boundaries() {
    let outline = parse::parse("yaml", include_str!("fixtures/config.yaml"));
    assert_eq!(find(&outline, "provenance").line, 1);
    assert_eq!(
        parent_label(&outline, find(&outline, "kind")),
        Some("management_home")
    );
    assert_eq!(
        parent_label(&outline, find(&outline, "nested")),
        Some("שלום")
    );
    assert_eq!(
        parent_label(&outline, find(&outline, "debug")),
        Some("flags")
    );
    assert_eq!(parent_label(&outline, find(&outline, "name")), Some("[0]"));
    assert!(!outline
        .entries
        .iter()
        .any(|entry| entry.label == "not_a_key"));
}

#[test]
fn yaml_comments_do_not_consume_sequence_indices() {
    for source in [
        "items: [one, # comment\n two]\n",
        "items:\n  - one\n  # comment\n  - two\n",
    ] {
        let outline = parse::parse("yaml", source);
        assert_eq!(parent_label(&outline, find(&outline, "[0]")), Some("items"));
        assert_eq!(parent_label(&outline, find(&outline, "[1]")), Some("items"));
        assert!(!outline.entries.iter().any(|entry| entry.label == "[2]"));
    }
}

#[test]
fn json_keys_arrays_and_escaped_labels_use_displayed_source_lines() {
    let source = include_str!("fixtures/config.json");
    let outline = parse::parse("json", source);
    assert_eq!(find(&outline, "kind").line, 3);
    assert_eq!(
        parent_label(&outline, find(&outline, "enabled")),
        Some("nested")
    );
    assert_eq!(
        parent_label(&outline, find(&outline, "[0]")),
        Some("targets")
    );
    assert_eq!(parent_label(&outline, find(&outline, "name")), Some("[0]"));
    assert_eq!(find(&outline, "escaped key").line, 10);
    let raw =
        serde_json::to_string(&serde_json::from_str::<serde_json::Value>(source).unwrap()).unwrap();
    let raw_outline = parse::parse("json", &raw);
    assert!(!raw_outline.entries.is_empty());
    assert!(raw_outline.entries.iter().all(|entry| entry.line == 0));
}

#[test]
fn toml_tables_inline_tables_and_repeated_table_arrays_keep_their_keys() {
    let outline = parse::parse("toml", include_str!("fixtures/config.toml"));
    assert_eq!(find(&outline, "management_home").line, 1);
    assert_eq!(
        parent_label(&outline, find(&outline, "kind")),
        Some("management_home")
    );
    assert_eq!(
        parent_label(&outline, find(&outline, "enabled")),
        Some("inline")
    );
    assert_eq!(
        parent_label(&outline, find(&outline, "model")),
        Some("t3.manager")
    );
    assert_eq!(
        outline
            .entries
            .iter()
            .filter(|entry| entry.label == "targets")
            .count(),
        2
    );
}

#[test]
fn rust_symbols_group_impls_and_traits_without_scanning_strings_or_comments() {
    let outline = parse::parse("rust", include_str!("fixtures/symbols.rs.txt"));
    assert_eq!(find(&outline, "struct Service").line, 0);
    assert_eq!(
        parent_label(&outline, find(&outline, "fn run")),
        Some("impl Service")
    );
    assert_eq!(
        parent_label(&outline, find(&outline, "fn stop")),
        Some("trait Stop")
    );
    assert_eq!(find(&outline, "fn main").line, 7);
    assert!(!outline
        .entries
        .iter()
        .any(|entry| entry.label.contains("not_a_symbol")));
}

#[test]
fn python_symbols_include_decorated_classes_methods_and_async_functions() {
    let outline = parse::parse("python", include_str!("fixtures/symbols.py.txt"));
    assert_eq!(find(&outline, "class Service").line, 2);
    assert_eq!(find(&outline, "def name").line, 4);
    assert_eq!(
        parent_label(&outline, find(&outline, "def run")),
        Some("class Service")
    );
    assert_eq!(find(&outline, "def main").line, 10);
    assert!(!outline
        .entries
        .iter()
        .any(|entry| entry.label.contains("not_a_symbol")));
}

#[test]
fn malformed_yaml_is_partial_or_empty_and_never_an_error() {
    // Positive control: the same parser finds actual YAML keys.
    assert!(!parse::parse("yaml", "valid:\n  child: true\n")
        .entries
        .is_empty());
    let source = include_str!("fixtures/malformed.yaml");
    let outline = parse::parse("yaml", source);
    for entry in &outline.entries {
        assert!(entry.line < source.lines().count());
        assert!(matches!(
            entry.label.as_str(),
            "valid" | "child" | "broken" | "after" | "[0]" | "[1]"
        ));
    }
    for language in ["yaml", "json", "toml", "rust", "python"] {
        assert!(parse::parse(language, "").entries.is_empty());
    }
    for (lang, source) in [
        ("text", "key: value"),
        ("csv", "a,b\n1,2"),
        ("yaml", "just ordinary words"),
    ] {
        assert!(parse::parse(lang, source).entries.is_empty());
    }
}

#[test]
fn parsing_and_tree_expansion_are_capped() {
    let now = std::time::Instant::now();
    assert!(
        parse::parse_with_clock("json", &" ".repeat(parse::MAX_BYTES + 1), || now)
            .entries
            .is_empty()
    );
    // A deadline cannot decide a test of the independent entry cap.
    let capped = parse::parse_with_clock("json", &many_json_keys(), || now);
    assert_eq!(capped.entries.len(), 1000);
    assert_eq!(capped.entries.first().unwrap().label, "key0");
    assert_eq!(capped.entries.last().unwrap().label, "key999");
    let outline = parse::parse_with_clock("yaml", "first:\n  child: true\nsecond: true\n", || now);
    let mut expanded = BTreeSet::new();
    assert_eq!(outline.visible(&expanded), vec![0, 2]);
    expanded.insert(0);
    assert_eq!(outline.visible(&expanded), vec![0, 1, 2]);
}

fn many_json_keys() -> String {
    format!(
        "{{{}}}",
        (0..3000)
            .map(|ix| format!("\"key{ix}\":{ix}"))
            .collect::<Vec<_>>()
            .join(",")
    )
}

#[test]
fn parsing_deadline_stops_at_the_budget_boundary() {
    let source = many_json_keys();
    let started = std::time::Instant::now();
    for (elapsed_ms, expected_entries) in [(99, 1000), (100, 0), (101, 0)] {
        let mut clock_calls = 0;
        let outline = parse::parse_with_clock("json", &source, || {
            let elapsed = if clock_calls == 0 { 0 } else { elapsed_ms };
            clock_calls += 1;
            started + Duration::from_millis(elapsed)
        });
        assert!(clock_calls > 1, "the deadline check must actually run");
        assert_eq!(outline.entries.len(), expected_entries, "{elapsed_ms} ms");
    }
}

struct Surface {
    input: Entity<EditorState>,
    outline: Entity<View>,
}
impl Render for Surface {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .child(
                div()
                    .debug_selector(|| "outline-editor-viewport".into())
                    .w(px(500.))
                    .h_full()
                    .child(Editor::new(&self.input).readonly(true).size_full()),
            )
            .child(
                div()
                    .w(px(280.))
                    .h_full()
                    .relative()
                    .child(self.outline.clone())
                    .child(View::track_viewport({
                        let outline = self.outline.downgrade();
                        move |_| outline.upgrade()
                    })),
            )
    }
}

#[gpui::test]
fn expanding_clicking_and_scrolling_use_the_real_editor_lines(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        // Scrollbar fade/width animations use wall time and independently ask
        // for frames. This viewport/idle regression uses the real reduced-motion
        // setting so those decorative animations cannot decide its result.
        cx.set_reduce_motion(true);
    });
    let source = format!(
        "first:\n  child: true\n{}second:\n  value: true\n{}",
        "# padding\n".repeat(100),
        "# tail\n".repeat(100)
    );
    let mut entities = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let input = cx.new(|cx| {
            let mut input = EditorState::new(window, cx)
                .soft_wrap(false)
                .line_number(true);
            input.set_value(source, window, cx);
            input
        });
        let outline = cx.new(|_| View::new());
        outline.update(cx, |view, cx| {
            // The outline stays idle until a supported file is actually bound.
            assert!(view.task.is_none());
            assert!(!view.has_entries());
            view.language = Some("yaml");
            view.attach(Some(input.clone()), true, cx);
        });
        entities = Some((input.clone(), outline.clone()));
        let surface = cx.new(|_| Surface { input, outline });
        Root::new(surface, window, cx)
    });
    let (input, outline) = entities.unwrap();
    visual.simulate_resize(size(px(900.), px(450.)));
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(130));
    visual.run_until_parked();
    outline.read_with(visual, |view, _| {
        assert_eq!(view.visible, vec![0, 2]);
        assert_eq!(view.data.entries[2].line, 102);
    });
    let expand = visual
        .debug_bounds("file-outline-toggle-0")
        .expect("a real expandable parent");
    visual.simulate_click(expand.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("file-outline-row-1").is_some());
    let second = visual
        .debug_bounds("file-outline-text-2")
        .expect("a real row click target");
    visual.simulate_click(second.center(), Modifiers::default());
    visual.run_until_parked();
    // As in the cached Markdown panel regression, the test platform needs an
    // explicit display frame. Paint publishes the viewport; observers then run.
    let frame = |visual: &mut VisualTestContext| {
        // Deliver the finite post-layout callbacks as a real platform would.
        for _ in 0..2 {
            visual.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
            visual.run_until_parked();
        }
        visual.run_until_parked();
    };
    frame(visual);
    input.read_with(visual, |input, _| {
        assert_eq!(input.top_visible_line(), Some(102))
    });
    outline.read_with(visual, |view, _| assert_eq!(view.current, Some(2)));
    // A real wheel scroll is independent of the outline's jump API and caret.
    let editor = visual.debug_bounds("outline-editor-viewport").unwrap();
    visual.simulate_event(ScrollWheelEvent {
        position: editor.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(600.))),
        ..Default::default()
    });
    visual.run_until_parked();
    frame(visual);
    input.read_with(visual, |input, _| {
        assert!((2..102).contains(&input.top_visible_line().unwrap()))
    });
    outline.read_with(visual, |view, _| assert_eq!(view.current, Some(1)));
    input.read_with(visual, |input, _| {
        assert_eq!(input.cursor_position(), Position::default())
    });
    // Frame demand must drain once the viewport settles. Deliver callbacks
    // without forcing extra paints (as the platform's idle frame loop does).
    let fired = std::rc::Rc::new(std::cell::Cell::new(false));
    visual.update(|window, cx| {
        let fired = fired.clone();
        window.on_next_frame(move |_, _| fired.set(true));
        assert_eq!(
            window.simulate_next_frame(cx),
            1,
            "callback probe positive control"
        );
    });
    assert!(fired.get());
    for _ in 0..2 {
        assert_eq!(
            visual.update(|window, cx| window.simulate_next_frame(cx)),
            0,
            "no outline frame callbacks at idle"
        );
        visual.run_until_parked();
    }
    outline.read_with(visual, |view, _| assert!(!view.sample_pending));
}

#[gpui::test]
fn file_panel_tracks_pretty_raw_and_hides_contents_without_hiding_backlinks(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
        reader_ui_state::set_json_raw(false, cx);
    });
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("config.json"),
        r#"{"first":{"child":true},"second":2}"#,
    )
    .unwrap();
    std::fs::write(root.path().join("plain.txt"), "Ordinary text\n").unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut reader = Reader::new(
                Opts {
                    vault: Some(root.path().to_path_buf()),
                    note: Some("config.json".into()),
                    index_dir: Some(root.path().join(".index")),
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
    let reader = reader.unwrap();
    let outline = reader.read_with(visual, |reader, cx| {
        reader.side_panels[1]
            .as_ref()
            .unwrap()
            .read(cx)
            .file_outline
            .clone()
            .unwrap()
    });
    visual.simulate_resize(size(px(1400.), px(800.)));
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(130));
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("file-contents-heading").is_some(),
        "positive control: actual file Contents rendered"
    );
    assert!(visual.debug_bounds("file-linked-from-heading").is_some());
    outline.read_with(visual, |view, _| {
        assert!(view.data.entries.iter().any(|entry| entry.line > 0));
        assert_eq!(view.current, None, "Pretty starts above the first key");
    });
    let raw = visual
        .debug_bounds("json-raw")
        .expect("actual JSON mode control");
    visual.simulate_click(raw.center(), Modifiers::default());
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(130));
    visual.run_until_parked();
    outline.read_with(visual, |view, _| {
        assert!(!view.data.entries.is_empty());
        assert!(
            view.data.entries.iter().all(|entry| entry.line == 0),
            "outline follows the displayed raw text"
        );
        assert_eq!(view.current, Some(2), "Raw uses its single displayed line");
    });
    reader.update_in(visual, |reader, window, cx| {
        reader.open_note("plain.txt", None, window, cx)
    });
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(130));
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("file-linked-from-heading").is_some(),
        "positive control: file panel remains rendered"
    );
    assert!(visual.debug_bounds("file-contents-heading").is_none());
    outline.read_with(visual, |view, _| {
        assert!(!view.has_entries());
        assert_eq!(
            view.current, None,
            "navigation clears the prior current entry"
        );
    });
}

#[test]
fn current_child_maps_to_the_visible_ancestor() {
    let now = std::time::Instant::now();
    let outline = parse::parse_with_clock("yaml", "first:\n  child: true\nsecond: true\n", || now);
    let mut expanded = BTreeSet::new();
    assert_eq!(outline.current(1, &outline.visible(&expanded)), Some(0));
    expanded.insert(0);
    assert_eq!(outline.current(1, &outline.visible(&expanded)), Some(1));
    assert_eq!(outline.current(2, &outline.visible(&expanded)), Some(2));
    assert_eq!(outline.current(0, &[]), None);
}

/// Exercise the production sampling wrapper outside a cached right panel,
/// including replacing the read-only preview with the writable Source editor.
#[gpui::test]
fn cached_panel_tracks_preview_and_source_without_changing_selection(cx: &mut TestAppContext) {
    let _cache = reader_side_panel::tests::CachedPanels::on();
    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
    });
    let root = tempfile::tempdir().unwrap();
    let session = tempfile::tempdir().unwrap();
    let source = format!(
        "first:\n  child: true\n{}second:\n  value: true\n{}",
        "# padding\n".repeat(100),
        "# tail\n".repeat(100)
    );
    std::fs::write(root.path().join("config.yaml"), &source).unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut reader = Reader::new(
                Opts {
                    vault: Some(root.path().to_path_buf()),
                    note: Some("config.yaml".into()),
                    session_directory: Some(session.path().to_path_buf()),
                    index_dir: Some(root.path().join(".index")),
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
    let reader = reader.unwrap();
    visual.simulate_resize(size(px(1400.), px(650.)));
    let settle = |visual: &mut VisualTestContext| {
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(130));
        visual.run_until_parked();
        for _ in 0..2 {
            visual.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
            visual.run_until_parked();
        }
    };
    settle(visual);
    let outline = reader.read_with(visual, |reader, cx| {
        reader.side_panels[1]
            .as_ref()
            .unwrap()
            .read(cx)
            .file_outline
            .clone()
            .unwrap()
    });
    for editable in [false, true] {
        if editable {
            reader.update_in(visual, |reader, window, cx| {
                reader.toggle_source(window, cx)
            });
            settle(visual);
        }
        let input = outline.read_with(visual, |view, _| view.input.clone().unwrap());
        reader.read_with(visual, |reader, cx| {
            if editable {
                assert_eq!(
                    Some(input.clone()),
                    reader.source_input(),
                    "Source owns the outline"
                );
            } else {
                assert_eq!(
                    input,
                    reader
                        .file_preview
                        .as_ref()
                        .unwrap()
                        .code
                        .as_ref()
                        .unwrap()
                        .read(cx)
                        .input()
                        .unwrap()
                );
            }
        });
        input.update(visual, |input, cx| input.set_selected_range(0..5, cx));
        settle(visual);
        let before = input.read_with(visual, |input, _| {
            (
                input.selected_range(),
                input.cursor_position(),
                input.text().clone(),
            )
        });
        // The strict surface regression above clicks the real row. A cached
        // subtree intentionally has no debug bounds on reused frames; invoke
        // its same jump action here without invalidating the cache for a probe.
        outline.update_in(visual, |view, window, cx| view.jump(2, window, cx));
        settle(visual);
        assert_eq!(
            input.read_with(visual, |input, _| input.top_visible_line()),
            Some(102)
        );
        outline.read_with(visual, |view, _| assert_eq!(view.current, Some(2)));
        let viewport = visual.debug_bounds("reader-document").unwrap();
        visual.simulate_event(ScrollWheelEvent {
            position: viewport.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(600.))),
            ..Default::default()
        });
        settle(visual);
        input.read_with(visual, |input, _| {
            assert!(
                (2..102).contains(&input.top_visible_line().unwrap()),
                "real wheel moved the viewport"
            );
            assert_eq!(
                (
                    input.selected_range(),
                    input.cursor_position(),
                    input.text().clone()
                ),
                before
            );
        });
        outline.read_with(visual, |view, _| {
            assert_eq!(view.current, Some(0), "collapsed child maps to parent");
            assert!(!view.sample_pending);
        });
        outline.update(visual, |view, cx| view.toggle(0, cx));
        settle(visual);
        outline.read_with(visual, |view, _| assert_eq!(view.current, Some(1)));
        outline.update(visual, |view, cx| view.toggle(0, cx));
        settle(visual);
        outline.read_with(visual, |view, _| assert_eq!(view.current, Some(0)));
    }
    assert_eq!(
        std::fs::read_to_string(root.path().join("config.yaml")).unwrap(),
        source
    );
}
