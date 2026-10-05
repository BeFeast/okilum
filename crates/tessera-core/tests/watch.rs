//! The vault watcher (#6): what a reader sees when files change under it.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use tessera_core::{Searcher, Vault, VaultWatcher};

fn fresh(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("tessera-watch-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::create_dir_all(root.join(".tessera-index")).unwrap();
    root
}

const WAIT: Duration = Duration::from_secs(3);

#[test]
fn a_single_save_arrives_as_one_batch_with_one_note() {
    let root = fresh("single");
    fs::write(root.join("notes/a.md"), "# A\n").unwrap();
    let mut w = VaultWatcher::new(&root).unwrap();

    fs::write(root.join("notes/a.md"), "# A\n\nedited\n").unwrap();
    let c = w.wait(WAIT).expect("a change must arrive");

    assert_eq!(c.changed.iter().collect::<Vec<_>>(), vec!["notes/a.md"]);
    assert!(c.removed.is_empty());
    assert!(!c.is_bulk());
}

#[test]
fn a_burst_of_writes_to_one_file_is_still_one_note() {
    // Editors save through temp-file + rename + chmod; a naive watcher would
    // report the same note three times, or three different paths.
    let root = fresh("burst");
    let mut w = VaultWatcher::new(&root).unwrap();
    for i in 0..10 {
        fs::write(root.join("notes/a.md"), format!("# A {i}\n")).unwrap();
        std::thread::sleep(Duration::from_millis(10));
    }
    let c = w.wait(WAIT).expect("a change must arrive");
    assert_eq!(c.changed.len(), 1, "{c:?}");
}

#[test]
fn a_deleted_note_is_reported_as_removed_not_changed() {
    let root = fresh("delete");
    fs::write(root.join("notes/a.md"), "# A\n").unwrap();
    let mut w = VaultWatcher::new(&root).unwrap();

    fs::remove_file(root.join("notes/a.md")).unwrap();
    let c = w.wait(WAIT).expect("a change must arrive");

    assert!(c.changed.is_empty(), "{c:?}");
    assert_eq!(c.removed.iter().collect::<Vec<_>>(), vec!["notes/a.md"]);
}

#[test]
fn the_legacy_index_and_service_directories_are_never_reported() {
    // A legacy index can live inside the vault. A watcher that reacted to
    // its own index writes would loop forever.
    let root = fresh("dotdirs");
    let mut w = VaultWatcher::new(&root).unwrap();

    fs::write(root.join(".tessera-index/meta.json"), "{}").unwrap();
    fs::write(root.join(".tessera-index/segment.md"), "not a note").unwrap();
    fs::create_dir_all(root.join(".obsidian")).unwrap();
    fs::write(root.join(".obsidian/workspace.md"), "not a note").unwrap();
    fs::write(root.join("notes/image.png"), "not a note either").unwrap();

    assert!(
        w.wait(Duration::from_millis(800)).is_none(),
        "nothing above is a note, so nothing must be reported"
    );
}

#[test]
fn a_bulk_change_is_flagged_so_the_caller_can_rescan_instead() {
    let root = fresh("bulk");
    let mut w = VaultWatcher::new(&root).unwrap();
    for i in 0..60 {
        fs::write(root.join(format!("notes/n{i:03}.md")), "# n\n").unwrap();
    }
    let c = w.wait(WAIT).expect("a change must arrive");
    assert!(
        c.is_bulk(),
        "60 notes in one batch is a bulk operation: {}",
        c.changed.len()
    );
}

#[test]
fn end_to_end_a_save_updates_search_without_a_rebuild() {
    // The pair #6 + #13 as one behaviour: file changes -> watcher batch ->
    // single-note index update -> search sees it.
    let root = fresh("e2e");
    fs::write(root.join("notes/a.md"), "# A\n\nplain\n").unwrap();
    let vault = Vault::scan(&root).unwrap();
    let index = root.join(".tessera-index");
    let searcher = Searcher::build(&vault, &index).unwrap();
    assert!(searcher
        .search("hippopotomonstrosesquippedaliophobia", 5)
        .unwrap()
        .is_empty());

    let mut w = VaultWatcher::new(&root).unwrap();
    fs::write(
        root.join("notes/a.md"),
        "# A\n\nhippopotomonstrosesquippedaliophobia\n",
    )
    .unwrap();
    let c = w.wait(WAIT).expect("a change must arrive");
    assert!(!c.is_bulk());

    // What a caller does with a small batch: rescan the vault (link
    // resolution may have changed) and update just the notes named.
    let vault = Vault::scan(&root).unwrap();
    for rel in &c.changed {
        searcher.update_note(&vault, rel).unwrap();
    }
    let hits = searcher
        .search("hippopotomonstrosesquippedaliophobia", 5)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "notes/a.md");
}

