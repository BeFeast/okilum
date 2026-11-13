//! #2817: Settings → Editor → Open notes in (Read or Edit), through the real
//! Reader open pipeline.
use super::*;
use ::core::prelude::v1::test;
use reader_ui_state::NoteOpenMode;

fn fixture(
    mode: Option<NoteOpenMode>,
    cx: &mut TestAppContext,
) -> (Entity<Reader>, &mut VisualTestContext, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    std::fs::create_dir(&root).unwrap();
    for (path, text) in [
        ("first.md", "# First\n\nOne.\n"),
        ("second.md", "# Second\n\n[[first|one]]\n"),
        ("third.md", "# Third\n\nThree.\n"),
        ("plain.txt", "Plain text has its own Read/Edit.\n"),
        ("table.csv", "a,b\n1,2\n"),
        ("tool.py", "print('code has its own Read/Edit')\n"),
    ] {
        std::fs::write(root.join(path), text).unwrap();
    }
    // Not UTF-8: the Reader shows it, the editor refuses it.
    std::fs::write(root.join("damaged.md"), b"# Damaged\n\n\xff\xfe broken\n").unwrap();
    let state = temp.path().join("state");
    std::fs::create_dir(&state).unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
        reader_ui_state::install(&state, cx);
        if let Some(mode) = mode {
            reader_ui_state::set_note_open_mode(mode, cx);
            reader_ui_state::flush(cx);
        }
    });
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("first.md".into()),
                    index_dir: Some(temp.path().join("index")),
                    session_directory: Some(state.clone()),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    visual.run_until_parked();
    (reader.unwrap(), visual, temp)
}

fn open(reader: &Entity<Reader>, visual: &mut VisualTestContext, rel: &str) {
    reader.update_in(visual, |r, window, cx| r.open_note(rel, None, window, cx));
    visual.run_until_parked();
}

/// `true`: the editor holds exactly this file; `false`: Read.
fn assert_open(reader: &Entity<Reader>, visual: &mut VisualTestContext, rel: &str, edit: bool) {
    reader.read_with(visual, |r, cx| {
        assert_eq!(r.selected_file(), rel, "positive control: the chosen file");
        match &r.editing {
            Some(editing) if edit => {
                assert_eq!(editing.store.path(), r.vault_root.join(rel));
                assert_eq!(
                    editing.input.read(cx).value().as_ref(),
                    std::fs::read_to_string(editing.store.path()).unwrap()
                );
            }
            Some(_) => panic!("{rel} must open in Read"),
            None if edit => panic!("{rel} must open in Edit"),
            None => {}
        }
    });
}

#[gpui::test]
fn default_is_read_and_unchanged(cx: &mut TestAppContext) {
    let (reader, visual, _temp) = fixture(None, cx);
    visual.update(|_, cx| {
        assert_eq!(reader_ui_state::note_open_mode(cx), NoteOpenMode::Read);
    });
    assert_open(&reader, visual, "first.md", false);
    open(&reader, visual, "second.md");
    assert_open(&reader, visual, "second.md", false);
    open(&reader, visual, "plain.txt");
    assert_open(&reader, visual, "plain.txt", false);
    // Today's per-window choice still carries: explicit Edit, then notes.
    reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
    open(&reader, visual, "third.md");
    assert_open(&reader, visual, "third.md", true);
}

#[gpui::test]
fn read_setting_opens_notes_in_read(cx: &mut TestAppContext) {
    let (reader, visual, _temp) = fixture(Some(NoteOpenMode::Read), cx);
    assert_open(&reader, visual, "first.md", false);
    open(&reader, visual, "second.md");
    assert_open(&reader, visual, "second.md", false);
}

#[gpui::test]
fn edit_setting_opens_notes_in_edit_and_files_in_read(cx: &mut TestAppContext) {
    let (reader, visual, _temp) = fixture(Some(NoteOpenMode::Edit), cx);
    // The window's first note, published before its state was restored.
    assert_open(&reader, visual, "first.md", true);
    open(&reader, visual, "second.md");
    assert_open(&reader, visual, "second.md", true);
    for file in ["plain.txt", "table.csv", "tool.py"] {
        open(&reader, visual, file);
        reader.read_with(visual, |r, _| {
            assert!(
                r.file_preview.is_some(),
                "positive control: {file} is a file view"
            )
        });
        assert_open(&reader, visual, file, false);
    }
    open(&reader, visual, "third.md");
    assert_open(&reader, visual, "third.md", true);
}

