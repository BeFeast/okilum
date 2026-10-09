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
        supervisor: "/Applications/Tessera.app/Contents/MacOS/supervisor".into(),
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
