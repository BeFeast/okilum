//! Reusing the migration scan's record reads in the registry scan of the same
//! `registry.lock` hold (#3272) must never change a registry decision,
//! including its errors. Every comparison runs the single-hold path (the
//! registry scan skips what the migration scan read) against two independent
//! full scans (the registry scan reads every record again, as before #3272).
use super::registry_reads::{HoldReads, Summary, ENABLED, READS, REUSED};
use super::*;

/// Records the registry scan skipped and record files read; both reset.
fn counters() -> [usize; 2] {
    [REUSED.with(|c| c.replace(0)), READS.with(|c| c.replace(0))]
}

/// Enables the reuse and restores that default when dropped, even after a
/// failed assertion.
struct Fresh;
impl Fresh {
    fn new() -> Self {
        ENABLED.with(|e| e.set(true));
        counters();
        Self
    }
}
impl Drop for Fresh {
    fn drop(&mut self) {
        ENABLED.with(|e| e.set(true));
    }
}

fn with_reuse<T>(enabled: bool, body: impl FnOnce() -> T) -> T {
    ENABLED.with(|e| e.set(enabled));
    let value = body();
    ENABLED.with(|e| e.set(true));
    value
}

/// The registry decision for `root`, as text, after the migration scan
/// walked the records in the same hold (without rewriting anything; a record
/// it would migrate counts as rewritten). With `reuse`, the registry scan
/// uses what that walk read; without, it is an independent full scan.
/// Consumes the outcome, so the enclosed scopes' locks are released before
/// the next scan.
fn decision(account: &Path, root: &Path, reuse: bool) -> String {
    let mut reads = HoldReads::default();
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    for entry in fs::read_dir(account).unwrap() {
        if let Ok(Some((path, _))) = Authority::legacy_record(&entry.unwrap(), &mut reads) {
            reads.rewritten(&path);
        }
    }
    if !reuse {
        reads = HoldReads::default();
    }
    match Authority::scan_registry(account, root, &reads) {
        Ok(Scan::Existing(a)) => {
            format!("existing {} {:?}", a.binding_path.display(), a.binding)
        }
        Ok(Scan::Enclosed(enclosed)) => format!(
            "enclosed {:?}",
            enclosed.iter().map(|e| &e.directory).collect::<Vec<_>>()
        ),
        Err(error) => format!("error {error:#}"),
    }
}

/// The migration scan's per-record decisions, as text.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn migration(account: &Path) -> Vec<String> {
    let mut reads = HoldReads::default();
    fs::read_dir(account)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            match Authority::legacy_record(&entry, &mut reads) {
                Ok(Some((path, binding))) => format!("migrate {} {binding:?}", path.display()),
                Ok(None) => format!("skip {:?}", entry.file_name()),
                Err(error) => format!("error {:?} {error:#}", entry.file_name()),
            }
        })
        .collect()
}

fn bindings(account: &Path) -> Vec<Binding> {
    fs::read_dir(account)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            (path.extension().is_some_and(|e| e == "json"))
                .then(|| read::<Binding>(&path).ok())
                .flatten()
        })
        .collect()
}

/// xorshift64*: deterministic scenarios without a dependency.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

#[derive(Default, Debug)]
struct Totals {
    decisions: usize,
    existing: usize,
    enclosed: usize,
    errors: usize,
    migrations: usize,
    reused: usize,
    unread_concerning: usize,
    full_reads: usize,
    /// Records changed in place with their size and times restored.
    same_stamp: usize,
}

const NAMES: [&str; 7] = ["a", "A", "b", "c d", "é", "nest", "Nest"];

