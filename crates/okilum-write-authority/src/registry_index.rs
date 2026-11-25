//! Derived, rebuildable summary cache of the account's writer records (#3272).
//!
//! Registering a new folder runs two scans over every `<key>.json` record in
//! the account directory: the legacy-migration scan and the registry scan.
//! Both still list the directory in the same order and, for every `.json`
//! entry, run the same `lstat`, open and read as before, with the same
//! errors. The cache only saves what comes after the read: parsing the bytes
//! and acting on a record that cannot matter.
//!
//! It maps a record's file name to the SHA-256 of the exact bytes it was
//! parsed from, together with a summary of that parse. A summary is used only
//! for a record whose bytes, just read in this scan, have that same digest.
//! It is then exactly what parsing those bytes would give, so no file
//! metadata (size, times, inode, file index) is trusted, and nothing about
//! the filesystem, its clock or other writers matters. A record whose bytes
//! cannot be opened or read fails the scan exactly as before, cached or not.
//!
//! Within one hold of `registry.lock` no record writer but this process can
//! run, so a registration's migration scan and its registry scan share that
//! hold. A record the first scan read, whose summary cannot concern the new
//! root, is not read again by the second; every other record is (as is a
//! record the migration itself rewrote). A record that cannot be read thus
//! still fails the registration, in the first scan, as before.
//!
//! A cached summary can only skip a record. Any record whose summary could
//! touch the decision (a possible physical alias, a nested root, a legacy
//! schema, a macOS mount-source volume) is parsed and handled by the
//! unchanged code. Any doubt about the cache file (missing, truncated,
//! another format or platform, a checksum mismatch, any decoding error)
//! discards it whole: every record is then parsed, as before, and a fresh
//! cache is written. The cache is a separate file without a `.json`
//! extension, so older builds, which only read `.json` entries, ignore it; it
//! is never synced, because losing it only costs the next scan its speed.
use super::*;
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
};

/// The cache file in the account directory. Never `*.json`: every build's
/// registry scan reads `*.json` entries as records.
pub(super) const FILE: &str = "registry-index-v1";
/// Bump the last byte whenever `Binding`, `Identity` or [`Summary`] change
/// what a record's bytes parse to (`registry_index_format_is_pinned`).
const MAGIC: &[u8; 8] = b"OKWRIDX2";
#[cfg(target_os = "linux")]
const PLATFORM: u8 = b'L';
#[cfg(target_os = "macos")]
const PLATFORM: u8 = b'M';
#[cfg(windows)]
const PLATFORM: u8 = b'W';
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
const PLATFORM: u8 = b'U';
/// A cache file larger than this is not read (about 2 KB per 10 records).
const MAX_BYTES: u64 = 256 * 1024 * 1024;

/// SHA-256 of a record's bytes.
pub(super) type Content = [u8; 32];

/// A record's bytes, read by `read_bytes` exactly as `read` reads them
/// (same checks, same errors), and their digest.
pub(super) fn read_record(path: &Path) -> Result<(Vec<u8>, Content)> {
    #[cfg(test)]
    READS.with(|reads| reads.set(reads.get() + 1));
    let bytes = read_bytes(path)?;
    let digest = Sha256::digest(&bytes).into();
    Ok((bytes, digest))
}

/// What the registry needs to know to skip a record it has read but not
/// parsed.
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

#[cfg(test)]
thread_local! {
    /// Tests: `false` turns the cache off, giving the pre-#3272 full scan.
    pub(super) static ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
    /// Tests: record files read (`read_bytes`, including `read`).
    pub(super) static READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Tests: records whose parse a scan skipped / records it parsed.
    pub(super) static HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static PARSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Tests: cached entries refused because the record's bytes changed.
    pub(super) static STALE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

struct Entry {
    digest: Content,
    summary: Summary,
    /// Listed by the current scan.
    seen: bool,
    /// `digest` is that of the bytes read from the record during the current
    /// hold of `registry.lock`.
    read: bool,
}

type Entries = HashMap<OsString, (Content, Summary)>;

/// Loaded on first use: opening an already registered folder never scans
/// and never pays for the cache.
pub(super) struct RecordIndex {
    account: PathBuf,
    entries: Option<HashMap<OsString, Entry>>,
    complete_scan: bool,
    changed: bool,
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
                .map(|(name, (digest, summary))| {
                    let entry = Entry {
                        digest,
                        summary,
                        seen: false,
                        read: false,
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

    /// The summary of the record named `name`, only when the bytes just read
    /// from it have the digest its summary was parsed from.
    pub(super) fn get(&mut self, name: &OsStr, digest: &Content) -> Option<&Summary> {
        let cached = self.entries().get_mut(name)?;
        cached.seen = true;
        if cached.digest != *digest {
            #[cfg(test)]
            STALE.with(|stale| stale.set(stale.get() + 1));
            cached.read = false;
            return None;
        }
        cached.read = true;
        #[cfg(test)]
        HITS.with(|hits| hits.set(hits.get() + 1));
        Some(&cached.summary)
    }

    /// The summary of the record named `name` when its bytes were already
    /// read, with this digest, earlier in the current hold of
    /// `registry.lock`, so no record writer can have changed them since.
    pub(super) fn read_in_this_hold(&mut self, name: &OsStr) -> Option<&Summary> {
        let cached = self.entries().get_mut(name)?;
        cached.seen = true;
        if !cached.read {
            return None;
        }
        #[cfg(test)]
        HITS.with(|hits| hits.set(hits.get() + 1));
        Some(&cached.summary)
    }

    /// This process is about to rewrite the record at `path` (migration):
    /// read it again before the next use.
    pub(super) fn rewritten(&mut self, path: &Path) {
        if let Some(cached) = path
            .file_name()
            .and_then(|name| self.entries().get_mut(name))
        {
            cached.read = false;
        }
    }

    /// Remember the record named `name`, parsed as `binding` from bytes with
    /// `digest`.
    pub(super) fn learn(&mut self, name: OsString, digest: Content, binding: &Binding) {
        #[cfg(test)]
        PARSES.with(|parses| parses.set(parses.get() + 1));
        #[cfg(test)]
        if self.disabled {
            return;
        }
        let entries = self.entries();
        let changed = match Summary::of(binding) {
            Some(summary) => {
                let same = entries
                    .get(&name)
                    .is_some_and(|old| old.digest == digest && old.summary == summary);
                let entry = Entry {
                    digest,
                    summary,
                    seen: true,
                    read: true,
                };
                entries.insert(name, entry);
                !same
            }
            None => entries.remove(&name).is_some(),
        };
        self.changed |= changed;
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
                .map(|(name, entry)| (name, (entry.digest, entry.summary)))
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
        let (digest, summary) = &entries[name];
        let name = name.to_str()?.as_bytes();
        let root = summary.root.to_str()?.as_bytes();
        body.extend_from_slice(&u16::try_from(name.len()).ok()?.to_le_bytes());
        body.extend_from_slice(name);
        body.extend_from_slice(digest);
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
        let digest: Content = cursor.take(32)?.try_into().ok()?;
        let schema = cursor.u32()?;
        let object = cursor.u64()?;
        let mount_source = match cursor.u8()? {
            0 => false,
            1 => true,
            _ => return None,
        };
        let length = cursor.u32()? as usize;
        let root = PathBuf::from(cursor.text(length)?);
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
            .insert(OsString::from(name), (digest, summary))
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
