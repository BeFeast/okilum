//! End-to-end desktop mutation contracts, executed on native Windows as well as Unix.
#![cfg(any(unix, windows))]
use std::{fs, path::Path};
use tessera_core::{
    file_editor::{FileEditor, Save},
    note_files,
    note_move::MovePlan,
    source_history,
};
fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("vault");
    fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let state = dir.path().join("drafts");
    (dir, root, state)
}
#[test]
fn windows_editor_lossless_save_history_and_recovery() {
    let (_dir, root, state) = fixture();
    let note = root.join("Note.md");
    let base = "\u{feff}---\r\ntitle: שלום Привет 🧠\r\n---\r\n# Note\r\n";
    let proposed = format!("{base}\r\nExact draft\r\n");
    fs::write(&note, base).unwrap();
    let mut editor = FileEditor::open(&note, &state).unwrap();
    assert_eq!(editor.text().as_bytes(), base.as_bytes());
    editor.set_text(proposed.clone()).unwrap();
    drop(editor);
    let mut restored = FileEditor::open(&note, &state).unwrap();
    assert_eq!(restored.text(), proposed);
    assert!(restored.dirty());
    assert_eq!(restored.save().unwrap(), Save::Saved);
    assert_eq!(fs::read(&note).unwrap(), proposed.as_bytes());
    drop(restored);
    assert!(!FileEditor::open(&note, &state).unwrap().dirty());
    let history = source_history::list(&state, &root).unwrap();
    assert!(history
        .versions
        .iter()
        .any(|v| v.text == base && !v.protected));
}
#[test]
fn windows_editor_clean_refresh_dirty_conflict_and_reviewed_keep_mine() {
    let (_dir, root, state) = fixture();
    let note = root.join("Note.md");
    fs::write(&note, "base").unwrap();
    let mut editor = FileEditor::open(&note, &state).unwrap();
    fs::write(&note, "clean external").unwrap();
    assert_eq!(editor.refresh_from_disk().unwrap(), Save::Saved);
    assert_eq!(editor.text(), "clean external");
    editor.set_text("mine שלום".into()).unwrap();
    fs::write(&note, "reviewed external").unwrap();
    assert_eq!(editor.refresh_from_disk().unwrap(), Save::Conflict);
    assert_eq!(editor.save().unwrap(), Save::Conflict);
    let reviewed = editor.current().unwrap();
    fs::write(&note, "later external").unwrap();
    assert_eq!(editor.keep_mine(&reviewed).unwrap(), Save::Conflict);
    assert_eq!(fs::read_to_string(&note).unwrap(), "later external");
    assert_eq!(editor.keep_mine("later external").unwrap(), Save::Saved);
    assert_eq!(fs::read_to_string(&note).unwrap(), "mine שלום");
}
#[test]
fn windows_editor_generation_order_and_pending_writer_exclusion() {
    let (_dir, root, state) = fixture();
    let note = root.join("Note.md");
    fs::write(&note, "base").unwrap();
    let mut editor = FileEditor::open(&note, &state).unwrap();
    let old = editor.queue_text("obsolete".into());
    let latest = editor.queue_text("latest".into());
    latest.persist().unwrap();
    old.persist().unwrap();
    assert!(FileEditor::open(&note, &state).is_err());
    let delayed = editor.queue_text("pending newest".into());
    drop(editor);
    assert!(FileEditor::open(&note, &state).is_err());
    delayed.persist().unwrap();
    let mut editor = FileEditor::open(&note, &state).unwrap();
    assert_eq!(editor.text(), "pending newest");
    let stale = editor.queue_text("stale before save".into());
    editor.set_text("saved newest".into()).unwrap();
    assert_eq!(editor.save().unwrap(), Save::Saved);
    stale.persist().unwrap();
    drop(editor);
    let editor = FileEditor::open(&note, &state).unwrap();
    assert_eq!(editor.text(), "saved newest");
    assert!(!editor.dirty());
}
#[test]
fn windows_editor_creation_templates_and_revision_bound_rename() {
    let (_dir, root, state) = fixture();
    note_files::create_folders(&root, Path::new("Folder/nested"), true).unwrap();
    assert!(note_files::create_folders(&root, Path::new("Folder/nested"), true).is_err());
    let note = Path::new("Folder/nested/Note.md");
    note_files::create_with_source(&root, note, b"exact\r\n").unwrap();
    assert!(note_files::create_with_source(&root, note, b"overwrite").is_err());
    let to = Path::new("Renamed.md");
    let plan = MovePlan::prepare(&root, note, to).unwrap();
    fs::write(root.join(note), "external").unwrap();
    assert!(plan.commit().is_err());
    assert!(!root.join(to).exists());
    assert!(MovePlan::prepare(&root, note, Path::new("../escape.md")).is_err());
    let plan = MovePlan::prepare(&root, note, to).unwrap();
    assert!(plan.commit().unwrap().warning.is_none());
    assert_eq!(fs::read(root.join(to)).unwrap(), b"external");
    fs::write(root.join("Taken.md"), "keep").unwrap();
    assert!(MovePlan::prepare(&root, to, Path::new("Taken.md")).is_err());
    let editor = FileEditor::open(&root.join(to), &state).unwrap();
    assert_eq!(editor.text(), "external");
    drop(editor);
    note_files::create_folders(&root, Path::new("_Assets/Templates"), false).unwrap();
    fs::write(
        root.join("_Assets/Templates/Note.md"),
        "\u{feff}# {{title}}\r\n",
    )
    .unwrap();
    let catalog = tessera_core::note_templates::Catalog::load(&root).unwrap();
    assert!(catalog.default_file().is_some());
}

