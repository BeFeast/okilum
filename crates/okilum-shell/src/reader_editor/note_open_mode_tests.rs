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
    let (reader, visual) = window(&temp, Some("first.md"), cx);
    (reader, visual, temp)
}

/// One more Reader window on the fixture's vault and state, as after a
/// relaunch: `None` restores the saved note instead of naming one.
fn window<'a>(
    temp: &tempfile::TempDir,
    note: Option<&str>,
    cx: &'a mut TestAppContext,
) -> (Entity<Reader>, &'a mut VisualTestContext) {
    let root = temp.path().join("vault");
    let state = temp.path().join("state");
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: note.map(Into::into),
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
    (reader.unwrap(), visual)
}

fn drafts(temp: &tempfile::TempDir) -> std::path::PathBuf {
    temp.path().join("state/editor-drafts")
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
fn explicit_read_wins_on_that_note_until_another_note_opens(cx: &mut TestAppContext) {
    let (reader, visual, _temp) = fixture(Some(NoteOpenMode::Edit), cx);
    assert_open(&reader, visual, "first.md", true);
    reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx)); // Read.
    visual.run_until_parked();
    // The rebuilt Reader view of this note is not reopened by the default.
    assert_open(&reader, visual, "first.md", false);
    open(&reader, visual, "first.md");
    assert_open(&reader, visual, "first.md", false);
    // The next note gets the default again.
    open(&reader, visual, "second.md");
    assert_open(&reader, visual, "second.md", true);
    open(&reader, visual, "first.md");
    assert_open(&reader, visual, "first.md", true);
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

#[gpui::test]
fn automatic_edit_owns_nothing_until_the_first_edit(cx: &mut TestAppContext) {
    let (reader, visual, temp) = fixture(Some(NoteOpenMode::Edit), cx);
    let path = temp.path().join("vault/first.md");
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        let editing = r
            .editing
            .as_ref()
            .expect("positive control: Edit by default");
        assert!(!editing.store.owns_lock(), "opening is not editing");
        assert!(!editing.store.dirty() && !editing.store.has_pending_revision());
        assert!(!editing.save_pending && editing.autosaving.is_none());
        assert!(editing.recovery_pending.is_none());
    });
    // Another window (or process) can still edit the note.
    let other = okilum_core::file_editor::FileEditor::open(&path, &drafts(&temp)).unwrap();
    drop(other);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        modified
    );
    assert!(
        std::fs::read_dir(drafts(&temp)).unwrap().all(|entry| entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|e| e == "lock")),
        "no recovery journal was written"
    );
    // Typing takes ownership.
    visual.simulate_input("x");
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        let editing = r.editing.as_ref().unwrap();
        assert!(editing.store.owns_lock(), "the first edit takes the note");
    });
    assert!(okilum_core::file_editor::FileEditor::open(&path, &drafts(&temp)).is_err());
}

#[gpui::test]
fn first_edit_after_another_window_took_the_note_returns_to_read(cx: &mut TestAppContext) {
    let (reader, visual, temp) = fixture(Some(NoteOpenMode::Edit), cx);
    let path = temp.path().join("vault/first.md");
    reader.read_with(visual, |r, _| assert!(r.editing.is_some()));
    let other = okilum_core::file_editor::FileEditor::open(&path, &drafts(&temp)).unwrap();
    visual.simulate_input("lost");
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        assert_eq!(r.current_rel, "first.md");
        assert!(r.editing.is_none(), "back to Read");
        let notice = r.link_notice.as_ref().expect("the reason is shown");
        assert!(notice.to_string().contains("being edited"), "{notice}");
    });
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "# First\n\nOne.\n");
    drop(other);
}

#[gpui::test]
fn note_another_window_edits_opens_in_read_with_notice(cx: &mut TestAppContext) {
    let (reader, visual, temp) = fixture(Some(NoteOpenMode::Edit), cx);
    let path = temp.path().join("vault/second.md");
    let other = okilum_core::file_editor::FileEditor::open(&path, &drafts(&temp)).unwrap();
    open(&reader, visual, "second.md");
    reader.read_with(visual, |r, _| {
        assert_eq!(r.current_rel, "second.md");
        assert!(r.editing.is_none());
        let notice = r
            .link_notice
            .as_ref()
            .expect("being edited elsewhere is shown");
        assert!(notice.to_string().contains("being edited"), "{notice}");
    });
    drop(other);
    open(&reader, visual, "second.md");
    assert_open(&reader, visual, "second.md", true);
}

