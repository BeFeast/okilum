//! #3272: a registration retires idle scopes of deleted folders, never a scope
//! that still fences a writer, a hold, a durable binding or an absent volume.
use super::*;
use std::time::{Duration, Instant};

fn records(account: &Path) -> usize {
    fs::read_dir(account)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "json")
        })
        .count()
}
fn registered(account: &Path, root: &Path) -> bool {
    let account = account.canonicalize().unwrap();
    account.join(format!("{}.json", key(root))).exists() || account.join(key(root)).exists()
}
/// Register a fresh folder: the call that runs pruning.
fn register_new(temp: &Path, account: &Path, name: &str) -> Authority {
    let root = temp.join(name);
    fs::create_dir(&root).unwrap();
    Authority::open_in(account, &root).unwrap()
}

#[cfg(target_os = "linux")]
fn on_other_volume(id: &Identity) -> Identity {
    let mut other = id.clone();
    other.3.as_mut().unwrap()[15] ^= 0xff;
    other
}
#[cfg(target_os = "macos")]
fn on_other_volume(id: &Identity) -> Identity {
    let mut other = id.clone();
    other.3.as_mut().unwrap()[15] ^= 0xff;
    other
}
#[cfg(windows)]
fn on_other_volume(id: &Identity) -> Identity {
    let mut other = id.clone();
    if let Some(filesystem_identity::FileIdentity::Full { volume, .. }) = other.3.as_mut() {
        *volume ^= 0xffff;
    }
    other
}

#[test]
fn prune_retires_only_idle_scopes_of_deleted_folders() -> Result<()> {
    let t = tempfile::tempdir()?;
    let base = t.path().canonicalize()?;
    let account = base.join("account");
    let open = |name: &str| register_new(&base, &account, name);

    let live = open("live");
    let stale = open("stale");
    let leased = open("leased");
    let held = open("held");
    let durable = open("durable");
    let legacy = open("legacy");
    let unmounted = open("unmounted");
    let replaced = open("replaced");

    let lease = leased.admit()?;
    let hold = held.begin_drain("merge")?;
    drop(hold); // the holder dies; the hold stays durable
    let binding = durable.durable_binding()?;
    fs::remove_file(legacy.directory.join(prune::PRUNABLE))?;
    let moved = on_other_volume(&unmounted.binding.root_identity);
    let unmounted = Authority {
        binding: Binding {
            root_identity: moved,
            ..unmounted.binding.clone()
        },
        ..unmounted
    };
    persist(&unmounted.binding_path, &unmounted.binding, false)?;
    for a in [&stale, &leased, &held, &durable, &legacy, &unmounted] {
        fs::remove_dir(a.root())?;
    }
    fs::rename(replaced.root(), base.join("replaced.previous"))?;
    fs::create_dir(replaced.root())?;

    open("trigger-1");
    assert!(
        registered(&account, live.root()),
        "an existing folder stays"
    );
    assert!(
        !registered(&account, stale.root()),
        "a deleted idle folder goes"
    );
    assert!(
        registered(&account, leased.root()),
        "a live lease pins the scope"
    );
    assert!(
        registered(&account, held.root()),
        "an unfinished hold stays"
    );
    assert!(
        registered(&account, durable.root()),
        "a durable binding pins the scope"
    );
    assert!(
        registered(&account, legacy.root()),
        "scopes without the marker stay"
    );
    assert!(
        registered(&account, unmounted.root()),
        "an absent volume stays"
    );
    assert!(
        registered(&account, replaced.root()),
        "a replaced root needs re-attach"
    );
    assert!(fs::read_dir(&account)?.all(|e| !prune::is_tombstone(&e.unwrap().path())));

    // Sync can still admit its durable binding without the root.
    drop(binding.admit_without_root()?);
    held.validate_binding()?;
    assert_eq!(held.state()?.phase, Phase::Draining, "the hold is intact");
    // Old handles of a retired scope stay refused, also once the folder returns.
    assert!(stale.admit().is_err());
    fs::create_dir(stale.root())?;
    assert!(stale.admit().is_err());
    let fresh = Authority::open_in(&account, stale.root())?;
    assert_ne!(fresh.binding.epoch, stale.binding.epoch);
    assert!(stale.admit().is_err(), "the new epoch fences old handles");
    fresh.operation()?;
    fs::remove_dir(stale.root())?;

    // The lease ends: the next registration retires that scope too.
    drop(lease);
    open("trigger-2");
    assert!(!registered(&account, leased.root()));
    assert!(registered(&account, live.root()));
    live.operation()?;
    Ok(())
}

