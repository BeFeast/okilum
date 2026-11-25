//! Derived, rebuildable summary cache of the account's writer records (#3272).
//!
//! Registering a new folder used to open and parse every `<key>.json` record
//! twice (the legacy-migration scan and the registry scan), about 20 us each.
//! Records are never deleted or rewritten for this; the registry still lists
//! the account directory in the same order and visits every `.json` entry, so
//! every decision is made exactly as before. What changes is only how an
//! entry that cannot matter is recognized: from a cached summary instead of
//! the file's content.
//!
//! A summary is used only while the directory entry still has the stamp of
//! the very file it was read from (Unix: inode, size, mtime and ctime;
//! Windows: size, creation and last-write time). Every record writer, in this
//! build and in older ones, replaces a record by renaming a new temporary
//! file over it, so a rewrite always gives the entry a new stamp. A record
//! is learned only once its stamp is at least [`SETTLE`] old, and only under
//! `registry.lock`, which every record writer also holds. A replacement made
//! after learning therefore always carries a later change time (Unix ctime,
//! Windows last-write time), even when a coarse filesystem clock, inode reuse
//! or NTFS name tunnelling would otherwise repeat the rest of the stamp.
//!
//! Within one hold of `registry.lock` no record writer but this process can
//! run, so a registration's migration scan and its registry scan share that
//! hold: a stamp checked by the first is not checked again by the second,
//! except for a record the migration itself rewrote.
//!
//! A cached summary can only skip a record. Any record whose summary could
//! touch the decision (a possible physical alias, a nested root, a legacy
//! schema, a macOS mount-source volume) is read from disk and handled by the
//! unchanged code. Any doubt about the cache file (missing, truncated,
//! another version or platform, a checksum mismatch, any decoding error)
//! discards it whole: the scan then reads every record, as before, and writes
//! a fresh cache. The cache is a separate file without a `.json` extension,
//! so older builds, which only read `.json` entries, ignore it; it is never
//! synced, because losing it only costs the next scan its speed.
use super::*;
use std::{
    collections::HashMap,
    ffi::OsString,
    fs::{DirEntry, Metadata},
    time::{Duration, SystemTime},
};

/// The cache file in the account directory. Never `*.json`: every build's
/// registry scan reads `*.json` entries as records.
pub(super) const FILE: &str = "registry-index-v1";
const MAGIC: &[u8; 8] = b"OKWRIDX1";
#[cfg(target_os = "linux")]
const PLATFORM: u8 = b'L';
#[cfg(target_os = "macos")]
const PLATFORM: u8 = b'M';
#[cfg(windows)]
const PLATFORM: u8 = b'W';
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
const PLATFORM: u8 = b'U';
/// A cache file larger than this is not read (about 6 KB per 10 records).
const MAX_BYTES: u64 = 256 * 1024 * 1024;
/// How old a record's last change must be before its summary may be cached.
pub(super) const SETTLE: Duration = Duration::from_secs(2);

/// What the registry needs to know to skip a record without reading it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Summary {
    pub(super) schema: u32,
    pub(super) root: PathBuf,
    /// `root_identity.1`: the object number every identity equality and
    /// legacy-candidate comparison requires to match (see `physical_candidate`).
    pub(super) object: u64,
    /// macOS: the root is on a UUID-less volume, named by its mount source.
    pub(super) mount_source: bool,
}

impl Summary {
    pub(super) fn of(binding: &Binding) -> Option<Self> {
        // Records are JSON, so their roots are always valid Unicode; keep the
        // cache format free of platform path encodings anyway.
        binding.root.to_str()?;
        Some(Self {
            schema: binding.schema,
            root: binding.root.clone(),
            object: binding.root_identity.1,
            #[cfg(target_os = "macos")]
            mount_source: binding.root_identity.is_mount_source(),
            #[cfg(not(target_os = "macos"))]
            mount_source: false,
        })
    }

    /// Whether the registry scan for `root` (with the identities of `root`
    /// and each ancestor) could act on this record. False only when none of
    /// the scan's checks can apply: the macOS mount-source check, a physical
    /// alias candidate, or a root nested inside `root`.
    pub(super) fn may_concern(&self, root: &Path, ancestors: &[Identity]) -> bool {
        self.mount_source
            || ancestors.iter().any(|identity| identity.1 == self.object)
            || (self.root.starts_with(root) && self.root != root)
    }
}

type Stamp = [u64; 6];

