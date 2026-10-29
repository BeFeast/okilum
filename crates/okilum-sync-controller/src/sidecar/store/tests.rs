use super::{UnixStore as Store, *};
use crate::sidecar::{
    authority::Reason,
    journal::{Fault, UnixJournal},
    supervisor::ipc::Scope,
    update::{Phase, Runtime, Update},
    Binding, Intent, LockedJournal,
};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
};
use std::{
    path::Path,
    time::{Duration, Instant},
};
use uuid::Uuid;

fn binding() -> Binding {
    Binding {
        instance: Uuid::from_u128(2),
        installation: Uuid::from_u128(1),
        owner: rustix::process::geteuid().as_raw().to_string(),
        supervisor: "/Applications/Okilum.app/Contents/MacOS/supervisor".into(),
        state_directory: "/private/state".into(),
        device_identity: "existing-device".into(),
    }
}
fn scope() -> Scope {
    Scope {
        installation: Uuid::from_u128(1),
        instance: Uuid::from_u128(2),
        generation: Uuid::from_u128(9),
    }
}
fn runtime(version: &str) -> Runtime {
    Runtime {
        version: version.into(),
        digest: format!("digest-{version}"),
        location: format!("/private/runtime/{version}"),
    }
}
fn soon() -> Instant {
    Instant::now() + Duration::from_millis(100)
}
fn directory() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    dir
}
fn put(dir: &Path, name: &str, data: &[u8]) {
    let path = dir.join(name);
    fs::write(&path, data).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn legacy(dir: &Path, intent: Intent, update: Option<(Phase, Intent)>) {
    let journal = Journal {
        binding: binding(),
        intent,
    };
    put(dir, NAME, &serde_json::to_vec(&journal).unwrap());
    if let Some((phase, stale)) = update {
        let update = Update {
            binding: binding(),
            previous: runtime("old"),
            candidate: runtime("new"),
            intent: stale,
            phase,
            rolled_back: false,
        };
        put(dir, LEGACY_UPDATE, &serde_json::to_vec(&update).unwrap());
    }
}
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    names.sort();
    names
}
/// A store whose journal already holds `first` (revision 1).
fn started() -> (tempfile::TempDir, Store) {
    let dir = directory();
    let store = Store::open_existing(dir.path()).unwrap();
    store
        .begin(soon())
        .unwrap()
        .commit(Envelope::first(binding()))
        .unwrap();
    (dir, store)
}
fn load(store: &Store) -> Stored {
    store.begin(soon()).unwrap().state().unwrap().clone()
}

#[test]
fn absent_is_inert_and_first_commit_survives_reopen_with_private_mode() {
    let dir = directory();
    let store = Store::open_existing(dir.path()).unwrap();
    assert_eq!(load(&store), Stored::Absent);
    assert!(files(dir.path()).is_empty());
    let first = Envelope::first(binding());
    store.begin(soon()).unwrap().commit(first.clone()).unwrap();
    drop(store);
    let store = Store::open_existing(dir.path()).unwrap();
    assert_eq!(load(&store), Stored::Current(first));
    assert_eq!(files(dir.path()).len(), 1);
    assert_eq!(
        fs::metadata(dir.path().join(NAME)).unwrap().mode() & 0o777,
        0o600
    );
}

#[test]
fn lock_is_per_transaction_and_contention_is_bounded() {
    let (dir, store) = started();
    let other = Store::open_existing(dir.path()).unwrap();
    let held = store.begin(soon()).unwrap();
    let started = Instant::now();
    let err = other
        .begin(Instant::now() + Duration::from_millis(30))
        .err()
        .unwrap();
    assert!(err.to_string().contains("busy"));
    assert!(started.elapsed() < Duration::from_secs(2));
    drop(held);
    // Positive control: the same store takes the lock again once released.
    assert!(other.begin(soon()).is_ok());
    assert!(store.begin(soon()).is_ok());
}