#[test]
fn prune_finishes_an_interrupted_retirement() -> Result<()> {
    let t = tempfile::tempdir()?;
    let base = t.path().canonicalize()?;
    let account = base.join("account");
    let a = register_new(&base, &account, "vault");
    let hold = a.begin_drain("completed")?;
    let token = hold.token().clone();
    hold.release(&token)?; // revision > 0: not an initial state
                           // Crash after the record became a tombstone, before the directory went.
    fs::rename(&a.binding_path, prune::tombstone(&a.account, a.root()))?;
    let again = Authority::open_in(&account, a.root())?;
    assert_ne!(again.binding.epoch, a.binding.epoch);
    again.operation()?;
    assert!(a.admit().is_err());

    // Another folder's leftover is finished by any registration.
    let b = register_new(&base, &account, "other");
    fs::rename(&b.binding_path, prune::tombstone(&b.account, b.root()))?;
    register_new(&base, &account, "trigger");
    assert!(!b.directory.exists());
    assert!(!prune::tombstone(&b.account, b.root()).exists());
    Ok(())
}

/// A scope written as registration writes it, without the per-file fsyncs.
fn synthetic(account: &Path, root: &Path) -> Result<()> {
    fs::create_dir(root)?;
    let directory = account.join(key(root));
    fs::create_dir(&directory)?;
    for name in [
        "admission.lock",
        "writers.lock",
        "owner.lock",
        prune::PRUNABLE,
    ] {
        fs::write(directory.join(name), b"")?;
    }
    let epoch = uuid::Uuid::new_v4().to_string();
    let state = Snapshot {
        epoch: epoch.clone(),
        revision: 0,
        operation_id: None,
        phase: Phase::Open,
    };
    fs::write(directory.join("state.json"), serde_json::to_vec(&state)?)?;
    let binding = Binding {
        schema: BINDING_SCHEMA,
        root: root.to_owned(),
        root_identity: identity(root)?,
        epoch,
        directory: identity(&directory)?,
        admission: identity(&directory.join("admission.lock"))?,
        writers: identity(&directory.join("writers.lock"))?,
        owner: identity(&directory.join("owner.lock"))?,
    };
    fs::write(
        account.join(format!("{}.json", key(root))),
        serde_json::to_vec(&binding)?,
    )?;
    Ok(())
}

fn timed(work: impl FnOnce()) -> Duration {
    let start = Instant::now();
    work();
    start.elapsed()
}

#[test]
fn prune_keeps_new_folder_registration_fast_after_10k_deleted_folders() -> Result<()> {
    const STALE: usize = 10_000;
    let t = tempfile::tempdir()?;
    let base = t.path().canonicalize()?;
    let account = base.join("account");
    let mut opened = 0;
    let mut open = || {
        opened += 1;
        register_new(&base, &account, &format!("new-{opened}"));
    };
    // Same-session baseline: a new folder in an almost empty account.
    open();
    let baseline = (0..3).map(|_| timed(&mut open)).min().unwrap();

    let account = account.canonicalize()?;
    let folders = base.join("deleted");
    fs::create_dir(&folders)?;
    for index in 0..STALE {
        synthetic(&account, &folders.join(format!("folder-{index}")))?;
    }
    fs::remove_dir_all(&folders)?;
    let live = records(&account) - STALE;

    let mut rounds = 0;
    let mut first = None;
    while records(&account) > live + rounds {
        rounds += 1;
        assert!(
            rounds <= STALE / prune::PER_REGISTRATION + 1,
            "{} deleted folders are still registered after {rounds} registrations",
            records(&account) - live - rounds + 1
        );
        let spent = timed(&mut open);
        first.get_or_insert(spent);
    }
    SCANNED.with(|count| count.set(0));
    let steady = (0..3).map(|_| timed(&mut open)).min().unwrap();
    let scanned = SCANNED.with(|count| count.get());
    println!(
        "baseline {baseline:?}; first registration over {STALE} deleted folders {:?}; \
         {rounds} registrations to retire them; steady {steady:?} ({scanned} records read by 3 registrations)",
        first.unwrap()
    );
    assert!(scanned <= 3 * (live + rounds + 3), "read {scanned} records");
    assert!(
        steady <= baseline * 3 + Duration::from_millis(30),
        "new-folder registration took {steady:?} (baseline {baseline:?})"
    );
    assert_eq!(
        fs::read_dir(&account)?.count(),
        2 * records(&account) + 1,
        "only records, scope directories and registry.lock remain"
    );
    Ok(())
}