#[test]
fn windows_vault_paths_nested_creation_templates_and_identity() {
    let (_dir, root, state) = fixture();
    let parent = Path::new("Dev").join("Projects").join("tessera-qa");
    note_files::create_folders(&root, &parent, false).unwrap();
    note_files::create_folders(&root, Path::new("_Assets/Templates"), false).unwrap();
    fs::write(
        root.join("_Assets/Templates/Daily Note.md"),
        "\u{feff}# {{title}}\r\nTemplate body\r\n",
    )
    .unwrap();
    let catalog = tessera_core::note_templates::Catalog::load(&root).unwrap();
    let now = time::macros::datetime!(2026-10-08 09:07 UTC);
    let native = parent.join("Native direct.md");
    #[cfg(windows)]
    assert!(
        native.to_str().unwrap().contains('\\'),
        "native input control"
    );
    let direct = note_files::create_from_template(
        &root,
        &native,
        now,
        &state,
        &catalog,
        Some("Daily Note.md"),
    )
    .unwrap();
    assert_eq!(direct, "\u{feff}# Native direct\r\nTemplate body\r\n");
    assert_eq!(fs::read(root.join(&native)).unwrap(), direct.as_bytes());
    for (folder, name, template, expected) in [
        (Path::new(""), "Root note", None, "Root note.md"),
        (
            parent.as_path(),
            "win-qa-b",
            None,
            "Dev/Projects/tessera-qa/win-qa-b.md",
        ),
        (
            parent.as_path(),
            "nested/שלום 🧠",
            Some("Daily Note.md"),
            "Dev/Projects/tessera-qa/nested/שלום 🧠.md",
        ),
    ] {
        let relative = note_files::typed_path(folder, name, false).unwrap();
        assert_eq!(relative.to_str().unwrap(), expected);
        let source =
            note_files::create_from_template(&root, &relative, now, &state, &catalog, template)
                .unwrap();
        assert_eq!(fs::read(root.join(&relative)).unwrap(), source.as_bytes());
        if template.is_some() {
            assert_eq!(source, "\u{feff}# שלום 🧠\r\nTemplate body\r\n");
        }
        assert!(
            note_files::create_from_template(&root, &relative, now, &state, &catalog, template,)
                .is_err(),
            "create must not overwrite"
        );
        let mut inventory = tessera_core::Vault::scan_metadata(&root).unwrap();
        // This native join matches the UI's previous broken registration input.
        let native: std::path::PathBuf = relative.components().collect();
        inventory.register_created_note(native.to_str().unwrap());
        assert!(inventory.notes.iter().all(|n| !n.path.contains('\\')));
        assert!(inventory.notes.iter().any(|n| n.path == expected));
    }
    let folder = note_files::typed_path(&parent, "win-qa-folder", true).unwrap();
    assert_eq!(
        folder.to_str().unwrap(),
        "Dev/Projects/tessera-qa/win-qa-folder"
    );
    note_files::create_folders(&root, &folder, true).unwrap();
    assert!(tessera_core::Vault::scan_metadata(&root)
        .unwrap()
        .entries
        .iter()
        .any(|e| {
            e.path == "Dev/Projects/tessera-qa/win-qa-folder"
                && e.kind == tessera_core::vault::EntryKind::Directory
        }));
    for invalid in ["../escape", "./note", "a//b", "a\\b"] {
        assert!(note_files::typed_path(&parent, invalid, false).is_err());
    }
    assert!(!root.parent().unwrap().join("escape.md").exists());
}