#[gpui::test]
fn automatic_edit_is_not_saved_as_the_session_mode(cx: &mut TestAppContext) {
    let (reader, visual, temp) = fixture(Some(NoteOpenMode::Edit), cx);
    assert_open(&reader, visual, "first.md", true);
    let root = reader.read_with(visual, |r, _| r.vault_root.clone());
    visual.update(|_, cx| {
        let (layout, _) = reader_ui_state::layout(&root, cx).unwrap();
        assert_eq!(layout.note, "first.md");
        assert!(
            !layout.source,
            "an automatic Edit is not a choice to restore"
        );
    });
    // Relaunch: the saved note opens by the setting, and a code, data or
    // text file opened next stays in Read.
    let (relaunched, visual) = window(&temp, None, cx);
    assert_open(&relaunched, visual, "first.md", true);
    for file in ["tool.py", "table.csv", "plain.txt"] {
        open(&relaunched, visual, file);
        assert_open(&relaunched, visual, file, false);
    }
}

#[cfg(unix)]
#[gpui::test]
fn read_only_note_opens_in_read_quietly(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt;
    let (reader, visual, temp) = fixture(Some(NoteOpenMode::Edit), cx);
    let folder = temp.path().join("vault");
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
    let writable = rustix::fs::access(&folder, rustix::fs::Access::WRITE_OK).is_ok();
    open(&reader, visual, "second.md");
    let state = reader.read_with(visual, |r, _| {
        (
            r.current_rel.clone(),
            r.editing.is_some(),
            r.link_notice.is_some(),
        )
    });
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    if writable {
        return; // Running as a user that ignores permissions.
    }
    assert_eq!(
        state,
        ("second.md".into(), false, false),
        "Read, without a notice"
    );
    open(&reader, visual, "third.md");
    assert_open(&reader, visual, "third.md", true);
}

/// What runs in the same UI turn as the first keystroke, before the input's
/// own mutation event reaches the Reader.
#[derive(Clone, Copy, Debug)]
enum Before {
    Blur,
    WatcherRefresh,
}

/// Insert `text` at the start and run `before` in the same update, so the
/// capture path runs before the SourceMutation subscriber (#2817 review).
fn type_then(reader: &Entity<Reader>, visual: &mut VisualTestContext, text: &str, before: Before) {
    reader.update_in(visual, |r, window, cx| {
        let input = r.editing.as_ref().unwrap().input.clone();
        input.update(cx, |input, cx| {
            input.replace_text_in_range(Some(0..0), text, window, cx)
        });
        match before {
            Before::Blur => {
                r.tree_focus.focus(window, cx);
                r.save_source_in_background(window, cx);
            }
            Before::WatcherRefresh => r.refresh_source_in_background(window, cx),
        }
    });
    visual.run_until_parked();
}

fn settle(visual: &mut VisualTestContext) {
    visual
        .executor()
        .advance_clock(std::time::Duration::from_secs(3));
    visual.run_until_parked();
}

fn assert_refused(
    reader: &Entity<Reader>,
    visual: &mut VisualTestContext,
    path: &Path,
    typed: &str,
) {
    reader.read_with(visual, |r, cx| {
        assert_eq!(r.current_rel, "first.md");
        assert!(
            r.editing.is_none(),
            "the refused editor returns to Read: {:?}",
            r.editing
                .as_ref()
                .map(|e| (e.store.owns_lock(), e.input.read(cx).value().to_string()))
        );
        let notice = r.link_notice.as_ref().expect("the reason is shown");
        assert!(notice.to_string().contains("being edited"), "{notice}");
        assert!(
            !r.note_source.contains(typed),
            "the refused keystroke is not shown"
        );
    });
    assert_eq!(std::fs::read_to_string(path).unwrap(), "# First\n\nOne.\n");
}

#[gpui::test]
fn first_edit_captured_by_blur_or_refresh_first_takes_ownership(cx: &mut TestAppContext) {
    for before in [Before::Blur, Before::WatcherRefresh] {
        let (reader, visual, temp) = fixture(Some(NoteOpenMode::Edit), cx);
        let path = temp.path().join("vault/first.md");
        type_then(&reader, visual, "mine ", before);
        reader.read_with(visual, |r, _| {
            let editing = r.editing.as_ref().expect("{before:?}: still editing");
            assert!(
                editing.store.owns_lock(),
                "{before:?}: captured only once owned"
            );
            assert!(
                !editing.save_failed,
                "{before:?}: recovery accepted the edit"
            );
        });
        reader.update_in(visual, |r, window, cx| {
            r.save_source_in_background(window, cx)
        });
        settle(visual);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "mine # First\n\nOne.\n",
            "{before:?}: the edit is saved"
        );
    }
}

