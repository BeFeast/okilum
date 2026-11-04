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

#[test]
fn issue2044_refusal_at_second_check_leaves_no_recovery_authority() {
    let (vault, state) = fixture();
    let p = preview(vault.path(), Path::new(COPY)).unwrap();
    assert!(apply_with_recheck_hook(
        &p,
        Choice::KeepThat,
        state.path(),
        || {
            std::fs::write(vault.path().join(COPY), b"later external version").unwrap();
        },
        || {},
        || {}
    )
    .is_err());
    assert!(history(state.path(), vault.path())
        .unwrap()
        .entries
        .is_empty());
    // Another computer/editor later resolves it. Restart must not resurrect it.
    std::fs::remove_file(vault.path().join(COPY)).unwrap();
    assert!(recover_startup(state.path()).unwrap().is_empty());
    assert!(!vault.path().join(COPY).exists());
}

#[test]
fn issue2044_failed_complete_marker_is_terminal_and_preserves_history() {
    for block_refusal_marker in [false, true] {
        let (vault, state) = fixture();
        let p = preview(vault.path(), Path::new(COPY)).unwrap();
        let outcome = apply_with_hooks(
            &p,
            Choice::KeepThat,
            state.path(),
            || {},
            || {
                for entry in std::fs::read_dir(state.path()).unwrap() {
                    let record = entry.unwrap().path();
                    if record.extension() == Some(OsStr::new("json")) {
                        // Exercise actual filesystem errors at marker creation;
                        // mode-bit failures do not work on privileged runners.
                        std::fs::create_dir(record.with_extension("complete")).unwrap();
                        if block_refusal_marker {
                            std::fs::create_dir(record.with_extension("refused")).unwrap();
                        }
                    }
                }
            },
        );
        let ApplyOutcome::NeedsAttention {
            recovery: receipt,
            reason,
        } = outcome.unwrap()
        else {
            panic!("failed acknowledgement cannot report success");
        };
        assert!(reason.contains("completion failed"));
        assert_eq!(
            retained_versions(&receipt.recovery_record).unwrap()[1].bytes,
            p.that.bytes
        );
        let history = history(state.path(), vault.path()).unwrap();
        assert!(history.warnings.is_empty());
        assert_eq!(history.entries.len(), 1);
        assert!(!history.entries[0].pending);
        assert!(!vault.path().join(COPY).exists());
        assert_eq!(
            std::fs::read(vault.path().join("note.md")).unwrap(),
            p.that.bytes
        );
        // Even deleting the resolved canonical file later grants no authority.
        std::fs::remove_file(vault.path().join("note.md")).unwrap();
        assert!(recover_startup(state.path()).unwrap().is_empty());
        assert!(!vault.path().join("note.md").exists());
        assert!(!vault.path().join(COPY).exists());
    }
}

#[test]
fn issue2044_shared_folders_require_proven_local_retention_before_mutation() {
    for policy in [
        None,
        Some(".okilum-index\n"),
        Some("!.okilum-save-*\n.okilum-save-*\n"),
        Some("!*\n.okilum-save-*\n"),
        Some("#include policy\n.okilum-save-*\n"),
        Some("/.okilum-save-*\n"),
    ] {
        let (vault, state) = fixture();
        std::fs::create_dir(vault.path().join(".stfolder")).unwrap();
        if let Some(policy) = policy {
            std::fs::write(vault.path().join(".stignore"), policy).unwrap();
        }
        let p = preview(vault.path(), Path::new(COPY)).unwrap();
        assert!(
            apply(&p, Choice::KeepThat, state.path()).is_err(),
            "{policy:?}"
        );
        assert_eq!(
            std::fs::read(vault.path().join("note.md")).unwrap(),
            p.this.bytes
        );
        assert_eq!(
            std::fs::read(vault.path().join(COPY)).unwrap(),
            p.that.bytes
        );
        assert_eq!(std::fs::read_dir(state.path()).unwrap().count(), 0);
        assert_eq!(
            std::fs::read_dir(vault.path()).unwrap().count(),
            3 + usize::from(policy.is_some())
        );
    }
    for policy in [
        ".okilum-save-*\n",
        "**/.okilum-save-*\n",
        "(?i)(?d).okilum-save-*\n",
        ".okilum-save-*\n!*\n",
    ] {
        let (vault, state) = fixture();
        std::fs::create_dir(vault.path().join(".stfolder")).unwrap();
        std::fs::write(vault.path().join(".stignore"), policy).unwrap();
        let p = preview(vault.path(), Path::new(COPY)).unwrap();
        assert!(
            matches!(
                apply(&p, Choice::KeepThat, state.path()).unwrap(),
                ApplyOutcome::Applied(_)
            ),
            "{policy}"
        );
    }
}

