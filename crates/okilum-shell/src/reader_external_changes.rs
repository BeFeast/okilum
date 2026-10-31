//! Files created, renamed and deleted outside Okilum reach a Reader that is
//! already running, in the same session and without a restart (#1314).
//!
//! The Reader drains its watcher from a 250 ms timer (`poll_vault`). These tests
//! drive that timer, not `poll_vault` directly: a first session that never
//! polled would pass a test that calls the poll by hand.

#[cfg(test)]
mod tests {
    use super::super::*;
    use ::core::prelude::v1::test;
    use okilum_core::vault::Resolution;
    use std::path::Path;

    const START: &str = "# Start\n\n[[Missing Target]]\n\n[[Ordinary]]\n";

    fn resolved(vault: &Vault, target: &str) -> bool {
        matches!(
            vault.resolve_from(target, "Start.md"),
            Resolution::Resolved { .. }
        )
    }

    fn has_note(reader: &Reader, path: &str) -> bool {
        reader.vault.notes.iter().any(|note| note.path == path)
    }

    fn in_inventory(reader: &Reader, path: &str) -> bool {
        reader
            .quick_open
            .inventory
            .as_ref()
            .is_some_and(|inventory| inventory.iter().any(|note| note.path == path))
    }

    fn in_tree(reader: &Reader, path: &str) -> bool {
        reader.vault.entries.iter().any(|entry| entry.path == path)
    }

    fn hits(reader: &Reader, word: &str) -> usize {
        reader
            .searcher
            .as_ref()
            .map_or(0, |searcher| searcher.search(word, 100).unwrap().len())
    }

    /// Let real time pass for the OS watcher and its quiet window, and the test
    /// clock for the Reader's own timers, until `done` holds or ~15 s are up.
    fn settle(
        visual: &mut VisualTestContext,
        reader: &Entity<Reader>,
        what: &str,
        done: impl Fn(&Reader) -> bool,
    ) {
        for _ in 0..150 {
            std::thread::sleep(Duration::from_millis(100));
            visual.executor().advance_clock(Duration::from_millis(300));
            visual.run_until_parked();
            if reader.read_with(visual, |reader, _| done(reader)) {
                return;
            }
        }
        panic!("not visible in the same session after 15 s: {what}");
    }