#[test]
fn commit_requires_exact_next_revision_same_epoch_and_binding() {
    let (_dir, store) = started();
    let Stored::Current(first) = load(&store) else {
        panic!()
    };
    let (second, _) = first.disable(scope()).unwrap();
    let (skipped, _) = second.disable(scope()).unwrap();
    {
        let mut tx = store.begin(soon()).unwrap();
        assert!(tx.commit(skipped.clone()).is_err()); // revision 3 over 1
        assert!(tx.commit(first.clone()).is_err()); // same revision
        let mut other_binding = binding();
        other_binding.device_identity = "other-device".into();
        assert!(tx.commit(Envelope::first(other_binding)).is_err());
        assert!(tx.commit(Envelope::first(binding())).is_err()); // new epoch, revision 1
                                                                 // None of those poisoned the transaction; the valid successor passes.
        tx.commit(second.clone()).unwrap();
        assert!(tx.commit(second.clone()).is_err()); // replay of a committed step
    }
    assert_eq!(load(&store), Stored::Current(second));
}

#[test]
fn absent_accepts_only_a_fresh_enable() {
    let dir = directory();
    let store = Store::open_existing(dir.path()).unwrap();
    let (spent, _) = Envelope::first(binding()).disable(scope()).unwrap();
    assert!(store.begin(soon()).unwrap().commit(spent).is_err());
    assert!(files(dir.path()).is_empty());
}

#[test]
fn failed_flush_leaves_old_state_and_denies_any_further_use() {
    let (dir, store) = started();
    let Stored::Current(first) = load(&store) else {
        panic!()
    };
    let (second, _) = first.disable(scope()).unwrap();
    store.dir.dir.fault.set(Some(Fault::BeforeRename));
    {
        let mut tx = store.begin(soon()).unwrap();
        assert!(tx.commit(second.clone()).is_err());
        assert!(tx.state().is_err() && tx.current().is_err());
        assert!(tx.commit(second.clone()).is_err());
    }
    store.dir.dir.fault.set(None);
    assert_eq!(load(&store), Stored::Current(first));
    assert_eq!(files(dir.path()).len(), 1, "temporary file was cleaned up");
    // Positive control: the identical commit succeeds without the fault.
    store.begin(soon()).unwrap().commit(second.clone()).unwrap();
    assert_eq!(load(&store), Stored::Current(second));
}

#[test]
fn directory_flush_failure_may_expose_the_new_revision_but_reports_failure() {
    let (_dir, store) = started();
    let Stored::Current(first) = load(&store) else {
        panic!()
    };
    let (second, _) = first.disable(scope()).unwrap();
    store.dir.dir.fault.set(Some(Fault::AfterRename));
    {
        let mut tx = store.begin(soon()).unwrap();
        assert!(tx.commit(second.clone()).is_err());
        assert!(tx.state().is_err());
    }
    store.dir.dir.fault.set(None);
    // The caller took no effect; a reload sees the committed revision, so a
    // token held for the old one is stale and recovery must re-arm.
    assert_eq!(load(&store), Stored::Current(second));
}

#[test]
fn legacy_state_only_accepts_the_exact_migration_shape() {
    for (intent, update) in [
        (Intent::Enabled, None),
        (Intent::Disabled, None),
        (Intent::Removed, None),
        (Intent::Enabled, Some((Phase::Stop, Intent::Enabled))),
    ] {
        let dir = directory();
        legacy(dir.path(), intent, update);
        let before = fs::read(dir.path().join(NAME)).unwrap();
        let store = Store::open_existing(dir.path()).unwrap();
        let mut tx = store.begin(soon()).unwrap();
        assert!(matches!(tx.state().unwrap(), Stored::Legacy { .. }));
        assert!(tx.current().unwrap().is_none());
        let mut other = binding();
        other.device_identity = "other-device".into();
        assert!(tx.commit(Envelope::first(other)).is_err());
        let (disabled, _) = Envelope::first(binding()).disable(scope()).unwrap();
        assert!(tx.commit(disabled).is_err()); // revision 2 over legacy
                                               // An update dropped or invented by the writer is not a migration.
        let plain = Envelope::first(binding());
        let mismatched = (intent != Intent::Enabled || update.is_some()).then_some(plain);
        if let Some(envelope) = mismatched {
            assert!(tx.commit(envelope).is_err());
        }
        drop(tx);
        assert_eq!(fs::read(dir.path().join(NAME)).unwrap(), before);
    }
    // Positive control: the exact migration of an Enabled legacy journal commits.
    let dir = directory();
    legacy(dir.path(), Intent::Enabled, None);
    let store = Store::open_existing(dir.path()).unwrap();
    assert!(store
        .begin(soon())
        .unwrap()
        .commit(Envelope::first(binding()))
        .is_ok());
}

