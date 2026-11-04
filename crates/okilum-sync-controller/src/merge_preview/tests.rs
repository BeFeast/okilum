use super::*;

fn snapshot(scope: &str, files: &[(&str, &[u8])]) -> Snapshot {
    Snapshot {
        scope: scope.into(),
        revision: "revision-1".into(),
        files: files
            .iter()
            .map(|(p, b)| (p.to_string(), b.to_vec()))
            .collect(),
    }
}
/// Fake serialized filesystem/REST pair, with originals retained before publish.
struct Fake {
    here: Snapshot,
    there: Snapshot,
    originals: Option<(Snapshot, Snapshot)>,
    writes: usize,
}
impl Backend for Fake {
    fn apply_if_unchanged(
        &mut self,
        here: &Snapshot,
        there: &Snapshot,
        merged: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), Error> {
        if &self.here != here || &self.there != there {
            return Err(Error::Stale);
        }
        self.originals = Some((self.here.clone(), self.there.clone()));
        self.here.files = merged.clone();
        self.there.files = merged.clone();
        self.writes += 1;
        Ok(())
    }
}
fn fixture() -> Fake {
    Fake {
        here: snapshot(
            "local/folder",
            &[
                ("equal", b"same"),
                ("binary", b"\xff\x00"),
                ("local", b"L"),
                ("conflict", b"a\r\n"),
                ("conflict.okilum-first-sync-conflict-1", b"reserved"),
            ],
        ),
        there: snapshot(
            "remote/folder",
            &[
                ("equal", b"same"),
                ("binary", b"\xfe\x00"),
                ("remote", b"R"),
                ("conflict", b"a\n"),
            ],
        ),
        originals: None,
        writes: 0,
    }
}
#[test]
fn exact_bytes_merge_converges_without_losing_any_original() {
    let mut fake = fixture();
    let before = (fake.here.clone(), fake.there.clone());
    let plan = Plan::preview(fake.here.clone(), fake.there.clone()).unwrap();
    assert_eq!(plan.count(Kind::Equal), 1);
    assert_eq!(plan.count(Kind::OnlyHere), 2);
    assert_eq!(plan.count(Kind::OnlyThere), 1);
    assert_eq!(plan.count(Kind::Different), 2);
    let entry = plan
        .entries()
        .iter()
        .find(|e| e.path == "conflict")
        .unwrap();
    assert_eq!(
        entry.conflict_path.as_deref(),
        Some("conflict.okilum-first-sync-conflict-2")
    );
    assert_eq!(fake.writes, 0);
    plan.apply(plan.consent(true).unwrap(), &mut fake).unwrap();
    assert_eq!(fake.writes, 1); // Positive control: the same adapter really applies.
    assert_eq!(fake.here.files, fake.there.files);
    assert_eq!(fake.originals, Some(before.clone()));
    for bytes in before.0.files.values().chain(before.1.files.values()) {
        assert!(fake.here.files.values().any(|value| value == bytes));
    }
}
#[test]
fn any_inventory_scope_revision_or_byte_change_aborts_before_writes() {
    for mutation in 0..5 {
        let mut fake = fixture();
        let plan = Plan::preview(fake.here.clone(), fake.there.clone()).unwrap();
        let consent = plan.consent(true).unwrap();
        match mutation {
            0 => fake.here.revision.push('2'),
            1 => fake.there.scope.push('2'),
            2 => {
                fake.there.files.insert("new".into(), b"new".to_vec());
            }
            3 => {
                fake.here.files.remove("equal");
            }
            _ => {
                fake.there.files.insert("equal".into(), b"edit".to_vec());
            }
        }
        assert_eq!(plan.apply(consent, &mut fake), Err(Error::Stale));
        assert_eq!(fake.writes, 0);
        assert!(fake.originals.is_none());
    }
}
#[test]
fn unrelated_requires_explicit_acknowledgement_and_consent_is_plan_bound() {
    let mut fake = fixture();
    fake.here = snapshot("local", &[("a", b"a")]);
    fake.there = snapshot("remote", &[("b", b"b")]);
    let plan = Plan::preview(fake.here.clone(), fake.there.clone()).unwrap();
    assert!(plan.unrelated_warning());
    assert!(matches!(
        plan.consent(false),
        Err(Error::UnrelatedConsentRequired)
    ));
    let consent = plan.consent(true).unwrap();
    fake.here.revision.push('2');
    let replacement = Plan::preview(fake.here.clone(), fake.there.clone()).unwrap();
    assert_eq!(replacement.apply(consent, &mut fake), Err(Error::Stale));
    assert_eq!(fake.writes, 0);
}
#[test]
fn unsafe_paths_and_file_directory_collisions_stop_preview() {
    for path in ["../a", "/absolute", "a//b", "a\\b", "C:drive", "a/./b"] {
        assert_eq!(
            Plan::preview(
                snapshot("a", &[(path, b"a")]),
                snapshot("b", &[("safe", b"b")])
            ),
            Err(Error::InvalidInventory)
        );
    }
    assert_eq!(
        Plan::preview(
            snapshot("a", &[("folder", b"a")]),
            snapshot("b", &[("folder/file", b"b")])
        ),
        Err(Error::InvalidInventory)
    );
}

