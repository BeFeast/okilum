use super::*;

fn fixture() -> (tempfile::TempDir, Authority) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("vault");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("note.md"), b"Canonical bytes\r\n").unwrap();
    let authority = Authority::open_in(&temporary.path().join("account"), &root).unwrap();
    (temporary, authority)
}
fn identities(binding: &mut Binding) -> [&mut Identity; 5] {
    [
        &mut binding.root_identity,
        &mut binding.directory,
        &mut binding.admission,
        &mut binding.writers,
        &mut binding.owner,
    ]
}
fn legacy(authority: &Authority) -> Binding {
    let mut binding = authority.binding.clone();
    binding.schema = 1;
    for identity in identities(&mut binding) {
        identity.0 += 1024;
        identity.3 = None;
    }
    // Write the exact legacy three-element arrays, not a synthetic new format.
    let mut value = serde_json::to_value(&binding).unwrap();
    for key in [
        "root_identity",
        "directory",
        "admission",
        "writers",
        "owner",
    ] {
        value[key].as_array_mut().unwrap().truncate(3);
    }
    persist(&authority.binding_path, &value, false).unwrap();
    read(&authority.binding_path).unwrap()
}

#[test]
fn identity_2692_native_uuid_and_device_renumbering() {
    let (_temporary, authority) = fixture();
    let current = identity(authority.root()).unwrap();
    assert!(current.3.is_some_and(|uuid| uuid != [0; 16]));
    assert_eq!(current, identity(authority.root()).unwrap());
    let mut hint_changed = authority.binding.clone();
    for identity in identities(&mut hint_changed) {
        identity.0 += 1024;
    }
    persist(&authority.binding_path, &hint_changed, false).unwrap();
    let reopened = Authority::open_in(&authority.account, authority.root()).unwrap();
    reopened.operation().unwrap();
    authority.admit().unwrap(); // a changed diagnostic hint does not replace authority
    assert_eq!(
        fs::read(authority.root().join("note.md")).unwrap(),
        b"Canonical bytes\r\n"
    );
}

#[test]
fn identity_2692_legacy_binding_migrates_without_changing_state() {
    let (_temporary, authority) = fixture();
    let state_before = fs::read(authority.directory.join("state.json")).unwrap();
    let legacy_binding = legacy(&authority);
    let reopened = Authority::open_in(&authority.account, authority.root()).unwrap();
    assert_eq!(reopened.binding.schema, 2);
    assert_eq!(reopened.binding.epoch, legacy_binding.epoch);
    assert_eq!(
        fs::read(authority.directory.join("state.json")).unwrap(),
        state_before
    );
    assert!(reopened.binding.root_identity.3.is_some());
    reopened.operation().unwrap();
    let stale = Authority {
        binding: legacy_binding,
        ..authority.clone()
    };
    assert!(
        stale.admit().is_err(),
        "pre-migration authorities remain fenced"
    );
    assert_eq!(
        fs::read(authority.root().join("note.md")).unwrap(),
        b"Canonical bytes\r\n"
    );
}

#[test]
fn identity_2692_migration_preserves_unfinished_hold() {
    let (_temporary, authority) = fixture();
    let hold = authority.begin_drain("unfinished-operation").unwrap();
    drop(hold); // model a process exit; durable Draining must not become Open
    let state_before = fs::read(authority.directory.join("state.json")).unwrap();
    legacy(&authority);
    let reopened = Authority::open_in(&authority.account, authority.root()).unwrap();
    assert_eq!(
        fs::read(authority.directory.join("state.json")).unwrap(),
        state_before
    );
    assert!(reopened.admit().is_err());
    assert_eq!(reopened.state().unwrap().phase, Phase::Draining);
}

#[test]
fn identity_2692_migration_refuses_a_live_writer_then_retries() {
    let (_temporary, authority) = fixture();
    let lease = authority.admit().unwrap();
    let binding = legacy(&authority);
    assert!(Authority::open_in(&authority.account, authority.root()).is_err());
    assert_eq!(read::<Binding>(&authority.binding_path).unwrap(), binding);
    drop(lease);
    Authority::open_in(&authority.account, authority.root())
        .unwrap()
        .operation()
        .unwrap();
}