#[test]
fn migration_preserves_intent_and_update_and_keeps_legacy_update_as_evidence() {
    for intent in [Intent::Enabled, Intent::Disabled, Intent::Removed] {
        for with_update in [false, true] {
            let dir = directory();
            let update = with_update.then_some((Phase::Rollback, Intent::Enabled));
            legacy(dir.path(), intent, update);
            let evidence = fs::read(dir.path().join(LEGACY_UPDATE)).ok();
            let store = Store::open_existing(dir.path()).unwrap();
            let migrated = store.begin(soon()).unwrap().migrate().unwrap();
            assert_eq!((migrated.revision(), migrated.intent()), (1, intent));
            assert!(migrated.stop().is_none() && !migrated.journal_epoch().is_nil());
            assert_eq!(migrated.update().map(|u| u.phase), update.map(|u| u.0));
            assert_eq!(fs::read(dir.path().join(LEGACY_UPDATE)).ok(), evidence);
            assert_eq!(load(&store), Stored::Current(migrated.clone()));
            // Idempotent, and never a second epoch.
            assert_eq!(store.begin(soon()).unwrap().migrate().unwrap(), migrated);
            // The v2 envelope is sole authority: later damage to update.json is ignored.
            if with_update {
                put(dir.path(), LEGACY_UPDATE, b"garbage");
                assert_eq!(load(&store), Stored::Current(migrated));
            }
        }
    }
}

#[test]
fn migration_then_arming_uses_revision_two() {
    let dir = directory();
    legacy(
        dir.path(),
        Intent::Disabled,
        Some((Phase::Stop, Intent::Enabled)),
    );
    let store = Store::open_existing(dir.path()).unwrap();
    let mut tx = store.begin(soon()).unwrap();
    let migrated = tx.migrate().unwrap();
    let (armed, token) = migrated.arm_recovery(scope()).unwrap();
    tx.commit(armed.clone()).unwrap();
    assert_eq!(armed.revision(), 2);
    assert_eq!(token.operation.reason, Reason::Disable);
}

#[test]
fn migration_rejects_bad_legacy_state_without_touching_it() {
    let bad_update = |tweak: &dyn Fn(&mut serde_json::Value)| {
        let dir = directory();
        legacy(
            dir.path(),
            Intent::Enabled,
            Some((Phase::Stop, Intent::Enabled)),
        );
        let path = dir.path().join(LEGACY_UPDATE);
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        tweak(&mut value);
        put(
            dir.path(),
            LEGACY_UPDATE,
            &serde_json::to_vec(&value).unwrap(),
        );
        dir
    };
    let cases = [
        bad_update(&|v| v["binding"]["device_identity"] = "other-device".into()),
        bad_update(&|v| v["unexpected"] = true.into()),
    ];
    for dir in cases {
        let before = (
            fs::read(dir.path().join(NAME)).unwrap(),
            fs::read(dir.path().join(LEGACY_UPDATE)).unwrap(),
        );
        let store = Store::open_existing(dir.path()).unwrap();
        let outcome = store
            .begin(soon())
            .and_then(|mut tx| tx.migrate().map(|_| ()));
        assert!(outcome.is_err());
        assert_eq!(fs::read(dir.path().join(NAME)).unwrap(), before.0);
        assert_eq!(fs::read(dir.path().join(LEGACY_UPDATE)).unwrap(), before.1);
    }
    // Corrupt lifecycle, unknown lifecycle field and an update with no lifecycle.
    for contents in [&b"{"[..], br#"{"binding":null,"intent":"Enabled","x":1}"#] {
        let dir = directory();
        put(dir.path(), NAME, contents);
        let store = Store::open_existing(dir.path()).unwrap();
        assert!(store.begin(soon()).is_err());
    }
    let dir = directory();
    put(dir.path(), LEGACY_UPDATE, b"{}");
    let store = Store::open_existing(dir.path()).unwrap();
    assert!(store.begin(soon()).is_err());
}

#[test]
fn flush_failure_during_migration_keeps_legacy_authority() {
    let dir = directory();
    legacy(dir.path(), Intent::Enabled, None);
    let before = fs::read(dir.path().join(NAME)).unwrap();
    let store = Store::open_existing(dir.path()).unwrap();
    store.dir.dir.fault.set(Some(Fault::BeforeRename));
    assert!(store.begin(soon()).unwrap().migrate().is_err());
    store.dir.dir.fault.set(None);
    assert_eq!(fs::read(dir.path().join(NAME)).unwrap(), before);
    assert!(matches!(load(&store), Stored::Legacy { .. }));
    assert!(store.begin(soon()).unwrap().migrate().is_ok());
}

