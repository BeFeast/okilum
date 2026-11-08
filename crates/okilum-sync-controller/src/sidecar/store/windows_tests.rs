use super::{StateDir, WindowsStore as Store};
use crate::sidecar::{
    authority::{Envelope, Stored},
    windows::private::PrivateDirectory,
    Binding, Intent,
};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::{
    fs,
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use uuid::Uuid;
use windows::Win32::{
    Foundation::{ERROR_ACCESS_DENIED, HANDLE},
    Security::{
        CreateRestrictedToken, CreateWellKnownSid, ImpersonateLoggedOnUser, RevertToSelf,
        WinWorldSid, DISABLE_MAX_PRIVILEGE, PSID, SID_AND_ATTRIBUTES, TOKEN_DUPLICATE, TOKEN_QUERY,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

fn binding() -> Binding {
    Binding {
        instance: Uuid::from_u128(2),
        installation: Uuid::from_u128(1),
        owner: crate::sidecar::windows::security::current_sid().unwrap(),
        supervisor: r"C:\Program Files\Okilum\supervisor.exe".into(),
        state_directory: "unused".into(),
        device_identity: "existing-device".into(),
    }
}
fn scope() -> crate::sidecar::supervisor::ipc::Scope {
    crate::sidecar::supervisor::ipc::Scope {
        installation: Uuid::from_u128(1),
        instance: Uuid::from_u128(2),
        generation: Uuid::from_u128(9),
    }
}
fn soon() -> Instant {
    Instant::now() + Duration::from_millis(200)
}
/// A prepared private directory (protected owner-only DACL) and a store on it.
fn prepared() -> (tempfile::TempDir, PathBuf, Store) {
    prepared_named("managed")
}
fn prepared_named(name: &str) -> (tempfile::TempDir, PathBuf, Store) {
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join(name);
    let text = path.to_str().unwrap().to_string();
    drop(PrivateDirectory::prepare(&text).unwrap());
    let store = Store::open_existing(&text).unwrap();
    (parent, path, store)
}
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn absent_is_inert_and_a_commit_survives_reopen() {
    let (_parent, path, store) = prepared();
    assert_eq!(
        store.begin(soon()).unwrap().state().unwrap(),
        &Stored::Absent
    );
    assert!(
        names(&path).is_empty(),
        "no lock file or journal was created"
    );
    let first = Envelope::first(binding());
    store.begin(soon()).unwrap().commit(first.clone()).unwrap();
    assert_eq!(names(&path), ["sidecar.json"]);
    let reopened = Store::open_existing(path.to_str().unwrap()).unwrap();
    assert_eq!(
        reopened.begin(soon()).unwrap().state().unwrap(),
        &Stored::Current(first)
    );
}

#[test]
fn unicode_and_spaces_state_path_preserves_journal_and_hint_across_reopen() {
    use crate::sidecar::store::Hint;
    let (_parent, path, store) = prepared_named("Sync state — заметки 世界 🦀");
    let mut bound = binding();
    bound.state_directory = path.to_str().unwrap().into();
    let first = Envelope::first(bound);
    let hint = Hint::new(Uuid::from_u128(9), 42).unwrap();
    store.begin(soon()).unwrap().commit(first.clone()).unwrap();
    store.publish_hint(soon(), &hint).unwrap();
    let (second, _) = first.disable(scope()).unwrap();
    store.dir.fault.set(true);
    assert!(store.begin(soon()).unwrap().commit(second.clone()).is_err());
    drop(store);

    // Reopen the exact spelling, including a trailing separator. Failed replace
    // must preserve both records and leave no temporary file at this path.
    let reopened = Store::open_existing(&format!("{}\\", path.display())).unwrap();
    assert_eq!(names(&path), ["endpoint.json", "sidecar.json"]);
    assert_eq!(
        reopened.begin(soon()).unwrap().state().unwrap(),
        &Stored::Current(first)
    );
    assert_eq!(reopened.read_hint(soon()).unwrap(), Some(hint));
    reopened
        .begin(soon())
        .unwrap()
        .commit(second.clone())
        .unwrap();
    reopened.clear_hint(soon()).unwrap();
    let final_store = Store::open_existing(path.to_str().unwrap()).unwrap();
    assert_eq!(
        final_store.begin(soon()).unwrap().state().unwrap(),
        &Stored::Current(second)
    );
    assert_eq!(final_store.read_hint(soon()).unwrap(), None);
    assert_eq!(names(&path), ["sidecar.json"]);
}

#[test]
fn windows_hint_discovery_retries_only_native_contention_within_its_deadline() {
    use crate::sidecar::store::Hint;
    let (_parent, path, store) = prepared();
    let hint = Hint::new(Uuid::new_v4(), 42).unwrap();
    store.publish_hint(soon(), &hint).unwrap();
    let exclusive = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path.join("endpoint.json"))
        .unwrap();
    let error = store.peek_hint().unwrap_err();
    assert!(super::hint::hint_sharing_violation(&error));

    // Keeping the actual incompatible kernel handle open must exhaust this
    // budget, rather than return absence or authorize a generation.
    let started = Instant::now();
    let deadline = started + Duration::from_millis(200);
    let error = store.peek_hint_until(deadline).unwrap_err();
    assert!(super::hint::hint_sharing_violation(&error));
    assert!(
        Instant::now() >= deadline,
        "hint contention returned before the discovery deadline"
    );

    // An actual concurrent holder releases its exact File, never a path/PID.
    let holder = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        drop(exclusive);
    });
    let observed = store.peek_hint_until(Instant::now() + Duration::from_secs(2));
    holder.join().unwrap();
    assert_eq!(observed.unwrap(), Some(hint.clone()));

    // Structural failures remain errors: retrying contention cannot adopt a
    // hard-linked or malformed hint, even when its generation looks plausible.
    fs::hard_link(path.join("endpoint.json"), path.join("alias.json")).unwrap();
    assert!(store.peek_hint_until(soon()).is_err());
    fs::remove_file(path.join("alias.json")).unwrap();
    assert_eq!(store.peek_hint_until(soon()).unwrap(), Some(hint));
    let malformed = b"{\"schema\":1,\"generation\":\"not-a-uuid\"}";
    store.dir.write("endpoint.json", malformed).unwrap();
    assert!(store.peek_hint_until(soon()).is_err());
    assert_eq!(fs::read(path.join("endpoint.json")).unwrap(), malformed);
}