#[test]
fn identity_2692_real_root_or_authority_replacement_stays_refused() {
    let (temporary, authority) = fixture();
    let binding = legacy(&authority);
    fs::rename(authority.root(), temporary.path().join("previous-vault")).unwrap();
    fs::create_dir(authority.root()).unwrap();
    let error = Authority::open_in(&authority.account, authority.root()).unwrap_err();
    assert_eq!(replaced_root(&error).as_deref(), Some(authority.root()));
    assert_eq!(read::<Binding>(&authority.binding_path).unwrap(), binding);

    let (_temporary, authority) = fixture();
    let mut binding = legacy(&authority);
    binding.writers.1 += 1;
    persist(&authority.binding_path, &binding, false).unwrap();
    assert!(Authority::open_in(&authority.account, authority.root()).is_err());
    assert_eq!(read::<Binding>(&authority.binding_path).unwrap(), binding);
}

#[test]
fn identity_2692_other_volume_or_birth_identity_is_not_a_hint() {
    let (_temporary, authority) = fixture();
    let mut other = authority.binding.clone();
    other.root_identity.3.as_mut().unwrap()[0] ^= 1;
    persist(&authority.binding_path, &other, false).unwrap();
    assert!(Authority::open_in(&authority.account, authority.root()).is_err());
    let mut other = authority.binding.clone();
    other.root_identity.2 = Some(other.root_identity.2.unwrap() + 1);
    persist(&authority.binding_path, &other, false).unwrap();
    assert!(Authority::open_in(&authority.account, authority.root()).is_err());
}

#[test]
fn identity_2692_concurrent_migration_publishes_one_preserved_binding() {
    let (_temporary, authority) = fixture();
    let legacy_binding = legacy(&authority);
    let state_before = fs::read(authority.directory.join("state.json")).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(4));
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let barrier = barrier.clone();
            let authority = &authority;
            scope.spawn(move || {
                barrier.wait();
                Authority::open_in(&authority.account, authority.root())
                    .unwrap()
                    .operation()
                    .unwrap();
            });
        }
    });
    let binding: Binding = read(&authority.binding_path).unwrap();
    assert_eq!(binding.schema, 2);
    assert_eq!(binding.epoch, legacy_binding.epoch);
    assert_eq!(
        fs::read(authority.directory.join("state.json")).unwrap(),
        state_before
    );
}

const EXPORT: &str = "10.10.0.14:/srv/nfs/Maestro";

/// Every identity on this thread comes from a UUID-less NFS mount until dropped.
struct Nfs;
impl Nfs {
    fn everywhere() -> Self {
        Self::mount("/".into(), EXPORT, None)
    }
    fn mount(prefix: PathBuf, source: &'static str, backing: Option<PathBuf>) -> Self {
        identity_macos::SIMULATED_MOUNTS.with(|mounts| {
            mounts.borrow_mut().push(identity_macos::SimulatedMount {
                prefix,
                source,
                backing,
            })
        });
        Self
    }
}
impl Drop for Nfs {
    fn drop(&mut self) {
        identity_macos::SIMULATED_MOUNTS.with(|mounts| mounts.borrow_mut().pop());
    }
}

#[test]
fn identity_3202_volume_without_uuid_admits_writers_across_remounts() {
    let _nfs = Nfs::everywhere();
    let (_temporary, authority) = fixture();
    let current = identity(authority.root()).unwrap();
    let uuid = current.3.expect("mount-source identity");
    assert_ne!(uuid, [0; 16]);
    assert_eq!(current, identity(authority.root()).unwrap());
    authority.admit().unwrap();
    // NFS assigns a new st_dev on every remount; the mount source does not change.
    let mut remounted = authority.binding.clone();
    for identity in identities(&mut remounted) {
        identity.0 += 1024;
    }
    persist(&authority.binding_path, &remounted, false).unwrap();
    let reopened = Authority::open_in(&authority.account, authority.root()).unwrap();
    {
        let _operation = reopened.operation().unwrap();
        fs::write(authority.root().join("note.md"), b"Edited on NFS\r\n").unwrap();
    }
    reopened.admit().unwrap();
    // Notes inside the vault resolve to the same scope.
    let nested = authority.root().join("folder");
    fs::create_dir(&nested).unwrap();
    let inside = Authority::open_in(&authority.account, &nested).unwrap();
    assert_eq!(inside.binding_path, authority.binding_path);
    assert_eq!(
        fs::read(authority.root().join("note.md")).unwrap(),
        b"Edited on NFS\r\n"
    );
}