struct Session {
    fake: Fake,
    fail_remote: bool,
    completed: bool,
}
impl ExclusiveSession for Session {
    fn snapshots(&mut self) -> Result<(Snapshot, Snapshot), Error> {
        Ok((self.fake.here.clone(), self.fake.there.clone()))
    }
    fn preserve_originals(&mut self, here: &Snapshot, there: &Snapshot) -> Result<(), Error> {
        assert!(self.fake.originals.is_none());
        self.fake.originals = Some((here.clone(), there.clone()));
        Ok(())
    }
    fn publish(&mut self, side: Side, files: &BTreeMap<String, Vec<u8>>) -> Result<(), Error> {
        assert!(self.fake.originals.is_some());
        if side == Side::There && self.fail_remote {
            return Err(Error::Backend("REST disconnected".into()));
        }
        match side {
            Side::Here => self.fake.here.files = files.clone(),
            Side::There => self.fake.there.files = files.clone(),
        }
        self.fake.writes += 1;
        Ok(())
    }
    fn complete(&mut self) -> Result<(), Error> {
        assert_eq!(self.fake.here.files, self.fake.there.files);
        self.completed = true;
        Ok(())
    }
}
#[test]
fn guarded_apply_preserves_originals_before_either_publish_and_stops_on_disconnect() {
    for fail_remote in [false, true] {
        let fake = fixture();
        let originals = (fake.here.clone(), fake.there.clone());
        let plan = Plan::preview(fake.here.clone(), fake.there.clone()).unwrap();
        let mut backend = GuardedBackend(Session {
            fake,
            fail_remote,
            completed: false,
        });
        let result = plan.apply(plan.consent(true).unwrap(), &mut backend);
        assert_eq!(result.is_err(), fail_remote);
        assert_eq!(backend.0.completed, !fail_remote);
        assert_eq!(backend.0.fake.writes, if fail_remote { 1 } else { 2 });
        assert_eq!(backend.0.fake.originals, Some(originals));
    }
}

