use tessera_core::Vault;

#[test]
fn snapshot_preserves_backlinks_exact_bytes_and_cancellation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("nested")).unwrap();
    for (path, text) in [
        (
            "a.md",
            "---\r\ntitle: Юникод\r\n---\r\n[[nested/b]] [[b]]\r\n`[[absent]]`\r\n",
        ),
        ("nested/b.md", "# B\n\n[[a]]\n"),
        ("b.md", "# Ambiguous B\n"),
    ] {
        std::fs::write(root.join(path), text).unwrap();
    }
    std::fs::write(root.join("invalid.md"), b"\xff [[a]]").unwrap();
    let old = Vault::scan(&root).unwrap();
    let (snapshot, bytes) = Vault::scan_snapshot_with(&root, &mut |_, _| Ok(())).unwrap();
    assert_eq!(
        old.notes.len(),
        4,
        "fixture must be admitted by directory policy"
    );
    assert_eq!(snapshot.notes.len(), old.notes.len());
    assert_eq!(bytes.len(), 3);
    assert!(!bytes.contains_key("invalid.md"));
    assert!(snapshot
        .unreadable
        .iter()
        .any(|entry| entry.path == root.join("invalid.md")));
    assert_eq!(
        std::fs::read(root.join("invalid.md")).unwrap(),
        b"\xff [[a]]"
    );
    for note in &old.notes {
        if note.path != "invalid.md" {
            assert_eq!(
                bytes[&note.path],
                std::fs::read(root.join(&note.path)).unwrap()
            );
        }
        assert_eq!(
            serde_json::to_value(snapshot.backlinks(&note.path)).unwrap(),
            serde_json::to_value(old.backlinks(&note.path)).unwrap()
        );
    }
    // Only nested/b.md contributes: the invalid source must not be decoded lossily.
    assert_eq!(snapshot.backlinks("a.md").len(), 1);
    assert!(Vault::scan_snapshot_with(&root, &mut |phase, _| {
        if phase == "Reading notes" {
            anyhow::bail!("cancel reading");
        }
        Ok(())
    })
    .is_err());
    assert!(Vault::scan_snapshot_with(&root, &mut |phase, _| {
        if phase == "Preparing backlinks" {
            anyhow::bail!("cancel backlinks");
        }
        Ok(())
    })
    .is_err());
}