fn stamp(meta: &Metadata) -> Option<Stamp> {
    if !meta.file_type().is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some([
            meta.ino(),
            meta.size(),
            meta.mtime() as u64,
            meta.mtime_nsec() as u64,
            meta.ctime() as u64,
            meta.ctime_nsec() as u64,
        ])
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        Some([
            meta.file_size(),
            meta.creation_time(),
            meta.last_write_time(),
            0,
            0,
            0,
        ])
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

/// When the file last changed in a way no writer can choose: Unix ctime
/// (set on every rename and write), Windows last-write time (a renamed
/// temporary file keeps the time it was written, while its creation time
/// can be tunnelled from the name it replaces).
fn changed(meta: &Metadata) -> Option<SystemTime> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let seconds = u64::try_from(meta.ctime()).ok()?;
        let nanos = u32::try_from(meta.ctime_nsec()).ok()?;
        SystemTime::UNIX_EPOCH.checked_add(Duration::new(seconds, nanos))
    }
    #[cfg(not(unix))]
    {
        meta.modified().ok()
    }
}

#[cfg(test)]
thread_local! {
    /// Tests: `false` turns the cache off, giving the pre-#3272 full scan.
    pub(super) static ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
    /// Tests: treat every record as settled, so a scenario can be cached
    /// without waiting [`SETTLE`].
    pub(super) static SETTLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Tests: records skipped from the cache / read from disk by scans.
    pub(super) static HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Tests: cached entries refused because the file changed since.
    pub(super) static STALE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

struct Entry {
    stamp: Stamp,
    summary: Summary,
    /// Listed by the current scan.
    seen: bool,
    /// Its stamp was checked during the current hold of `registry.lock`.
    checked: bool,
}

type Entries = HashMap<OsString, (Stamp, Summary)>;

/// Loaded on first use: opening an already registered folder never scans
/// and never pays for the cache.
pub(super) struct RecordIndex {
    account: PathBuf,
    entries: Option<HashMap<OsString, Entry>>,
    complete_scan: bool,
    changed: bool,
    now: Option<SystemTime>,
    #[cfg(test)]
    disabled: bool,
}

impl RecordIndex {
    pub(super) fn new(account: &Path) -> Self {
        Self {
            account: account.to_owned(),
            entries: None,
            complete_scan: false,
            changed: false,
            now: None,
            #[cfg(test)]
            disabled: !ENABLED.with(|enabled| enabled.get()),
        }
    }

    /// The cache file's entries, or none on any doubt.
    fn entries(&mut self) -> &mut HashMap<OsString, Entry> {
        let account = &self.account;
        #[cfg(test)]
        let disabled = self.disabled;
        self.entries.get_or_insert_with(|| {
            #[cfg(test)]
            if disabled {
                return HashMap::new();
            }
            read_file(&account.join(FILE))
                .unwrap_or_default()
                .into_iter()
                .map(|(name, (stamp, summary))| {
                    let entry = Entry {
                        stamp,
                        summary,
                        seen: false,
                        checked: false,
                    };
                    (name, entry)
                })
                .collect()
        })
    }

    #[cfg(test)]
    pub(super) fn len(&mut self) -> usize {
        self.entries().len()
    }

    /// The summary of the record listed as `entry` at `path`, only while the
    /// entry is still the exact file it was learned from.
    pub(super) fn get(&mut self, entry: &DirEntry, path: &Path) -> Option<&Summary> {
        let cached = self.entries().get_mut(path.file_name()?)?;
        cached.seen = true;
        // Checked earlier in this same registry.lock hold, and no record
        // writer can have run since: skip the second stat.
        if !cached.checked {
            if stamp(&entry.metadata().ok()?)? != cached.stamp {
                #[cfg(test)]
                STALE.with(|stale| stale.set(stale.get() + 1));
                return None;
            }
            cached.checked = true;
        }
        #[cfg(test)]
        HITS.with(|hits| hits.set(hits.get() + 1));
        Some(&cached.summary)
    }

    /// Remember a record read from `entry`. `read` is the metadata of the
    /// handle the content came from; it is cached only when the directory
    /// entry still shows that same file and its last change has settled.
    pub(super) fn learn(&mut self, entry: &DirEntry, read: Option<&Metadata>, binding: &Binding) {
        #[cfg(test)]
        READS.with(|reads| reads.set(reads.get() + 1));
        #[cfg(test)]
        if self.disabled {
            return;
        }
        let name = entry.file_name();
        let learned = (|| {
            let read = read?;
            let listed = entry.metadata().ok()?;
            let stamp_read = stamp(read)?;
            (stamp(&listed)? == stamp_read && self.settled(read)?).then_some(())?;
            // A record is only ever stored under its root's key; anything else
            // is read every time rather than cached (the cache refuses it).
            (name.to_str()? == format!("{}.json", key(&binding.root))).then_some(())?;
            Some(Entry {
                stamp: stamp_read,
                summary: Summary::of(binding)?,
                seen: true,
                checked: true,
            })
        })();
        let entries = self.entries();
        let changed = match learned {
            Some(value) => {
                let same = entries
                    .get(&name)
                    .is_some_and(|old| old.stamp == value.stamp && old.summary == value.summary);
                entries.insert(name, value);
                !same
            }
            None => entries.remove(&name).is_some(),
        };
        self.changed |= changed;
    }

    /// This process is about to rewrite the record at `path` (migration):
    /// check its stamp again before the next use.
    pub(super) fn rewritten(&mut self, path: &Path) {
        if let Some(cached) = path
            .file_name()
            .and_then(|name| self.entries().get_mut(name))
        {
            cached.checked = false;
        }
    }

    fn settled(&mut self, meta: &Metadata) -> Option<bool> {
        #[cfg(test)]
        if SETTLED.with(|settled| settled.get()) {
            return Some(true);
        }
        let now = *self.now.get_or_insert_with(SystemTime::now);
        Some(changed(meta)?.checked_add(SETTLE)? <= now)
    }

    /// The scan listed the whole directory; forget names it did not see.
    pub(super) fn scanned_all(&mut self) {
        self.complete_scan = true;
    }

    /// Best effort: a lost or torn cache file is detected and rebuilt.
    pub(super) fn save(self) {
        let Some(mut entries) = self.entries else {
            return;
        };
        let mut changed = self.changed;
        if self.complete_scan {
            let before = entries.len();
            entries.retain(|_, entry| entry.seen);
            changed |= entries.len() != before;
        }
        #[cfg(test)]
        if self.disabled {
            return;
        }
        if changed {
            let entries = entries
                .into_iter()
                .map(|(name, entry)| (name, (entry.stamp, entry.summary)))
                .collect();
            let _ = write_file(&self.account, &entries);
        }
    }
}

fn encode(entries: &Entries) -> Option<Vec<u8>> {
    let mut body = Vec::with_capacity(entries.len() * 200 + 4);
    body.extend_from_slice(&u32::try_from(entries.len()).ok()?.to_le_bytes());
    let mut names: Vec<_> = entries.keys().collect();
    names.sort();
    for name in names {
        let (stamp, summary) = &entries[name];
        let name = name.to_str()?.as_bytes();
        let root = summary.root.to_str()?.as_bytes();
        body.extend_from_slice(&u16::try_from(name.len()).ok()?.to_le_bytes());
        body.extend_from_slice(name);
        for value in stamp {
            body.extend_from_slice(&value.to_le_bytes());
        }
        body.extend_from_slice(&summary.schema.to_le_bytes());
        body.extend_from_slice(&summary.object.to_le_bytes());
        body.push(u8::from(summary.mount_source));
        body.extend_from_slice(&u32::try_from(root.len()).ok()?.to_le_bytes());
        body.extend_from_slice(root);
    }
    let mut out = Vec::with_capacity(body.len() + 41);
    out.extend_from_slice(MAGIC);
    out.push(PLATFORM);
    out.extend_from_slice(&Sha256::digest(&body));
    out.extend_from_slice(&body);
    Some(out)
}

struct Cursor<'a>(&'a [u8]);
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Some(head)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn text(&mut self, n: usize) -> Option<&'a str> {
        std::str::from_utf8(self.take(n)?).ok()
    }
}