#[gpui::test]
fn edit_setting_keeps_live_preview_choice(cx: &mut TestAppContext) {
    let (reader, visual, _temp) = fixture(Some(NoteOpenMode::Edit), cx);
    reader.update_in(visual, |r, window, cx| {
        assert!(r.editing.is_some());
        r.set_live_preview(true, window, cx);
    });
    open(&reader, visual, "second.md");
    reader.read_with(visual, |r, _| assert!(r.source_live_preview()));
    reader.update_in(visual, |r, window, cx| {
        r.set_live_preview(false, window, cx)
    });
    open(&reader, visual, "third.md");
    reader.read_with(visual, |r, _| {
        assert!(r.editing.is_some());
        assert!(!r.source_live_preview(), "Source is kept too");
    });
}

#[gpui::test]
fn explicit_read_in_a_window_wins_until_edit_is_chosen(cx: &mut TestAppContext) {
    let (reader, visual, _temp) = fixture(Some(NoteOpenMode::Edit), cx);
    assert_open(&reader, visual, "first.md", true);
    reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx)); // Read.
    visual.run_until_parked();
    assert_open(&reader, visual, "first.md", false);
    open(&reader, visual, "second.md");
    assert_open(&reader, visual, "second.md", false);
    reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx)); // Edit.
    visual.run_until_parked();
    open(&reader, visual, "third.md");
    assert_open(&reader, visual, "third.md", true);
}

#[gpui::test]
fn changed_setting_applies_to_the_next_note(cx: &mut TestAppContext) {
    let (reader, visual, _temp) = fixture(None, cx);
    assert_open(&reader, visual, "first.md", false);
    visual.update(|_, cx| reader_ui_state::set_note_open_mode(NoteOpenMode::Edit, cx));
    visual.run_until_parked();
    assert_open(&reader, visual, "first.md", false);
    open(&reader, visual, "second.md");
    assert_open(&reader, visual, "second.md", true);
    // Switching back to Read: the next note opens in Read again.
    visual.update(|_, cx| reader_ui_state::set_note_open_mode(NoteOpenMode::Read, cx));
    open(&reader, visual, "third.md");
    assert_open(&reader, visual, "third.md", false);
}

#[gpui::test]
fn note_that_cannot_be_edited_stays_in_read_without_notice(cx: &mut TestAppContext) {
    let (reader, visual, _temp) = fixture(Some(NoteOpenMode::Edit), cx);
    open(&reader, visual, "damaged.md");
    reader.read_with(visual, |r, _| {
        assert_eq!(r.current_rel, "damaged.md");
        assert!(r.editing.is_none(), "an uneditable note stays readable");
        assert!(r.link_notice.is_none(), "the default asked for nothing");
    });
    // Positive control: an explicit Edit of the same note reports why.
    reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
    reader.read_with(visual, |r, _| {
        assert!(r.editing.is_none());
        assert!(
            r.link_notice.is_some(),
            "explicit Edit explains the refusal"
        );
    });
    // The default still applies to the next editable note.
    open(&reader, visual, "second.md");
    assert_open(&reader, visual, "second.md", true);
}

fn assert_new_note_is_editable(mode: NoteOpenMode, cx: &mut TestAppContext) {
    let (reader, visual, temp) = fixture(Some(mode), cx);
    reader.update_in(visual, |r, window, cx| r.new_note(None, window, cx));
    visual.run_until_parked();
    visual.simulate_input("Created");
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        assert!(
            r.source_live_preview(),
            "{mode:?}: a new note opens in Live Preview"
        );
    });
    assert_open(&reader, visual, "Created.md", true);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("vault/Created.md")).unwrap(),
        "# Created\n"
    );
}

#[gpui::test]
fn new_note_is_editable_with_read_setting(cx: &mut TestAppContext) {
    assert_new_note_is_editable(NoteOpenMode::Read, cx);
}

#[gpui::test]
fn new_note_is_editable_with_edit_setting(cx: &mut TestAppContext) {
    assert_new_note_is_editable(NoteOpenMode::Edit, cx);
}