#[gpui::test]
fn first_edit_captured_by_blur_or_refresh_first_is_refused_when_taken(cx: &mut TestAppContext) {
    for before in [Before::Blur, Before::WatcherRefresh] {
        let (reader, visual, temp) = fixture(Some(NoteOpenMode::Edit), cx);
        let path = temp.path().join("vault/first.md");
        let other = okilum_core::file_editor::FileEditor::open(&path, &drafts(&temp)).unwrap();
        type_then(&reader, visual, "mine ", before);
        assert_refused(&reader, visual, &path, "mine");
        drop(other);
    }
}

fn ime_commit(reader: &Entity<Reader>, visual: &mut VisualTestContext) {
    let input = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone());
    reader.update_in(visual, |_, window, cx| {
        input.focus_handle(cx).focus(window, cx)
    });
    visual.run_until_parked();
    reader.update_in(visual, |_, window, cx| {
        input.update(cx, |i, cx| {
            i.replace_and_mark_text_in_range(Some(0..0), "か", Some(1..1), window, cx)
        });
    });
    visual.run_until_parked();
    reader.update_in(visual, |_, window, cx| {
        input.update(cx, |i, cx| {
            let marked = i.marked_text_range(window, cx);
            if marked.is_some() {
                i.replace_text_in_range(marked, "漢", window, cx)
            }
        });
    });
    visual.run_until_parked();
}

/// The test clipboard, read the way the note editor's exact provider reads
/// the system clipboard (the platform clipboard is not reachable in tests).
struct Clipboard(&'static str);
impl gpui_component::input::clipboard::ExactClipboardProvider for Clipboard {
    fn read_text_now(
        &self,
        _: &App,
    ) -> Option<Result<Option<String>, gpui_component::input::clipboard::ClipboardReadError>> {
        Some(Ok(Some(self.0.into())))
    }
    fn read_text(
        &self,
        _: gpui_component::input::clipboard::ClipboardReadRequest,
        _: &App,
    ) -> Task<Result<Option<String>, gpui_component::input::clipboard::ClipboardReadError>> {
        Task::ready(Ok(Some(self.0.into())))
    }
}

fn paste(reader: &Entity<Reader>, visual: &mut VisualTestContext) {
    let input = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone());
    input.update_in(visual, |input, window, cx| {
        input.set_exact_clipboard_provider(Some(Arc::new(Clipboard("pasted "))), cx);
        input.set_selected_range(0..0, cx);
        input.focus_handle(cx).focus(window, cx);
        window.activate_window();
    });
    visual.dispatch_action(gpui_component::input::Paste);
    visual.run_until_parked();
}

#[gpui::test]
fn ime_commit_or_paste_as_first_input_takes_ownership(cx: &mut TestAppContext) {
    let (reader, visual, temp) = fixture(Some(NoteOpenMode::Edit), cx);
    let path = temp.path().join("vault/first.md");
    ime_commit(&reader, visual);
    reader.read_with(visual, |r, _| {
        assert!(
            r.editing.as_ref().unwrap().store.owns_lock(),
            "IME composition owns"
        );
    });
    reader.update_in(visual, |r, window, cx| {
        r.save_source_in_background(window, cx)
    });
    settle(visual);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "漢# First\n\nOne.\n"
    );

    let (reader, visual, temp) = fixture(Some(NoteOpenMode::Edit), cx);
    let path = temp.path().join("vault/first.md");
    paste(&reader, visual);
    reader.read_with(visual, |r, cx| {
        let editing = r.editing.as_ref().unwrap();
        assert!(
            editing.input.read(cx).value().contains("pasted"),
            "positive control: the paste reached the editor: {:?}",
            editing.input.read(cx).value()
        );
        assert!(editing.store.owns_lock(), "paste owns");
    });
    reader.update_in(visual, |r, window, cx| {
        r.save_source_in_background(window, cx)
    });
    settle(visual);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "pasted # First\n\nOne.\n"
    );
}

#[gpui::test]
fn ime_commit_or_paste_as_first_input_is_refused_when_taken(cx: &mut TestAppContext) {
    let (reader, visual, temp) = fixture(Some(NoteOpenMode::Edit), cx);
    let path = temp.path().join("vault/first.md");
    let other = okilum_core::file_editor::FileEditor::open(&path, &drafts(&temp)).unwrap();
    ime_commit(&reader, visual);
    assert_refused(&reader, visual, &path, "か");
    drop(other);

    let (reader, visual, temp) = fixture(Some(NoteOpenMode::Edit), cx);
    let path = temp.path().join("vault/first.md");
    let other = okilum_core::file_editor::FileEditor::open(&path, &drafts(&temp)).unwrap();
    paste(&reader, visual);
    assert_refused(&reader, visual, &path, "pasted");
    drop(other);
}

