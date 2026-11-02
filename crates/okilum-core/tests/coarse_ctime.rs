//! Real filesystem coverage, with no emulation of filesystem timestamps.
use okilum_core::{
    file_editor::{FileEditor, Save},
    link_rewrite::{CandidateIndex, Preview},
    vault::warm,
};
use std::{fs, time::Instant};

#[test]
fn real_preserved_mtime_edits_refresh_warm_graph_editor_and_link_preview() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("vault");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("from.md"), "target").unwrap();
    let path = root.join("ref.md");
    let mut collisions = 0;
    for preserve_mtime in [false, true] {
        for _attempt in 0..32 {
            fs::write(&path, "abcdefgh").unwrap();
            let before = fs::metadata(&path).unwrap();
            let (_, cold, _) =
                warm::reconcile_parallel(&root, None, false, &mut |_, _| Ok(())).unwrap();
            let index = CandidateIndex::from_snapshot(&cold);
            let preview =
                Preview::prepare_with(&root, "from.md", "to.md", Some(&index), &mut |_, _| Ok(()))
                    .unwrap();
            let mut editor = FileEditor::open(&path, &dir.path().join("state")).unwrap();
            fs::write(&path, "[[from]]").unwrap();
            if preserve_mtime {
                fs::File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_modified(before.modified().unwrap())
                    .unwrap();
            }
            let after = fs::metadata(&path).unwrap();
            assert_eq!(before.len(), after.len());
            if preserve_mtime {
                assert_eq!(before.modified().unwrap(), after.modified().unwrap());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let collision = before.len() == after.len()
                    && before.modified().unwrap() == after.modified().unwrap()
                    && (before.ctime(), before.ctime_nsec()) == (after.ctime(), after.ctime_nsec());
                collisions += usize::from(collision);
                if collision {
                    eprintln!(
                        "REAL_CTIME before={}.{:09} after={}.{:09} collision={}",
                        before.ctime(),
                        before.ctime_nsec(),
                        after.ctime(),
                        after.ctime_nsec(),
                        collision
                    );
                }
            }
            let (vault, changed, stats) =
                warm::reconcile_parallel(&root, Some(&cold), false, &mut |_, _| Ok(())).unwrap();
            let warm_ok = changed.source("ref.md").as_deref() == Some("[[from]]")
                && vault.backlinks("from.md").len() == 1;
            let rejected = preview.validate(&root).is_err();
            let next =
                Preview::prepare_with(&root, "from.md", "to.md", Some(&index), &mut |_, _| Ok(()))
                    .unwrap();
            let new_links = next.changed_notes();
            assert_eq!(editor.refresh_from_disk().unwrap(), Save::Saved);
            let editor_ok = editor.text() == "[[from]]";
            if !warm_ok || !rejected || new_links != 1 || !editor_ok {
                eprintln!("REAL_CTIME failure preserve_mtime={preserve_mtime} warm_updated={warm_ok} stale_preview_rejected={rejected} fresh_link_notes={new_links} editor_refreshed={editor_ok}");
            }
            assert!(warm_ok && rejected && new_links == 1 && editor_ok);
            // The unchanged target can cross the local two-second hashing
            // window between cold and warm reads. An optional fingerprint
            // aging out is conservative revalidation, not another edited note.
            // Keep exact accounting while still rejecting any stat/content
            // change or a differing pair of fingerprints for that target.
            assert_eq!(cold.source("from.md").as_deref(), Some("target"));
            assert_eq!(changed.source("from.md").as_deref(), Some("target"));
            let before_target = cold.source_revision("from.md").unwrap();
            let after_target = changed.source_revision("from.md").unwrap();
            let policy_transition = before_target != after_target;
            if policy_transition {
                let mut before = serde_json::to_value(before_target).unwrap();
                let mut after = serde_json::to_value(after_target).unwrap();
                let before_hash = before
                    .as_object_mut()
                    .unwrap()
                    .remove("content_hash")
                    .unwrap();
                let after_hash = after
                    .as_object_mut()
                    .unwrap()
                    .remove("content_hash")
                    .unwrap();
                assert_eq!(before, after, "Unedited target metadata changed");
                assert_ne!(
                    before_hash.is_null(),
                    after_hash.is_null(),
                    "Unedited target fingerprints differ"
                );
                eprintln!("REAL_CTIME unchanged target crossed fingerprint policy window");
            }
            assert_eq!(
                stats.reuse.changed_revision,
                1 + usize::from(policy_transition)
            );
        }
    }
    eprintln!(
        "REAL_CTIME completed 64 edits; identical-stat collisions detected safely={collisions}"
    );
}

#[test]
#[ignore = "same-host baseline/fallback warm-index measurement"]
fn warm_index_fingerprint_profile() {
    let dir = tempfile::tempdir().unwrap();
    let source = format!("# Note\n{}\n", "ordinary source text ".repeat(400));
    for i in 0..4000 {
        fs::write(dir.path().join(format!("note-{i}.md")), &source).unwrap();
    }
    let (_, cold, _) =
        warm::reconcile_parallel(dir.path(), None, false, &mut |_, _| Ok(())).unwrap();
    // Age local ctime beyond the conservative tick window, exactly the same
    // setup for the baseline and fixed implementations on each host.
    std::thread::sleep(std::time::Duration::from_secs(3));
    let (_, snapshot, _) =
        warm::reconcile_parallel(dir.path(), Some(&cold), false, &mut |_, _| Ok(())).unwrap();
    for sample in 0..3 {
        let started = Instant::now();
        let (_, _, stats) =
            warm::reconcile_parallel(dir.path(), Some(&snapshot), false, &mut |_, _| Ok(()))
                .unwrap();
        eprintln!("CTIME_PROFILE sample={sample} notes=4000 source_bytes={} elapsed_ms={:.3} read={} reused={} graph_reused={} reuse={:?}", source.len()*4000, started.elapsed().as_secs_f64()*1000., stats.read, stats.reused, stats.graph_reused, stats.reuse);
        assert_eq!((stats.read, stats.reused), (0, 4000));
        assert!(stats.graph_reused);
    }
}
