//! Real native-close callback and application Quit regressions (#2651).
use super::{
    tests::{click, settle, vault, START},
    *,
};
use ::core::prelude::v1::test;

const EDITED: &str = "# Start\r\n\r\nonly copy, edited café 雪\r\n";

fn open(
    cx: &mut TestAppContext,
    root: &Path,
    state: &Path,
    mode: Option<bool>,
) -> (Entity<Reader>, VisualTestContext, AnyWindowHandle) {
    cx.update(|cx| {
        crate::test_init(cx);
        cx.set_reduce_motion(true);
        cx.set_quit_mode(gpui::QuitMode::Explicit);
        if !cx.has_global::<reader_open::Readers>() {
            bind_keys(cx);
            reader_open::install(cx);
            #[cfg(all(unix, feature = "brain"))]
            crate::brain::app_quit::install(cx);
            #[cfg(not(all(unix, feature = "brain")))]
            crate::reader_app_menu::install(cx);
        }
        reader_open::create_window(
            Opts {
                vault: Some(root.to_owned()),
                note: Some("Start.md".into()),
                session_directory: Some(state.to_owned()),
                index_dir: Some(state.join("index")),
                ..Default::default()
            },
            None,
            None,
            cx,
        )
        .unwrap();
    });
    cx.run_until_parked();
    let reader = cx.update(|cx| {
        reader_open::readers(cx)
            .into_iter()
            .find(|reader| reader.read(cx).vault_root == root)
            .unwrap()
    });
    let handle = reader.read_with(cx, |r, _| r.reader_window);
    let mut visual = VisualTestContext::from_window(handle, cx);
    settle(&mut visual, &reader, "real window loaded", |r| {
        r.document_ready() && r.note_source.contains("only copy")
    });
    if let Some(live) = mode {
        reader.update_in(&mut visual, |r, window, cx| {
            r.toggle_source(window, cx);
            r.set_live_preview(live, window, cx);
            r.editing.as_ref().unwrap().set_value(EDITED, window, cx);
        });
        visual.run_until_parked();
    }
    (reader, visual, handle)
}

fn assert_choices(visual: &mut VisualTestContext) {
    visual.update(|window, cx| window.draw(cx).clear(cx));
    for selector in [
        "deleted-note-cancel",
        "deleted-note-dont-save",
        "deleted-note-save-as",
    ] {
        assert!(
            visual.debug_bounds(selector).is_some(),
            "visible choice: {selector}"
        );
    }
}

fn close_choices(cx: &mut TestAppContext, mode: Option<bool>) {
    let (temp, root) = vault();
    let state = temp.path().join("state");
    let (reader, mut visual, handle) = open(cx, &root, &state, mode);
    // GPUI's simulator invokes the production callback and returns its verdict;
    // it does not remove the window on a true verdict.
    assert!(
        visual.simulate_close(),
        "positive control: ordinary close allowed"
    );
    std::fs::remove_file(root.join("Start.md")).unwrap();
    // No watcher/clock wait: close itself must notice a just-deleted file.
    assert!(
        !visual.simulate_close(),
        "deleted note blocks unchosen close"
    );
    visual.run_until_parked();
    assert_choices(&mut visual);
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    assert_choices(&mut visual);
    assert!(cx.update(|cx| cx.windows().contains(&handle)));
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert!(!visual.update(|window, cx| window.has_active_dialog(cx)));
    assert!(cx.update(|cx| cx.windows().contains(&handle)));
    reader.read_with(cx, |r, cx| {
        assert_eq!(
            r.deleted_contents(cx).as_deref(),
            Some(if mode.is_some() { EDITED } else { START })
        );
    });

    assert!(!visual.simulate_close());
    visual.run_until_parked();
    click(&mut visual, "deleted-note-cancel");
    assert!(cx.update(|cx| cx.windows().contains(&handle)));
    assert!(!visual.simulate_close());
    visual.run_until_parked();
    click(&mut visual, "deleted-note-dont-save");
    assert!(
        !cx.update(|cx| cx.windows().contains(&handle)),
        "explicit choice closes"
    );
    assert!(
        !root.join("Start.md").exists(),
        "deleted source never recreated"
    );
    if mode.is_some() {
        assert!(
            okilum_core::file_editor::FileEditor::has_unsaved_draft(
                &root.join("Start.md"),
                &state.join("editor-drafts")
            )
            .unwrap(),
            "edited contents remain recoverable"
        );
    }
}

#[gpui::test]
fn read_close_offers_choices_and_only_explicit_discard_closes(cx: &mut TestAppContext) {
    close_choices(cx, None);
}

#[gpui::test]
fn source_close_offers_choices_and_preserves_the_draft(cx: &mut TestAppContext) {
    close_choices(cx, Some(false));
}

#[gpui::test]
fn live_close_offers_choices_and_preserves_the_draft(cx: &mut TestAppContext) {
    close_choices(cx, Some(true));
}

#[gpui::test]
fn close_save_as_cancellation_and_collision_stay_open_then_exact_copy_closes(
    cx: &mut TestAppContext,
) {
    let (temp, root) = vault();
    let (reader, mut visual, handle) = open(cx, &root, &temp.path().join("state"), Some(true));
    std::fs::remove_file(root.join("Start.md")).unwrap();
    let existing = temp.path().join("existing.md");
    std::fs::write(&existing, "do not replace").unwrap();
    for destination in [None, Some(existing.clone())] {
        assert!(!visual.simulate_close());
        visual.run_until_parked();
        click(&mut visual, "deleted-note-save-as");
        assert!(visual.did_prompt_for_new_path());
        visual.simulate_new_path_selection(move |_| destination);
        visual.run_until_parked();
        assert!(cx.update(|cx| cx.windows().contains(&handle)));
        reader.read_with(cx, |r, cx| {
            assert_eq!(r.deleted_contents(cx).as_deref(), Some(EDITED))
        });
        assert!(!root.join("Start.md").exists());
        assert_eq!(
            std::fs::read_to_string(&existing).unwrap(),
            "do not replace"
        );
    }
    assert!(!visual.simulate_close());
    visual.run_until_parked();
    click(&mut visual, "deleted-note-save-as");
    let saved = temp.path().join("saved.md");
    let selected = saved.clone();
    visual.simulate_new_path_selection(move |_| Some(selected));
    visual.run_until_parked();
    assert_eq!(std::fs::read(&saved).unwrap(), EDITED.as_bytes());
    assert!(!cx.update(|cx| cx.windows().contains(&handle)));
    assert!(!root.join("Start.md").exists());
}

