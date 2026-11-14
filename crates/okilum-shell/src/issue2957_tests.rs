//! #2957: after Maximize note or a panel hide in a read-only file viewer,
//! keyboard focus must stay in the drawn document so the next chord works
//! without a click. Every step dispatches the real platform keystroke
//! through the rendered window.
use super::*;
use crate::reader_layout::Panel;
use ::core::prelude::v1::test;

/// Synthetic JSON, opened in the Formatted (read-only) code viewer.
const TEST_JSON: &str =
    "{\n  \"name\": \"sample\",\n  \"items\": [1, 2, 3],\n  \"nested\": {\"flag\": true}\n}\n";

struct Fixture {
    reader: Entity<Reader>,
    _temp: tempfile::TempDir,
}

fn open_json(cx: &mut TestAppContext, width: f32) -> (Fixture, &mut VisualTestContext) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    let state = temp.path().join("state");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("test1.json"), TEST_JSON).unwrap();
    std::fs::write(root.join("note.md"), "# Note\n\nSynthetic.\n").unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
        reader_ui_state::install(&state, cx);
        reader_ui_state::set_code_view("json", reader_code_file::View::Formatted, cx);
    });
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("test1.json".into()),
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
    visual.simulate_resize(size(px(width), px(768.)));
    settle(visual);
    let reader = reader.unwrap();
    reader.read_with(visual, |r, cx| {
        let preview = r
            .file_preview
            .as_ref()
            .expect("JSON opens as a file preview");
        let code = preview
            .code
            .as_ref()
            .expect("JSON opens in the code viewer");
        assert_eq!(
            code.read(cx).active_view(),
            reader_code_file::View::Formatted
        );
        assert!(code.read(cx).input().is_some(), "content loaded");
        assert!(r.editing.is_none(), "read-only view, not Edit");
    });
    (
        Fixture {
            reader,
            _temp: temp,
        },
        visual,
    )
}

fn settle(visual: &mut VisualTestContext) {
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(130));
    visual.run_until_parked();
}

fn press(visual: &mut VisualTestContext, keys: &str) {
    visual.simulate_keystrokes(keys);
    settle(visual);
}

/// A real click into the drawn document, as in the QA steps.
fn click_document(fixture: &Fixture, visual: &mut VisualTestContext) {
    let bounds = visual
        .debug_bounds("reader-code-file")
        .expect("the code viewer is drawn");
    visual.simulate_click(
        point(bounds.left() + px(60.), bounds.top() + px(40.)),
        Modifiers::default(),
    );
    settle(visual);
    fixture.reader.update_in(visual, |r, window, cx| {
        let code = r.file_preview.as_ref().unwrap().code.clone().unwrap();
        assert!(
            code.read(cx).contains_focus(window, cx),
            "positive control: the click put focus in the document"
        );
    });
}

fn visible(fixture: &Fixture, visual: &mut VisualTestContext, panel: Panel) -> bool {
    fixture.reader.read_with(visual, |r, _| {
        r.panels.visible(panel, f32::from(r.body_bounds.size.width))
    })
}

#[gpui::test]
fn issue2957_maximize_chord_twice_in_formatted_json_restores_panels_and_keeps_shortcuts(
    cx: &mut TestAppContext,
) {
    let (fixture, visual) = open_json(cx, 1366.);
    fixture.reader.update(visual, |r, cx| {
        r.panels.open(Panel::Notes);
        r.panels.open(Panel::Backlinks);
        cx.notify();
    });
    settle(visual);
    assert!(visible(&fixture, visual, Panel::Notes));
    assert!(visible(&fixture, visual, Panel::Backlinks));
    click_document(&fixture, visual);

    press(visual, crate::MAXIMIZE_NOTE_KEY);
    fixture.reader.update_in(visual, |r, window, cx| {
        assert!(r.panels.maximized(), "first press maximizes");
        let code = r.file_preview.as_ref().unwrap().code.clone().unwrap();
        assert!(
            code.read(cx).contains_focus(window, cx),
            "focus stays in the JSON viewer, so its keyboard scrolling and selection keep working"
        );
    });
    assert!(!visible(&fixture, visual, Panel::Notes));
    assert!(!visible(&fixture, visual, Panel::Backlinks));

    // Ctrl+Shift+W (Windows) / ⌥⌘W / Ctrl+Alt+W, still without a click.
    let full_width = visual.update(|_, cx| reader_ui_state::full_note_width(cx));
    press(visual, NOTE_WIDTH_KEY);
    assert_ne!(
        visual.update(|_, cx| reader_ui_state::full_note_width(cx)),
        full_width,
        "the note width chord works right after Maximize"
    );

    press(visual, crate::MAXIMIZE_NOTE_KEY);
    fixture.reader.read_with(visual, |r, _| {
        assert!(!r.panels.maximized(), "second press restores");
    });
    assert!(visible(&fixture, visual, Panel::Notes));
    assert!(visible(&fixture, visual, Panel::Backlinks));

    // Ctrl/Cmd+E switches to Edit without a click.
    press(visual, "secondary-e");
    fixture.reader.read_with(visual, |r, _| {
        assert!(r.editing.is_some(), "the Edit chord works after restore");
    });
    // Edit keeps working as before: Maximize twice from the editor.
    press(visual, crate::MAXIMIZE_NOTE_KEY);
    fixture.reader.update_in(visual, |r, window, cx| {
        assert!(r.panels.maximized());
        let input = r.editing.as_ref().unwrap().input().focus_handle(cx);
        assert!(input.is_focused(window), "Edit keeps its editor focused");
    });
    press(visual, crate::MAXIMIZE_NOTE_KEY);
    fixture
        .reader
        .read_with(visual, |r, _| assert!(!r.panels.maximized()));
}

