//! The registration index (#3272) must never change a registry decision,
//! including its errors. Every comparison runs the same scan twice on the
//! same account: once with the index (`ENABLED`) and once as the pre-#3272
//! full scan, which parses every record.
use super::registry_index::{self, RecordIndex, Summary, ENABLED, HITS, PARSES, READS, STALE};
use super::*;

/// Records skipped from the index, record files read, records parsed, and
/// index entries refused because the bytes changed; all reset.
fn counters() -> [usize; 4] {
    [
        HITS.with(|c| c.replace(0)),
        READS.with(|c| c.replace(0)),
        PARSES.with(|c| c.replace(0)),
        STALE.with(|c| c.replace(0)),
    ]
}

/// Enables the index and restores that default when dropped, even after a
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

fn with_index<T>(enabled: bool, body: impl FnOnce() -> T) -> T {
    ENABLED.with(|e| e.set(enabled));
    let value = body();
    ENABLED.with(|e| e.set(true));
    value
}

/// The registry decision for `root`, as text. Consumes the outcome, so the
/// enclosed scopes' locks are released before the next scan. With `walk`,
/// the migration scan first walks the records with the same index, as in
/// one registry.lock hold (without rewriting anything).
fn decision(account: &Path, root: &Path, enabled: bool, walk: bool) -> String {
    with_index(enabled, || {
        let mut index = RecordIndex::new(account);
        #[cfg(any(target_os = "macos", target_os = "linux", windows))]
        if walk {
            for entry in fs::read_dir(account).unwrap() {
                if let Ok(Some((path, _))) = Authority::legacy_record(&entry.unwrap(), &mut index) {
                    index.rewritten(&path);
                }
            }
            counters();
        }
        match Authority::scan_registry(account, root, &mut index) {
            Ok(Scan::Existing(a)) => {
                format!("existing {} {:?}", a.binding_path.display(), a.binding)
            }
            Ok(Scan::Enclosed(enclosed)) => format!(
                "enclosed {:?}",
                enclosed.iter().map(|e| &e.directory).collect::<Vec<_>>()
            ),
            Err(error) => format!("error {error:#}"),
        }
    })
}

/// The migration scan's per-record decisions, as text.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn migration(account: &Path, enabled: bool) -> Vec<String> {
    with_index(enabled, || {
        let mut index = RecordIndex::new(account);
        fs::read_dir(account)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                match Authority::legacy_record(&entry, &mut index) {
                    Ok(Some((path, binding))) => format!("migrate {} {binding:?}", path.display()),
                    Ok(None) => format!("skip {:?}", entry.file_name()),
                    Err(error) => format!("error {:?} {error:#}", entry.file_name()),
                }
            })
            .collect()
    })
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

#[derive(Clone, Copy, Debug)]
enum IndexFile {
    Latest,
    Older,
    Missing,
    Truncated,
    Flipped,
    OtherPlatform,
    Garbage,
}
const INDEX_FILES: [IndexFile; 7] = [
    IndexFile::Latest,
    IndexFile::Older,
    IndexFile::Missing,
    IndexFile::Truncated,
    IndexFile::Flipped,
    IndexFile::OtherPlatform,
    IndexFile::Garbage,
];

#[derive(Default, Debug)]
struct Totals {
    decisions: usize,
    existing: usize,
    enclosed: usize,
    errors: usize,
    migrations: usize,
    hits: usize,
    stale: usize,
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
    // Build the index with every record known, then keep that file.
    for name in ["fresh", "fresh-1"] {
        let fresh = world.join(name);
        fs::create_dir(&fresh).unwrap();
        Authority::open_in(&account, &fresh).unwrap();
    }
    let older = fs::read(account.join(registry_index::FILE)).unwrap();
    // Some later registrations the older index does not know.
    for folder in &folders {
        if rng.chance(15) {
            let _ = Authority::open_in(&account, folder);
        }
    }
    let fresh = world.join("fresh-2");
    fs::create_dir(&fresh).unwrap();
    let _ = Authority::open_in(&account, &fresh);
    // Changes after the index was written: deleted, moved, replaced (and
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
    let latest = fs::read(account.join(registry_index::FILE)).unwrap();