    fn open_first_session<'a>(
        cx: &'a mut TestAppContext,
        root: &Path,
        cache: &Path,
    ) -> (Entity<Reader>, &'a mut VisualTestContext) {
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.to_path_buf()),
                        note: Some("Start.md".into()),
                        index_dir: Some(cache.to_path_buf()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        visual.run_until_parked();
        (entity.unwrap(), visual)
    }

    #[gpui::test]
    fn the_first_session_of_a_fresh_vault_sees_outside_changes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("Start.md"), START).unwrap();
        std::fs::write(root.join("Ordinary.md"), "# Ordinary\n\nordinaryword\n").unwrap();
        // No cache, no recorded state: the first session of a fresh profile.
        let (reader, visual) = open_first_session(cx, &root, &temp.path().join("cache"));
        reader.read_with(visual, |reader, _| {
            assert!(
                reader.watcher.is_some(),
                "positive control: a watcher is registered"
            );
            assert!(has_note(reader, "Ordinary.md"), "positive control: scanned");
            assert!(!resolved(&reader.vault, "Missing Target"));
        });

        // Create: the link starts to resolve, the tree, quick open and search follow.
        std::fs::write(
            root.join("Missing Target.md"),
            "# Missing Target\n\ntargetword\n",
        )
        .unwrap();
        std::fs::write(
            root.join("External Control.md"),
            "# External\n\ncontrolword\n",
        )
        .unwrap();
        settle(visual, &reader, "created notes", |r| {
            resolved(&r.vault, "Missing Target")
                && has_note(r, "External Control.md")
                && in_inventory(r, "External Control.md")
                && in_tree(r, "External Control.md")
                && hits(r, "controlword") == 1
                && hits(r, "targetword") == 1
        });

        // Rename.
        std::fs::rename(root.join("Ordinary.md"), root.join("Ordinary Two.md")).unwrap();
        settle(visual, &reader, "renamed note", |r| {
            has_note(r, "Ordinary Two.md")
                && !has_note(r, "Ordinary.md")
                && in_tree(r, "Ordinary Two.md")
                && !in_tree(r, "Ordinary.md")
                && !resolved(&r.vault, "Ordinary")
                && hits(r, "ordinaryword") == 1
        });

        // Delete.
        std::fs::remove_file(root.join("Missing Target.md")).unwrap();
        settle(visual, &reader, "deleted note", |r| {
            !has_note(r, "Missing Target.md")
                && !in_tree(r, "Missing Target.md")
                && !resolved(&r.vault, "Missing Target")
                && hits(r, "targetword") == 0
        });
    }

    #[gpui::test]
    fn the_first_session_sees_folders_and_a_sync_burst(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("Start.md"), START).unwrap();
        let (reader, visual) = open_first_session(cx, &root, &temp.path().join("cache"));
        reader.read_with(visual, |reader, _| {
            assert!(reader.watcher.is_some(), "positive control: watcher");
        });

        // A folder with notes appears, is renamed, then deleted.
        std::fs::create_dir_all(root.join("Folder/Inner")).unwrap();
        std::fs::write(root.join("Folder/A.md"), "# A\n\nfolderword\n").unwrap();
        std::fs::write(root.join("Folder/Inner/B.md"), "# B\n").unwrap();
        settle(visual, &reader, "new folder", |r| {
            has_note(r, "Folder/A.md")
                && has_note(r, "Folder/Inner/B.md")
                && in_tree(r, "Folder/Inner/B.md")
                && hits(r, "folderword") == 1
        });
        std::fs::rename(root.join("Folder"), root.join("Moved")).unwrap();
        settle(visual, &reader, "renamed folder", |r| {
            has_note(r, "Moved/A.md")
                && has_note(r, "Moved/Inner/B.md")
                && !has_note(r, "Folder/A.md")
                && !in_tree(r, "Folder/Inner/B.md")
                && hits(r, "folderword") == 1
        });
        std::fs::remove_dir_all(root.join("Moved")).unwrap();
        settle(visual, &reader, "deleted folder", |r| {
            !has_note(r, "Moved/A.md")
                && !has_note(r, "Moved/Inner/B.md")
                && !in_tree(r, "Moved/Inner/B.md")
                && hits(r, "folderword") == 0
        });

        // A Syncthing pull: 60 files, in folders, within a fraction of a second.
        for index in 0..60 {
            let dir = root.join(format!("Pull/{}", index % 6));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(format!("Note {index}.md")),
                "# Pulled\n\nburstword\n",
            )
            .unwrap();
        }
        settle(visual, &reader, "60 pulled notes", |r| {
            (0..60).all(|index| has_note(r, &format!("Pull/{}/Note {index}.md", index % 6)))
                && hits(r, "burstword") == 60
        });
        reader.read_with(visual, |r, _| {
            assert!(in_tree(r, "Pull/5/Note 59.md"));
            assert!(in_inventory(r, "Pull/0/Note 0.md"));
        });
        std::fs::remove_dir_all(root.join("Pull")).unwrap();
        settle(visual, &reader, "60 pulled notes removed", |r| {
            !(0..60).any(|index| has_note(r, &format!("Pull/{}/Note {index}.md", index % 6)))
                && hits(r, "burstword") == 0
        });
    }

    /// #1314: the poll timer ran against the Reader's "current window", which
    /// is whichever window drew it last. Settings/About draw the Reader, and
    /// closing that window leaves the Reader with no current window until its
    /// own window draws again; one tick in that gap ended the timer for the
    /// session, so nothing outside was ever seen again.
    #[gpui::test]
    fn closing_settings_does_not_end_the_watcher_poll(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("Start.md"), START).unwrap();
        let (reader, visual) = open_first_session(cx, &root, &temp.path().join("cache"));
        let reader_window = reader.read_with(visual, |reader, _| reader.reader_window);
        visual.update(|_, cx| reader_settings::show(Some(reader.downgrade()), cx));
        visual.run_until_parked();
        let settings = visual
            .update(|_, cx| {
                cx.windows()
                    .into_iter()
                    .find(|window| *window != reader_window)
            })
            .expect("positive control: the Settings window opened");
        settings
            .update(visual, |_, window, _| window.remove_window())
            .unwrap();
        // No draw of the Reader's window in between: the gap the timer can hit.
        let current = visual
            .update(|_, cx| cx.with_window(reader.entity_id(), |window, _| window.window_handle()));
        assert!(
            current.is_none(),
            "positive control: the Reader has no current window, as in the failing session"
        );
        for _ in 0..4 {
            visual.executor().advance_clock(Duration::from_millis(300));
            visual.run_until_parked();
        }
        std::fs::write(
            root.join("External Control.md"),
            "# External\n\ncontrolword\n",
        )
        .unwrap();
        settle(
            visual,
            &reader,
            "a note created after Settings was closed",
            |r| has_note(r, "External Control.md") && hits(r, "controlword") == 1,
        );
    }
}
