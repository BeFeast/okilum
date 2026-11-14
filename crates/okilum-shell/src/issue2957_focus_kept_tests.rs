//! #2957 review (PR #2966): Maximize note and a panel hide only change the
//! layout. Whatever holds focus keeps it while it is still drawn (Find, a
//! rename), and typing continues there; an element the change hides hands
//! focus to the document; a toolbar or menu invocation with focus in the
//! document leaves the chords working without a click. Every chord is a real
//! keystroke through the rendered window. Fixtures are synthetic.
//!
//! Only Reader APIs that predate the fix are used, so these tests also
//! build against the production code before #2957.
use super::*;
use crate::reader_layout::Panel;
use ::core::prelude::v1::test;

const TEST_JSON: &str =
    "{\n  \"name\": \"sample\",\n  \"items\": [1, 2, 3],\n  \"nested\": {\"flag\": true}\n}\n";
const TEST_NOTE: &str = "# Note\n\nSynthetic text for the find field.\n";

struct Fixture {
    reader: Entity<Reader>,
    _temp: tempfile::TempDir,
}

fn open<'a>(cx: &'a mut TestAppContext, note: &str) -> (Fixture, &'a mut VisualTestContext) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    let state = temp.path().join("state");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("test1.json"), TEST_JSON).unwrap();
    std::fs::write(root.join("note.md"), TEST_NOTE).unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
        reader_ui_state::install(&state, cx);
        reader_ui_state::set_code_view("json", reader_code_file::View::Formatted, cx);
    });
    let mut reader = None;
    let note = note.to_owned();
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some(note.clone()),
                    session_directory: Some(state.clone()),
                    index_dir: Some(temp.path().join("index")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    visual.simulate_resize(size(px(1366.), px(768.)));
    settle(visual);
    let fixture = Fixture {
        reader: reader.unwrap(),
        _temp: temp,
    };
    fixture.reader.update(visual, |r, cx| {
        r.panels.open(Panel::Notes);
        r.panels.open(Panel::Backlinks);
        cx.notify();
    });
    settle(visual);
    assert!(visible(&fixture, visual, Panel::Notes));
    assert!(visible(&fixture, visual, Panel::Backlinks));
    (fixture, visual)
}

/// Frames are drawn explicitly: focus that a layout change leaves undrawn
/// is only detected while a frame is drawn.
fn settle(visual: &mut VisualTestContext) {
    for _ in 0..3 {
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(130));
        visual.update(|window, cx| window.draw(cx).clear(cx));
    }
    visual.run_until_parked();
}

fn press(visual: &mut VisualTestContext, keys: &str) {
    visual.simulate_keystrokes(keys);
    settle(visual);
}

fn type_text(visual: &mut VisualTestContext, text: &str) {
    visual.simulate_input(text);
    settle(visual);
}

fn visible(fixture: &Fixture, visual: &mut VisualTestContext, panel: Panel) -> bool {
    fixture.reader.read_with(visual, |r, _| {
        r.panels.visible(panel, f32::from(r.body_bounds.size.width))
    })
}

fn maximized(fixture: &Fixture, visual: &mut VisualTestContext) -> bool {
    fixture
        .reader
        .read_with(visual, |r, _| r.panels.maximized())
}

/// A real click on a control measured in a fresh frame.
fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = reader_viewer_chrome::tests::measured_bounds(visual, selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    visual.simulate_click(bounds.center(), Modifiers::default());
    settle(visual);
}

fn click_code_viewer(visual: &mut VisualTestContext) {
    let bounds = visual
        .debug_bounds("reader-code-file")
        .expect("the code viewer is drawn");
    visual.simulate_click(
        point(bounds.left() + px(60.), bounds.top() + px(40.)),
        Modifiers::default(),
    );
    settle(visual);
}

/// The query field of the header Find (the viewer's or the file editor's).
fn find_query(fixture: &Fixture, visual: &mut VisualTestContext) -> Entity<InputState> {
    fixture.reader.read_with(visual, |r, cx| {
        r.viewer_find_target(cx)
            .expect("the viewer offers Find")
            .input
    })
}

fn focused(input: &Entity<InputState>, visual: &mut VisualTestContext) -> bool {
    visual.update(|window, cx| input.read(cx).focus_handle(cx).is_focused(window))
}

fn value(input: &Entity<InputState>, visual: &mut VisualTestContext) -> String {
    visual.update(|_, cx| input.read(cx).value().to_string())
}

fn note_width(visual: &mut VisualTestContext) -> bool {
    visual.update(|_, cx| reader_ui_state::full_note_width(cx))
}

