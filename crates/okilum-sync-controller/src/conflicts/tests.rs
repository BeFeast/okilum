use super::*;
const COPY: &str = "note.sync-conflict-20261007-123456-ABCDEFG.md";
fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let vault = tempfile::tempdir().unwrap();
    let recovery = tempfile::tempdir().unwrap();
    std::fs::write(vault.path().join("note.md"), b"\xef\xbb\xbfthis\r\n").unwrap();
    std::fs::write(vault.path().join(COPY), b"that\r\n\xff").unwrap();
    (vault, recovery)
}
#[test]
fn all_explicit_choices_preserve_exact_originals_and_publish_expected_sources() {
    for choice in [Choice::KeepThis, Choice::KeepThat, Choice::KeepBoth] {
        let (vault, recovery) = fixture();
        let p = preview(vault.path(), Path::new(COPY)).unwrap();
        let before = [p.this.bytes.clone(), p.that.bytes.clone()];
        let ApplyOutcome::Applied(receipt) = apply(&p, choice, recovery.path()).unwrap() else {
            panic!("not applied")
        };
        assert_eq!(
            std::fs::read(vault.path().join("note.md")).unwrap(),
            before[usize::from(choice == Choice::KeepThat)]
        );
        assert!(!vault.path().join(COPY).exists());
        for (path, bytes) in receipt.originals.iter().zip(&before) {
            assert_eq!(&std::fs::read(vault.path().join(path)).unwrap(), bytes);
        }
        if let Some(path) = receipt.kept_both {
            assert_eq!(std::fs::read(vault.path().join(path)).unwrap(), before[1]);
        }
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(receipt.recovery_record).unwrap()).unwrap();
        assert_eq!(record["this_bytes"], serde_json::json!(before[0]));
        assert_eq!(record["that_bytes"], serde_json::json!(before[1]));
        assert!(
            apply(&p, choice, recovery.path()).is_err(),
            "cannot replay consumed preview"
        );
    }
}
#[test]
fn edits_and_identical_byte_replacements_of_either_input_refuse_without_mutation() {
    for name in ["note.md", COPY] {
        for replace in [false, true] {
            let (vault, recovery) = fixture();
            let p = preview(vault.path(), Path::new(COPY)).unwrap();
            let path = vault.path().join(name);
            if replace {
                let fresh = vault.path().join("replacement");
                std::fs::write(&fresh, std::fs::read(&path).unwrap()).unwrap();
                std::fs::rename(fresh, &path).unwrap();
            } else {
                std::fs::write(&path, b"external edit").unwrap();
            }
            let current = [
                std::fs::read(vault.path().join("note.md")).unwrap(),
                std::fs::read(vault.path().join(COPY)).unwrap(),
            ];
            assert!(apply(&p, Choice::KeepThat, recovery.path()).is_err());
            assert_eq!(
                std::fs::read(vault.path().join("note.md")).unwrap(),
                current[0]
            );
            assert_eq!(std::fs::read(vault.path().join(COPY)).unwrap(), current[1]);
            assert_eq!(std::fs::read_dir(recovery.path()).unwrap().count(), 0);
        }
    }
}
#[test]
fn unsafe_paths_links_and_unavailable_recovery_cannot_authorize_source_changes() {
    let (vault, recovery) = fixture();
    let p = preview(vault.path(), Path::new(COPY)).unwrap();
    assert!(apply(&p, Choice::KeepThat, &recovery.path().join("missing")).is_err());
    assert!(apply(&p, Choice::KeepThat, vault.path()).is_err());
    assert_eq!(
        std::fs::read(vault.path().join("note.md")).unwrap(),
        p.this.bytes
    );
    assert_eq!(
        std::fs::read(vault.path().join(COPY)).unwrap(),
        p.that.bytes
    );
    assert!(preview(vault.path(), &vault.path().join(COPY)).is_err());
    assert!(preview(
        vault.path(),
        Path::new("../note.sync-conflict-20261007-123456-ABCDEFG.md")
    )
    .is_err());
    std::fs::remove_file(vault.path().join(COPY)).unwrap();
    std::os::unix::fs::symlink(vault.path().join("note.md"), vault.path().join(COPY)).unwrap();
    assert!(preview(vault.path(), Path::new(COPY)).is_err());
}
#[test]
fn replaced_ancestor_with_identical_contents_is_refused() {
    let (vault, recovery) = fixture();
    std::fs::create_dir(vault.path().join("nested")).unwrap();
    for name in ["note.md", COPY] {
        std::fs::rename(
            vault.path().join(name),
            vault.path().join("nested").join(name),
        )
        .unwrap();
    }
    let p = preview(vault.path(), &Path::new("nested").join(COPY)).unwrap();
    std::fs::rename(vault.path().join("nested"), vault.path().join("old")).unwrap();
    std::fs::create_dir(vault.path().join("nested")).unwrap();
    for name in ["note.md", COPY] {
        std::fs::copy(
            vault.path().join("old").join(name),
            vault.path().join("nested").join(name),
        )
        .unwrap();
    }
    assert!(apply(&p, Choice::KeepThat, recovery.path()).is_err());
}