#[test]
fn invalid_or_missing_state_paths_are_refused_without_creating_or_adopting_state() {
    let (parent, path, store) = prepared();
    let text = path.to_str().unwrap();
    for invalid in [
        format!("{text}\0ignored"),
        format!("{text}\\..\\managed"),
        format!("{text}\\."),
        text.replace('\\', "/"),
        "relative state 世界".into(),
    ] {
        assert!(
            Store::open_existing(&invalid).is_err(),
            "accepted {invalid:?}"
        );
    }
    let missing = parent.path().join("missing state 世界");
    assert!(Store::open_existing(missing.to_str().unwrap()).is_err());
    assert!(!missing.exists(), "read-only open created missing state");
    assert_eq!(names(parent.path()), ["managed"]);
    assert!(names(&path).is_empty());
    // A valid directory still works after all refused opens, including the NUL
    // suffix whose prefix names this directory (never silently truncate it).
    store
        .begin(soon())
        .unwrap()
        .commit(Envelope::first(binding()))
        .unwrap();
    assert!(Store::open_existing(text).unwrap().begin(soon()).is_ok());
}

#[test]
fn directory_in_record_slot_is_an_error_not_absence_and_is_never_replaced() {
    use crate::sidecar::store::Hint;
    let (_parent, path, store) = prepared();
    let journal = path.join("sidecar.json");
    fs::create_dir(&journal).unwrap();
    fs::write(journal.join("preserve"), b"journal obstacle").unwrap();
    assert!(store.begin(soon()).is_err());
    assert_eq!(
        fs::read(journal.join("preserve")).unwrap(),
        b"journal obstacle"
    );
    fs::remove_dir_all(&journal).unwrap();
    let first = Envelope::first(binding());
    store.begin(soon()).unwrap().commit(first.clone()).unwrap();
    let journal_bytes = fs::read(&journal).unwrap();

    let endpoint = path.join("endpoint.json");
    fs::create_dir(&endpoint).unwrap();
    fs::write(endpoint.join("preserve"), b"hint obstacle").unwrap();
    let hint = Hint::new(Uuid::from_u128(9), 42).unwrap();
    assert!(store.read_hint(soon()).is_err());
    assert!(store.peek_hint().is_err());
    assert!(store.publish_hint(soon(), &hint).is_err());
    assert!(store.clear_hint(soon()).is_err());
    assert_eq!(
        fs::read(endpoint.join("preserve")).unwrap(),
        b"hint obstacle"
    );
    assert_eq!(fs::read(&journal).unwrap(), journal_bytes);
    assert_eq!(names(&path), ["endpoint.json", "sidecar.json"]);
    fs::remove_dir_all(&endpoint).unwrap();
    // Positive controls: the same operations succeed after the obstacle is gone.
    store.publish_hint(soon(), &hint).unwrap();
    assert_eq!(store.read_hint(soon()).unwrap(), Some(hint));
    store.clear_hint(soon()).unwrap();
    assert_eq!(
        store.begin(soon()).unwrap().state().unwrap(),
        &Stored::Current(first)
    );
}