#[test]
fn identity_3202_mount_source_is_scoped_and_errors_stay_errors() {
    let nfs = identity_macos::mount_source_digest(b"nfs", b"10.10.0.14:/srv/nfs/Maestro").unwrap();
    assert_eq!(
        nfs,
        identity_macos::mount_source_digest(b"nfs", b"10.10.0.14:/srv/nfs/Maestro").unwrap()
    );
    assert_ne!(
        nfs,
        identity_macos::mount_source_digest(b"nfs", b"10.10.0.14:/srv/nfs/Other").unwrap()
    );
    assert_ne!(
        nfs,
        identity_macos::mount_source_digest(b"smbfs", b"10.10.0.14:/srv/nfs/Maestro").unwrap()
    );
    assert!(identity_macos::mount_source_digest(b"nfs", b"").is_err());

    let (_temporary, authority) = fixture();
    let handle = File::open(authority.root()).unwrap();
    let native = identity_macos::from_file(&handle).unwrap();
    for missing in [libc::EINVAL, libc::ENOTSUP, libc::ENOATTR] {
        let fallback = identity_macos::from_file_with(
            &handle,
            |_| Err(std::io::Error::from_raw_os_error(missing)),
            |_| identity_macos::mount_source_digest(b"nfs", EXPORT.as_bytes()),
        )
        .unwrap();
        assert_eq!((fallback.1, fallback.2), (native.1, native.2));
        assert_ne!(
            fallback, native,
            "no-UUID identity cannot alias a native UUID"
        );
    }
    let zero = identity_macos::from_file_with(
        &handle,
        |_| Ok(None),
        |_| identity_macos::mount_source_digest(b"nfs", EXPORT.as_bytes()),
    )
    .unwrap();
    assert!(zero.3.is_some_and(|uuid| uuid != [0; 16]));
    let error = identity_macos::from_file_with(
        &handle,
        |_| Err(std::io::Error::from_raw_os_error(libc::EIO)),
        |_| identity_macos::mount_source_digest(b"nfs", EXPORT.as_bytes()),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("Cannot read stable writer volume identity"));
}

#[test]
fn identity_3202_native_binding_does_not_accept_a_volume_without_uuid() {
    let (_temporary, authority) = fixture();
    let _nfs = Nfs::everywhere();
    assert!(Authority::open_in(&authority.account, authority.root()).is_err());
}

#[test]
fn identity_3202_native_roots_are_not_limited() {
    let (temporary, authority) = fixture();
    let other = temporary.path().join("other");
    fs::create_dir(&other).unwrap();
    let second = Authority::open_in(&authority.account, &other).unwrap();
    assert_ne!(second.binding_path, authority.binding_path);
    second.operation().unwrap();
    authority.operation().unwrap();
}

/// Second spelling of `EXPORT`'s server for the same export.
const ALIAS: &str = "nas.example:/srv/nfs/Maestro";

/// Server storage for UUID-less exports; each mount mirrors its directories.
struct Exports {
    _temporary: tempfile::TempDir,
    base: PathBuf,
    server: PathBuf,
    account: PathBuf,
}
fn exports(folders: &[&str]) -> Exports {
    let temporary = tempfile::tempdir().unwrap();
    let base = temporary.path().canonicalize().unwrap();
    let server = base.join("server");
    for folder in folders {
        fs::create_dir_all(server.join(folder)).unwrap();
    }
    let account = base.join("account");
    Exports {
        _temporary: temporary,
        base,
        server,
        account,
    }
}
/// Mount `server` at `base/name` with mount source `source`. Removing the
/// mount point unmounts it; `mirror` mounts it again.
fn mount(base: &Path, server: &Path, name: &str, source: &'static str) -> (PathBuf, Nfs) {
    let prefix = base.join(name);
    mirror(server, &prefix);
    let nfs = Nfs::mount(prefix.clone(), source, Some(server.to_path_buf()));
    (prefix, nfs)
}
fn mirror(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            mirror(&entry.path(), &to.join(entry.file_name()));
        }
    }
}
fn bound(account: &Path, root: &Path) -> bool {
    account.join(format!("{}.json", key(root))).exists()
}
fn release(hold: Hold) {
    let token = hold.token().clone();
    hold.release(&token).unwrap();
}
fn refused(account: &Path, root: &Path, reason: &str) {
    let error = Authority::open_in(account, root).unwrap_err();
    assert!(
        format!("{error:#}").contains(reason),
        "{}: {error:#}",
        root.display()
    );
    assert!(!bound(account, root), "{}: no new scope", root.display());
}