#[test]
fn racing_replacement_is_detected_after_capture_and_both_versions_survive() {
    let (vault, recovery) = fixture();
    let p = preview(vault.path(), Path::new(COPY)).unwrap();
    let outcome = apply_before_capture(&p, Choice::KeepThat, recovery.path(), || {
        let fresh = vault.path().join("replacement");
        std::fs::write(&fresh, b"racing writer").unwrap();
        std::fs::rename(fresh, vault.path().join("note.md")).unwrap();
    })
    .unwrap();
    let ApplyOutcome::NeedsAttention {
        recovery: receipt, ..
    } = outcome
    else {
        panic!("race reported resolved")
    };
    assert_eq!(
        std::fs::read(vault.path().join("note.md")).unwrap(),
        b"racing writer"
    );
    assert_eq!(
        std::fs::read(vault.path().join(COPY)).unwrap(),
        p.that.bytes
    );
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(receipt.recovery_record).unwrap()).unwrap();
    assert_eq!(record["this_bytes"], serde_json::json!(p.this.bytes));
    assert_eq!(record["that_bytes"], serde_json::json!(p.that.bytes));
}

#[test]
fn failure_after_first_capture_restores_original_and_never_overwrites_other_input() {
    let (vault, recovery) = fixture();
    let p = preview(vault.path(), Path::new(COPY)).unwrap();
    let outcome = apply_before_capture(&p, Choice::KeepThat, recovery.path(), || {
        std::fs::remove_file(vault.path().join(COPY)).unwrap();
        std::fs::create_dir(vault.path().join(COPY)).unwrap();
    })
    .unwrap();
    assert!(matches!(outcome, ApplyOutcome::NeedsAttention { .. }));
    assert_eq!(
        std::fs::read(vault.path().join("note.md")).unwrap(),
        p.this.bytes
    );
    assert!(vault.path().join(COPY).is_dir());
}

#[test]
fn intervening_creation_during_publication_is_never_overwritten_or_reported_resolved() {
    let (vault, recovery) = fixture();
    let p = preview(vault.path(), Path::new(COPY)).unwrap();
    let outcome = apply_with_hooks(
        &p,
        Choice::KeepBoth,
        recovery.path(),
        || {},
        || {
            std::fs::write(vault.path().join("note.md"), b"external creation").unwrap();
        },
    )
    .unwrap();
    let ApplyOutcome::NeedsAttention {
        recovery: receipt, ..
    } = outcome
    else {
        panic!("collision reported resolved")
    };
    assert_eq!(
        std::fs::read(vault.path().join("note.md")).unwrap(),
        b"external creation"
    );
    assert_eq!(
        std::fs::read(vault.path().join(&receipt.originals[0])).unwrap(),
        p.this.bytes
    );
    assert_eq!(
        std::fs::read(vault.path().join(COPY)).unwrap(),
        p.that.bytes
    );
    assert!(!vault.path().join(receipt.kept_both.unwrap()).exists());
}

#[test]
fn filename_with_an_earlier_invalid_marker_uses_only_the_valid_suffix() {
    let vault = tempfile::tempdir().unwrap();
    let original = "note.sync-conflict-invalid.md";
    let copy = "note.sync-conflict-invalid.sync-conflict-20261007-123456-ABCDEFG.md";
    std::fs::write(vault.path().join(original), b"this").unwrap();
    std::fs::write(vault.path().join(copy), b"that").unwrap();
    assert_eq!(
        preview(vault.path(), Path::new(copy)).unwrap().this.path,
        Path::new(original)
    );
}

#[test]
fn archived_input_edited_before_publication_refuses_the_unseen_revision() {
    let (vault, recovery) = fixture();
    let p = preview(vault.path(), Path::new(COPY)).unwrap();
    let outcome = apply_with_hooks(
        &p,
        Choice::KeepThat,
        recovery.path(),
        || {},
        || {
            for entry in std::fs::read_dir(vault.path()).unwrap() {
                let entry = entry.unwrap();
                if entry.file_name().to_string_lossy().ends_with("-that") {
                    std::fs::write(entry.path(), b"unseen edit").unwrap();
                }
            }
        },
    )
    .unwrap();
    assert!(matches!(outcome, ApplyOutcome::NeedsAttention { .. }));
    assert_eq!(
        std::fs::read(vault.path().join("note.md")).unwrap(),
        p.this.bytes
    );
    assert_eq!(
        std::fs::read(vault.path().join(COPY)).unwrap(),
        b"unseen edit"
    );
}