#[test]
fn windows_vault_paths_nested_rename_updates_both_wikilinks_and_relative_link() {
    let (_dir, root, state) = fixture();
    let parent = Path::new("Dev").join("Projects").join("tessera-qa");
    note_files::create_folders(&root, &parent, false).unwrap();
    let from = parent.join("qa-target.md");
    let mut to = from.clone();
    to.set_file_name("qa-target-w.md");
    #[cfg(windows)]
    assert!(
        to.to_str().unwrap().contains('\\'),
        "native rename input control"
    );
    let source = parent.join("qa source.md");
    fs::write(root.join(&from), "# Target\n").unwrap();
    fs::write(
        root.join(&source),
        "[[qa-target]]\n[[qa-target]]\n[Relative Path, sibling](qa-target.md)\n",
    )
    .unwrap();
    let preview = tessera_core::link_rewrite::Preview::prepare(
        &root,
        from.to_str().unwrap(),
        to.to_str().unwrap(),
    )
    .unwrap();
    assert_eq!(preview.from, "Dev/Projects/tessera-qa/qa-target.md");
    assert_eq!(preview.to, "Dev/Projects/tessera-qa/qa-target-w.md");
    assert_eq!(preview.changes.len(), 3);
    assert!(preview.skipped.is_empty(), "{:?}", preview.skipped);
    assert!(preview
        .changes
        .iter()
        .all(|c| !c.after.contains("%5C") && !c.after.contains('\\')));
    let applied = preview
        .apply(&root, &state, &mut std::collections::BTreeMap::new())
        .unwrap();
    assert!(applied.moved && applied.warning.is_none());
    assert_eq!(
        fs::read_to_string(root.join(&source)).unwrap(),
        "[[qa-target-w]]\n[[qa-target-w]]\n[Relative Path, sibling](./qa-target-w.md)\n"
    );
    let inventory = tessera_core::Vault::scan_metadata(&root).unwrap();
    assert_eq!(
        inventory
            .resolve_from("qa-target-w", "Dev/Projects/tessera-qa/qa source.md")
            .path(),
        Some(preview.to.as_str())
    );
    assert_eq!(
        inventory
            .resolve_markdown("./qa-target-w.md", "Dev/Projects/tessera-qa/qa source.md")
            .path(),
        Some(preview.to.as_str())
    );
    assert!(!root.join(from).exists());
    assert_eq!(fs::read(root.join(to)).unwrap(), b"# Target\n");
}