/// (a) Find in the Formatted JSON viewer, then in its Edit view.
#[gpui::test]
fn issue2957_find_keeps_focus_and_query_through_maximize_and_panel_hide_in_formatted_json(
    cx: &mut TestAppContext,
) {
    let (fixture, visual) = open(cx, "test1.json");
    fixture.reader.read_with(visual, |r, _| {
        let preview = r.file_preview.as_ref().expect("JSON opens as a preview");
        assert!(preview.code.is_some(), "JSON opens in the code viewer");
        assert!(r.editing.is_none(), "read-only Formatted view");
    });
    click_code_viewer(visual);

    press(visual, "secondary-f");
    let query = find_query(&fixture, visual);
    assert!(focused(&query, visual), "positive control: Find has focus");
    type_text(visual, "na");
    assert_eq!(value(&query, visual), "na", "positive control: typing");

    press(visual, crate::MAXIMIZE_NOTE_KEY);
    assert!(maximized(&fixture, visual), "the chord maximizes from Find");
    assert!(!visible(&fixture, visual, Panel::Notes));
    assert!(focused(&query, visual), "Find keeps focus after Maximize");
    type_text(visual, "me");
    assert_eq!(value(&query, visual), "name", "typing continues the query");

    press(visual, crate::MAXIMIZE_NOTE_KEY);
    assert!(!maximized(&fixture, visual), "the chord restores from Find");
    assert!(visible(&fixture, visual, Panel::Backlinks));
    assert!(focused(&query, visual), "Find keeps focus after restore");

    press(visual, BACKLINKS_KEY);
    assert!(
        !visible(&fixture, visual, Panel::Backlinks),
        "the chord hides the docked right panel from Find"
    );
    assert!(focused(&query, visual), "Find keeps focus after the hide");
    type_text(visual, "s");
    assert_eq!(value(&query, visual), "names", "typing continues the query");

    // Edit view of the same file: its Find keeps focus too, instead of the
    // editor taking it.
    click_code_viewer(visual);
    press(visual, "secondary-e");
    fixture.reader.read_with(visual, |r, _| {
        assert!(r.editing.is_some(), "positive control: Edit view");
    });
    press(visual, "secondary-f");
    let edit_query = find_query(&fixture, visual);
    assert!(
        focused(&edit_query, visual),
        "positive control: the Edit view's Find has focus"
    );
    let typed = value(&edit_query, visual);
    press(visual, crate::MAXIMIZE_NOTE_KEY);
    assert!(maximized(&fixture, visual));
    assert!(
        focused(&edit_query, visual),
        "the Edit view's Find keeps focus after Maximize"
    );
    type_text(visual, "it");
    assert_eq!(
        value(&edit_query, visual),
        format!("{typed}it"),
        "typing continues the Edit view's query"
    );
    press(visual, crate::MAXIMIZE_NOTE_KEY);
    assert!(!maximized(&fixture, visual));
    assert!(focused(&edit_query, visual));
}

/// (2) A JSON file is shown and an inline rename runs in the left sidebar;
/// the right panel's toolbar button hides that panel. The rename is still
/// drawn, so it keeps focus and typing continues the name.
#[gpui::test]
fn issue2957_sidebar_rename_keeps_focus_when_the_toolbar_hides_the_right_panel_over_json(
    cx: &mut TestAppContext,
) {
    let (fixture, visual) = open(cx, "test1.json");
    click_code_viewer(visual);
    fixture.reader.update_in(visual, |r, window, cx| {
        r.begin_rename("note.md".into(), window, cx);
    });
    settle(visual);
    let rename = fixture.reader.read_with(visual, |r, _| {
        assert_eq!(
            r.file_preview.as_ref().map(|p| p.rel.as_str()),
            Some("test1.json"),
            "positive control: the JSON stays shown"
        );
        r.renaming
            .as_ref()
            .expect("positive control: rename started")
            .input
            .clone()
    });
    assert!(
        focused(&rename, visual),
        "positive control: the rename field has focus"
    );

    click(visual, "reader-backlinks-preserve");
    assert!(
        !visible(&fixture, visual, Panel::Backlinks),
        "the toolbar button hides the right panel"
    );
    assert!(visible(&fixture, visual, Panel::Notes));
    assert!(
        focused(&rename, visual),
        "the rename keeps focus after the toolbar hide"
    );
    type_text(visual, "re");
    assert_eq!(value(&rename, visual), "re", "typing continues the name");
    type_text(visual, "named");
    assert_eq!(value(&rename, visual), "renamed");
}