pub(super) fn decode(bytes: &[u8]) -> Option<Entries> {
    let mut cursor = Cursor(bytes);
    if cursor.take(MAGIC.len())? != MAGIC || cursor.u8()? != PLATFORM {
        return None;
    }
    let checksum = cursor.take(32)?;
    if Sha256::digest(cursor.0).as_slice() != checksum {
        return None;
    }
    let count = cursor.u32()? as usize;
    let mut entries = HashMap::with_capacity(count.min(1 << 20));
    for _ in 0..count {
        let length = cursor.u16()? as usize;
        let name = cursor.text(length)?;
        let mut stamp = [0; 6];
        for value in &mut stamp {
            *value = cursor.u64()?;
        }
        let schema = cursor.u32()?;
        let object = cursor.u64()?;
        let mount_source = match cursor.u8()? {
            0 => false,
            1 => true,
            _ => return None,
        };
        let length = cursor.u32()? as usize;
        let root = PathBuf::from(cursor.text(length)?);
        // Names were checked against their roots' keys when learned; the
        // checksum covers the rest.
        if !name.ends_with(".json") || !root.is_absolute() {
            return None;
        }
        let summary = Summary {
            schema,
            root,
            object,
            mount_source,
        };
        if entries
            .insert(OsString::from(name), (stamp, summary))
            .is_some()
        {
            return None;
        }
    }
    cursor.0.is_empty().then_some(entries)
}

fn read_file(path: &Path) -> Option<Entries> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    File::open(path)
        .ok()?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    decode(&bytes)
}

fn write_file(account: &Path, entries: &Entries) -> Option<()> {
    let bytes = encode(entries)?;
    let mut temp = tempfile::NamedTempFile::new_in(account).ok()?;
    temp.write_all(&bytes).ok()?;
    temp.persist(account.join(FILE)).ok()?;
    Some(())
}