#[test]
fn identity_3204_several_uuid_less_vaults_have_separate_scopes() {
    let e = exports(&["A", "B", "C"]);
    let (first, _first) = mount(&e.base, &e.server, "by-ip", EXPORT);
    let (second, _second) = mount(&e.base, &e.server, "by-hostname", ALIAS);
    // Folders on one export, through one mount and through its alias.
    let a = Authority::open_in(&e.account, &first.join("A")).unwrap();
    let b = Authority::open_in(&e.account, &first.join("B")).unwrap();
    let c = Authority::open_in(&e.account, &second.join("C")).unwrap();
    assert_ne!(a.binding_path, b.binding_path);
    assert_ne!(a.binding_path, c.binding_path);
    assert_ne!(b.binding_path, c.binding_path);
    let hold = a.begin_drain("exclusive-operation").unwrap();
    assert!(a.admit().is_err());
    b.operation().unwrap();
    c.operation().unwrap();
    Authority::open_in(&e.account, &first.join("B"))
        .unwrap()
        .operation()
        .unwrap();
    release(hold);
    a.operation().unwrap();
    // Another export cannot be proved disjoint (it may be nested): refused.
    let other = e.base.join("other-server");
    fs::create_dir_all(other.join("D")).unwrap();
    let (third, _third) = mount(&e.base, &other, "other", "nas.example:/srv/nfs/Other");
    refused(&e.account, &third.join("D"), "cannot prove");
}

/// #3209 review: `nas:/share/Vault/Sub` and `nas:/share` mounted separately.
/// The child's chain ends at its mount root, so nothing proves the parent
/// opened through the wider export does not contain it.
#[test]
fn identity_3204_nested_export_cannot_bypass_a_hold() {
    let e = exports(&["Vault/Sub"]);
    let (child_mount, _child_mount) = mount(
        &e.base,
        &e.server.join("Vault").join("Sub"),
        "child",
        "nas.example:/srv/nfs/Maestro/Vault/Sub",
    );
    let (all, _all) = mount(&e.base, &e.server, "all", EXPORT);
    let child = Authority::open_in(&e.account, &child_mount).unwrap();
    let hold = child.begin_drain("exclusive-operation").unwrap();
    refused(&e.account, &all.join("Vault"), "cannot prove");
    refused(&e.account, &all, "cannot prove");
    assert!(child.admit().is_err(), "the child hold stays in force");
    release(hold);

    // The other order: the wider export first, then the nested one.
    let e = exports(&["Vault/Sub"]);
    let (all, _all) = mount(&e.base, &e.server, "all", EXPORT);
    let (child_mount, _child_mount) = mount(
        &e.base,
        &e.server.join("Vault").join("Sub"),
        "child",
        "nas.example:/srv/nfs/Maestro/Vault/Sub",
    );
    let vault = Authority::open_in(&e.account, &all.join("Vault")).unwrap();
    let _hold = vault.begin_drain("exclusive-operation").unwrap();
    refused(&e.account, &child_mount, "cannot prove");
}

/// #3209 review: `Vault` is replaced while `Sub` (the registered root) is
/// kept, so the stored record names the old `Vault`. With the first mount
/// gone, the stale record must not prove that `B/Vault` lies outside `Sub`.
#[test]
fn identity_3204_replaced_ancestor_never_proves_disjointness() {
    let e = exports(&["Vault/Sub"]);
    let (first, _first) = mount(&e.base, &e.server, "by-ip", EXPORT);
    let (second, _second) = mount(&e.base, &e.server, "by-hostname", ALIAS);
    let child = Authority::open_in(&e.account, &first.join("Vault").join("Sub")).unwrap();
    let recorded = child.recorded_ancestry().unwrap().unwrap();
    drop(child.begin_drain("exclusive-operation").unwrap()); // the holder exits
                                                             // Replace Vault on the server, keeping Sub.
    fs::rename(e.server.join("Vault"), e.server.join("OldVault")).unwrap();
    fs::create_dir(e.server.join("Vault")).unwrap();
    fs::rename(
        e.server.join("OldVault").join("Sub"),
        e.server.join("Vault").join("Sub"),
    )
    .unwrap();
    mirror(&e.server, &second);
    assert_eq!(
        identity(&second.join("Vault").join("Sub")).unwrap().1,
        child.binding.root_identity.1,
        "the root is kept"
    );
    assert_ne!(
        identity(&second.join("Vault")).unwrap().1,
        recorded[0].1,
        "the record names the old Vault"
    );
    fs::remove_dir_all(&first).unwrap();
    refused(&e.account, &second.join("Vault"), "unfinished writer hold");
    assert!(child.binding_path.exists());
}

