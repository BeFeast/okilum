//! Record reads shared within one hold of `registry.lock` (#3272).
//!
//! Registering a new folder runs two scans over every `<key>.json` record in
//! the account directory under one hold of `registry.lock`: the
//! legacy-migration scan and then the registry scan. While that lock is held
//! no record writer but this process can run, so a record the migration scan
//! read and parsed is still what it read when the registry scan reaches it.
//!
//! The registry scan therefore skips a record the migration scan already
//! read when its summary shows that none of the registry checks can concern
//! the new root. It reads again, exactly as before, every other record: one
//! whose summary may concern the root (a possible physical alias, a nested
//! root, a macOS mount-source volume), one the migration rewrote, and one the
//! migration scan did not read. A record that cannot be read or parsed still
//! fails the registration, in the migration scan, with the same error.
//!
//! Nothing here outlives the hold: no file is written or read. A
//! `registry-index-v1` file left in the account directory by an earlier
//! build of #3272 is not a `.json` entry, so every scan ignores it.
use super::*;
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
};

/// A record read with `read`, exactly as the scans read it before #3272.
pub(super) fn read_record(path: &Path) -> Result<Binding> {
    #[cfg(test)]
    READS.with(|reads| reads.set(reads.get() + 1));
    read(path)
}

/// What the registry scan needs to know to skip a record already read in
/// this hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Summary {
    pub(super) root: PathBuf,
    /// `root_identity.1`: the object number every identity equality and
    /// legacy-candidate comparison requires to match (see `physical_candidate`).
    pub(super) object: u64,
    /// macOS: the root is on a UUID-less volume, named by its mount source.
    pub(super) mount_source: bool,
}

impl Summary {
    pub(super) fn of(binding: &Binding) -> Self {
        Self {
            root: binding.root.clone(),
            object: binding.root_identity.1,
            #[cfg(target_os = "macos")]
            mount_source: binding.root_identity.is_mount_source(),
            #[cfg(not(target_os = "macos"))]
            mount_source: false,
        }
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
    /// Tests: `false` turns the reuse off, giving the pre-#3272 two full scans.
    pub(super) static ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
    /// Tests: records read by the two scans (`read_record`).
    pub(super) static READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Tests: records the registry scan skipped because this hold read them.
    pub(super) static REUSED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The records read in the current hold of `registry.lock`, by file name.
/// Create one per hold; drop it when the hold ends.
#[derive(Default)]
pub(super) struct HoldReads {
    read: HashMap<OsString, Summary>,
}

impl HoldReads {
    /// The record named `name` was just read, under this hold, as `binding`.
    pub(super) fn learn(&mut self, name: OsString, binding: &Binding) {
        #[cfg(test)]
        if !ENABLED.with(|enabled| enabled.get()) {
            return;
        }
        self.read.insert(name, Summary::of(binding));
    }

    /// Whether the registry scan for `root` (with the identities of `root`
    /// and each ancestor) can skip the record named `name`: it was read
    /// earlier in this hold, so no record writer can have changed it since,
    /// and it cannot concern `root`.
    pub(super) fn skip(&self, name: &OsStr, root: &Path, ancestors: &[Identity]) -> bool {
        let skip = self
            .read
            .get(name)
            .is_some_and(|summary| !summary.may_concern(root, ancestors));
        #[cfg(test)]
        if skip {
            REUSED.with(|reused| reused.set(reused.get() + 1));
        }
        skip
    }

    /// This process is about to rewrite the record at `path` (migration):
    /// whatever it writes, read it again before the next use.
    pub(super) fn rewritten(&mut self, path: &Path) {
        if let Some(name) = path.file_name() {
            self.read.remove(name);
        }
    }
}
