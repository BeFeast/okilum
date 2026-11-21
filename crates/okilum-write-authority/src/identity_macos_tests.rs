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
    reopened.operation().unwrap();
    reopened.admit().unwrap();
    assert_eq!(
        fs::read(authority.root().join("note.md")).unwrap(),
        b"Canonical bytes\r\n"
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
fn identity_3202_mount_source_alias_cannot_bypass_a_hold() {
    let _nfs = Nfs::everywhere();
    let (temporary, authority) = fixture();
    // The same export mounted again by hostname: another path and mount source
    // spelling for the same server object (same inode and birth time).
    let alias = temporary.path().canonicalize().unwrap().join("by-hostname");
    fs::create_dir(&alias).unwrap();
    let _second_mount = Nfs::mount(
        alias.clone(),
        "maestro.oklabs.uk:/srv/nfs/Maestro",
        Some(authority.root().to_path_buf()),
    );
    let through_alias = identity(&alias).unwrap();
    let original = identity(authority.root()).unwrap();
    assert_ne!(
        through_alias, original,
        "source spellings give distinct digests"
    );
    assert_eq!((through_alias.1, through_alias.2), (original.1, original.2));

    let _hold = authority.begin_drain("exclusive-operation").unwrap();
    // The alias resolves to the one existing authority, which is held.
    let resolved = Authority::open_in(&authority.account, &alias).unwrap();
    assert_eq!(resolved.root(), authority.root());
    assert_eq!(resolved.binding_path, authority.binding_path);
    assert!(resolved.admit().is_err(), "hold must fence the alias");
    assert!(
        !authority
            .account
            .join(format!("{}.json", key(&alias)))
            .exists(),
        "no second authority for the same physical folder"
    );
}

#[test]
fn identity_3202_held_child_fences_its_parent_through_another_mount() {
    let _nfs = Nfs::everywhere();
    let temporary = tempfile::tempdir().unwrap();
    let base = temporary.path().canonicalize().unwrap();
    let vault = base.join("by-ip").join("Vault");
    fs::create_dir_all(vault.join("Sub")).unwrap();
    fs::write(vault.join("Sub").join("note.md"), b"Canonical bytes\r\n").unwrap();
    let account = base.join("account");
    // The child scope is registered and held through the first mount.
    let child = Authority::open_in(&account, &vault.join("Sub")).unwrap();
    let _hold = child.begin_drain("exclusive-operation").unwrap();
    // The parent is then opened through a second mount of the same export.
    let alias = base.join("by-hostname");
    fs::create_dir_all(alias.join("Vault")).unwrap();
    let _second_mount = Nfs::mount(
        alias.clone(),
        "maestro.oklabs.uk:/srv/nfs/Maestro",
        Some(base.join("by-ip")),
    );
    let parent = alias.join("Vault");
    let error = Authority::open_in(&account, &parent).unwrap_err();
    assert!(
        format!("{error:#}").contains("through another path"),
        "{error:#}"
    );
    assert!(
        !account.join(format!("{}.json", key(&parent))).exists(),
        "no overlapping authority for the parent"
    );
    assert!(child.admit().is_err(), "the child hold stays in force");
}