#[test]
fn windows_vault_paths_unrelated_edit_rechecked_new_referrer_refused() {
    use tessera_core::link_rewrite::{CandidateIndex, Preview};
    for indexed in [false, true] {
        let (_dir, root, state) = fixture();
        fs::write(root.join("Old.md"), "# Old\n").unwrap();
        fs::write(root.join("ref.md"), "[[Old]]\n").unwrap();
        fs::write(root.join("unrelated.md"), "plain old text").unwrap();
        let (_, snapshot, _) =
            tessera_core::vault::warm::reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        let index = CandidateIndex::from_snapshot(&snapshot);
        let preview = Preview::prepare_with(
            &root,
            "Old.md",
            "New.md",
            indexed.then_some(&index),
            &mut |_, _| Ok(()),
        )
        .unwrap();
        fs::write(
            root.join("unrelated.md"),
            "new unrelated text\n[web](https://example.com)\n",
        )
        .unwrap();
        preview.validate(&root).unwrap();
        fs::write(root.join("unrelated.md"), "new incoming [[Old]]\n").unwrap();
        assert!(
            preview.validate(&root).is_err(),
            "new referrer must require approval"
        );
        fs::write(root.join("unrelated.md"), "new unrelated text\n").unwrap();
        fs::write(root.join("ref.md"), "external edit [[Old]]\n").unwrap();
        assert!(
            preview.validate(&root).is_err(),
            "approved source remains revision-bound"
        );
        fs::write(root.join("ref.md"), "[[Old]]\n").unwrap();
        assert!(
            preview
                .apply(&root, &state, &mut std::collections::BTreeMap::new())
                .unwrap()
                .moved
        );
        assert_eq!(
            fs::read_to_string(root.join("unrelated.md")).unwrap(),
            "new unrelated text\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("ref.md")).unwrap(),
            "[[New]]\n"
        );
    }
}
#[test]
fn windows_editor_rename_rewrites_links_and_directory_inventory() {
    let (_dir, root, state) = fixture();
    fs::write(root.join("Old.md"), "# Old\n").unwrap();
    fs::write(root.join("ref.md"), "[[Old]]\r\n[old](Old.md)\r\n").unwrap();
    let preview = tessera_core::link_rewrite::Preview::prepare(&root, "Old.md", "New.md").unwrap();
    let result = preview
        .apply(&root, &state, &mut std::collections::BTreeMap::new())
        .unwrap();
    assert!(result.warning.is_none());
    assert!(root.join("New.md").exists() && !root.join("Old.md").exists());
    assert_eq!(
        fs::read_to_string(root.join("ref.md")).unwrap(),
        "[[New]]\r\n[old](./New.md)\r\n"
    );
    note_files::create_folders(&root, Path::new("Folder"), true).unwrap();
    fs::write(root.join("Folder/inside.md"), "inside").unwrap();
    let snapshot =
        tessera_core::note_move::DirectorySnapshot::read(&root, Path::new("Folder")).unwrap();
    let plan = tessera_core::note_move::DirectoryMovePlan::prepare(
        &root,
        Path::new("Folder"),
        Path::new("Moved"),
        &snapshot,
    )
    .unwrap();
    assert!(plan.commit().unwrap().warning.is_none());
    assert_eq!(
        fs::read_to_string(root.join("Moved/inside.md")).unwrap(),
        "inside"
    );
    fs::write(root.join("Moved/inside.md"), "[[Moved/inside]]\r\n").unwrap();
    let reference = fs::read_to_string(root.join("ref.md")).unwrap();
    fs::write(
        root.join("ref.md"),
        format!("{reference}\n[[Moved/inside]]\n"),
    )
    .unwrap();
    let preview = tessera_core::link_rewrite::Preview::prepare(&root, "Moved", "Final").unwrap();
    let applied = preview
        .apply(&root, &state, &mut std::collections::BTreeMap::new())
        .unwrap();
    assert!(applied.warning.is_none(), "{:?}", applied.warning);
    assert!(root.join("Final/inside.md").exists() && !root.join("Moved").exists());
    let vault = tessera_core::Vault::scan(&root).unwrap();
    assert_eq!(vault.backlinks("Final/inside.md").len(), 1);
}
#[test]
fn windows_editor_crash_child() {
    let Some(dir) = std::env::var_os("TESSERA_EDITOR_CRASH_FIXTURE") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let mut editor = FileEditor::open(&dir.join("vault/Note.md"), &dir.join("drafts")).unwrap();
    editor.set_text("durable child שלום\r\n".into()).unwrap();
    // No destructor or save/clean-shutdown acknowledgement; OS releases locks.
    std::process::exit(37);
}
#[test]
fn windows_editor_crash_recovery_keeps_disk_and_conflict_baseline() {
    let (dir, root, state) = fixture();
    let note = root.join("Note.md");
    fs::write(&note, "base").unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "windows_editor_crash_child"])
        .env("TESSERA_EDITOR_CRASH_FIXTURE", dir.path())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(37), "child reached durable draft write");
    assert_eq!(fs::read_to_string(&note).unwrap(), "base");
    fs::write(&note, "external after crash").unwrap();
    let mut editor = FileEditor::open(&note, &state).unwrap();
    assert_eq!(editor.text(), "durable child שלום\r\n");
    assert_eq!(editor.save().unwrap(), Save::Conflict);
    assert_eq!(fs::read_to_string(&note).unwrap(), "external after crash");
}
#[cfg(windows)]
#[test]
fn windows_editor_sharing_error_keeps_recoverable_draft() {
    use std::os::windows::fs::OpenOptionsExt;
    let (_dir, root, state) = fixture();
    let note = root.join("Note.md");
    fs::write(&note, "base").unwrap();
    let mut editor = FileEditor::open(&note, &state).unwrap();
    editor.set_text("mine".into()).unwrap();
    let blocking = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&note)
        .unwrap();
    assert!(editor.save().is_err());
    assert!(editor.dirty());
    drop(editor);
    drop(blocking);
    let mut recovered = FileEditor::open(&note, &state).unwrap();
    assert_eq!(recovered.text(), "mine");
    assert_eq!(recovered.save().unwrap(), Save::Saved);
}

