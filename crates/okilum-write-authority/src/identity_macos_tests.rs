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
