//! Synthetic link-dense folder moves. Compare both implementations on one runner.
#![cfg(unix)]
use okilum_core::link_rewrite::{Operation, Preview};
use std::{collections::BTreeMap, fs, time::Instant};

#[test]
#[ignore = "same-runner baseline comparison in scripts/ci/folder-move-check.py"]
fn folder_move_2682_link_dense_preview_and_exact_undo() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    let state = temp.path().join("state");
    for directory in ["AQA", "Bulk", "Destination"] {
        fs::create_dir_all(root.join(directory)).unwrap();
    }
    for n in 0..10 {
        fs::write(
            root.join(format!("AQA/Note-{n:02}.md")),
            b"# Child\r\n\r\nUnchanged bytes.\r\n",
        )
        .unwrap();
    }
    for n in 0..5000 {
        fs::write(
            root.join(format!("Bulk/Other-{n:05}.md")),
            b"# Unrelated\r\n\r\nNo links.\r\n",
        )
        .unwrap();
    }
    let source = ("---\r\ntitle: Keep this property\r\n---\r\n\r\n".to_owned()
        + &"See [[AQA/Note-00|target]] and [[AQA/Note-01|second]].\r\n".repeat(20)
        + "\r\n`[[AQA/Note-00]]` stays literal.\r\n")
        .into_bytes();
    for n in 0..277 {
        fs::write(root.join(format!("Bulk/Referrer-{n:05}.md")), &source).unwrap();
    }
    for (operation, destination) in [("rename", "AQA-renamed"), ("move", "Destination/AQA")] {
        let mut samples = Vec::new();
        let mut selected = None;
        for _ in 0..3 {
            let started = Instant::now();
            let preview = Preview::prepare(&root, "AQA", destination).unwrap();
            samples.push(started.elapsed().as_secs_f64() * 1000.);
            assert_eq!(preview.changed_notes(), 277);
            assert_eq!(preview.changes.len(), 277 * 40);
            assert!(preview.skipped.is_empty(), "{:?}", preview.skipped);
            assert!(preview.skipped_files.is_empty());
            assert_eq!(preview.timings.files_read, 5000 + 277 + 10);
            selected = Some(preview);
        }
        println!(
            "FOLDER_MOVE_2682_SAMPLE {}",
            serde_json::json!({
                "operation": operation, "preview_ms": samples,
                "unrelated_notes": 5000, "referrers": 277, "links": 277 * 40,
            })
        );
        let preview = selected.unwrap();
        // Changes between preview and approval still invalidate the operation.
        let original = root.join("Bulk/Referrer-00000.md");
        fs::write(&original, b"External edit\r\n").unwrap();
        assert!(preview.validate(&root).is_err());
        fs::write(&original, &source).unwrap();
        let applied = preview.apply(&root, &state, &mut BTreeMap::new()).unwrap();
        assert!(applied.moved);
        assert!(!root.join("AQA").exists());
        let expected = String::from_utf8(source.clone())
            .unwrap()
            .replace(
                "[[AQA/Note-00|target]]",
                &format!("[[{destination}/Note-00|target]]"),
            )
            .replace(
                "[[AQA/Note-01|second]]",
                &format!("[[{destination}/Note-01|second]]"),
            );
        for n in 0..277 {
            assert_eq!(
                fs::read(root.join(format!("Bulk/Referrer-{n:05}.md"))).unwrap(),
                expected.as_bytes()
            );
        }
        Operation::revert(&applied.journal, &state).unwrap();
        assert!(root.join("AQA/Note-00.md").is_file());
        assert!(!root.join(destination).exists());
        for n in 0..277 {
            assert_eq!(
                fs::read(root.join(format!("Bulk/Referrer-{n:05}.md"))).unwrap(),
                source
            );
        }
        for n in [0, 4999] {
            assert_eq!(
                fs::read(root.join(format!("Bulk/Other-{n:05}.md"))).unwrap(),
                b"# Unrelated\r\n\r\nNo links.\r\n"
            );
        }
    }
}