#[test]
fn issue2044_editor_copies_share_inventory_preview_actions_and_history() {
    for choice in [Choice::KeepThis, Choice::KeepThat, Choice::KeepBoth] {
        let (vault, state) = fixture();
        let editor = "note.okilum-conflict-20261104-123456-ABC2345.md";
        std::fs::rename(vault.path().join(COPY), vault.path().join(editor)).unwrap();
        let inventory = inspect_note(vault.path(), Path::new("note.md")).unwrap();
        assert!(inventory.complete);
        assert_eq!(inventory.copies, [PathBuf::from(editor)]);
        let p = preview(vault.path(), Path::new(editor)).unwrap();
        let before = [p.this.bytes.clone(), p.that.bytes.clone()];
        assert!(matches!(
            apply(&p, choice, state.path()).unwrap(),
            ApplyOutcome::Applied(_)
        ));
        let history = history(state.path(), vault.path()).unwrap();
        assert_eq!(history.entries.len(), 1);
        assert!(!history.entries[0].pending);
        assert_eq!(history.entries[0].canonical, Path::new("note.md"));
        for (retained, bytes) in history.entries[0].versions.iter().zip(before) {
            assert_eq!(retained.bytes, bytes);
        }
        assert!(inspect(vault.path()).unwrap().copies.is_empty());
    }
}

#[test]
#[ignore = "subprocess crash control; invoked by issue2044_crashes_recover_exact_bytes"]
fn issue2044_crash_child() {
    let root =
        PathBuf::from(std::env::var_os("OKILUM_CONFLICT_CRASH_ROOT").expect("crash fixture root"));
    let p = preview(&root.join("vault"), Path::new(COPY)).unwrap();
    apply(&p, Choice::KeepBoth, &root.join("state")).unwrap();
    panic!("crash checkpoint was not fired");
}

#[test]
fn issue2044_crashes_recover_exact_bytes_without_overwriting_intervening_writers() {
    use sha2::{Digest, Sha256};
    for step in [
        "journal",
        "capture-this",
        "capture-that",
        "publish-this",
        "publish-both",
    ] {
        for replace in [false, true] {
            let fixture = tempfile::tempdir().unwrap();
            let vault = fixture.path().join("vault");
            let state = fixture.path().join("state");
            std::fs::create_dir(&vault).unwrap();
            std::fs::create_dir(&state).unwrap();
            let before = [
                b"\xef\xbb\xbfthis\r\n".as_slice(),
                b"that\r\n\xff".as_slice(),
            ];
            std::fs::write(vault.join("note.md"), before[0]).unwrap();
            std::fs::write(vault.join(COPY), before[1]).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "conflicts::resolution_tests::issue2044_crash_child",
                    "--exact",
                    "--ignored",
                    "--nocapture",
                ])
                .env("OKILUM_CONFLICT_CRASH_ROOT", fixture.path())
                .env("OKILUM_CONFLICT_CRASH_STEP", step)
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(86), "control must terminate at {step}");
            if replace {
                std::fs::write(vault.join("note.md"), b"intervening writer").unwrap();
            }
            let messages = recover_interrupted(&state, &vault).unwrap();
            assert_eq!(messages.len(), 1);
            assert_eq!(
                std::fs::read(vault.join("note.md")).unwrap(),
                if replace {
                    b"intervening writer".as_slice()
                } else {
                    before[0]
                }
            );
            assert_eq!(std::fs::read(vault.join(COPY)).unwrap(), before[1]);
            let entries = history(&state, &vault).unwrap().entries;
            for (version, bytes) in entries[0].versions.iter().zip(before) {
                assert_eq!(Sha256::digest(&version.bytes), Sha256::digest(bytes));
            }
            assert!(
                recover_interrupted(&state, &vault).unwrap().is_empty(),
                "recovered journals cannot resurrect future deletions"
            );
            assert_eq!(
                std::fs::read(vault.join(COPY)).unwrap(),
                before[1],
                "recovery is idempotent"
            );
        }
    }
}

#[test]
fn issue2044_non_utf8_paths_refuse_before_journal_serialization() {
    let (vault, _) = fixture();
    let canonical = OsStr::from_bytes(b"bad-\xff.md");
    let copy = OsStr::from_bytes(b"bad-\xff.sync-conflict-20261104-123456-ABC2345.md");
    std::fs::write(vault.path().join(canonical), b"this").unwrap();
    std::fs::write(vault.path().join(copy), b"that").unwrap();
    assert!(preview(vault.path(), Path::new(copy))
        .unwrap_err()
        .to_string()
        .contains("UTF-8 source paths"));
    assert_eq!(
        std::fs::read(vault.path().join(canonical)).unwrap(),
        b"this"
    );
    assert_eq!(std::fs::read(vault.path().join(copy)).unwrap(), b"that");
}

#[test]
fn issue2044_multiple_valid_markers_cannot_silently_choose_a_source() {
    for markers in [
        ["sync", "sync"],
        ["sync", "okilum"],
        ["okilum", "sync"],
        ["okilum", "okilum"],
    ] {
        let copy = format!(
            "note.{}-conflict-20261104-123456-ABC2345.{}-conflict-20261104-123457-DEF2345.md",
            markers[0], markers[1]
        );
        assert!(
            possible_copy(copy.as_bytes()),
            "inventory must expose ambiguous candidates"
        );
        assert!(
            canonical_path(Path::new(&copy)).is_none(),
            "multiple valid markers need explicit inspection"
        );
    }
}