    for mode in INDEX_FILES {
        let path = account.join(registry_index::FILE);
        let bytes = match mode {
            IndexFile::Latest => Some(latest.clone()),
            IndexFile::Older => Some(older.clone()),
            IndexFile::Missing => None,
            IndexFile::Truncated => Some(latest[..latest.len() / 2].to_vec()),
            IndexFile::Flipped => {
                let mut bytes = latest.clone();
                let at = 41 + rng.below(bytes.len() - 41);
                bytes[at] ^= 0x10;
                Some(bytes)
            }
            IndexFile::OtherPlatform => {
                let mut bytes = latest.clone();
                bytes[8] ^= 0x01;
                Some(bytes)
            }
            IndexFile::Garbage => Some(b"not an index".to_vec()),
        };
        match &bytes {
            Some(bytes) => fs::write(&path, bytes).unwrap(),
            None => fs::remove_file(&path).unwrap(),
        }
        let usable = matches!(mode, IndexFile::Latest | IndexFile::Older);
        assert_eq!(
            bytes.as_deref().and_then(registry_index::decode).is_some(),
            usable,
            "seed {seed}: {mode:?} index file"
        );
        for (number, probe) in probes.iter().enumerate() {
            let walk = number % 2 == 1;
            counters();
            let indexed = decision(&account, probe, true, walk);
            let [hits, _, _, stale] = counters();
            let full = decision(&account, probe, false, walk);
            let [full_hits, full_reads, _, _] = counters();
            // A record the full scan fails to read (a corrupt one listed first,
            // as NTFS lists by name) ends it before anything is counted.
            assert_eq!(full_hits, 0, "the full scan reads every record");
            totals.full_reads += full_reads;
            assert_eq!(
                indexed,
                full,
                "seed {seed}: {mode:?} index, root {}",
                probe.display()
            );
            if !usable && !walk {
                assert_eq!(hits, 0, "seed {seed}: a refused {mode:?} index is unused");
            }
            totals.decisions += 1;
            totals.hits += hits;
            totals.stale += stale;
            match indexed.split(' ').next() {
                Some("existing") => totals.existing += 1,
                Some("enclosed") => totals.enclosed += 1,
                _ => totals.errors += 1,
            }
            // The skip predicate itself, against every readable record.
            let ancestors: Vec<Identity> =
                probe.ancestors().map(|p| identity(p).unwrap()).collect();
            for binding in bindings(&account) {
                let summary = Summary::of(&binding).unwrap();
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
            let indexed = migration(&account, true);
            let full = migration(&account, false);
            assert_eq!(indexed, full, "seed {seed}: {mode:?} index, migration");
            totals.migrations += indexed.iter().filter(|d| d.starts_with("migrate")).count();
        }
    }
}

#[test]
fn registry_index_decisions_equal_the_full_scan() {
    let seeds: u64 = std::env::var("REGISTRY_INDEX_SEEDS")
        .ok()
        .and_then(|seeds| seeds.parse().ok())
        .unwrap_or(40);
    let mut totals = Totals::default();
    for seed in 0..seeds {
        scenario(seed, &mut totals);
    }
    println!("registry index equivalence: {seeds} seeds, {totals:?}");
    assert_eq!(totals.unread_concerning, 0, "the skip predicate is sound");
    // Controls: the comparisons exercised the index and every outcome.
    assert!(totals.hits > 0, "the index skipped records");
    assert!(
        totals.stale > 0,
        "changed records were refused from the index"
    );
    assert!(totals.existing > 0 && totals.enclosed > 0 && totals.errors > 0);
    assert!(totals.migrations > 0, "legacy records were compared");
    assert!(
        totals.same_stamp > 0,
        "records changed under an equal stamp"
    );
}

#[test]
fn registry_index_is_ignored_by_older_builds_and_changes_no_record() {
    let _fresh = Fresh::new();
    let temp = tempfile::tempdir().unwrap();
    let account = temp.path().join("account");
    let mut roots = Vec::new();
    for name in ["one", "two", "three"] {
        let root = temp.path().join(name);
        fs::create_dir(&root).unwrap();
        roots.push(Authority::open_in(&account, &root).unwrap());
    }
    let index = account.join(registry_index::FILE);
    assert!(index.is_file(), "registration wrote the index");
    assert!(
        index.extension().is_none_or(|e| e != "json"),
        "every build reads only *.json entries as records"
    );
    let records = |account: &Path| -> Vec<(PathBuf, Vec<u8>)> {
        let mut records: Vec<_> = fs::read_dir(account)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .map(|p| (p.clone(), fs::read(p).unwrap()))
            .collect();
        records.sort();
        records
    };
    let before = records(&account);
    // An older build: the full scan, with the index file present.
    let older = temp.path().join("older");
    fs::create_dir(&older).unwrap();
    with_index(false, || Authority::open_in(&account, &older)).unwrap();
    let parent = temp.path().canonicalize().unwrap();
    let full = decision(&account.canonicalize().unwrap(), &parent, false, false);
    assert!(full.starts_with("enclosed"), "{full}");
    // The records written with the index are byte-identical afterwards.
    let after = records(&account);
    assert_eq!(after.len(), before.len() + 1);
    for record in &before {
        assert!(after.contains(record));
    }
    for authority in roots {
        authority.operation().unwrap();
    }
}

#[test]
fn registry_index_reads_every_record_and_parses_only_unknown_ones() {
    let _fresh = Fresh::new();
    let temp = tempfile::tempdir().unwrap();
    let account = temp.path().join("account");
    let count = 120;
    for number in 0..count {
        let root = temp.path().join(format!("folder-{number}"));
        fs::create_dir(&root).unwrap();
        Authority::open_in(&account, &root).unwrap();
    }
    // A missing index: the migration scan reads and parses every record once;
    // the registry scan, in the same registry.lock hold, uses what it read.
    fs::remove_file(account.join(registry_index::FILE)).unwrap();
    counters();
    let root = temp.path().join("first");
    fs::create_dir(&root).unwrap();
    Authority::open_in(&account, &root).unwrap();
    let [hits, reads, parses, _] = counters();
    assert_eq!((reads, parses, hits), (count, count, count));
    // With the index: every record is still read (a read error still stops
    // the registration), but only the one the last registration added is
    // parsed.
    let root = temp.path().join("second");
    fs::create_dir(&root).unwrap();
    Authority::open_in(&account, &root).unwrap();
    let [hits, reads, parses, _] = counters();
    assert_eq!((reads, parses, hits), (count + 1, 1, 2 * count + 1));
    // Control: without the index every record is read and parsed twice.
    let root = temp.path().join("third");
    fs::create_dir(&root).unwrap();
    with_index(false, || Authority::open_in(&account, &root)).unwrap();
    let [hits, reads, parses, _] = counters();
    assert_eq!((reads, parses, hits), (2 * (count + 2), 2 * (count + 2), 0));
}

/// Rewrites `path` in place with `bytes` and restores its modification time
/// (and, on Windows, its creation time), as `SetFileTime` allows any program
/// to. Only Unix ctime still changes; Okilum's index trusts none of these.
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

/// An account of four registered folders, the first three in the index, and a
/// new unrelated folder to register next.
struct Cached {
    _temp: tempfile::TempDir,
    account: PathBuf,
    authorities: Vec<Authority>,
    new: PathBuf,
}
fn cached() -> Cached {
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
    Cached {
        _temp: temp,
        account,
        authorities,
        new,
    }
}

/// Every decision the index can take on `cached`'s account, with and
/// without it: the registry scan (alone and after the migration scan in the
/// same hold), the migration scan, and a whole registration.
fn decisions(cached: &Cached, enabled: bool) -> Vec<String> {
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
fn registry_index_notices_a_change_that_keeps_size_and_times() {
    let _fresh = Fresh::new();
    for valid in [false, true] {
        let cached = cached();
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
            persist_bytes(record, &padded);
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
        let before = decisions(&cached, true);
        assert!(
            before.last().unwrap() == "registered",
            "control: the unchanged account registers {before:?}"
        );
        overwrite_keeping_stamp(record, &changed);
        counters();
        let indexed = decisions(&cached, true);
        let [_, _, _, stale] = counters();
        let full = decisions(&cached, false);
        assert_eq!(indexed, full, "valid: {valid}");
        assert!(stale > 0, "the index entry was refused");
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
}

/// Writes `bytes` over `path` the way records are written (temp + rename).
fn persist_bytes(path: &Path, bytes: &[u8]) {
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap()).unwrap();
    temp.write_all(bytes).unwrap();
    temp.persist(path).unwrap();
}

/// Review of #3284, finding 2: a cached record that can be listed and
/// stat'ed but not opened or read still stops the registration, with the
/// full scan's error. Windows: another handle denies read sharing; Unix: no
/// read permission (not enforced for root, so skipped there).
#[test]
fn registry_index_keeps_the_read_error_of_a_cached_record() {
    let _fresh = Fresh::new();
    let cached = cached();
    let record = cached.authorities[1].binding_path.clone();
    let before = decisions(&cached, true);
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
    let indexed = decisions(&cached, true);
    let full = decisions(&cached, false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert_eq!(indexed, full);
    assert!(
        indexed.last().unwrap().starts_with("error"),
        "the registration stops as the full scan does: {indexed:?}"
    );
}

/// The index caches what a record's bytes parse to; its MAGIC must change
/// with that parse. This pins the definitions it depends on.
#[test]
fn registry_index_format_is_pinned() {
    fn item(source: &str, start: &str, end: &str) -> String {
        let source = source.replace('\r', "");
        let from = source.find(start).expect(start);
        let to = from + source[from..].find(end).expect(end) + end.len();
        source[from..to].to_owned()
    }
    let shape = [
        item(include_str!("lib.rs"), "struct Binding {", "\n}"),
        item(include_str!("lib.rs"), "struct Identity(", ");"),
        item(include_str!("lib.rs"), "fn parse<", "\n}"),
        item(
            include_str!("identity_linux.rs"),
            "pub struct Identity(",
            ");",
        ),
        item(
            include_str!("identity_macos.rs"),
            "pub struct Identity(",
            ");",
        ),
        item(
            include_str!("identity_windows.rs"),
            "pub struct Identity(",
            ");",
        ),
        item(
            include_str!("identity_windows.rs"),
            "pub enum FileIdentity {",
            "\n}",
        ),
        item(
            include_str!("registry_index.rs"),
            "pub(super) fn of(",
            "\n    }",
        ),
    ]
    .join("\n");
    assert_eq!(
        format!("{:x}", Sha256::digest(shape)),
        "20646442785db736052749a83b86338ec0172893f316c7b09bbb17a47c82e731",
        "a record's parse changed: bump registry_index::MAGIC, then update this pin"
    );
}

#[test]
fn registry_index_refuses_a_damaged_or_foreign_file() {
    let _fresh = Fresh::new();
    let temp = tempfile::tempdir().unwrap();
    let account = temp.path().join("account");
    for name in ["one", "two"] {
        let root = temp.path().join(name);
        fs::create_dir(&root).unwrap();
        Authority::open_in(&account, &root).unwrap();
    }
    let path = account.join(registry_index::FILE);
    let bytes = fs::read(&path).unwrap();
    assert_eq!(registry_index::decode(&bytes).map(|e| e.len()), Some(1));
    for at in 0..bytes.len() {
        let mut damaged = bytes.clone();
        damaged[at] ^= 0x01;
        assert!(registry_index::decode(&damaged).is_none(), "byte {at}");
    }
    for length in 0..bytes.len() {
        assert!(registry_index::decode(&bytes[..length]).is_none());
    }
    let mut longer = bytes.clone();
    longer.push(0);
    assert!(registry_index::decode(&longer).is_none());
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
#[test]
fn registry_index_rereads_a_record_the_migration_rewrote() {
    let _fresh = Fresh::new();
    let temp = tempfile::tempdir().unwrap();
    let account = temp.path().join("account");
    let mut authorities = Vec::new();
    for name in ["one", "two", "three"] {
        let root = temp.path().join(name);
        fs::create_dir(&root).unwrap();
        authorities.push(Authority::open_in(&account, &root).unwrap());
    }
    // Make "two" a schema 1 record as older builds wrote it, then let a
    // registration learn it into the index without migrating it: its folder
    // is unrelated, so a failed migration is ignored.
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
    let busy = file(&legacy.directory.join("writers.lock"), false).unwrap();
    busy.try_lock().unwrap(); // a live writer: migration refuses and retries later
    let root = temp.path().join("four");
    fs::create_dir(&root).unwrap();
    Authority::open_in(&account, &root).unwrap();
    assert_eq!(read::<Binding>(&legacy.binding_path).unwrap().schema, 1);
    drop(busy);
    // Now the migration scan rewrites it in the same lock hold as the
    // registry scan, which must read the new file instead of using what the
    // migration scan read before the rewrite.
    counters();
    let root = temp.path().join("five");
    fs::create_dir(&root).unwrap();
    Authority::open_in(&account, &root).unwrap();
    assert_eq!(read::<Binding>(&legacy.binding_path).unwrap().schema, 2);
    let [_, reads, parses, stale] = counters();
    // Migration: reads all four records, parses "two" (cached schema 1, to
    // migrate) and "four" (new); registry scan: reads and parses "two" again,
    // now different bytes.
    assert_eq!((reads, parses, stale), (5, 3, 1));
}