#[test]
fn issue2044_hidden_and_extensionless_notes_have_inventory_preview_and_resolution() {
    for (note, copy) in [
        ("TODO", "TODO.okilum-conflict-20261104-123456-ABC2345"),
        ("TODO", "TODO.okilum-conflict-20261104-123456-ABC2345."),
        ("TODO", "TODO.sync-conflict-20261104-123456-ABC2345"),
        (".hidden", ".sync-conflict-20261104-123456-ABC2345.hidden"),
        (".hidden", ".hidden.okilum-conflict-20261104-123456-ABC2345"),
    ] {
        let vault = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(vault.path().join(note), b"this").unwrap();
        std::fs::write(vault.path().join(copy), b"that").unwrap();
        assert_eq!(canonical_path(Path::new(copy)), Some(note.into()));
        assert_eq!(
            inspect_note(vault.path(), Path::new(note)).unwrap().copies,
            [PathBuf::from(copy)]
        );
        let p = preview(vault.path(), Path::new(copy)).unwrap();
        assert!(matches!(
            apply(&p, Choice::KeepThat, state.path()).unwrap(),
            ApplyOutcome::Applied(_)
        ));
        assert_eq!(std::fs::read(vault.path().join(note)).unwrap(), b"that");
    }
    assert!(canonical_path(Path::new(".sync-conflict-20261104-123456-ABC2345")).is_none());
    assert!(canonical_path(Path::new(".sync-conflict-20261104-123456-ABC2345.")).is_none());
}

#[test]
fn issue2044_symlink_vault_alias_supports_inventory_preview_and_interrupted_recovery() {
    let (vault, state) = fixture();
    let aliases = tempfile::tempdir().unwrap();
    let alias = aliases.path().join("vault-alias");
    std::os::unix::fs::symlink(vault.path(), &alias).unwrap();
    assert_eq!(inspect(&alias).unwrap().copies, [PathBuf::from(COPY)]);
    assert_eq!(
        inspect_note(&alias, Path::new("note.md")).unwrap().copies,
        [PathBuf::from(COPY)]
    );
    let p = preview(&alias, Path::new(COPY)).unwrap();
    assert_eq!(p.root, vault.path().canonicalize().unwrap());
    let ApplyOutcome::Applied(receipt) = apply(&p, Choice::KeepBoth, state.path()).unwrap() else {
        panic!("not applied")
    };
    std::fs::remove_file(receipt.recovery_record.with_extension("complete")).unwrap();
    std::fs::remove_file(vault.path().join("note.md")).unwrap();
    assert_eq!(recover_interrupted(state.path(), &alias).unwrap().len(), 1);
    assert_eq!(
        std::fs::read(vault.path().join("note.md")).unwrap(),
        p.this.bytes
    );
}

#[test]
#[ignore = "restart subprocess control; invoked by issue2044_crash_gap_restarts_before_indexing"]
fn issue2044_startup_child() {
    let root = PathBuf::from(std::env::var_os("OKILUM_CONFLICT_CRASH_ROOT").unwrap());
    let messages = recover_startup(&root.join("state")).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        std::fs::read(root.join("vault/note.md")).unwrap(),
        b"this before crash"
    );
    assert_eq!(
        std::fs::read(root.join("vault").join(COPY)).unwrap(),
        b"that before crash"
    );
    // The first filesystem inventory after restart sees the repaired canonical
    // path; no UI action or previously opened note is needed.
    assert!(root.join("vault/note.md").is_file());
    assert_eq!(
        inspect(&root.join("vault")).unwrap().copies,
        [PathBuf::from(COPY)]
    );
}

#[test]
fn issue2044_crash_gap_restarts_before_indexing() {
    for step in ["capture-this", "capture-that"] {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("vault")).unwrap();
        std::fs::create_dir(root.path().join("state")).unwrap();
        std::fs::write(root.path().join("vault/note.md"), b"this before crash").unwrap();
        std::fs::write(root.path().join("vault").join(COPY), b"that before crash").unwrap();
        let run = |name: &str, crash: Option<&str>| {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([name, "--exact", "--ignored", "--nocapture"])
                .env("OKILUM_CONFLICT_CRASH_ROOT", root.path());
            if let Some(step) = crash {
                child.env("OKILUM_CONFLICT_CRASH_STEP", step);
            } else {
                child.env_remove("OKILUM_CONFLICT_CRASH_STEP");
            }
            child.status().unwrap()
        };
        assert_eq!(
            run(
                "conflicts::resolution_tests::issue2044_crash_child",
                Some(step)
            )
            .code(),
            Some(86)
        );
        assert!(
            !root.path().join("vault/note.md").exists(),
            "positive gap control: {step}"
        );
        assert!(run("conflicts::resolution_tests::issue2044_startup_child", None).success());
        assert_eq!(
            std::fs::read(root.path().join("vault/note.md")).unwrap(),
            b"this before crash"
        );
    }
}

#[test]
fn issue2044_startup_skips_terminal_records_even_after_vault_deletion() {
    let (vault, state) = fixture();
    let p = preview(vault.path(), Path::new(COPY)).unwrap();
    let ApplyOutcome::Applied(receipt) = apply(&p, Choice::KeepThis, state.path()).unwrap() else {
        panic!("not applied");
    };
    std::fs::remove_dir_all(vault.path()).unwrap();
    assert!(
        recover_startup(state.path()).unwrap().is_empty(),
        "completed records cannot warn about intentionally deleted vaults"
    );
    std::fs::remove_file(receipt.recovery_record.with_extension("complete")).unwrap();
    let mut record = receipt.recovery_record.clone();
    recovery::refused(&mut record).unwrap();
    assert!(recover_startup(state.path()).unwrap().is_empty());
    // Terminal journals remain History evidence; no startup JSON read is needed.
    std::fs::write(&record, b"unreadable old terminal payload").unwrap();
    assert!(recover_startup(state.path()).unwrap().is_empty());
}
