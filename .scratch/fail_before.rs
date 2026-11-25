
// Scratch (#3272 review): the new regression tests against the first PR head.
/// Rewrites `path` in place with `bytes` and restores its modification time
/// (and, on Windows, its creation time), as `SetFileTime` allows any program
/// to. Only Unix ctime still changes; Okilum's index trusts none of these.
fn fb_overwrite(path: &Path, bytes: &[u8]) {
    let before = fs::metadata(path).unwrap();
    let file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .unwrap();
    (&file).write_all(bytes).unwrap();
    let times = fs::FileTimes::new()
        .set_accessed(before.accessed().unwrap())
        .set_modified(before.modified().unwrap());
    #[cfg(windows)]
    let times = {
        use std::os::windows::fs::FileTimesExt;
        times.set_created(before.created().unwrap())
    };
    file.set_times(times).unwrap();
    drop(file);
    let after = fs::metadata(path).unwrap();
    assert_eq!(after.len(), before.len(), "same size");
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        assert_eq!(
            (
                after.file_size(),
                after.creation_time(),
                after.last_write_time()
            ),
            (
                before.file_size(),
                before.creation_time(),
                before.last_write_time()
            ),
            "the whole Windows stamp of #3284's first version is unchanged"
        );
    }
}

/// An account of four registered folders, the first three in the index, and a
/// new unrelated folder to register next.
struct FbCached {
    _temp: tempfile::TempDir,
    account: PathBuf,
    authorities: Vec<Authority>,
    new: PathBuf,
}
fn fb_cached() -> FbCached {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let account = base.join("account");
    let mut authorities = Vec::new();
    for name in ["one", "two", "three", "four"] {
        let root = base.join(name);
        fs::create_dir(&root).unwrap();
        authorities.push(Authority::open_in(&account, &root).unwrap());
    }
    let account = account.canonicalize().unwrap();
    assert_eq!(RecordIndex::new(&account).len(), 3, "records known");
    let new = base.join("new");
    fs::create_dir(&new).unwrap();
    FbCached {
        _temp: temp,
        account,
        authorities,
        new,
    }
}

/// Every decision the index can take on `cached`'s account, with and
/// without it: the registry scan (alone and after the migration scan in the
/// same hold), the migration scan, and a whole registration.
fn fb_decisions(cached: &FbCached, enabled: bool) -> Vec<String> {
    let mut decisions = vec![
        decision(&cached.account, &cached.new, enabled, false),
        decision(&cached.account, &cached.new, enabled, true),
    ];
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    decisions.extend(migration(&cached.account, enabled));
    let account = cached.account.clone();
    let new = cached.new.clone();
    decisions.push(with_index(enabled, || {
        match Authority::open_in(&account, &new) {
            Ok(authority) => {
                // Undo, so the next comparison sees the same account.
                fs::remove_file(&authority.binding_path).unwrap();
                fs::remove_dir_all(&authority.directory).unwrap();
                "registered".to_owned()
            }
            Err(error) => format!("error {error:#}"),
        }
    }));
    decisions
}

/// Review of #3284, finding 1: a record replaced or rewritten in place with
/// an equal size and restored times (Windows: the whole first-version stamp)
/// must be noticed. Malformed: the full scan stops with an explicit-recovery
/// error. Valid with a changed physical identity: the full scan sees an alias
/// of the new folder with an unfinished hold.
#[test]
fn fail_before_registry_index_notices_a_change_that_keeps_size_and_times() {
    let _settled = Settled::new();
    let mut mismatches = Vec::new();
    for valid in [false, true] {
        let cached = fb_cached();
        let record = &cached.authorities[1].binding_path;
        let original = fs::read(record).unwrap();
        let changed = if valid {
            // An unfinished hold: the physical-alias check refuses the new
            // folder while this record claims its identity.
            drop(cached.authorities[1].begin_drain("same stamp"));
            let original = fs::read(record).unwrap();
            let mut binding: Binding = serde_json::from_slice(&original).unwrap();
            binding.root_identity = identity(&cached.new).unwrap();
            let mut bytes = serde_json::to_vec(&binding).unwrap();
            assert!(bytes.len() <= original.len() + 64);
            // Pad the original and the change to one length (JSON allows
            // trailing whitespace), and let the index learn the original.
            let length = original.len() + 64;
            let mut padded = original.clone();
            padded.resize(length, b' ');
            fb_persist(record, &padded);
            bytes.resize(length, b' ');
            let warm = cached.new.with_file_name("warm");
            fs::create_dir(&warm).unwrap();
            Authority::open_in(&cached.account, &warm).unwrap();
            bytes
        } else {
            let mut bytes = original.clone();
            bytes[0] = b'x';
            bytes
        };
        let before = fb_decisions(&cached, true);
        assert!(
            before.last().unwrap() == "registered",
            "control: the unchanged account registers {before:?}"
        );
        fb_overwrite(record, &changed);
        let indexed = fb_decisions(&cached, true);
        let full = fb_decisions(&cached, false);
        println!("fail-before same stamp, valid={valid}: indexed {:?} | full {:?}", indexed.last(), full.last());
        if indexed != full {
            mismatches.push(valid);
            continue;
        }
        let expected = if valid {
            "Physical vault identity has an unfinished hold"
        } else {
            "Invalid writer authority; explicit recovery required"
        };
        assert!(
            indexed.last().unwrap().contains(expected),
            "the registration stops as the full scan does: {indexed:?}"
        );
    }
    assert!(mismatches.is_empty(), "decisions differ from the full scan (valid = {mismatches:?})");
}

/// Writes `bytes` over `path` the way records are written (temp + rename).
fn fb_persist(path: &Path, bytes: &[u8]) {
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap()).unwrap();
    temp.write_all(bytes).unwrap();
    temp.persist(path).unwrap();
}

/// Review of #3284, finding 2: a cached record that can be listed and
/// stat'ed but not opened or read still stops the registration, with the
/// full scan's error. Windows: another handle denies read sharing; Unix: no
/// read permission (not enforced for root, so skipped there).
#[test]
fn fail_before_registry_index_keeps_the_read_error_of_a_cached_record() {
    let _settled = Settled::new();
    let cached = fb_cached();
    let record = cached.authorities[1].binding_path.clone();
    let before = fb_decisions(&cached, true);
    assert_eq!(before.last().unwrap(), "registered", "control: {before:?}");
    #[cfg(windows)]
    let _deny = {
        use std::os::windows::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&record)
            .unwrap()
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            println!("root ignores file permissions; nothing to check");
            return;
        }
        fs::set_permissions(&record, fs::Permissions::from_mode(0o000)).unwrap();
    }
    // The record is still listed with metadata: only its content is gone.
    let listed = fs::read_dir(&cached.account)
        .unwrap()
        .map(|entry| entry.unwrap())
        .find(|entry| entry.path() == record)
        .unwrap();
    listed.metadata().unwrap();
    assert!(
        File::open(&record).is_err(),
        "control: the record is unreadable"
    );
    let indexed = fb_decisions(&cached, true);
    let full = fb_decisions(&cached, false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
    }
    println!("fail-before unreadable: indexed {:?} | full {:?}", indexed.last(), full.last());
    assert_eq!(indexed, full);
    assert!(
        indexed.last().unwrap().starts_with("error"),
        "the registration stops as the full scan does: {indexed:?}"
    );
}