#[test]
fn identity_3204_mount_alias_resolves_to_the_held_scope() {
    let e = exports(&["Vault/Sub"]);
    let (first, _first) = mount(&e.base, &e.server, "by-ip", EXPORT);
    let (second, _second) = mount(&e.base, &e.server, "by-hostname", ALIAS);
    let through_alias = identity(&second.join("Vault")).unwrap();
    let original = identity(&first.join("Vault")).unwrap();
    assert_ne!(through_alias, original, "spellings give distinct digests");
    assert_eq!((through_alias.1, through_alias.2), (original.1, original.2));

    let vault = Authority::open_in(&e.account, &first.join("Vault")).unwrap();
    let hold = vault.begin_drain("exclusive-operation").unwrap();
    for path in [second.join("Vault"), second.join("Vault").join("Sub")] {
        let resolved = Authority::open_in(&e.account, &path).unwrap();
        assert_eq!(resolved.binding_path, vault.binding_path);
        assert!(
            resolved.admit().is_err(),
            "the hold fences {}",
            path.display()
        );
        assert!(!bound(&e.account, &path), "no second scope");
    }
    release(hold);
    Authority::open_in(&e.account, &second.join("Vault"))
        .unwrap()
        .operation()
        .unwrap();
}

#[test]
fn identity_3204_child_first_then_parent_through_alias_keeps_one_scope() {
    let e = exports(&["Vault/Sub"]);
    let (first, _first) = mount(&e.base, &e.server, "by-ip", EXPORT);
    let (second, _second) = mount(&e.base, &e.server, "by-hostname", ALIAS);
    let child = Authority::open_in(&e.account, &first.join("Vault").join("Sub")).unwrap();
    let parent = second.join("Vault");

    let hold = child.begin_drain("exclusive-operation").unwrap();
    refused(&e.account, &parent, "unfinished writer hold");
    release(hold);
    let lease = child.admit().unwrap();
    refused(&e.account, &parent, "publishing");
    drop(lease);

    // Promoted: the child's handles now resolve the parent scope.
    let parent = Authority::open_in(&e.account, &parent).unwrap();
    assert_ne!(parent.binding_path, child.binding_path);
    let hold = parent.begin_drain("exclusive-operation").unwrap();
    assert!(
        child.admit().is_err(),
        "parent hold fences the child handle"
    );
    let reopened = Authority::open_in(&e.account, &first.join("Vault").join("Sub")).unwrap();
    assert!(reopened.admit().is_err(), "and a reopened child");
    release(hold);
    child.operation().unwrap();
    reopened.operation().unwrap();
}

#[test]
fn identity_3204_held_child_fences_parent_after_its_mount_is_gone() {
    let e = exports(&["Vault/Sub"]);
    let (first, _first) = mount(&e.base, &e.server, "by-ip", EXPORT);
    let (second, _second) = mount(&e.base, &e.server, "by-hostname", ALIAS);
    let child = Authority::open_in(&e.account, &first.join("Vault").join("Sub")).unwrap();
    drop(child.begin_drain("exclusive-operation").unwrap()); // the holder exits
    fs::remove_dir_all(&first).unwrap(); // the first mount is gone
    refused(&e.account, &second.join("Vault"), "unfinished writer hold");
    refused(
        &e.account,
        &second.join("Vault").join("Sub"),
        "unfinished writer hold",
    );
    assert!(child.binding_path.exists(), "the held scope is kept");
}

