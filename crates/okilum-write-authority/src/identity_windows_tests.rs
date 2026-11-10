use super::*;

fn fixture() -> (tempfile::TempDir, tempfile::TempDir, Authority) {
    let temp = tempfile::tempdir().unwrap();
    // Model normal Windows account metadata on NTFS even when vault TEMP is ReFS.
    let account = tempfile::tempdir_in(std::env::var_os("LOCALAPPDATA").unwrap()).unwrap();
    let root = temp.path().join("vault");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("note.md"), b"canonical").unwrap();
    let authority = Authority::open_in(account.path(), &root).unwrap();
    (temp, account, authority)
}
fn legacy(authority: &Authority) {
    let mut binding = authority.binding.clone();
    binding.schema = 1;
    for id in [
        &mut binding.root_identity,
        &mut binding.directory,
        &mut binding.admission,
        &mut binding.writers,
        &mut binding.owner,
    ] {
        id.3 = None;
    }
    persist(&authority.binding_path, &binding, false).unwrap();
}
#[test]
fn windows_legacy_authority_upgrade_or_explicit_recovery_preserves_hold_and_epoch() {
    let (_temp, _account, authority) = fixture();
    drop(authority.begin_drain("unfinished").unwrap());
    let state = fs::read(authority.directory.join("state.json")).unwrap();
    legacy(&authority);
    let reopened = Authority::open_in(&authority.account, authority.root())
        .or_else(|error| {
            assert!(replaced_root(&error).is_some());
            Authority::rebind_in(&authority.account, authority.root())
        })
        .unwrap();
    assert_eq!(reopened.binding.schema, 2);
    assert_eq!(reopened.binding.epoch, authority.binding.epoch);
    assert_eq!(
        fs::read(authority.directory.join("state.json")).unwrap(),
        state
    );
    assert!(reopened.admit().is_err());
    assert!(
        authority.admit().is_err(),
        "old full-ID lease cannot bypass migration"
    );
}
#[test]
fn windows_legacy_held_alias_never_mints_another_open_authority() {
    let (temp, _account, authority) = fixture();
    drop(authority.begin_drain("unfinished").unwrap());
    legacy(&authority);
    let moved = temp.path().join("moved");
    fs::rename(authority.root(), &moved).unwrap();
    assert!(Authority::open_in(&authority.account, &moved).is_err());
    assert!(!authority
        .account
        .join(format!("{}.json", key(&moved.canonicalize().unwrap())))
        .exists());
    assert_eq!(fs::read(moved.join("note.md")).unwrap(), b"canonical");
}
#[test]
fn windows_upper_file_id_bits_are_equality_not_just_a_migration_hint() {
    let (_temp, _account, authority) = fixture();
    let original = authority.binding.root_identity.clone();
    let mut changed = original.clone();
    let Some(filesystem_identity::FileIdentity::Full { file, .. }) = changed.3.as_mut() else {
        panic!("full ID")
    };
    file[15] ^= 1;
    assert_ne!(original, changed);
    let mut old = original.clone();
    old.3 = None;
    assert!(
        old.legacy_candidate(&changed),
        "a partial legacy match must fence, never admit"
    );
    assert!(
        !old.legacy_matches(&changed),
        "missing upper bits cannot authorize upgrade"
    );
}