fn scenario(seed: u64, totals: &mut Totals) {
    let _fresh = Fresh::new();
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let account = base.join("account");
    let world = base.join("w");
    fs::create_dir(&world).unwrap();
    // Nested and case-variant folders; a case-insensitive volume folds them.
    let mut folders = Vec::new();
    for _ in 0..12 {
        let mut path = world.clone();
        for _ in 0..1 + rng.below(3) {
            path.push(NAMES[rng.below(NAMES.len())]);
        }
        fs::create_dir_all(&path).unwrap();
        folders.push(path.canonicalize().unwrap());
    }
    folders.sort();
    folders.dedup();
    let mut registered = Vec::new();
    for folder in &folders {
        if rng.chance(60) {
            if let Ok(authority) = Authority::open_in(&account, folder) {
                registered.push(authority);
            }
        }
    }
    // Unfinished holds: the holder is gone, the hold stays durable.
    for authority in &registered {
        if rng.chance(15) {
            drop(authority.begin_drain("equivalence"));
        }
    }
    let account = account.canonicalize().unwrap();
    for name in ["fresh", "fresh-1"] {
        let fresh = world.join(name);
        fs::create_dir(&fresh).unwrap();
        Authority::open_in(&account, &fresh).unwrap();
    }
    // Changes between registrations: deleted, moved, replaced (and
    // re-attached), reused paths, legacy and corrupt records.
    let mut probes = vec![world.clone(), base.clone()];
    for (number, folder) in folders.iter().enumerate() {
        if !rng.chance(35) || !folder.exists() {
            probes.push(folder.clone());
            continue;
        }
        let record = account.join(format!("{}.json", key(folder)));
        match rng.below(8) {
            0 => fs::remove_dir_all(folder).unwrap(),
            1 => {
                let moved = world.join(format!("moved-{number}"));
                fs::rename(folder, &moved).unwrap();
                probes.push(moved);
            }
            2 => {
                fs::remove_dir_all(folder).unwrap();
                fs::create_dir(folder).unwrap();
            }
            3 => {
                fs::remove_dir_all(folder).unwrap();
                fs::create_dir(folder).unwrap();
                let _ = Authority::rebind_in(&account, folder);
            }
            4 => {
                if let Ok(mut binding) = read::<Binding>(&record) {
                    binding.schema = 1;
                    for identity in [
                        &mut binding.root_identity,
                        &mut binding.directory,
                        &mut binding.admission,
                        &mut binding.writers,
                        &mut binding.owner,
                    ] {
                        identity.3 = None;
                    }
                    persist(&record, &binding, false).unwrap();
                }
            }
            5 => {
                if record.exists() {
                    fs::write(&record, b"{").unwrap();
                }
            }
            6 => {
                if let Ok(mut bytes) = fs::read(&record) {
                    // Corrupt in place, same size, times restored: nothing
                    // but the content tells this record changed.
                    bytes[0] = b'x';
                    overwrite_keeping_stamp(&record, &bytes);
                    totals.same_stamp += 1;
                }
            }
            _ => {
                // Another registered folder takes this path.
                let other = &folders[rng.below(folders.len())];
                if other != folder && other.exists() && !folder.starts_with(other) {
                    let aside = world.join(format!("aside-{number}"));
                    fs::rename(folder, &aside).unwrap();
                    if !other.starts_with(folder) {
                        fs::rename(other, folder).unwrap();
                    }
                    probes.push(aside);
                }
            }
        }
        probes.push(folder.clone());
    }
    let unregistered = world.join("probe-new");
    fs::create_dir(&unregistered).unwrap();
    probes.push(unregistered);
    let probes: Vec<PathBuf> = probes
        .into_iter()
        .filter_map(|probe| probe.canonicalize().ok())
        .collect();

    for probe in &probes {
        counters();
        let reused = decision(&account, probe, true);
        let [hits, _] = counters();
        let full = decision(&account, probe, false);
        let [full_hits, full_reads] = counters();
        assert_eq!(full_hits, 0, "the full scan reads every record");
        totals.full_reads += full_reads;
        assert_eq!(reused, full, "seed {seed}: root {}", probe.display());
        totals.decisions += 1;
        totals.reused += hits;
        match reused.split(' ').next() {
            Some("existing") => totals.existing += 1,
            Some("enclosed") => totals.enclosed += 1,
            _ => totals.errors += 1,
        }
        // The skip predicate itself, against every readable record.
        let ancestors: Vec<Identity> = probe.ancestors().map(|p| identity(p).unwrap()).collect();
        for binding in bindings(&account) {
            let summary = Summary::of(&binding);
            let concerning = physical_candidate(&ancestors, &binding.root_identity)
                || (binding.root.starts_with(probe) && binding.root != *probe);
            #[cfg(target_os = "macos")]
            let concerning = concerning
                || (ancestors[0].is_mount_source()
                    && binding.root_identity.is_mount_source()
                    && binding.root_identity != ancestors[0]);
            if concerning && !summary.may_concern(probe, &ancestors) {
                totals.unread_concerning += 1;
            }
        }
    }
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    {
        totals.migrations += migration(&account)
            .iter()
            .filter(|d| d.starts_with("migrate"))
            .count();
    }
}

#[test]
fn registry_reads_decisions_equal_two_full_scans() {
    let seeds: u64 = std::env::var("REGISTRY_READS_SEEDS")
        .ok()
        .and_then(|seeds| seeds.parse().ok())
        .unwrap_or(40);
    let mut totals = Totals::default();
    for seed in 0..seeds {
        scenario(seed, &mut totals);
    }
    println!("registry reads equivalence: {seeds} seeds, {totals:?}");
    assert_eq!(totals.unread_concerning, 0, "the skip predicate is sound");
    // Controls: the comparisons exercised the reuse and every outcome.
    assert!(totals.reused > 0, "the registry scan reused records");
    assert!(totals.full_reads > 0, "the full scans read records");
    assert!(totals.existing > 0 && totals.enclosed > 0 && totals.errors > 0);
    assert!(totals.migrations > 0, "legacy records were compared");
    assert!(
        totals.same_stamp > 0,
        "records changed under an equal stamp"
    );
}