/// (a) In a note: a tree rename field keeps focus while a right-panel hide
/// leaves it drawn, and typing continues; Maximize hides it with the Notes
/// panel, and the drawn note takes focus so chords keep working. The note's
/// own Find keeps focus through Maximize.
#[gpui::test]
fn issue2957_rename_field_and_note_find_keep_focus_and_a_hidden_field_hands_it_to_the_note(
    cx: &mut TestAppContext,
) {
    let (fixture, visual) = open(cx, "note.md");
    fixture.reader.update_in(visual, |r, window, cx| {
        assert!(r.file_preview.is_none() && r.editing.is_none(), "a note");
        r.begin_rename("note.md".into(), window, cx);
    });
    settle(visual);
    let rename = fixture.reader.read_with(visual, |r, _| {
        r.renaming
            .as_ref()
            .expect("positive control: rename started")
            .input
            .clone()
    });
    assert!(
        focused(&rename, visual),
        "positive control: the rename field has focus"
    );

    press(visual, BACKLINKS_KEY);
    assert!(!visible(&fixture, visual, Panel::Backlinks), "hidden");
    assert!(visible(&fixture, visual, Panel::Notes));
    assert!(
        focused(&rename, visual),
        "the rename field keeps focus while its panel stays"
    );
    type_text(visual, "re");
    assert_eq!(value(&rename, visual), "re", "typing continues the rename");

    press(visual, crate::MAXIMIZE_NOTE_KEY);
    assert!(maximized(&fixture, visual));
    assert!(!visible(&fixture, visual, Panel::Notes));
    fixture.reader.update_in(visual, |r, window, cx| {
        assert!(
            r.content.read(cx).focus_handle().is_focused(window),
            "the hidden rename field hands focus to the drawn note"
        );
    });
    let width = note_width(visual);
    press(visual, NOTE_WIDTH_KEY);
    assert_ne!(note_width(visual), width, "a chord works without a click");
    press(visual, crate::MAXIMIZE_NOTE_KEY);
    assert!(!maximized(&fixture, visual), "restores without a click");
    fixture.reader.update_in(visual, |r, window, cx| {
        r.renaming = None;
        let focus = r.content.read(cx).focus_handle().clone();
        focus.focus(window, cx);
        cx.notify();
    });
    settle(visual);

    press(visual, "secondary-f");
    let query = fixture
        .reader
        .read_with(visual, |r, _| r.find_input.clone());
    assert!(focused(&query, visual), "positive control: note Find");
    type_text(visual, "Syn");
    press(visual, crate::MAXIMIZE_NOTE_KEY);
    assert!(maximized(&fixture, visual));
    assert!(
        focused(&query, visual),
        "note Find keeps focus after Maximize"
    );
    type_text(visual, "th");
    assert_eq!(value(&query, visual), "Synth", "typing continues the query");
    press(visual, crate::MAXIMIZE_NOTE_KEY);
    assert!(!maximized(&fixture, visual));
    press(visual, BACKLINKS_KEY);
    assert!(visible(&fixture, visual, Panel::Backlinks), "shown again");
}

/// (b) The header Maximize button, the toolbar right-panel toggle and the
/// MaximizeNote action as the menu sends it, with focus in the document:
/// focus stays there and the chords work afterwards without a click.
#[gpui::test]
fn issue2957_toolbar_and_menu_maximize_and_panel_toggle_keep_chords_without_a_click(
    cx: &mut TestAppContext,
) {
    let (fixture, visual) = open(cx, "test1.json");
    // Wide enough that the viewer header keeps its Maximize button visible.
    visual.simulate_resize(size(px(1920.), px(1000.)));
    settle(visual);
    click_code_viewer(visual);
    // Whatever the click focused inside the code viewer is the document.
    let in_document = visual.update(|window, cx| window.focused(cx));
    assert!(in_document.is_some(), "positive control: the click focused");
    let document_focused = |_: &Fixture, visual: &mut VisualTestContext| {
        visual.update(|window, cx| window.focused(cx)) == in_document
    };

    click(visual, "reader-maximize-note");
    assert!(maximized(&fixture, visual), "the header button maximizes");
    assert!(
        document_focused(&fixture, visual),
        "focus stays in the document, not on the button"
    );
    press(visual, crate::MAXIMIZE_NOTE_KEY);
    assert!(
        !maximized(&fixture, visual),
        "the chord restores right after the button"
    );
    assert!(visible(&fixture, visual, Panel::Backlinks));

    click(visual, "reader-backlinks-preserve");
    assert!(
        !visible(&fixture, visual, Panel::Backlinks),
        "the toolbar toggle hides the right panel"
    );
    assert!(document_focused(&fixture, visual));
    press(visual, BACKLINKS_KEY);
    assert!(
        visible(&fixture, visual, Panel::Backlinks),
        "the chord shows it again right after the toolbar toggle"
    );

    visual.dispatch_action(crate::MaximizeNote);
    settle(visual);
    assert!(maximized(&fixture, visual), "the menu action maximizes");
    let width = note_width(visual);
    press(visual, NOTE_WIDTH_KEY);
    assert_ne!(note_width(visual), width, "a chord works after the menu");
    press(visual, crate::MAXIMIZE_NOTE_KEY);
    assert!(!maximized(&fixture, visual), "the chord restores");
    press(visual, "secondary-e");
    fixture.reader.read_with(visual, |r, _| {
        assert!(r.editing.is_some(), "the Edit chord works without a click");
    });
}