#[gpui::test]
fn issue2957_right_panel_chord_twice_in_formatted_json_docked(cx: &mut TestAppContext) {
    let (fixture, visual) = open_json(cx, 1366.);
    fixture.reader.update(visual, |r, cx| {
        r.panels.open(Panel::Backlinks);
        cx.notify();
    });
    settle(visual);
    assert!(visible(&fixture, visual, Panel::Backlinks));
    click_document(&fixture, visual);

    press(visual, BACKLINKS_KEY);
    assert!(
        !visible(&fixture, visual, Panel::Backlinks),
        "first press hides"
    );
    press(visual, BACKLINKS_KEY);
    assert!(
        visible(&fixture, visual, Panel::Backlinks),
        "second press shows the panel again without a click"
    );
    press(visual, BACKLINKS_KEY);
    assert!(!visible(&fixture, visual, Panel::Backlinks));
    press(visual, "secondary-e");
    fixture.reader.read_with(visual, |r, _| {
        assert!(r.editing.is_some(), "the Edit chord works after a hide");
    });
}

#[gpui::test]
fn issue2957_right_panel_chord_and_backdrop_in_formatted_json_overlay(cx: &mut TestAppContext) {
    let (fixture, visual) = open_json(cx, 700.);
    assert!(
        fixture
            .reader
            .read_with(visual, |r, _| reader_layout::overlay(f32::from(
                r.body_bounds.size.width
            ))),
        "positive control: 700 px uses the overlay layout"
    );
    fixture.reader.update(visual, |r, cx| {
        r.panels.close(Panel::Notes);
        r.panels.close(Panel::Backlinks);
        cx.notify();
    });
    settle(visual);
    assert!(!visible(&fixture, visual, Panel::Backlinks));
    click_document(&fixture, visual);

    press(visual, BACKLINKS_KEY);
    assert!(visible(&fixture, visual, Panel::Backlinks), "opens");
    press(visual, BACKLINKS_KEY);
    assert!(!visible(&fixture, visual, Panel::Backlinks), "hides");
    press(visual, BACKLINKS_KEY);
    assert!(
        visible(&fixture, visual, Panel::Backlinks),
        "the chord works again after the overlay hid, without a click"
    );

    // A click on the backdrop dismisses the overlay; the chord still works.
    let backdrop = visual
        .debug_bounds("reader-panel-backdrop")
        .expect("overlay backdrop is drawn");
    visual.simulate_click(
        point(backdrop.left() + px(30.), backdrop.center().y),
        Modifiers::default(),
    );
    settle(visual);
    assert!(
        !visible(&fixture, visual, Panel::Backlinks),
        "backdrop dismisses"
    );
    press(visual, BACKLINKS_KEY);
    assert!(
        visible(&fixture, visual, Panel::Backlinks),
        "the chord works after a backdrop dismissal"
    );
}

/// No note selected: the main area draws the empty vault page, not the note's
/// TextView, so focus must go to the Reader itself.
#[gpui::test]
fn issue2957_maximize_and_right_panel_chords_twice_with_no_note_selected(cx: &mut TestAppContext) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("Fresh");
    let state = temp.path().join("state");
    std::fs::create_dir(&root).unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
        reader_ui_state::install(&state, cx);
    });
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
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
    fixture.reader.update_in(visual, |r, window, cx| {
        assert!(r.selected_file().is_empty(), "no note is selected");
        r.panels.open(Panel::Notes);
        r.panels.open(Panel::Backlinks);
        // Where a click on the empty page puts focus: the Reader's own handle.
        r.focus_handle.focus(window, cx);
        cx.notify();
    });
    settle(visual);
    assert!(
        visual.debug_bounds("reader-empty-vault").is_some(),
        "positive control: the empty vault page is drawn"
    );
    assert!(visible(&fixture, visual, Panel::Notes));
    assert!(visible(&fixture, visual, Panel::Backlinks));

    press(visual, crate::MAXIMIZE_NOTE_KEY);
    fixture.reader.read_with(visual, |r, _| {
        assert!(r.panels.maximized(), "first press maximizes");
    });
    press(visual, crate::MAXIMIZE_NOTE_KEY);
    fixture.reader.read_with(visual, |r, _| {
        assert!(
            !r.panels.maximized(),
            "second press restores without a click"
        );
    });
    assert!(visible(&fixture, visual, Panel::Backlinks));

    press(visual, BACKLINKS_KEY);
    assert!(
        !visible(&fixture, visual, Panel::Backlinks),
        "first press hides"
    );
    press(visual, BACKLINKS_KEY);
    assert!(
        visible(&fixture, visual, Panel::Backlinks),
        "second press shows the panel again without a click"
    );
}