/// An earlier build of #3272 kept a `registry-index-v1` file in the account
/// directory. It is ignored, left as it is, and nothing but the new record
/// and its scope directory is written.
#[test]
fn registry_reads_ignore_an_old_index_file_and_write_none() {
    let _fresh = Fresh::new();
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let account = base.join("account");
    let mut authorities = Vec::new();
    for name in ["one", "two"] {
        let root = base.join(name);
        fs::create_dir(&root).unwrap();
        authorities.push(Authority::open_in(&account, &root).unwrap());
    }
    // That build's header, then bytes that describe no record here.
    let old = account.join("registry-index-v1");
    let mut bytes = b"OKWRIDX2".to_vec();
    bytes.extend((0..512u32).map(|n| (n.wrapping_mul(2_654_435_761) >> 24) as u8));
    fs::write(&old, &bytes).unwrap();
    let listing = || -> Vec<std::ffi::OsString> {
        let mut names: Vec<_> = fs::read_dir(&account)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let before = listing();
    let root = base.join("three");
    fs::create_dir(&root).unwrap();
    authorities.push(Authority::open_in(&account, &root).unwrap());
    // A registration that encloses the others scans every record as well.
    let enclosing = decision(&account.canonicalize().unwrap(), &base, true);
    assert!(enclosing.starts_with("enclosed"), "{enclosing}");
    let added: Vec<_> = listing()
        .into_iter()
        .filter(|name| !before.contains(name))
        .collect();
    let mut expected = vec![
        std::ffi::OsString::from(key(&root)),
        std::ffi::OsString::from(format!("{}.json", key(&root))),
    ];
    expected.sort();
    assert_eq!(added, expected, "only the new record and its scope");
    assert_eq!(fs::read(&old).unwrap(), bytes, "the old file is untouched");
    for authority in authorities {
        authority.operation().unwrap();
    }
}

#[test]
fn registry_reads_read_each_record_once_per_registration() {
    let _fresh = Fresh::new();
    let temp = tempfile::tempdir().unwrap();
    let account = temp.path().join("account");
    let count = 120;
    for number in 0..count {
        let root = temp.path().join(format!("folder-{number}"));
        fs::create_dir(&root).unwrap();
        Authority::open_in(&account, &root).unwrap();
    }
    // The migration scan reads every record once; the registry scan, in the
    // same registry.lock hold, uses what it read.
    counters();
    let root = temp.path().join("first");
    fs::create_dir(&root).unwrap();
    Authority::open_in(&account, &root).unwrap();
    assert_eq!(counters(), [count, count], "[reused, reads]");
    // Nothing carries over to the next hold: it reads every record again,
    // including the one the last registration added.
    let root = temp.path().join("second");
    fs::create_dir(&root).unwrap();
    Authority::open_in(&account, &root).unwrap();
    assert_eq!(counters(), [count + 1, count + 1], "[reused, reads]");
    // Control: without the reuse every record is read twice.
    let root = temp.path().join("third");
    fs::create_dir(&root).unwrap();
    with_reuse(false, || Authority::open_in(&account, &root)).unwrap();
    assert_eq!(counters(), [0, 2 * (count + 2)], "[reused, reads]");
}

/// Rewrites `path` in place with `bytes` and restores its modification time
/// (and, on Windows, its creation time), as `SetFileTime` allows any program
/// to. Only Unix ctime still changes; nothing in Okilum trusts these.
fn overwrite_keeping_stamp(path: &Path, bytes: &[u8]) {
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

/// An account of four registered folders and a new unrelated folder to
/// register next.
struct Account {
    _temp: tempfile::TempDir,
    account: PathBuf,
    authorities: Vec<Authority>,
    new: PathBuf,
}
fn account() -> Account {
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
    let new = base.join("new");
    fs::create_dir(&new).unwrap();
    Account {
        _temp: temp,
        account,
        authorities,
        new,
    }
}

/// Every decision the reuse can affect on `account`, with and without it:
/// the registry scan after the migration scan, the migration scan, and a
/// whole registration.
fn decisions(account: &Account, reuse: bool) -> Vec<String> {
    let mut decisions = vec![decision(&account.account, &account.new, reuse)];
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    decisions.extend(migration(&account.account));
    let new = account.new.clone();
    decisions.push(with_reuse(reuse, || {
        match Authority::open_in(&account.account, &new) {
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
/// after an earlier registration read it must be noticed. Malformed: the
/// registration stops with an explicit-recovery error. Valid with a changed
/// physical identity: it sees an alias of the new folder with an unfinished
/// hold.
#[test]
fn registry_reads_notice_a_change_that_keeps_size_and_times() {
    let _fresh = Fresh::new();
    for valid in [false, true] {
        let account = account();
        let record = &account.authorities[1].binding_path;
        let original = fs::read(record).unwrap();
        let changed = if valid {
            // An unfinished hold: the physical-alias check refuses the new
            // folder while this record claims its identity.
            drop(account.authorities[1].begin_drain("same stamp"));
            let original = fs::read(record).unwrap();
            let mut binding: Binding = serde_json::from_slice(&original).unwrap();
            binding.root_identity = identity(&account.new).unwrap();
            let mut bytes = serde_json::to_vec(&binding).unwrap();
            assert!(bytes.len() <= original.len() + 64);
            // Pad the original and the change to one length (JSON allows
            // trailing whitespace), and let a registration read the original.
            let length = original.len() + 64;
            let mut padded = original.clone();
            padded.resize(length, b' ');
            persist_bytes(record, &padded);
            bytes.resize(length, b' ');
            let warm = account.new.with_file_name("warm");
            fs::create_dir(&warm).unwrap();
            Authority::open_in(&account.account, &warm).unwrap();
            bytes
        } else {
            let mut bytes = original.clone();
            bytes[0] = b'x';
            bytes
        };
        let before = decisions(&account, true);
        assert!(
            before.last().unwrap() == "registered",
            "control: the unchanged account registers {before:?}"
        );
        overwrite_keeping_stamp(record, &changed);
        let reused = decisions(&account, true);
        let full = decisions(&account, false);
        assert_eq!(reused, full, "valid: {valid}");
        let expected = if valid {
            "Physical vault identity has an unfinished hold"
        } else {
            "Invalid writer authority; explicit recovery required"
        };
        assert!(
            reused.last().unwrap().contains(expected),
            "the registration stops as the full scans do: {reused:?}"
        );
    }
}

/// Writes `bytes` over `path` the way records are written (temp + rename).
fn persist_bytes(path: &Path, bytes: &[u8]) {
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap()).unwrap();
    temp.write_all(bytes).unwrap();
    temp.persist(path).unwrap();
}

/// Review of #3284, finding 2: a record that can be listed and stat'ed but
/// not opened or read still stops the registration, with the full scans'
/// error. Windows: another handle denies read sharing; Unix: no read
/// permission (not enforced for root, so skipped there).
#[test]
fn registry_reads_keep_the_read_error_of_a_record() {
    let _fresh = Fresh::new();
    let account = account();
    let record = account.authorities[1].binding_path.clone();
    let before = decisions(&account, true);
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
    let listed = fs::read_dir(&account.account)
        .unwrap()
        .map(|entry| entry.unwrap())
        .find(|entry| entry.path() == record)
        .unwrap();
    listed.metadata().unwrap();
    assert!(
        File::open(&record).is_err(),
        "control: the record is unreadable"
    );
    let reused = decisions(&account, true);
    let full = decisions(&account, false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert_eq!(reused, full);
    assert!(
        reused.last().unwrap().starts_with("error"),
        "the registration stops as the full scans do: {reused:?}"
    );
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
#[test]
fn registry_reads_reread_a_record_the_migration_rewrote() {
    let _fresh = Fresh::new();
    let temp = tempfile::tempdir().unwrap();
    let account = temp.path().join("account");
    let mut authorities = Vec::new();
    for name in ["one", "two", "three"] {
        let root = temp.path().join(name);
        fs::create_dir(&root).unwrap();
        authorities.push(Authority::open_in(&account, &root).unwrap());
    }
    // Make "two" a schema 1 record as older builds wrote it.
    let legacy = &authorities[1];
    let mut binding = legacy.binding.clone();
    binding.schema = 1;
    for identity in [
        &mut binding.root_identity,
        &mut binding.directory,
        &mut binding.admission,
        &mut binding.writers,
        &mut binding.owner,
    ] {
        identity.3 = None;
    }
    persist(&legacy.binding_path, &binding, false).unwrap();
    // The migration scan rewrites it in the same lock hold as the registry
    // scan, which must read the new file instead of using what the migration
    // scan read before the rewrite.
    counters();
    let root = temp.path().join("four");
    fs::create_dir(&root).unwrap();
    Authority::open_in(&account, &root).unwrap();
    assert_eq!(read::<Binding>(&legacy.binding_path).unwrap().schema, 2);
    // Migration: reads all three records; registry scan: reuses "one" and
    // "three", reads "two" again.
    assert_eq!(counters(), [2, 4], "[reused, reads]");
}