#[gpui::test]
fn close_dont_save_refuses_a_failed_recovery_write(cx: &mut TestAppContext) {
    let (temp, root) = vault();
    let state = temp.path().join("state");
    let (reader, mut visual, handle) = open(cx, &root, &state, Some(false));
    std::fs::remove_file(root.join("Start.md")).unwrap();
    assert!(!visual.simulate_close());
    visual.run_until_parked();
    let drafts = state.join("editor-drafts");
    let backup = state.join("drafts-backup");
    std::fs::rename(&drafts, &backup).unwrap();
    std::fs::write(&drafts, "not a directory").unwrap();
    reader.update_in(&mut visual, |r, window, cx| {
        r.editing
            .as_ref()
            .unwrap()
            .set_value("newer unprotected contents", window, cx);
    });
    click(&mut visual, "deleted-note-dont-save");
    assert!(cx.update(|cx| cx.windows().contains(&handle)));
    reader.read_with(cx, |r, cx| {
        assert!(r.editing.is_some());
        assert_eq!(
            r.deleted_contents(cx).as_deref(),
            Some("newer unprotected contents")
        );
    });
    assert!(!root.join("Start.md").exists());
    std::fs::remove_file(&drafts).unwrap();
    std::fs::rename(&backup, &drafts).unwrap();
    assert!(!visual.simulate_close());
    visual.run_until_parked();
    click(&mut visual, "deleted-note-dont-save");
    assert!(!cx.update(|cx| cx.windows().contains(&handle)));
}

fn quit_choices(cx: &mut TestAppContext, mode: Option<bool>) {
    let (temp, root) = vault();
    let (reader, mut visual, _) = open(cx, &root, &temp.path().join("state"), mode);
    std::fs::remove_file(root.join("Start.md")).unwrap();
    visual.simulate_keystrokes("secondary-q");
    visual.run_until_parked();
    assert_choices(&mut visual);
    click(&mut visual, "deleted-note-cancel");
    reader.read_with(cx, |r, cx| {
        assert!(r.deleted_current().is_some());
        assert_eq!(
            r.deleted_contents(cx).as_deref(),
            Some(if mode.is_some() { EDITED } else { START })
        );
    });
    visual.simulate_keystrokes("secondary-q");
    visual.run_until_parked();
    assert_choices(&mut visual);
    click(&mut visual, "deleted-note-dont-save");
    // TestPlatform::quit is a no-op; verify the real continuation crossed the
    // save guard without a fresh prompt or a canonical recreation.
    reader.read_with(cx, |r, _| {
        assert!(r.editing.is_none());
        assert!(r.deleted_note.is_none());
        assert!(!r.deleted_note_asking);
    });
    assert!(!visual.update(|window, cx| window.has_active_dialog(cx)));
    assert!(!root.join("Start.md").exists());
}

#[gpui::test]
fn read_keyboard_quit_offers_choices(cx: &mut TestAppContext) {
    quit_choices(cx, None);
}

#[gpui::test]
fn source_keyboard_quit_offers_choices(cx: &mut TestAppContext) {
    quit_choices(cx, Some(false));
}

#[gpui::test]
fn live_keyboard_quit_offers_choices(cx: &mut TestAppContext) {
    quit_choices(cx, Some(true));
}

#[gpui::test]
fn quit_asks_each_window_and_cancelling_the_second_keeps_its_contents(cx: &mut TestAppContext) {
    let (first_temp, first_root) = vault();
    let (second_temp, second_root) = vault();
    let (first, mut first_visual, first_window) = open(
        cx,
        &first_root,
        &first_temp.path().join("state"),
        Some(false),
    );
    let (second, mut second_visual, second_window) = open(
        cx,
        &second_root,
        &second_temp.path().join("state"),
        Some(true),
    );
    std::fs::remove_file(first_root.join("Start.md")).unwrap();
    std::fs::remove_file(second_root.join("Start.md")).unwrap();
    second_visual.simulate_keystrokes("secondary-q");
    first_visual.run_until_parked();
    assert_choices(&mut first_visual);
    assert!(!second_visual.update(|window, cx| window.has_active_dialog(cx)));
    click(&mut first_visual, "deleted-note-dont-save");
    second_visual.run_until_parked();
    assert_choices(&mut second_visual);
    click(&mut second_visual, "deleted-note-cancel");
    assert!(
        cx.update(
            |cx| cx.windows().contains(&first_window) && cx.windows().contains(&second_window)
        )
    );
    first.read_with(cx, |r, _| assert!(r.editing.is_none()));
    second.read_with(cx, |r, cx| {
        assert_eq!(r.deleted_contents(cx).as_deref(), Some(EDITED))
    });
    assert!(okilum_core::file_editor::FileEditor::has_unsaved_draft(
        &first_root.join("Start.md"),
        &first_temp.path().join("state/editor-drafts")
    )
    .unwrap());
    assert!(!first_root.join("Start.md").exists());
    assert!(!second_root.join("Start.md").exists());
}