#[test]
fn lock_is_exclusive_across_handles_and_bounded() {
    let (_parent, path, store) = prepared();
    let clone = store.clone();
    let tx = store.begin(soon()).unwrap();
    let busy = |s: &Store| {
        s.begin(Instant::now() + Duration::from_millis(30))
            .err()
            .unwrap()
            .to_string()
    };
    assert!(busy(&clone).contains("busy"));
    // A separately opened store (another process would be the same) sees it too,
    // and can still be opened while the lock is held.
    let other = Store::open_existing(path.to_str().unwrap()).unwrap();
    assert!(busy(&other).contains("busy"));
    drop(store);
    drop(clone);
    assert!(
        busy(&other).contains("busy"),
        "the transaction owns the lock"
    );
    drop(tx);
    assert!(other.begin(soon()).is_ok()); // positive control
}

#[test]
fn directory_cannot_be_renamed_while_locked_and_replacement_is_detected() {
    let (parent, path, store) = prepared();
    let moved = parent.path().join("moved");
    let tx = store.begin(soon()).unwrap();
    let error = fs::rename(&path, &moved).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(32), "expected sharing violation");
    drop(tx);
    // Between transactions a replaced directory is a different object.
    fs::rename(&path, &moved).unwrap();
    drop(PrivateDirectory::prepare(path.to_str().unwrap()).unwrap());
    let error = store.begin(soon()).err().unwrap().to_string();
    assert!(error.contains("replaced"), "{error}");
}

#[test]
fn foreign_or_linked_journal_files_are_refused_and_left_alone() {
    let (_parent, path, store) = prepared();
    let first = Envelope::first(binding());
    store.begin(soon()).unwrap().commit(first).unwrap();
    let journal = path.join("sidecar.json");
    // Hard link: a second name for the journal.
    let alias = path.join("alias.json");
    fs::hard_link(&journal, &alias).unwrap();
    assert!(store.begin(soon()).is_err());
    fs::remove_file(&alias).unwrap();
    assert!(store.begin(soon()).is_ok()); // control
                                          // A file written by anyone else gets the inherited DACL, not the protected grant.
    let good = fs::read(&journal).unwrap();
    fs::remove_file(&journal).unwrap();
    fs::write(&journal, &good).unwrap();
    assert!(store.begin(soon()).is_err());
    assert_eq!(
        fs::read(&journal).unwrap(),
        good,
        "never rewritten or repaired"
    );
}

#[test]
fn failed_replacement_leaves_the_old_journal_and_no_temporary_file() {
    let (_parent, path, store) = prepared();
    let first = Envelope::first(binding());
    store.begin(soon()).unwrap().commit(first.clone()).unwrap();
    let (second, _) = first.disable(scope()).unwrap();
    // Injected failure after the temporary file was fully written and flushed.
    store.dir.fault.set(true);
    {
        let mut tx = store.begin(soon()).unwrap();
        assert!(tx.commit(second.clone()).is_err());
        assert!(tx.state().is_err());
    }
    store.dir.fault.set(false);
    assert_eq!(names(&path), ["sidecar.json"]);
    assert_eq!(
        store.begin(soon()).unwrap().state().unwrap(),
        &Stored::Current(first.clone())
    );
    // A reader that shares reads but not deletes blocks the replacement for real
    // (the commit itself still reads the journal first, so it must be allowed to).
    let reader = fs::OpenOptions::new()
        .read(true)
        .share_mode(1) // FILE_SHARE_READ
        .open(path.join("sidecar.json"))
        .unwrap();
    assert!(store.begin(soon()).unwrap().commit(second.clone()).is_err());
    drop(reader);
    assert_eq!(names(&path), ["sidecar.json"]);
    // Positive control: the identical commit succeeds once nothing interferes.
    store.begin(soon()).unwrap().commit(second.clone()).unwrap();
    let after = store.begin(soon()).unwrap();
    assert_eq!(after.current().unwrap().unwrap().intent(), Intent::Disabled);
}