#[cfg(unix)]
mod filesystem_regressions {
    use super::super::filesystem::{FilesystemSession, MutationGate};
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::{
        fs,
        os::unix::fs::{symlink, PermissionsExt},
        path::PathBuf,
    };
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    struct Sandbox {
        base: PathBuf,
    }
    impl Sandbox {
        fn new() -> Self {
            let base = std::env::var_os("OKILUM_MERGE_TEST_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir)
                .join(format!(
                    "okilum-merge-{}-{}",
                    std::process::id(),
                    SEQUENCE.fetch_add(1, Ordering::Relaxed)
                ));
            fs::create_dir(&base).unwrap();
            for name in ["here", "there"] {
                fs::create_dir(base.join(name)).unwrap();
            }
            fs::write(base.join("here/conflict"), b"local\r\n").unwrap();
            fs::write(base.join("here/only-here"), b"\xff\x00").unwrap();
            fs::write(base.join("there/conflict"), b"remote\n").unwrap();
            fs::write(base.join("there/only-there"), b"R").unwrap();
            Self { base }
        }
        fn open(&self) -> FilesystemSession {
            let gate = MutationGate::acquire(&self.base.join("gate")).unwrap();
            FilesystemSession::open(
                &self.base.join("here"),
                &self.base.join("there"),
                &self.base.join("operation"),
                gate,
            )
            .unwrap()
        }
        fn prepare(&self) -> (FilesystemSession, Plan) {
            let mut session = self.open();
            let (here, there) = session.snapshots().unwrap();
            let plan = Plan::preview(here.clone(), there.clone()).unwrap();
            session.preserve_originals(&here, &there).unwrap();
            (session, plan)
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }
    #[test]
    fn preparation_routes_pending_originals_to_recovery_without_publishing() {
        use filesystem::PreparedSession;
        let sandbox = Sandbox::new();
        let original_here = fs::read(sandbox.base.join("here/conflict")).unwrap();
        let original_there = fs::read(sandbox.base.join("there/conflict")).unwrap();
        let PreparedSession::Fresh { plan, mut session } =
            PreparedSession::inspect(sandbox.open()).unwrap()
        else {
            panic!("fresh operation must have a reviewable plan");
        };
        assert!(!sandbox.base.join("operation/originals.v1").exists());
        session.preserve_originals(&plan.here, &plan.there).unwrap();
        session.publish(Side::Here, &plan.merged).unwrap();
        drop(session); // Simulate interruption after one side was published.
        let PreparedSession::Recovery(mut session) =
            PreparedSession::inspect(sandbox.open()).unwrap()
        else {
            panic!("pending originals cannot be replaced by fresh consent");
        };
        assert_eq!(
            fs::read(sandbox.base.join("there/conflict")).unwrap(),
            original_there
        );
        assert!(!sandbox.base.join("operation/complete.v1").exists());
        let preview = session.preview_recovery().unwrap();
        session.recover_previewed(&preview).unwrap();
        assert_eq!(session.snapshots().unwrap().0.files, plan.merged);
        assert_eq!(session.snapshots().unwrap().1.files, plan.merged);
        assert_eq!(
            fs::read(sandbox.base.join("there/conflict")).unwrap(),
            original_here
        );
        drop(session);
        assert!(PreparedSession::inspect(sandbox.open()).is_err());
        assert!(sandbox.base.join("operation/originals.v1").exists());
    }

    #[test]
    fn cancelling_fresh_preparation_keeps_roots_and_operation_unmodified() {
        use filesystem::PreparedSession;
        let sandbox = Sandbox::new();
        let mut session = sandbox.open();
        let before = session.snapshots().unwrap();
        drop(session);
        let prepared = PreparedSession::inspect(sandbox.open()).unwrap();
        drop(prepared); // Close the queued preview before any Apply consent.
        let mut session = sandbox.open();
        assert_eq!(session.snapshots().unwrap(), before);
        assert_eq!(
            session.recovery_status().unwrap(),
            filesystem::RecoveryStatus::Fresh
        );
        assert!(!sandbox.base.join("operation/originals.v1").exists());
        // Positive control: the released guard can be reacquired and applied.
        drop(session);
        let PreparedSession::Fresh { plan, session } =
            PreparedSession::inspect(sandbox.open()).unwrap()
        else {
            panic!("cancelled preparation must remain fresh");
        };
        plan.apply(plan.consent(true).unwrap(), &mut GuardedBackend(session))
            .unwrap();
        assert!(sandbox.base.join("operation/complete.v1").exists());
    }

    #[test]
    fn actual_filesystem_apply_is_lossless_and_retains_permissions_and_private_originals() {
        let sandbox = Sandbox::new();
        fs::set_permissions(
            sandbox.base.join("there/conflict"),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        let mut session = sandbox.open();
        let (here, there) = session.snapshots().unwrap();
        let plan = Plan::preview(here, there).unwrap();
        let mut backend = GuardedBackend(session);
        plan.apply(plan.consent(true).unwrap(), &mut backend)
            .unwrap();
        assert_eq!(
            fs::read(sandbox.base.join("there/conflict")).unwrap(),
            b"local\r\n"
        );
        assert_eq!(
            fs::read(
                sandbox
                    .base
                    .join("here/conflict.okilum-first-sync-conflict-1")
            )
            .unwrap(),
            b"remote\n"
        );
        assert_eq!(
            fs::metadata(sandbox.base.join("there/conflict"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert_eq!(
            fs::metadata(sandbox.base.join("operation/originals.v1"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let bytes = fs::read(sandbox.base.join("operation/originals.v1")).unwrap();
        assert!(bytes.windows(b"local\r\n".len()).any(|b| b == b"local\r\n"));
        assert!(bytes.windows(b"remote\n".len()).any(|b| b == b"remote\n"));
        assert!(sandbox.base.join("operation/complete.v1").exists());
    }
    #[test]
    fn reopen_recovers_before_publish_after_one_side_and_after_both_sides() {
        for published in 0..=2 {
            let sandbox = Sandbox::new();
            let (mut session, plan) = sandbox.prepare();
            if published >= 1 {
                session.publish(Side::Here, &plan.merged).unwrap();
            }
            if published >= 2 {
                session.publish(Side::There, &plan.merged).unwrap();
            }
            // Simulate a leftover staged write that never reached canonical files.
            fs::write(sandbox.base.join("operation/staging"), b"incomplete").unwrap();
            drop(session);
            let mut reopened = sandbox.open();
            reopened.recover().unwrap();
            let (here, there) = reopened.snapshots().unwrap();
            assert_eq!(here.files, plan.merged);
            assert_eq!(there.files, plan.merged);
            reopened.recover().unwrap(); // Lost completion response is idempotent.
        }
    }
    #[test]
    fn recovery_preflights_both_sides_and_preserves_third_version_additions_and_deletions() {
        for mutation in 0..3 {
            let sandbox = Sandbox::new();
            let (mut session, plan) = sandbox.prepare();
            session.publish(Side::Here, &plan.merged).unwrap();
            drop(session);
            match mutation {
                0 => fs::write(sandbox.base.join("there/conflict"), b"new edit").unwrap(),
                1 => fs::write(sandbox.base.join("there/new"), b"new file").unwrap(),
                _ => fs::remove_file(sandbox.base.join("there/conflict")).unwrap(),
            }
            let mut reopened = sandbox.open();
            let before = reopened.snapshots().unwrap();
            assert_eq!(reopened.recover(), Err(Error::Stale));
            assert_eq!(reopened.snapshots().unwrap(), before);
            assert!(!sandbox.base.join("operation/complete.v1").exists());
        }
    }
    #[test]
    fn shared_gate_excludes_another_operation_and_releases_on_drop() {
        let sandbox = Sandbox::new();
        let session = sandbox.open();
        assert!(MutationGate::acquire(&sandbox.base.join("gate")).is_err());
        drop(session);
        assert!(MutationGate::acquire(&sandbox.base.join("gate")).is_ok());
    }
    #[test]
    fn completed_operation_never_overwrites_later_edits_or_restarts_with_old_consent() {
        let sandbox = Sandbox::new();
        let (mut session, plan) = sandbox.prepare();
        session.recover().unwrap();
        drop(session);
        // Even an exact original value must not trigger replay after completion.
        fs::write(sandbox.base.join("there/conflict"), b"remote\n").unwrap();
        let mut session = sandbox.open();
        assert_eq!(session.recover(), Err(Error::Stale));
        assert_eq!(
            fs::read(sandbox.base.join("there/conflict")).unwrap(),
            b"remote\n"
        );
        assert!(session.publish(Side::There, &plan.merged).is_err());
    }
    #[test]
    fn stale_preview_corrupt_journal_symlinks_and_replaced_roots_stop() {
        let sandbox = Sandbox::new();
        let mut session = sandbox.open();
        let (here, there) = session.snapshots().unwrap();
        fs::write(sandbox.base.join("here/conflict"), b"later edit").unwrap();
        assert_eq!(session.preserve_originals(&here, &there), Err(Error::Stale));
        assert!(!sandbox.base.join("operation/originals.v1").exists());
        symlink(
            sandbox.base.join("there/conflict"),
            sandbox.base.join("here/linked"),
        )
        .unwrap();
        assert_eq!(session.snapshots(), Err(Error::InvalidInventory));
        fs::remove_file(sandbox.base.join("here/linked")).unwrap();
        fs::write(sandbox.base.join("operation/originals.v1"), b"truncated").unwrap();
        assert!(session.recover().is_err());
        fs::rename(sandbox.base.join("here"), sandbox.base.join("old-here")).unwrap();
        fs::create_dir(sandbox.base.join("here")).unwrap();
        assert_eq!(session.snapshots(), Err(Error::Stale));
    }
    #[test]
    fn reject_state_inside_vault_or_index_before_creating_it() {
        let sandbox = Sandbox::new();
        let gate = MutationGate::acquire(&sandbox.base.join("gate")).unwrap();
        assert!(FilesystemSession::open(
            &sandbox.base.join("here"),
            &sandbox.base.join("there"),
            &sandbox.base.join("here/state"),
            gate
        )
        .is_err());
        assert!(!sandbox.base.join("here/state").exists());
    }
    #[test]
    fn recovery_handles_partial_file_publication_and_detects_checksum_corruption() {
        use super::super::filesystem::RecoveryStatus;
        let sandbox = Sandbox::new();
        let (session, plan) = sandbox.prepare();
        assert_eq!(session.recovery_status().unwrap(), RecoveryStatus::Pending);
        drop(session);
        // Crash after a single file rename, not a whole-side publish.
        let (path, bytes) = plan
            .merged
            .iter()
            .find(|(p, _)| p.as_str() == "conflict")
            .unwrap();
        fs::write(sandbox.base.join("there").join(path), bytes).unwrap();
        let mut session = sandbox.open();
        session.recover().unwrap();
        assert_eq!(session.recovery_status().unwrap(), RecoveryStatus::Complete);
        drop(session);

        let sandbox = Sandbox::new();
        let (session, _) = sandbox.prepare();
        drop(session);
        let journal = sandbox.base.join("operation/originals.v1");
        let mut bytes = fs::read(&journal).unwrap();
        let index = bytes
            .windows(b"remote\n".len())
            .position(|b| b == b"remote\n")
            .unwrap();
        bytes[index] ^= 1;
        fs::write(&journal, bytes).unwrap();
        let mut session = sandbox.open();
        let before = session.snapshots().unwrap();
        assert!(session.recover().is_err());
        assert_eq!(session.snapshots().unwrap(), before);
    }
    #[test]
    fn replaced_gate_or_state_stops_existing_session_and_reversed_scope_cannot_recover() {
        let sandbox = Sandbox::new();
        let session = sandbox.open();
        fs::rename(
            sandbox.base.join("gate/mutation.lock"),
            sandbox.base.join("gate/old.lock"),
        )
        .unwrap();
        fs::write(sandbox.base.join("gate/mutation.lock"), b"").unwrap();
        assert_eq!(session.recovery_status(), Err(Error::Stale));
        drop(session);

        let sandbox = Sandbox::new();
        let (session, _) = sandbox.prepare();
        drop(session);
        let gate = MutationGate::acquire(&sandbox.base.join("gate")).unwrap();
        let mut reversed = FilesystemSession::open(
            &sandbox.base.join("there"),
            &sandbox.base.join("here"),
            &sandbox.base.join("operation"),
            gate,
        )
        .unwrap();
        assert_eq!(reversed.recover(), Err(Error::Stale));
        drop(reversed);
        let mut session = sandbox.open();
        fs::rename(
            sandbox.base.join("operation"),
            sandbox.base.join("old-operation"),
        )
        .unwrap();
        fs::create_dir(sandbox.base.join("operation")).unwrap();
        assert_eq!(session.snapshots(), Err(Error::Stale));
    }
    #[test]
    fn idle_and_returned_session_cleanup_precedes_whole_completion_and_gate_release() {
        use super::super::completion::Work;
        use std::{
            future::Future,
            pin::Pin,
            sync::mpsc,
            task::{Context, Poll, Waker},
        };
        let sandbox = Sandbox::new();
        let work = Work::default();
        let owner = work.reserve().unwrap().owned(sandbox.open());
        let gate_path = sandbox.base.join("gate");
        let callback_gate = gate_path.clone();
        let (send, receive) = mpsc::channel();
        work.when_returned(Box::new(move |snapshot| {
            // Positive control: whole return occurs only AFTER the real session
            // releases its shared gate, not merely after a worker yielded it.
            assert!(MutationGate::acquire(&callback_gate).is_ok());
            send.send(snapshot).unwrap();
        }));
        let next = work.reserve().unwrap();
        let queued = owner.into_queued(next, |session| session);
        work.close();
        let mut job = Box::pin(queued.job(std::future::ready));
        let waker = Waker::noop().clone();
        let Poll::Ready(output) = Pin::as_mut(&mut job).poll(&mut Context::from_waker(&waker))
        else {
            panic!("fixture returns");
        };
        drop(job);
        assert!(MutationGate::acquire(&gate_path).is_err());
        assert!(receive.try_recv().is_err());
        drop(output);
        assert!(receive.try_recv().unwrap().returned());
    }

    #[test]
    fn recovery_review_is_read_only_exact_and_cancel_preserves_pending_operation() {
        use super::super::filesystem::RecoveryStatus;
        let sandbox = Sandbox::new();
        let (mut session, plan) = sandbox.prepare();
        session.publish(Side::Here, &plan.merged).unwrap();
        let before = session.snapshots().unwrap();
        let originals = fs::read(sandbox.base.join("operation/originals.v1")).unwrap();
        assert_eq!(
            session.operation_directory(),
            sandbox.base.join("operation")
        );
        assert_eq!(
            session.originals_path(),
            sandbox.base.join("operation/originals.v1")
        );
        assert_eq!(
            session.roots(),
            [
                sandbox.base.join("here").as_path(),
                sandbox.base.join("there").as_path()
            ]
        );
        let preview = session.preview_recovery().unwrap();
        assert_eq!(
            preview.operation_directory(),
            sandbox.base.join("operation")
        );
        assert_eq!(
            preview.originals_path(),
            sandbox.base.join("operation/originals.v1")
        );
        assert_eq!(
            preview.roots(),
            [
                sandbox.base.join("here").as_path(),
                sandbox.base.join("there").as_path()
            ]
        );
        use std::os::unix::fs::MetadataExt;
        let operation_metadata = fs::metadata(sandbox.base.join("operation")).unwrap();
        assert_eq!(
            preview.operation_identity(),
            (operation_metadata.dev(), operation_metadata.ino())
        );
        assert_eq!(preview.original_counts(), [2, 2]);
        assert_eq!(preview.original_bytes(), [9, 8]);
        assert_eq!(preview.remaining_counts(), [0, 3]);
        assert_eq!(preview.plan(), &plan);
        assert_eq!(preview.originals_digest().len(), 64);
        assert!(preview
            .revision_fingerprints()
            .iter()
            .all(|revision| revision.len() == 64));
        assert_eq!(session.snapshots().unwrap(), before);
        assert_eq!(
            fs::read(sandbox.base.join("operation/originals.v1")).unwrap(),
            originals
        );
        // Cancel drops the read-only session/token; it has no mutation method.
        drop(preview);
        drop(session);
        let mut session = sandbox.open();
        assert_eq!(session.recovery_status().unwrap(), RecoveryStatus::Pending);
        assert_eq!(session.snapshots().unwrap(), before);
        assert_eq!(
            fs::read(sandbox.base.join("operation/originals.v1")).unwrap(),
            originals
        );
        let preview = session.preview_recovery().unwrap();
        session.recover_previewed(&preview).unwrap(); // Positive control after new consent.
        assert_eq!(session.recovery_status().unwrap(), RecoveryStatus::Complete);
        assert_eq!(
            fs::read(sandbox.base.join("operation/originals.v1")).unwrap(),
            originals
        );
    }
    #[test]
    fn explicit_recovery_refuses_any_changed_revision_before_either_side_is_written() {
        for metadata_only in [false, true] {
            let sandbox = Sandbox::new();
            let (mut session, plan) = sandbox.prepare();
            let preview = session.preview_recovery().unwrap();
            if metadata_only {
                let path = sandbox.base.join("there/conflict");
                let mode = fs::metadata(&path).unwrap().permissions().mode();
                fs::set_permissions(path, fs::Permissions::from_mode(mode ^ 0o100)).unwrap();
            } else {
                // This is a valid partial merge, so plain recover could accept
                // it. A user's already displayed preview must still be stale.
                fs::write(
                    sandbox.base.join("there/conflict"),
                    &plan.merged["conflict"],
                )
                .unwrap();
            }
            let before = session.snapshots().unwrap();
            let journal = fs::read(sandbox.base.join("operation/originals.v1")).unwrap();
            assert_eq!(session.recover_previewed(&preview), Err(Error::Stale));
            assert_eq!(session.snapshots().unwrap(), before);
            assert_eq!(
                fs::read(sandbox.base.join("operation/originals.v1")).unwrap(),
                journal
            );
            assert!(!sandbox.base.join("operation/complete.v1").exists());
            let refreshed = session.preview_recovery().unwrap();
            assert_ne!(
                refreshed.revision_fingerprints(),
                preview.revision_fingerprints()
            );
            session.recover_previewed(&refreshed).unwrap();
        }
    }
    #[test]
    fn recovery_consent_cannot_be_transplanted_to_another_pending_operation() {
        let sandbox = Sandbox::new();
        let (mut session, _) = sandbox.prepare();
        let preview = session.preview_recovery().unwrap();
        drop(session);
        fs::rename(
            sandbox.base.join("operation"),
            sandbox.base.join("previous-operation"),
        )
        .unwrap();
        let (mut new_operation, _) = sandbox.prepare();
        assert_eq!(new_operation.recover_previewed(&preview), Err(Error::Stale));
        // Same roots, bytes and journal hash; different operation generation.
        let modified = preview.clone();
        // Use the unchanged token against a different named operation instead.
        drop(new_operation);
        fs::rename(
            sandbox.base.join("operation"),
            sandbox.base.join("other-operation"),
        )
        .unwrap();
        let gate = MutationGate::acquire(&sandbox.base.join("gate")).unwrap();
        let mut other = FilesystemSession::open(
            &sandbox.base.join("here"),
            &sandbox.base.join("there"),
            &sandbox.base.join("other-operation"),
            gate,
        )
        .unwrap();
        assert_eq!(other.recover_previewed(&modified), Err(Error::Stale));
    }
}

mod completion_regressions {
    use super::super::completion::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::{
        future::Future,
        pin::Pin,
        sync::Arc,
        task::{Context, Poll},
    };
    struct PendingCleanup(Arc<AtomicBool>, Arc<AtomicBool>);
    impl Future for PendingCleanup {
        type Output = ();
        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
            if self.0.load(Ordering::Acquire) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }
    }
    impl Drop for PendingCleanup {
        fn drop(&mut self) {
            self.1.store(true, Ordering::Release);
        }
    }
    #[test]
    fn cancel_and_return_signal_wait_for_actual_future_cleanup() {
        let work = Work::default();
        let release = Arc::new(AtomicBool::new(false));
        let cleaned = Arc::new(AtomicBool::new(false));
        let mut future = Box::pin(
            work.reserve()
                .unwrap()
                .queued(PendingCleanup(release.clone(), cleaned.clone()))
                .join(|future| future),
        );
        let waker = std::task::Waker::noop().clone();
        let mut cx = Context::from_waker(&waker);
        assert!(future.as_mut().poll(&mut cx).is_pending());
        let (send, receive) = std::sync::mpsc::channel();
        let observed = cleaned.clone();
        work.when_returned(Box::new(move |snapshot| {
            assert!(observed.load(Ordering::Acquire));
            send.send(snapshot).unwrap();
        }));
        work.close();
        assert!(receive.try_recv().is_err());
        assert_eq!(work.snapshot().pending, 1);
        assert!(
            work.reserve().is_none(),
            "close prevents late work admission"
        );
        release.store(true, Ordering::Release);
        assert!(future.as_mut().poll(&mut cx).is_ready());
        assert!(receive.try_recv().unwrap().returned());
    }
    #[test]
    fn dropping_unpolled_work_destroys_captures_before_completion() {
        let work = Work::default();
        let cleaned = Arc::new(AtomicBool::new(false));
        let future = work.reserve().unwrap().track(PendingCleanup(
            Arc::new(AtomicBool::new(false)),
            cleaned.clone(),
        ));
        let observed = cleaned.clone();
        let (send, receive) = std::sync::mpsc::channel();
        work.when_returned(Box::new(move |snapshot| {
            assert!(observed.load(Ordering::Acquire));
            send.send(snapshot).unwrap();
        }));
        work.close();
        assert!(receive.try_recv().is_err());
        drop(future);
        assert!(receive.try_recv().unwrap().returned());
    }
    #[test]
    fn queue_cancel_before_first_poll_releases_owned_resources_first() {
        let work = Work::default();
        let cleaned = Arc::new(AtomicBool::new(false));
        let queued = work.reserve().unwrap().queued(PendingCleanup(
            Arc::new(AtomicBool::new(false)),
            cleaned.clone(),
        ));
        let observed = cleaned.clone();
        let (send, receive) = std::sync::mpsc::channel();
        work.when_returned(Box::new(move |snapshot| {
            assert!(observed.load(Ordering::Acquire));
            send.send(snapshot).unwrap();
        }));
        work.close();
        drop(queued);
        assert!(receive.try_recv().unwrap().returned());
    }
    struct OutputResource(Arc<AtomicBool>);
    impl Drop for OutputResource {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    #[test]
    fn parent_cancel_waits_for_worker_and_returned_resource_cleanup() {
        let work = Work::default();
        let parent = work.reserve().unwrap().queued(());
        let worker_cleaned = Arc::new(AtomicBool::new(false));
        let output_cleaned = Arc::new(AtomicBool::new(false));
        let output = OutputResource(output_cleaned.clone());
        let cleanup = PendingCleanup(Arc::new(AtomicBool::new(true)), worker_cleaned.clone());
        let mut job = Box::pin(work.reserve().unwrap().queued((cleanup, output)).job(
            |(cleanup, output)| async move {
                drop(cleanup);
                output
            },
        ));
        let (send, receive) = std::sync::mpsc::channel();
        let a = worker_cleaned.clone();
        let b = output_cleaned.clone();
        work.when_returned(Box::new(move |snapshot| {
            assert!(a.load(Ordering::Acquire) && b.load(Ordering::Acquire));
            send.send(snapshot).unwrap();
        }));
        work.close();
        drop(parent);
        assert_eq!(work.snapshot().pending, 1);
        assert!(receive.try_recv().is_err());
        let waker = std::task::Waker::noop().clone();
        let mut cx = Context::from_waker(&waker);
        let Poll::Ready(returned) = job.as_mut().poll(&mut cx) else {
            panic!("positive control: worker returns");
        };
        assert!(worker_cleaned.load(Ordering::Acquire));
        assert!(!output_cleaned.load(Ordering::Acquire));
        assert!(
            receive.try_recv().is_err(),
            "result still owns its resource"
        );
        returned.consume(drop);
        assert!(receive.try_recv().unwrap().returned());
    }
    #[test]
    fn readback_and_source_changed_are_not_whole_child_completion() {
        let work = Work::unknown();
        work.readback(Readback {
            here_scope: "a".into(),
            here_revision: "r1".into(),
            there_scope: "b".into(),
            there_revision: "r2".into(),
            merged_sha256: "verified-content".into(),
        });
        work.close();
        let snapshot = work.snapshot();
        assert!(snapshot.readback.is_some());
        assert!(!snapshot.returned());
        assert!(snapshot.opaque);
        assert!(
            !work.same(&Work::default()),
            "a fresh mount cannot reuse old completion"
        );
        assert!(work.same(&work.clone()));
    }
}

mod completion_ownership_regressions {
    use super::super::completion::*;
    use std::{
        future::Future,
        pin::Pin,
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc,
        },
        task::{Context, Poll, Waker},
    };
    struct Resource {
        cleaned: Arc<AtomicBool>,
        value: u8,
    }
    impl Drop for Resource {
        fn drop(&mut self) {
            self.cleaned.store(true, Ordering::Release);
        }
    }
    fn poll<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }
    fn fixture() -> (Work, Resource, mpsc::Receiver<Snapshot>) {
        let work = Work::default();
        let cleaned = Arc::new(AtomicBool::new(false));
        let observed = cleaned.clone();
        let (send, receive) = mpsc::channel();
        work.when_returned(Box::new(move |snapshot| {
            assert!(
                observed.load(Ordering::Acquire),
                "resource cleanup must precede return notification"
            );
            send.send(snapshot).unwrap();
        }));
        (work, Resource { cleaned, value: 1 }, receive)
    }
    #[test]
    fn idle_owned_borrows_keep_resource_counted_until_actual_cleanup() {
        let (work, resource, receive) = fixture();
        let mut owner = work.reserve().unwrap().owned(resource);
        assert_eq!(owner.as_ref().value, 1);
        owner.as_mut().value = 2;
        assert_eq!(owner.as_ref().value, 2);
        work.close();
        work.close();
        assert!(work.reserve().is_none());
        assert_eq!(work.snapshot().pending, 1);
        assert!(!work.snapshot().returned());
        assert!(receive.try_recv().is_err());
        drop(owner);
        assert!(receive.try_recv().unwrap().returned());
    }
    #[test]
    fn idle_to_queue_to_output_has_no_uncounted_ownership_gap() {
        let (work, resource, receive) = fixture();
        let owner = work.reserve().unwrap().owned(resource);
        let queued_reservation = work.reserve().unwrap();
        assert_eq!(work.snapshot().pending, 2);
        let queued = owner.into_queued(queued_reservation, |resource| resource);
        assert_eq!(work.snapshot().pending, 1);
        work.close();
        let mut job = Box::pin(queued.job(std::future::ready));
        let Poll::Ready(output) = poll(job.as_mut()) else {
            panic!("ready fixture must execute");
        };
        drop(job);
        assert_eq!(
            work.snapshot().pending,
            1,
            "executor return still owns its output"
        );
        assert!(receive.try_recv().is_err());
        assert_eq!(output.as_ref().value, 1);
        output.consume(drop);
        assert!(receive.try_recv().unwrap().returned());
    }
    #[test]
    fn close_before_first_poll_drops_queued_captures_before_notification() {
        let (work, resource, receive) = fixture();
        let owner = work.reserve().unwrap().owned(resource);
        let reservation = work.reserve().unwrap();
        let queued = owner.into_queued(reservation, |resource| resource);
        let future = queued.join(|resource| async move {
            drop(resource);
        });
        work.close();
        assert!(receive.try_recv().is_err());
        drop(future);
        assert!(receive.try_recv().unwrap().returned());
    }
    #[test]
    fn abandoned_parent_does_not_certify_a_separately_owned_worker_output() {
        let (work, resource, receive) = fixture();
        let parent = work
            .reserve()
            .unwrap()
            .queued(())
            .join(|_| std::future::pending());
        let mut job = Box::pin(
            work.reserve()
                .unwrap()
                .queued(resource)
                .job(std::future::ready),
        );
        work.close();
        drop(parent);
        assert_eq!(work.snapshot().pending, 1);
        assert!(receive.try_recv().is_err());
        let Poll::Ready(output) = poll(job.as_mut()) else {
            panic!("ready worker positive control");
        };
        drop(job);
        assert!(receive.try_recv().is_err());
        drop(output);
        assert!(receive.try_recv().unwrap().returned());
    }
    #[test]
    fn panicking_payload_conversion_still_cleans_before_reservation_release() {
        let (work, resource, receive) = fixture();
        let owner = work.reserve().unwrap().owned(resource);
        let next = work.reserve().unwrap();
        work.close();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Queued<()> =
                owner.into_queued(next, |_resource| panic!("conversion interrupted"));
        }));
        assert!(panic.is_err());
        assert!(receive.try_recv().unwrap().returned());
    }
}