#[test]
fn older_binary_strict_parser_rejects_the_v2_envelope() {
    let (dir, store) = started();
    drop(store);
    let journal = UnixJournal::open_existing(dir.path()).unwrap();
    assert!(journal.load().is_err());
    // Positive control: the same reader accepts a legacy record.
    let other = directory();
    legacy(other.path(), Intent::Enabled, None);
    assert!(UnixJournal::open_existing(other.path())
        .unwrap()
        .load()
        .unwrap()
        .is_some());
}

#[test]
fn corrupt_unknown_schema_and_replaced_directory_fail_closed() {
    let (dir, store) = started();
    let Stored::Current(good) = load(&store) else {
        panic!()
    };
    let value = serde_json::to_value(&good).unwrap();
    for tweak in [
        |v: &mut serde_json::Value| v["schema"] = 3.into(),
        |v: &mut serde_json::Value| v["revision"] = 0.into(),
        |v: &mut serde_json::Value| v["extra"] = true.into(),
    ] {
        let mut v = value.clone();
        tweak(&mut v);
        put(dir.path(), NAME, &serde_json::to_vec(&v).unwrap());
        assert!(store.begin(soon()).is_err());
    }
    put(dir.path(), NAME, b"{");
    assert!(store.begin(soon()).is_err());
    put(dir.path(), NAME, &serde_json::to_vec(&value).unwrap());
    assert!(store.begin(soon()).is_ok());
    let displaced = dir.path().with_extension("moved");
    fs::rename(dir.path(), &displaced).unwrap();
    fs::create_dir(dir.path()).unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(store.begin(soon()).is_err());
    fs::remove_dir_all(displaced).unwrap();
}

#[test]
fn repair_after_exhaustion_is_persisted_with_a_new_epoch() {
    let (dir, store) = started();
    let Stored::Current(good) = load(&store) else {
        panic!()
    };
    let mut v = serde_json::to_value(&good).unwrap();
    v["revision"] = u64::MAX.into();
    put(dir.path(), NAME, &serde_json::to_vec(&v).unwrap());
    let mut tx = store.begin(soon()).unwrap();
    let spent = tx.current().unwrap().unwrap().clone();
    assert!(spent
        .disable(scope())
        .unwrap_err()
        .to_string()
        .contains("exhausted"));
    assert!(tx.commit(spent.repair_epoch()).is_err()); // not a sequential commit
    let repaired = tx.repair().unwrap();
    assert_eq!(
        (repaired.revision(), repaired.intent()),
        (1, spent.intent())
    );
    assert_ne!(repaired.journal_epoch(), spent.journal_epoch());
    drop(tx);
    assert_eq!(load(&store), Stored::Current(repaired));
    let dir2 = directory();
    let empty = Store::open_existing(dir2.path()).unwrap();
    assert!(empty.begin(soon()).unwrap().repair().is_err());
}

#[test]
fn clones_share_one_exclusive_lock_and_a_transaction_owns_it() {
    let (dir, store) = started();
    let clone = store.clone();
    let tx = store.begin(soon()).unwrap();
    // Same open directory description: flock alone would let this re-enter.
    let err = clone
        .begin(Instant::now() + Duration::from_millis(30))
        .err()
        .unwrap();
    assert!(err.to_string().contains("busy"));
    // The transaction does not borrow the handle that opened it.
    drop(store);
    drop(clone);
    let other = Store::open_existing(dir.path()).unwrap();
    assert!(other
        .begin(Instant::now() + Duration::from_millis(30))
        .is_err());
    assert!(tx.current().unwrap().is_some());
    drop(tx);
    assert!(other.begin(soon()).is_ok());
}