#[test]
fn only_the_exact_next_revision_commits() {
    let (_parent, _path, store) = prepared();
    let first = Envelope::first(binding());
    let (second, _) = first.disable(scope()).unwrap();
    let (third, _) = second.disable(scope()).unwrap();
    let mut tx = store.begin(soon()).unwrap();
    assert!(tx.commit(second.clone()).is_err()); // nothing stored yet
    tx.commit(first.clone()).unwrap();
    assert!(tx.commit(third).is_err()); // skipped a revision
    tx.commit(second).unwrap();
}

/// Same construction as the private-pipe fixture: Everyone as the only restricting
/// SID and all privileges removed, so access needs a grant to Everyone.
fn restricted_token() -> OwnedHandle {
    let mut raw = HANDLE::default();
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY, &mut raw).unwrap();
    }
    let token = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let mut world = [0usize; 16];
    let mut size = std::mem::size_of_val(&world) as u32;
    let world_sid = PSID(world.as_mut_ptr().cast());
    unsafe {
        CreateWellKnownSid(WinWorldSid, None, Some(world_sid), &mut size).unwrap();
    }
    let restricting = [SID_AND_ATTRIBUTES {
        Sid: world_sid,
        Attributes: 0,
    }];
    let mut output = HANDLE::default();
    unsafe {
        CreateRestrictedToken(
            HANDLE(token.as_raw_handle()),
            DISABLE_MAX_PRIVILEGE,
            None,
            None,
            Some(&restricting),
            &mut output,
        )
        .unwrap();
        OwnedHandle::from_raw_handle(output.0)
    }
}
struct Impersonation;
impl Drop for Impersonation {
    fn drop(&mut self) {
        // Never leak an impersonated token into another test.
        if unsafe { RevertToSelf() }.is_err() {
            std::process::abort();
        }
    }
}

#[test]
fn restricted_token_can_neither_read_the_journal_nor_take_the_lock() {
    let (_parent, path, store) = prepared();
    store
        .begin(soon())
        .unwrap()
        .commit(Envelope::first(binding()))
        .unwrap();
    let journal = path.join("sidecar.json");
    let token = restricted_token();
    let (read, lock) = {
        unsafe { ImpersonateLoggedOnUser(HANDLE(token.as_raw_handle())).unwrap() };
        let _restore = Impersonation;
        (
            fs::read(&journal),
            PrivateDirectory::lock_exclusive(path.to_str().unwrap()).err(),
        )
    };
    assert_eq!(
        read.unwrap_err().raw_os_error(),
        Some(ERROR_ACCESS_DENIED.0 as i32)
    );
    let lock = lock.expect("restricted token took the directory lock");
    let denied = windows::core::HRESULT::from_win32(ERROR_ACCESS_DENIED.0);
    assert!(
        lock.downcast_ref::<windows::core::Error>()
            .is_some_and(|e| e.code() == denied),
        "{lock:#}"
    );
    // Positive control: the same objects are open to the ordinary owner token.
    assert!(fs::read(&journal).is_ok());
    assert!(store.begin(soon()).is_ok());
}

#[test]
fn a_hint_round_trips_without_a_journal_and_foreign_files_are_never_deleted() {
    use crate::sidecar::store::Hint;
    let (_parent, path, store) = prepared();
    let hint = Hint::new(Uuid::from_u128(9), 42).unwrap();
    assert_eq!(store.read_hint(soon()).unwrap(), None);
    store.publish_hint(soon(), &hint).unwrap();
    assert_eq!(names(&path), ["endpoint.json"], "no journal was created");
    assert_eq!(store.read_hint(soon()).unwrap(), Some(hint.clone()));
    assert_eq!(
        store.begin(soon()).unwrap().state().unwrap(),
        &Stored::Absent,
        "the hint is not authority"
    );
    store.clear_hint(soon()).unwrap();
    store.clear_hint(soon()).unwrap(); // idempotent
    assert!(names(&path).is_empty());

    // A file written by anyone else (inherited DACL, not the protected grant) is
    // refused on read and publish, and clear leaves it alone.
    fs::write(path.join("endpoint.json"), b"{}").unwrap();
    assert!(store.read_hint(soon()).is_err());
    assert!(store.publish_hint(soon(), &hint).is_err());
    assert!(store.clear_hint(soon()).is_err());
    assert_eq!(fs::read(path.join("endpoint.json")).unwrap(), b"{}");
    fs::remove_file(path.join("endpoint.json")).unwrap();
    store.publish_hint(soon(), &hint).unwrap(); // positive control after cleanup
}