#[gpui::test]
fn refused_first_edit_keeps_the_place_in_read(cx: &mut TestAppContext) {
    let (reader, visual, temp) = fixture(Some(NoteOpenMode::Edit), cx);
    visual.simulate_resize(size(px(1000.), px(600.)));
    let long: String = (1..=200).map(|n| format!("Paragraph {n}.\n\n")).collect();
    std::fs::write(
        temp.path().join("vault/long.md"),
        format!("# Long\n\n{long}"),
    )
    .unwrap();
    let path = temp.path().join("vault/long.md");
    open(&reader, visual, "long.md");
    reader.update_in(visual, |r, _, cx| {
        let input = r.editing.as_ref().expect("Edit by default").input.clone();
        // Caret and view far down: typing keeps the caret in view.
        let at = input.read(cx).value().find("Paragraph 150.").unwrap();
        input.update(cx, |input, cx| {
            input.set_selected_range(at..at, cx);
            input.scroll_line_to_top(300, cx);
        });
    });
    visual.run_until_parked();
    let top = reader.read_with(visual, |r, cx| {
        r.editing
            .as_ref()
            .unwrap()
            .input
            .read(cx)
            .top_visible_line()
            .unwrap()
    });
    assert!(
        top > 200,
        "positive control: the editor scrolled to line {top}"
    );
    let other = okilum_core::file_editor::FileEditor::open(&path, &drafts(&temp)).unwrap();
    visual.simulate_input("x");
    visual.run_until_parked();
    reader.read_with(visual, |r, cx| {
        assert!(r.editing.is_none());
        let block = r
            .navigation
            .pending_landing
            .unwrap_or_else(|| r.content.read(cx).list_state().logical_scroll_top())
            .item_ix;
        assert!(
            block > 80,
            "Read opens near the editor's place, block {block}"
        );
        assert!(r
            .link_notice
            .as_ref()
            .unwrap()
            .to_string()
            .contains("being edited"));
    });
    assert!(!std::fs::read_to_string(&path).unwrap().contains('x'));
    drop(other);
}

fn assert_untouched(reader: &Entity<Reader>, visual: &mut VisualTestContext, why: &str) {
    reader.read_with(visual, |r, _| {
        let editing = r.editing.as_ref().expect("still in Edit");
        assert!(!editing.store.owns_lock(), "{why}: no ownership");
        assert!(!editing.store.dirty(), "{why}: not dirty");
        assert!(
            !editing.store.has_pending_revision(),
            "{why}: no pending revision"
        );
        assert!(
            editing.recovery_pending.is_none(),
            "{why}: no recovery scheduled"
        );
        assert!(
            editing.autosaving.is_none() && !editing.save_pending,
            "{why}: no autosave scheduled"
        );
    });
}

#[gpui::test]
fn watcher_adoption_of_equal_text_keeps_an_untouched_editor_clean(cx: &mut TestAppContext) {
    let (reader, visual, temp) = fixture(Some(NoteOpenMode::Edit), cx);
    let path = temp.path().join("vault/first.md");
    assert_untouched(&reader, visual, "positive control after open");
    // The watcher's clean adoption replaces the native buffer with the same
    // bytes in a new revision.
    reader.update_in(visual, |r, window, cx| {
        let text = std::fs::read_to_string(&path).unwrap();
        r.editing
            .as_mut()
            .unwrap()
            .replace_clean_source(text, window, cx);
    });
    visual.run_until_parked();
    assert_untouched(&reader, visual, "after an equal-text watcher adoption");
    reader.update_in(visual, |r, window, cx| {
        r.save_source_in_background(window, cx)
    });
    settle(visual);
    assert_untouched(&reader, visual, "after blur");
}

#[gpui::test]
fn equal_text_input_takes_nothing(cx: &mut TestAppContext) {
    let (reader, visual, temp) = fixture(Some(NoteOpenMode::Edit), cx);
    let path = temp.path().join("vault/first.md");
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let input = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone());
    // Input events that leave the text equal: a character replaced by
    // itself, and an empty insertion (as an Undo with nothing to undo).
    for (range, text) in [(2..3, "F"), (0..0, "")] {
        reader.update_in(visual, |r, window, cx| {
            input.update(cx, |i, cx| {
                i.replace_text_in_range(Some(range), text, window, cx)
            });
            r.save_source_in_background(window, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            assert_eq!(
                r.editing.as_ref().unwrap().input.read(cx).value().as_ref(),
                "# First\n\nOne.\n",
                "positive control: the input event left the text equal"
            );
        });
        assert_untouched(&reader, visual, "equal-text input");
    }
    assert!(okilum_core::file_editor::FileEditor::open(&path, &drafts(&temp)).is_ok());
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        modified
    );
}