mod hint_tests {
    use super::*;
    use crate::sidecar::store::Hint;
    use std::os::unix::fs::symlink;

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }
    fn hint(generation: u128, started: u64) -> Hint {
        Hint::new(Uuid::from_u128(generation), started).unwrap()
    }

    #[test]
    fn a_hint_round_trips_without_creating_or_touching_the_journal() {
        let dir = directory();
        let store = Store::open_existing(dir.path()).unwrap();
        assert_eq!(store.read_hint(soon()).unwrap(), None);
        store.publish_hint(soon(), &hint(9, 42)).unwrap();
        assert_eq!(
            names(dir.path()),
            ["endpoint.json"],
            "no journal was created"
        );
        assert_eq!(
            fs::metadata(dir.path().join("endpoint.json"))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(store.read_hint(soon()).unwrap(), Some(hint(9, 42)));
        assert_eq!(load(&store), Stored::Absent, "the hint is not authority");
        // A newer supervisor replaces it.
        store.publish_hint(soon(), &hint(10, 43)).unwrap();
        assert_eq!(
            store.read_hint(soon()).unwrap().unwrap().generation(),
            Uuid::from_u128(10)
        );
        store.clear_hint(soon()).unwrap();
        store.clear_hint(soon()).unwrap(); // idempotent
        assert_eq!(store.read_hint(soon()).unwrap(), None);
        assert!(names(dir.path()).is_empty());
    }

    #[test]
    fn publishing_leaves_a_committed_journal_and_its_revision_alone() {
        let (dir, store) = started();
        let before = fs::read(dir.path().join(NAME)).unwrap();
        store.publish_hint(soon(), &hint(9, 42)).unwrap();
        assert_eq!(fs::read(dir.path().join(NAME)).unwrap(), before);
        let stored = load(&store);
        let Stored::Current(envelope) = stored else {
            panic!()
        };
        assert_eq!(envelope.revision(), 1);
        let (second, _) = envelope.disable(scope()).unwrap();
        store.begin(soon()).unwrap().commit(second).unwrap(); // still commits
    }

    #[test]
    fn malformed_or_unsafe_hints_are_errors_and_never_deleted_by_clear() {
        let dir = directory();
        let store = Store::open_existing(dir.path()).unwrap();
        assert!(Hint::new(Uuid::nil(), 1).is_err());
        assert!(Hint::new(Uuid::from_u128(9), 0).is_err());
        store.publish_hint(soon(), &hint(9, 42)).unwrap();
        let good = fs::read(dir.path().join("endpoint.json")).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&good).unwrap();
        let mutate = |f: &dyn Fn(&mut serde_json::Value)| {
            let mut v = value.clone();
            f(&mut v);
            put(
                dir.path(),
                "endpoint.json",
                &serde_json::to_vec(&v).unwrap(),
            );
            store.read_hint(soon())
        };
        assert!(mutate(&|_| {}).is_ok()); // control
        assert!(mutate(&|v| v["schema"] = 2.into()).is_err());
        assert!(mutate(&|v| v["generation"] = Uuid::nil().to_string().into()).is_err());
        assert!(mutate(&|v| v["started"] = 0.into()).is_err());
        assert!(
            mutate(&|v| v["pid"] = 4242.into()).is_err(),
            "no PID field is accepted"
        );
        put(dir.path(), "endpoint.json", b"{");
        assert!(store.read_hint(soon()).is_err());
        // Shared permissions and a redirected name are refused, and clear leaves them.
        put(dir.path(), "endpoint.json", &good);
        fs::set_permissions(
            dir.path().join("endpoint.json"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(store.read_hint(soon()).is_err());
        assert!(store.clear_hint(soon()).is_err());
        assert!(dir.path().join("endpoint.json").exists());
        fs::remove_file(dir.path().join("endpoint.json")).unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::write(outside.path(), b"preserve").unwrap();
        symlink(outside.path(), dir.path().join("endpoint.json")).unwrap();
        assert!(store.read_hint(soon()).is_err());
        assert!(store.publish_hint(soon(), &hint(9, 42)).is_err());
        assert!(store.clear_hint(soon()).is_err());
        assert_eq!(fs::read(outside.path()).unwrap(), b"preserve");
    }

    #[test]
    fn hint_operations_wait_for_the_lock_only_until_the_deadline() {
        let (dir, store) = started();
        let other = Store::open_existing(dir.path()).unwrap();
        let held = store.begin(soon()).unwrap();
        for result in [
            other.publish_hint(Instant::now() + Duration::from_millis(30), &hint(9, 42)),
            other.clear_hint(Instant::now() + Duration::from_millis(30)),
            other
                .read_hint(Instant::now() + Duration::from_millis(30))
                .map(|_| ()),
        ] {
            assert!(result.unwrap_err().to_string().contains("busy"));
        }
        drop(held);
        other.publish_hint(soon(), &hint(9, 42)).unwrap(); // positive control
    }
}

mod selection_tests {
    use super::*;
    use crate::sidecar::store::Selection;

    fn selection(dir: &Path) -> Selection {
        let location = dir.join("runtime").join("v1").join("syncthing");
        Selection::new(
            "v1",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            &location,
        )
        .unwrap()
    }

    #[test]
    fn a_selection_round_trips_privately_without_touching_the_journal() {
        let (dir, store) = started();
        let before = fs::read(dir.path().join(NAME)).unwrap();
        assert_eq!(store.read_selection(soon()).unwrap(), None);
        let chosen = selection(dir.path());
        store.publish_selection(soon(), &chosen).unwrap();
        assert_eq!(store.read_selection(soon()).unwrap(), Some(chosen));
        let path = dir.path().join("runtime.json");
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(fs::read(dir.path().join(NAME)).unwrap(), before);
        // A malformed or foreign record is an error, never a default binary.
        put(dir.path(), "runtime.json", b"{\"schema\":1}");
        assert!(store.read_selection(soon()).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.read_selection(soon()).is_err());
    }

    #[test]
    fn selection_operations_wait_for_the_lock_only_until_the_deadline() {
        let (dir, store) = started();
        let other = Store::open_existing(dir.path()).unwrap();
        let held = store.begin(soon()).unwrap();
        let chosen = selection(dir.path());
        let short = Instant::now() + Duration::from_millis(30);
        assert!(other
            .publish_selection(short, &chosen)
            .unwrap_err()
            .to_string()
            .contains("busy"));
        assert!(other
            .read_selection(short)
            .unwrap_err()
            .to_string()
            .contains("busy"));
        drop(held);
        other.publish_selection(soon(), &chosen).unwrap(); // positive control
    }
}

mod launch_authority {
    use super::*;
    use crate::sidecar::authority::launch::Phase as LaunchPhase;

    fn fresh() -> (tempfile::TempDir, Store, Binding) {
        let dir = directory();
        let store = Store::open_existing(dir.path()).unwrap();
        let binding = binding();
        store
            .begin(soon())
            .unwrap()
            .commit(Envelope::first_with_launch(binding.clone()))
            .unwrap();
        (dir, store, binding)
    }
    fn phase(store: &Store) -> LaunchPhase {
        let Stored::Current(saved) = load(store) else {
            panic!()
        };
        saved.launch_registration().unwrap().phase()
    }

    #[test]
    fn fresh_intent_dispatch_and_claim_are_distinct_durable_one_use_steps() {
        let (dir, store, binding) = fresh();
        assert_eq!(phase(&store), LaunchPhase::Ready);
        assert!(store.claim_launch(&binding, soon()).is_err());
        let dispatch = store.dispatch_launch(&binding, soon()).unwrap();
        assert_eq!(phase(&store), LaunchPhase::Dispatched);
        assert!(store.dispatch_launch(&binding, soon()).is_err());
        let generation = dispatch
            .execute(|generation| {
                // Positive control: helper can take the lock during the native effect.
                assert_eq!(store.claim_launch(&binding, soon())?, generation);
                Ok(generation)
            })
            .unwrap();
        let reopened = Store::open_existing(dir.path()).unwrap();
        assert_eq!(phase(&reopened), LaunchPhase::Claimed);
        assert!(reopened.claim_launch(&binding, soon()).is_err());
        assert!(reopened.dispatch_launch(&binding, soon()).is_err());
        let Stored::Current(saved) = load(&reopened) else {
            panic!()
        };
        assert_eq!(
            saved.launch_registration().unwrap().generation(),
            generation
        );
        // Repeated Enable and epoch repair preserve spent authority.
        let mut tx = reopened.begin(soon()).unwrap();
        tx.commit(saved.enable().unwrap()).unwrap();
        tx.repair().unwrap();
        drop(tx);
        assert!(reopened.claim_launch(&binding, soon()).is_err());
        assert!(reopened.dispatch_launch(&binding, soon()).is_err());
    }

    #[test]
    fn absent_legacy_missing_corrupt_or_changed_authority_never_grants_permission() {
        let dir = directory();
        let store = Store::open_existing(dir.path()).unwrap();
        assert!(store.dispatch_launch(&binding(), soon()).is_err());
        assert!(store.claim_launch(&binding(), soon()).is_err());
        legacy(dir.path(), Intent::Enabled, None);
        assert!(store.dispatch_launch(&binding(), soon()).is_err());
        store.begin(soon()).unwrap().migrate().unwrap();
        assert!(store.dispatch_launch(&binding(), soon()).is_err());
        assert!(store.claim_launch(&binding(), soon()).is_err());
        let (dir, store, binding) = fresh();
        let mut changed = binding.clone();
        changed.instance = Uuid::new_v4();
        assert!(store.dispatch_launch(&changed, soon()).is_err());
        let Stored::Current(saved) = load(&store) else {
            panic!()
        };
        let good = serde_json::to_value(saved).unwrap();
        for damage in [0, 1, 2] {
            let mut value = good.clone();
            match damage {
                0 => {
                    value.as_object_mut().unwrap().remove("launch");
                }
                1 => value["launch"]["generation"] = Uuid::nil().to_string().into(),
                _ => value["launch"]["phase"] = "Invented".into(),
            }
            put(dir.path(), NAME, &serde_json::to_vec(&value).unwrap());
            assert!(store.dispatch_launch(&binding, soon()).is_err());
            assert!(store.claim_launch(&binding, soon()).is_err());
        }
        put(dir.path(), NAME, b"{");
        assert!(store.dispatch_launch(&binding, soon()).is_err());
    }

    #[test]
    fn failed_flush_never_returns_a_dispatch_or_claim_even_after_visible_rename() {
        for fault in [Fault::BeforeRename, Fault::AfterRename] {
            for claim in [false, true] {
                let (_dir, store, binding) = fresh();
                if claim {
                    store
                        .dispatch_launch(&binding, soon())
                        .unwrap()
                        .execute(|_| Ok(()))
                        .unwrap();
                }
                store.dir.dir.fault.set(Some(fault));
                if claim {
                    assert!(store.claim_launch(&binding, soon()).is_err());
                } else {
                    assert!(store.dispatch_launch(&binding, soon()).is_err());
                }
                store.dir.dir.fault.set(None);
                if matches!(fault, Fault::AfterRename) {
                    // Visible but unconfirmed writes are not permission on reload.
                    if claim {
                        assert!(store.claim_launch(&binding, soon()).is_err());
                    } else {
                        assert!(store.dispatch_launch(&binding, soon()).is_err());
                    }
                } else {
                    // Positive control: no effect occurred, old phase is unchanged.
                    assert_eq!(
                        phase(&store),
                        if claim {
                            LaunchPhase::Dispatched
                        } else {
                            LaunchPhase::Ready
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn two_independent_handles_can_dispatch_once_and_claim_once() {
        let (dir, _store, binding) = fresh();
        for claim in [false, true] {
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let workers: Vec<_> = (0..2)
                .map(|_| {
                    let path = dir.path().to_owned();
                    let binding = binding.clone();
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        let store = Store::open_existing(&path).unwrap();
                        barrier.wait();
                        if claim {
                            store.claim_launch(&binding, soon()).is_ok()
                        } else {
                            store.dispatch_launch(&binding, soon()).is_ok()
                        }
                    })
                })
                .collect();
            assert_eq!(
                workers
                    .into_iter()
                    .map(|w| w.join().unwrap())
                    .filter(|won| *won)
                    .count(),
                1
            );
        }
    }

    #[test]
    fn disabled_or_pending_stop_cannot_claim_and_spent_records_cannot_be_rearmed() {
        let (_dir, store, binding) = fresh();
        store
            .dispatch_launch(&binding, soon())
            .unwrap()
            .execute(|_| Ok(()))
            .unwrap();
        let mut tx = store.begin(soon()).unwrap();
        let saved = tx.current().unwrap().unwrap().clone();
        tx.commit(saved.disable(scope()).unwrap().0).unwrap();
        drop(tx);
        assert!(store.claim_launch(&binding, soon()).is_err());
        // Hand-crafted next revisions cannot clear/change/reset the ledger.
        let mut tx = store.begin(soon()).unwrap();
        let saved = tx.current().unwrap().unwrap().clone();
        for damage in [0, 1, 2] {
            let mut value = serde_json::to_value(saved.enable().unwrap()).unwrap();
            match damage {
                0 => value["launch"] = serde_json::Value::Null,
                1 => value["launch"]["generation"] = Uuid::new_v4().to_string().into(),
                _ => value["launch"]["phase"] = "Ready".into(),
            }
            let forged = Envelope::from_slice(&serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(tx.commit(forged).is_err());
        }
    }
}
