//! The registration index (#3272) must never change a registry decision.
//! Every comparison runs the same scan twice on the same account: once with
//! the index (`ENABLED`) and once as the pre-#3272 full scan, which reads
//! every record.
use super::registry_index::{self, RecordIndex, Summary, ENABLED, HITS, READS, SETTLED, STALE};
use super::*;

fn counters() -> [usize; 3] {
    [
        HITS.with(|c| c.replace(0)),
        READS.with(|c| c.replace(0)),
        STALE.with(|c| c.replace(0)),
    ]
}

/// Settles every record for the test (no SETTLE wait) and restores the
/// defaults when dropped, even after a failed assertion.
struct Settled;
impl Settled {
    fn new() -> Self {
        SETTLED.with(|s| s.set(true));
        ENABLED.with(|e| e.set(true));
        counters();
        Self
    }
}
impl Drop for Settled {
    fn drop(&mut self) {
        SETTLED.with(|s| s.set(false));
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
}

const NAMES: [&str; 7] = ["a", "A", "b", "c d", "é", "nest", "Nest"];

fn scenario(seed: u64, totals: &mut Totals) {
    let _settled = Settled::new();
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
        match rng.below(7) {
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
                    // Another size than any record: the stamp changes even
                    // within one coarse timestamp tick.
                    fs::write(&record, b"{").unwrap();
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
            let [hits, _, stale] = counters();
            let full = decision(&account, probe, false, walk);
            let [full_hits, full_reads, _] = counters();
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
}

#[test]
fn registry_index_is_ignored_by_older_builds_and_changes_no_record() {
    let _settled = Settled::new();
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
fn registry_index_reads_only_unknown_records() {
    let _settled = Settled::new();
    let temp = tempfile::tempdir().unwrap();
    let account = temp.path().join("account");
    let count = 120;
    for number in 0..count {
        let root = temp.path().join(format!("folder-{number}"));
        fs::create_dir(&root).unwrap();
        Authority::open_in(&account, &root).unwrap();
    }
    // A missing index: one full read of the records (and the migration scan
    // learns them first), then a saved index.
    fs::remove_file(account.join(registry_index::FILE)).unwrap();
    counters();
    let root = temp.path().join("first");
    fs::create_dir(&root).unwrap();
    Authority::open_in(&account, &root).unwrap();
    let [hits, reads, _] = counters();
    assert_eq!(reads, count, "the migration scan read every record once");
    assert_eq!(hits, count, "the registry scan used what it learned");
    // With the index: only the record the last registration added is read,
    // by the migration scan; the registry scan, in the same lock hold, uses
    // what that scan checked and learned.
    let root = temp.path().join("second");
    fs::create_dir(&root).unwrap();
    Authority::open_in(&account, &root).unwrap();
    let [hits, reads, _] = counters();
    assert_eq!((reads, hits), (1, 2 * count + 1));
    // Control: without the index every record is read twice.
    let root = temp.path().join("third");
    fs::create_dir(&root).unwrap();
    with_index(false, || Authority::open_in(&account, &root)).unwrap();
    let [hits, reads, _] = counters();
    assert_eq!((reads, hits), (2 * (count + 2), 0));
}

#[test]
fn registry_index_caches_a_record_only_after_it_settles() {
    ENABLED.with(|e| e.set(true));
    let temp = tempfile::tempdir().unwrap();
    let account = temp.path().join("account");
    let new = |name: &str| {
        let root = temp.path().join(name);
        fs::create_dir(&root).unwrap();
        Authority::open_in(&account, &root).unwrap();
    };
    new("one");
    new("two");
    let account = account.canonicalize().unwrap();
    assert!(
        !account.join(registry_index::FILE).exists(),
        "a record younger than SETTLE is never cached"
    );
    std::thread::sleep(registry_index::SETTLE + std::time::Duration::from_millis(200));
    new("three");
    assert_eq!(RecordIndex::new(&account).len(), 2, "both settled records");
}

#[test]
fn registry_index_refuses_a_damaged_or_foreign_file() {
    let _settled = Settled::new();
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
    let _settled = Settled::new();
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
    // registry scan, which must read the new file instead of trusting the
    // stamp checked before the rewrite.
    counters();
    let root = temp.path().join("five");
    fs::create_dir(&root).unwrap();
    Authority::open_in(&account, &root).unwrap();
    assert_eq!(read::<Binding>(&legacy.binding_path).unwrap().schema, 2);
    let [_, reads, stale] = counters();
    // Migration: "two" (cached schema 1, read to migrate) and "four" (new);
    // registry scan: "two" again, now a different file.
    assert_eq!((reads, stale), (3, 1));
}