#[test]
fn reading_notes_is_not_a_change() {
    // The reader reads the whole vault on every open (backlinks) and on
    // every rebuild. On Linux the OS reports those reads as events; if they
    // counted as changes, each rebuild would trigger the next one. This is
    // the 300%-CPU-forever bug from the first POC on a 4k-note vault.
    let root = fresh("reads");
    for i in 0..(tessera_core::watch::BULK_THRESHOLD + 10) {
        fs::write(
            root.join(format!("notes/n{i}.md")),
            format!("# n{i}\n\n[[n0]]\n"),
        )
        .unwrap();
    }
    let mut w = VaultWatcher::new(&root).unwrap();
    assert!(
        w.wait(Duration::from_millis(500)).is_none(),
        "nothing has happened yet"
    );

    let v = Vault::scan(&root).unwrap();
    let _ = v.backlinks("notes/n0.md");
    for n in &v.notes {
        let _ = v.read_note(&n.path);
    }

    let c = w.wait(Duration::from_secs(1));
    assert!(
        c.is_none(),
        "reading every note produced a change batch: {c:?}"
    );
}

#[test]
fn new_cyrillic_note_in_hidden_folder_is_reported_but_service_notes_are_not() {
    let root = fresh("hidden-arrival");
    for directory in ["_Assets", ".ordinary", "node_modules"] {
        fs::create_dir_all(root.join(directory)).unwrap();
    }
    let mut watcher = VaultWatcher::new(&root).unwrap();
    for path in [
        "_Assets/Новая заметка.md",
        ".ordinary/скрытая.md",
        "node_modules/generated.md",
    ] {
        fs::write(root.join(path), "# Created after watching").unwrap();
    }
    let changes = watcher.wait(WAIT).expect("new notes must be reported");
    assert!(changes.changed.contains("_Assets/Новая заметка.md"));
    assert!(changes.changed.contains(".ordinary/скрытая.md"));
    assert!(!changes.changed.contains("node_modules/generated.md"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn note_rename_and_atomic_save_are_incremental_but_directory_move_requires_rescan() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::write(root.join("notes/a.md"), "# A").unwrap();
    let mut watcher = VaultWatcher::new(&root).unwrap();
    fs::rename(root.join("notes/a.md"), root.join("notes/b.md")).unwrap();
    let changes = watcher.wait(WAIT).unwrap();
    assert!(!changes.rescan, "known note rename: {changes:?}");
    assert!(changes.changed.contains("notes/b.md"));
    assert!(changes.removed.contains("notes/a.md"));
    let staging = root.join("notes/.tessera-save-test");
    fs::write(&staging, "replacement").unwrap();
    fs::rename(&staging, root.join("notes/b.md")).unwrap();
    let changes = watcher.wait(WAIT).unwrap();
    assert!(!changes.rescan, "atomic save: {changes:?}");
    assert!(changes.changed.contains("notes/b.md"));
    fs::rename(root.join("notes"), root.join("moved")).unwrap();
    let changes = watcher.wait(WAIT).unwrap();
    assert!(
        changes.rescan,
        "directory event is the positive rescan control: {changes:?}"
    );
}