#[test]
fn identity_3204_stale_binding_is_retired_through_another_mount() {
    let e = exports(&["Vault"]);
    let (first, _first) = mount(&e.base, &e.server, "by-ip", EXPORT);
    let (second, _second) = mount(&e.base, &e.server, "by-hostname", ALIAS);
    let old = Authority::open_in(&e.account, &first.join("Vault")).unwrap();
    old.operation().unwrap();
    // A completed hold leaves state.json at a later revision.
    release(old.begin_drain("completed-operation").unwrap());
    fs::remove_dir_all(&first).unwrap();
    let new = Authority::open_in(&e.account, &second.join("Vault")).unwrap();
    assert_ne!(new.binding_path, old.binding_path);
    assert!(!old.binding_path.exists(), "the stale binding is retired");
    new.operation().unwrap();
    assert!(old.admit().is_err(), "old handles fail closed");
    // The first mount comes back: the same folder keeps one scope.
    mirror(&e.server, &first);
    let again = Authority::open_in(&e.account, &first.join("Vault")).unwrap();
    assert_eq!(again.binding_path, new.binding_path);
    assert!(!bound(&e.account, &first.join("Vault")));
    let hold = again.begin_drain("exclusive-operation").unwrap();
    assert!(new.admit().is_err());
    release(hold);
    // Now the second mount is gone too: the first path registers again in its
    // retired directory, and handles of both old scopes stay fenced.
    release(new.begin_drain("completed-operation").unwrap());
    fs::remove_dir_all(&second).unwrap();
    let returned = Authority::open_in(&e.account, &first.join("Vault")).unwrap();
    assert_eq!(returned.binding_path, old.binding_path);
    assert_ne!(returned.binding.epoch, old.binding.epoch);
    assert!(!new.binding_path.exists(), "the second binding is retired");
    returned.operation().unwrap();
    assert!(old.admit().is_err(), "the retired first scope stays fenced");
    assert!(
        new.admit().is_err(),
        "the retired second scope stays fenced"
    );
    let hold = returned.begin_drain("exclusive-operation").unwrap();
    assert!(Authority::open_in(&e.account, &first.join("Vault"))
        .unwrap()
        .admit()
        .is_err());
    release(hold);

    // A stale binding with an unfinished hold is kept and refuses the new path.
    let e = exports(&["Vault"]);
    let (first, _first) = mount(&e.base, &e.server, "by-ip", EXPORT);
    let (second, _second) = mount(&e.base, &e.server, "by-hostname", ALIAS);
    let old = Authority::open_in(&e.account, &first.join("Vault")).unwrap();
    drop(old.begin_drain("exclusive-operation").unwrap());
    fs::remove_dir_all(&first).unwrap();
    refused(&e.account, &second.join("Vault"), "unfinished writer hold");
    assert!(old.binding_path.exists());
}

#[test]
fn identity_3204_child_admits_again_when_its_alias_parent_mount_is_gone() {
    let e = exports(&["Vault/Sub"]);
    let (first, _first) = mount(&e.base, &e.server, "by-ip", EXPORT);
    let (second, _second) = mount(&e.base, &e.server, "by-hostname", ALIAS);
    let child_path = second.join("Vault").join("Sub");
    Authority::open_in(&e.account, &child_path).unwrap();
    let parent = Authority::open_in(&e.account, &first.join("Vault")).unwrap();
    fs::remove_dir_all(&first).unwrap();
    let reopened = Authority::open_in(&e.account, &child_path).unwrap();
    reopened.operation().unwrap();
    assert!(!parent.binding_path.exists(), "idle stale parent retired");

    // A held parent keeps fencing the child.
    let e = exports(&["Vault/Sub"]);
    let (first, _first) = mount(&e.base, &e.server, "by-ip", EXPORT);
    let (second, _second) = mount(&e.base, &e.server, "by-hostname", ALIAS);
    let child_path = second.join("Vault").join("Sub");
    let child = Authority::open_in(&e.account, &child_path).unwrap();
    let parent = Authority::open_in(&e.account, &first.join("Vault")).unwrap();
    drop(parent.begin_drain("exclusive-operation").unwrap());
    fs::remove_dir_all(&first).unwrap();
    assert!(Authority::open_in(&e.account, &child_path).is_err());
    assert!(child.admit().is_err());
    assert!(parent.binding_path.exists());
}

#[test]
fn identity_3204_scope_without_ancestry_record_is_conservative() {
    let e = exports(&["Vault/Sub"]);
    let (first, _first) = mount(&e.base, &e.server, "by-ip", EXPORT);
    let (second, _second) = mount(&e.base, &e.server, "by-hostname", ALIAS);
    // A #3202 binding has no record; its own path still shows the enclosure.
    let child = Authority::open_in(&e.account, &first.join("Vault").join("Sub")).unwrap();
    let record = child.directory.join("ancestry.json");
    fs::remove_file(&record).unwrap();
    let hold = child.begin_drain("exclusive-operation").unwrap();
    refused(&e.account, &second.join("Vault"), "unfinished writer hold");
    release(hold);
    // Unreachable and without a record: overlap is unknown, so refused.
    assert!(!record.exists());
    fs::remove_dir_all(&first).unwrap();
    refused(&e.account, &second.join("Vault"), "cannot prove");
}