#[cfg(windows)]
#[test]
fn windows_editor_changed_parent_keeps_the_latest_queued_draft() {
    let (_dir, root, state) = fixture();
    fs::create_dir(root.join("Folder")).unwrap();
    let note = root.join("Folder/Note.md");
    fs::write(&note, "base").unwrap();
    let mut editor = FileEditor::open(&note, &state).unwrap();
    let pending = editor.queue_text("latest queued draft".into());
    fs::rename(root.join("Folder"), root.join("Moved")).unwrap();
    fs::create_dir(root.join("Folder")).unwrap();
    fs::write(&note, "unrelated replacement").unwrap();
    assert!(editor.save().is_err());
    assert_eq!(fs::read_to_string(&note).unwrap(), "unrelated replacement");
    assert_eq!(
        fs::read_to_string(root.join("Moved/Note.md")).unwrap(),
        "base"
    );
    drop(editor);
    drop(pending);
    let editor = FileEditor::open(&note, &state).unwrap();
    assert_eq!(editor.text(), "latest queued draft");
    assert!(editor.dirty());
}
#[cfg(windows)]
#[test]
fn windows_editor_creation_undo_refuses_changes_and_replacements() {
    use tessera_core::windows_files::{checked_identity, undo_created};
    let (_dir, root, _state) = fixture();
    let note = root.join("Note.md");
    fs::write(&note, "initial").unwrap();
    let identity = checked_identity(&note).unwrap();
    fs::write(&note, "edited").unwrap();
    assert!(undo_created(&note, identity, Some(b"initial")).is_err());
    assert_eq!(fs::read(&note).unwrap(), b"edited");
    fs::write(root.join("replacement.md"), "initial").unwrap();
    fs::rename(root.join("replacement.md"), &note).unwrap();
    assert!(undo_created(&note, identity, Some(b"initial")).is_err());
    let identity = checked_identity(&note).unwrap();
    undo_created(&note, identity, Some(b"initial")).unwrap();
    assert!(!note.exists());
    note_files::create_folders(&root, Path::new("Folder"), true).unwrap();
    let folder = root.join("Folder");
    let identity = checked_identity(&folder).unwrap();
    fs::write(folder.join("asset.txt"), "keep").unwrap();
    assert!(undo_created(&folder, identity, None).is_err());
    assert_eq!(fs::read(folder.join("asset.txt")).unwrap(), b"keep");
    fs::remove_file(folder.join("asset.txt")).unwrap();
    undo_created(&folder, identity, None).unwrap();
    assert!(!folder.exists());
}
