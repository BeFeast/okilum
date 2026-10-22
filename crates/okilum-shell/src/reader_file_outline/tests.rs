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
    assert!(parse::parse("json", &" ".repeat(parse::MAX_BYTES + 1))
        .entries
        .is_empty());
    let many = format!(
        "{{{}}}",
        (0..3000)
            .map(|ix| format!("\"key{ix}\":{ix}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let capped = parse::parse("json", &many);
    assert_eq!(capped.entries.len(), 1000);
    let outline = parse::parse("yaml", "first:\n  child: true\nsecond: true\n");
    let mut expanded = BTreeSet::new();
    assert_eq!(outline.visible(&expanded), vec![0, 2]);
    expanded.insert(0);
    assert_eq!(outline.visible(&expanded), vec![0, 1, 2]);
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
            .child(div().w(px(280.)).h_full().child(self.outline.clone()))
    }
}

#[gpui::test]
fn expanding_and_clicking_use_the_real_editor_lines(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
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
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.run_until_parked();
    input.read_with(visual, |input, _| {
        assert_eq!(input.top_visible_line(), Some(102))
    });
    input.read_with(visual, |input, _| {
        assert_eq!(input.cursor_position(), Position::default())
    });
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
        assert!(view.data.entries.iter().any(|entry| entry.line > 0))
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
    outline.read_with(visual, |view, _| assert!(!view.has_entries()));
}
