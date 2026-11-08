//! Lossless first-sync planning. No daemon discovery, file IO or implicit consent.
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// Complete, authenticated inventory of regular canonical files. Adapters must
/// reject symlinks, unreadable entries, incomplete pagination and unknown kinds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// Binds device, folder, canonical root and ignore policy to this inventory.
    pub scope: String,
    /// Opaque revision of the complete inventory, including additions/deletions.
    pub revision: String,
    pub files: BTreeMap<String, Vec<u8>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Equal,
    OnlyHere,
    OnlyThere,
    Different,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub kind: Kind,
    /// Both sides retain the local version at `path` and the remote version here.
    pub conflict_path: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    here: Snapshot,
    there: Snapshot,
    entries: Vec<Entry>,
    merged: BTreeMap<String, Vec<u8>>,
    fingerprint: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidInventory,
    SameScope,
    Stale,
    UnrelatedConsentRequired,
    Backend(String),
}
/// Opaque consent cannot be transplanted to a different plan or revision.
#[derive(Clone, Debug)]
pub struct Consent([u8; 32]);

impl Plan {
    /// Exact read-only inputs retained by this revision-bound plan.
    pub fn originals(&self) -> (&Snapshot, &Snapshot) {
        (&self.here, &self.there)
    }
    /// Proposed lossless union. Inspecting it grants no publication authority.
    pub fn merged_files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.merged
    }
    pub fn preview(here: Snapshot, there: Snapshot) -> Result<Self, Error> {
        validate(&here)?;
        validate(&there)?;
        if here.scope == there.scope {
            return Err(Error::SameScope);
        }
        let paths: BTreeSet<_> = here
            .files
            .keys()
            .chain(there.files.keys())
            .cloned()
            .collect();
        // File/directory collisions cannot be resolved by creating a conflict file.
        for path in &paths {
            if ancestors(path).any(|parent| paths.contains(parent)) {
                return Err(Error::InvalidInventory);
            }
        }
        let mut reserved = paths.clone();
        let mut merged = here.files.clone();
        let mut entries = Vec::new();
        for path in paths {
            let mut conflict_path = None;
            let kind = match (here.files.get(&path), there.files.get(&path)) {
                (Some(a), Some(b)) if a == b => Kind::Equal,
                (Some(_), Some(b)) => {
                    let mut n = 1u64;
                    let target = loop {
                        let candidate = conflict_copy_path(&path, n);
                        if !reserved
                            .iter()
                            .any(|p| p == &candidate || p.starts_with(&format!("{candidate}/")))
                        {
                            break candidate;
                        }
                        n = n.checked_add(1).ok_or(Error::InvalidInventory)?;
                    };
                    reserved.insert(target.clone());
                    merged.insert(target.clone(), b.clone());
                    conflict_path = Some(target);
                    Kind::Different
                }
                (Some(_), None) => Kind::OnlyHere,
                (None, Some(b)) => {
                    merged.insert(path.clone(), b.clone());
                    Kind::OnlyThere
                }
                (None, None) => unreachable!(),
            };
            entries.push(Entry {
                path,
                kind,
                conflict_path,
            });
        }
        let fingerprint = plan_fingerprint(&here, &there);
        Ok(Self {
            here,
            there,
            entries,
            merged,
            fingerprint,
        })
    }
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
    pub fn count(&self, kind: Kind) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.kind == kind)
            .count()
    }
    /// Conservative warning: fewer than half of paths occur on both sides.
    /// This is a visible heuristic, never authority to reject or overwrite data.
    pub fn unrelated_warning(&self) -> bool {
        let overlap = self.count(Kind::Equal) + self.count(Kind::Different);
        overlap * 2 < self.entries.len()
    }
    pub fn consent(&self, acknowledge_unrelated: bool) -> Result<Consent, Error> {
        if self.unrelated_warning() && !acknowledge_unrelated {
            return Err(Error::UnrelatedConsentRequired);
        }
        Ok(Consent(self.fingerprint))
    }
    pub fn apply(
        &self,
        consent: Consent,
        backend: &mut (impl Backend + ?Sized),
    ) -> Result<(), Error> {
        if consent.0 != self.fingerprint {
            return Err(Error::Stale);
        }
        backend.apply_if_unchanged(&self.here, &self.there, &self.merged)
    }
}
/// Insert the visible marker before the extension so Markdown and binary
/// copies retain their file type. The number is chosen against both inventories.
fn conflict_copy_path(path: &str, number: u64) -> String {
    let path = std::path::Path::new(path);
    let stem = path.file_stem().unwrap().to_str().unwrap();
    let extension = path.extension().and_then(|value| value.to_str());
    let extension = extension
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    path.with_file_name(format!(
        "{stem}.okilum-first-sync-conflict-{number}{extension}"
    ))
    .to_str()
    .unwrap()
    .to_owned()
}
fn plan_fingerprint(here: &Snapshot, there: &Snapshot) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"okilum-first-sync-consent-v1\0");
    for snapshot in [here, there] {
        for value in [snapshot.scope.as_bytes(), snapshot.revision.as_bytes()] {
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value);
        }
        hash.update((snapshot.files.len() as u64).to_le_bytes());
        for (path, bytes) in &snapshot.files {
            for value in [path.as_bytes(), bytes.as_slice()] {
                hash.update((value.len() as u64).to_le_bytes());
                hash.update(value);
            }
        }
    }
    hash.finalize().into()
}
/// Serialized adapter boundary. Before ANY mutation, re-read and compare both
/// complete snapshots (scope, revision AND bytes). Abort with Stale on mismatch.
/// Hold exclusive mutation ownership through apply. Preserve both originals in
/// durable recovery state before replacing a conflicting destination; never
/// delete canonical files. Success means exact read-back equality on both sides.
/// An adapter without these capabilities must return Backend and stay stopped.
/// Remote inventory revisions alone are not an atomic REST write precondition.
pub trait Backend: Send {
    fn apply_if_unchanged(
        &mut self,
        here: &Snapshot,
        there: &Snapshot,
        merged: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), Error>;
}
/// Session acquired by the parent adapter under exclusive filesystem/REST
/// mutation ownership. The guard must remain held until this value is dropped.
/// A live daemon that can mutate files concurrently cannot provide this session.
pub trait ExclusiveSession: Send {
    fn snapshots(&mut self) -> Result<(Snapshot, Snapshot), Error>;
    /// Durable, lossless originals outside the deletable index, committed before
    /// either publish. Refuse an existing unfinished recovery operation.
    fn preserve_originals(&mut self, here: &Snapshot, there: &Snapshot) -> Result<(), Error>;
    /// Atomic per-file publication, preserving existing permissions. No deletions.
    fn publish(&mut self, side: Side, files: &BTreeMap<String, Vec<u8>>) -> Result<(), Error>;
    /// Mark the durable recovery operation complete only after both read-backs.
    fn complete(&mut self) -> Result<(), Error>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Here,
    There,
}

pub struct GuardedBackend<S: ExclusiveSession>(pub S);
impl<S: ExclusiveSession> Backend for GuardedBackend<S> {
    fn apply_if_unchanged(
        &mut self,
        here: &Snapshot,
        there: &Snapshot,
        merged: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), Error> {
        let current = self.0.snapshots()?;
        if current.0 != *here || current.1 != *there {
            return Err(Error::Stale);
        }
        self.0.preserve_originals(here, there)?;
        self.0.publish(Side::Here, merged)?;
        self.0.publish(Side::There, merged)?;
        let (after_here, after_there) = self.0.snapshots()?;
        if after_here.scope != here.scope
            || after_there.scope != there.scope
            || after_here.files != *merged
            || after_there.files != *merged
        {
            return Err(Error::Backend(
                "Merge read-back differs; recovery required".into(),
            ));
        }
        self.0.complete()
    }
}

fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').map(|(index, _)| &path[..index])
}
fn validate(snapshot: &Snapshot) -> Result<(), Error> {
    if snapshot.scope.is_empty() || snapshot.revision.is_empty() || snapshot.files.is_empty() {
        return Err(Error::InvalidInventory);
    }
    for path in snapshot.files.keys() {
        if path.contains('\\')
            || path.contains('\0')
            || path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == ".." || p.contains(':'))
        {
            return Err(Error::InvalidInventory);
        }
    }
    Ok(())
}
#[cfg(test)]
#[path = "merge_preview/tests.rs"]
mod tests;

/// Offline filesystem implementation. Deliberately unavailable on platforms
/// without the Unix file identity/permission checks used below.
#[cfg(unix)]
pub mod filesystem {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{
        fs::{self, File, OpenOptions},
        io::{Read, Write},
        os::unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        },
        path::{Path, PathBuf},
    };

    const LIMIT: u64 = 256 * 1024 * 1024;
    const MAGIC: &[u8] = b"OKILUM-FIRST-SYNC-1\0";
    fn failure(message: impl ToString) -> Error {
        Error::Backend(message.to_string())
    }
    fn io<T>(result: std::io::Result<T>) -> Result<T, Error> {
        result.map_err(failure)
    }
    fn sync(path: &Path) -> Result<(), Error> {
        io(io(File::open(path))?.sync_all())
    }
    fn identity(path: &Path) -> Result<(u64, u64), Error> {
        let metadata = io(fs::symlink_metadata(path))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(Error::InvalidInventory);
        }
        Ok((metadata.dev(), metadata.ino()))
    }
    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
    fn private_directory(path: &Path) -> Result<(), Error> {
        if !io(path.try_exists())? {
            io(fs::DirBuilder::new().mode(0o700).create(path))?;
        }
        let metadata = io(fs::symlink_metadata(path))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata.mode() & 0o077 != 0 {
            return Err(failure("Private state directory required"));
        }
        sync(path)?;
        sync(path.parent().ok_or(Error::InvalidInventory)?)
    }

    /// Shared application mutation gate, held for the entire preview/apply or
    /// recovery session. ALL enrollment, source-edit, import and Sync mutations
    /// of either root must acquire this SAME canonical gate path, not their own
    /// per-operation locks. Store the gate outside both vaults in private app
    /// state. Busy returns immediately; it never waits on the UI.
    ///
    /// This only excludes cooperating writers. The caller must have independently
    /// quiesced external editors/watchers/daemons on BOTH roots before opening a
    /// FilesystemSession, and keep them stopped until it is dropped. No method
    /// here starts/stops a daemon or claims that a REST pause proves quiescence.
    pub struct MutationGate {
        file: File,
        path: PathBuf,
    }
    impl MutationGate {
        pub fn acquire(directory: &Path) -> Result<Self, Error> {
            private_directory(directory)?;
            let path = directory.join("mutation.lock");
            if let Ok(m) = fs::symlink_metadata(&path) {
                if !m.is_file()
                    || m.file_type().is_symlink()
                    || m.nlink() != 1
                    || m.mode() & 0o077 != 0
                {
                    return Err(failure("Unsafe mutation gate"));
                }
            }
            let file = io(OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(&path))?;
            let before = io(fs::symlink_metadata(&path))?;
            let opened = io(file.metadata())?;
            if before.file_type().is_symlink()
                || before.dev() != opened.dev()
                || before.ino() != opened.ino()
            {
                return Err(failure("Mutation gate replaced"));
            }
            file.try_lock().map_err(failure)?;
            sync(directory)?;
            Ok(Self {
                file,
                path: io(path.canonicalize())?,
            })
        }
        fn check(&self) -> Result<(), Error> {
            let current = io(fs::symlink_metadata(&self.path))?;
            let locked = io(self.file.metadata())?;
            if !current.is_file()
                || current.file_type().is_symlink()
                || current.nlink() != 1
                || current.mode() & 0o077 != 0
                || current.dev() != locked.dev()
                || current.ino() != locked.ino()
            {
                return Err(Error::Stale);
            }
            Ok(())
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum RecoveryStatus {
        Fresh,
        Pending,
        Complete,
    }

    /// Read-only recovery review. The opaque current snapshots bind the user's
    /// next Recover click to both complete revisions, including metadata-only
    /// changes. A new preview never silently inherits consent from an old one.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct RecoveryPreview {
        operation_directory: PathBuf,
        operation_identity: (u64, u64),
        originals_path: PathBuf,
        originals_digest: String,
        roots: [PathBuf; 2],
        original_plan: Plan,
        current: (Snapshot, Snapshot),
    }
    impl RecoveryPreview {
        pub fn operation_directory(&self) -> &Path {
            &self.operation_directory
        }
        pub fn operation_identity(&self) -> (u64, u64) {
            self.operation_identity
        }
        pub fn originals_path(&self) -> &Path {
            &self.originals_path
        }
        pub fn originals_digest(&self) -> &str {
            &self.originals_digest
        }
        pub fn roots(&self) -> [&Path; 2] {
            [&self.roots[0], &self.roots[1]]
        }
        pub fn plan(&self) -> &Plan {
            &self.original_plan
        }
        pub fn original_counts(&self) -> [usize; 2] {
            [
                self.original_plan.here.files.len(),
                self.original_plan.there.files.len(),
            ]
        }
        pub fn original_bytes(&self) -> [usize; 2] {
            [
                self.original_plan.here.files.values().map(Vec::len).sum(),
                self.original_plan.there.files.values().map(Vec::len).sum(),
            ]
        }
        pub fn remaining_counts(&self) -> [usize; 2] {
            [&self.current.0, &self.current.1].map(|side| {
                self.original_plan
                    .merged
                    .iter()
                    .filter(|(path, bytes)| side.files.get(*path) != Some(*bytes))
                    .count()
            })
        }
        /// Compact exact fingerprints for display; equality uses full snapshots.
        pub fn revision_fingerprints(&self) -> [String; 2] {
            [&self.current.0, &self.current.1].map(|snapshot| {
                let mut bytes = Vec::new();
                encode_snapshot(&mut bytes, snapshot);
                hex(&Sha256::digest(bytes))
            })
        }
    }

    /// A concrete pair of offline directory trees. State is a new operation's
    /// private durable directory OUTSIDE both roots and outside any index.
    /// Version 1 requires state and both roots on the same filesystem for atomic
    /// rename from private staging. Unsupported mounts fail before any publish.
    pub struct FilesystemSession {
        roots: [PathBuf; 2],
        root_ids: [(u64, u64); 2],
        state: PathBuf,
        state_id: (u64, u64),
        _gate: MutationGate,
    }
    /// Background-only preparation under the already-held offline ownership.
    /// Pending originals always win over a new preview: new consent cannot
    /// supersede an interrupted operation. Neither variant publishes files.
    pub enum PreparedSession {
        Fresh {
            plan: Box<Plan>,
            session: FilesystemSession,
        },
        Recovery(FilesystemSession),
    }
    impl PreparedSession {
        pub fn inspect(mut session: FilesystemSession) -> Result<Self, Error> {
            match session.recovery_status()? {
                RecoveryStatus::Fresh => {
                    let (here, there) = session.snapshots()?;
                    let plan = Plan::preview(here, there)?;
                    Ok(Self::Fresh {
                        plan: Box::new(plan),
                        session,
                    })
                }
                RecoveryStatus::Pending => Ok(Self::Recovery(session)),
                RecoveryStatus::Complete => Err(failure(
                    "Completed merge retained; choose a new operation generation",
                )),
            }
        }
    }
    impl FilesystemSession {
        pub fn open(
            here: &Path,
            there: &Path,
            state: &Path,
            gate: MutationGate,
        ) -> Result<Self, Error> {
            gate.check()?;
            let roots = [io(here.canonicalize())?, io(there.canonicalize())?];
            if roots.iter().any(|root| gate.path.starts_with(root)) {
                return Err(failure("Shared mutation gate must be outside vaults"));
            }
            // Do not silently accept a symlink alias supplied as the root.
            identity(here)?;
            identity(there)?;
            if roots[0].starts_with(&roots[1]) || roots[1].starts_with(&roots[0]) {
                return Err(Error::SameScope);
            }
            // Validate the prospective state location before creating anything.
            let state_parent = io(state
                .parent()
                .ok_or(Error::InvalidInventory)?
                .canonicalize())?;
            let prospective = state_parent.join(state.file_name().ok_or(Error::InvalidInventory)?);
            if roots
                .iter()
                .any(|root| prospective.starts_with(root) || root.starts_with(&prospective))
                || prospective
                    .components()
                    .any(|p| p.as_os_str() == ".okilum-index")
            {
                return Err(failure("Recovery state must be outside vaults and index"));
            }
            private_directory(state)?;
            let state = io(state.canonicalize())?;
            if roots
                .iter()
                .any(|root| state.starts_with(root) || root.starts_with(&state))
                || state.components().any(|p| p.as_os_str() == ".okilum-index")
            {
                return Err(failure("Recovery state must be outside vaults and index"));
            }
            let root_ids = [identity(&roots[0])?, identity(&roots[1])?];
            let state_id = identity(&state)?;
            let device = state_id.0;
            if root_ids.iter().any(|id| id.0 != device) {
                return Err(failure("Atomic offline merge requires a single filesystem"));
            }
            Ok(Self {
                roots,
                root_ids,
                state,
                state_id,
                _gate: gate,
            })
        }
        fn check_roots(&self) -> Result<(), Error> {
            self._gate.check()?;
            if identity(&self.state)? != self.state_id {
                return Err(Error::Stale);
            }
            for (root, expected) in self.roots.iter().zip(self.root_ids) {
                if identity(root)? != expected {
                    return Err(Error::Stale);
                }
            }
            Ok(())
        }
        fn scope(&self, side: usize) -> String {
            format!(
                "offline:{}:{}:{}",
                hex(self.roots[side].as_os_str().as_bytes()),
                self.root_ids[side].0,
                self.root_ids[side].1
            )
        }
        fn inventory(&self, side: usize) -> Result<Snapshot, Error> {
            self.check_roots()?;
            let mut files = BTreeMap::new();
            let mut revisions = BTreeMap::new();
            let mut budget = LIMIT;
            let mut entries = 100_000;
            walk(
                &self.roots[side],
                &self.roots[side],
                &mut files,
                &mut revisions,
                &mut budget,
                &mut entries,
                0,
            )?;
            Ok(Snapshot {
                scope: self.scope(side),
                revision: format!("{:?}", revisions),
                files,
            })
        }
        /// Inspect under the same gate before offering preview versus recovery.
        /// A completed generation is retained, never recycled for a new merge.
        pub fn recovery_status(&self) -> Result<RecoveryStatus, Error> {
            self.check_roots()?;
            match fs::symlink_metadata(self.journal()) {
                Ok(_) => {
                    self.originals()?;
                    Ok(if self.completed()? {
                        RecoveryStatus::Complete
                    } else {
                        RecoveryStatus::Pending
                    })
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if self.completed()? {
                        return Err(failure("Completion without originals"));
                    }
                    Ok(RecoveryStatus::Fresh)
                }
                Err(error) => Err(failure(error)),
            }
        }
        /// These accessors do no IO and can populate a loading/cancelled view.
        pub fn operation_directory(&self) -> &Path {
            &self.state
        }
        pub fn originals_path(&self) -> PathBuf {
            self.journal()
        }
        pub fn roots(&self) -> [&Path; 2] {
            [&self.roots[0], &self.roots[1]]
        }

        /// Run on the background executor. Does not publish, journal, complete,
        /// delete or resolve anything. Only a valid pending operation is offered.
        pub fn preview_recovery(&mut self) -> Result<RecoveryPreview, Error> {
            if self.recovery_status()? != RecoveryStatus::Pending {
                return Err(Error::Stale);
            }
            let (here, there) = self.originals()?;
            let original_plan = Plan::preview(here.clone(), there.clone())?;
            let current = self.snapshots()?;
            compatible(&current.0.files, &here.files, &original_plan.merged)?;
            compatible(&current.1.files, &there.files, &original_plan.merged)?;
            Ok(RecoveryPreview {
                operation_directory: self.state.clone(),
                operation_identity: self.state_id,
                originals_path: self.journal(),
                originals_digest: hex(&Sha256::digest(read_regular(&self.journal(), LIMIT)?)),
                roots: self.roots.clone(),
                original_plan,
                current,
            })
        }
        /// Explicit consent to this exact displayed pending operation. Compare
        /// both full revisions AND the retained journal before any publish.
        /// Local gate ownership alone is not proof of global writer exclusion.
        pub fn recover_previewed(&mut self, preview: &RecoveryPreview) -> Result<(), Error> {
            if self.preview_recovery()? != *preview {
                return Err(Error::Stale);
            }
            self.recover()
        }

        fn journal(&self) -> PathBuf {
            self.state.join("originals.v1")
        }
        fn completed(&self) -> Result<bool, Error> {
            match fs::symlink_metadata(self.state.join("complete.v1")) {
                Ok(_) => {
                    if read_regular(&self.state.join("complete.v1"), 32)? != b"complete\n" {
                        return Err(failure("Invalid completion record"));
                    }
                    Ok(true)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(failure(error)),
            }
        }
        fn originals(&self) -> Result<(Snapshot, Snapshot), Error> {
            self.check_roots()?;
            let bytes = read_regular(&self.journal(), LIMIT)?;
            let payload_length = bytes.len().checked_sub(32).ok_or(Error::InvalidInventory)?;
            let (payload, digest) = bytes.split_at(payload_length);
            if &Sha256::digest(payload)[..] != digest {
                return Err(failure("Recovery originals checksum differs"));
            }
            let mut cursor = payload;
            if !cursor.starts_with(MAGIC) {
                return Err(failure("Unknown recovery journal"));
            }
            cursor = &cursor[MAGIC.len()..];
            let here = decode_snapshot(&mut cursor)?;
            let there = decode_snapshot(&mut cursor)?;
            if !cursor.is_empty() || here.scope != self.scope(0) || there.scope != self.scope(1) {
                return Err(Error::Stale);
            }
            validate(&here)?;
            validate(&there)?;
            Ok((here, there))
        }
        /// Explicit forward recovery of the exact consented merge. Reconstructs
        /// its deterministic target from durable originals. Every extant path
        /// must still contain either that side's original or the planned bytes;
        /// unrelated additions, missing originals or third-version edits STOP
        /// before writes on EITHER side. Originals are retained after completion.
        pub fn recover(&mut self) -> Result<(), Error> {
            let (here, there) = self.originals()?;
            let plan = Plan::preview(here.clone(), there.clone())?;
            let current = self.snapshots()?;
            if self.completed()? {
                return if current.0.files == plan.merged && current.1.files == plan.merged {
                    Ok(())
                } else {
                    Err(Error::Stale)
                };
            }
            compatible(&current.0.files, &here.files, &plan.merged)?;
            compatible(&current.1.files, &there.files, &plan.merged)?;
            self.publish(Side::Here, &plan.merged)?;
            self.publish(Side::There, &plan.merged)?;
            self.complete()
        }
        fn write_durable(
            &self,
            path: &Path,
            bytes: &[u8],
            permissions: fs::Permissions,
        ) -> Result<(), Error> {
            // The shared gate serializes this fixed staging name. A leftover
            // after a crash is disposable; it never authorizes a canonical write.
            let staging = self.state.join("staging");
            if let Ok(metadata) = fs::symlink_metadata(&staging) {
                if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.nlink() != 1
                {
                    return Err(failure("Unsafe recovery staging file"));
                }
                io(fs::remove_file(&staging))?;
            }
            let mut file = io(OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&staging))?;
            io(file.write_all(bytes))?;
            io(file.set_permissions(permissions))?;
            io(file.sync_all())?;
            io(fs::rename(&staging, path))?;
            sync(path.parent().ok_or(Error::InvalidInventory)?)?;
            sync(&self.state)
        }
    }
    impl ExclusiveSession for FilesystemSession {
        fn snapshots(&mut self) -> Result<(Snapshot, Snapshot), Error> {
            Ok((self.inventory(0)?, self.inventory(1)?))
        }
        fn preserve_originals(&mut self, here: &Snapshot, there: &Snapshot) -> Result<(), Error> {
            if self.recovery_status()? != RecoveryStatus::Fresh {
                return Err(failure(
                    "Operation already has originals; use explicit recovery",
                ));
            }
            if self.snapshots()? != (here.clone(), there.clone()) {
                return Err(Error::Stale);
            }
            // Also validates collision handling and target bounds before durable
            // mutation authority: the merged read-back must fit the same limits.
            let plan = Plan::preview(here.clone(), there.clone())?;
            let total = plan
                .merged
                .values()
                .try_fold(0u64, |n, bytes| n.checked_add(bytes.len() as u64))
                .ok_or(Error::InvalidInventory)?;
            let mut target_entries: BTreeSet<&str> =
                plan.merged.keys().map(String::as_str).collect();
            for path in plan.merged.keys() {
                target_entries.extend(ancestors(path));
            }
            if total > LIMIT || target_entries.len() > 100_000 {
                return Err(failure("Merged inventory size limit"));
            }
            let mut bytes = MAGIC.to_vec();
            encode_snapshot(&mut bytes, here);
            encode_snapshot(&mut bytes, there);
            let checksum = Sha256::digest(&bytes);
            bytes.extend_from_slice(&checksum);
            if bytes.len() as u64 > LIMIT {
                return Err(failure("Recovery journal size limit"));
            }
            self.write_durable(&self.journal(), &bytes, fs::Permissions::from_mode(0o600))
        }
        fn publish(&mut self, side: Side, files: &BTreeMap<String, Vec<u8>>) -> Result<(), Error> {
            if self.completed()? {
                return Err(failure("Completed operation cannot publish again"));
            }
            let originals = self.originals()?;
            let plan = Plan::preview(originals.0.clone(), originals.1.clone())?;
            if files != &plan.merged {
                return Err(Error::Stale);
            }
            let index = if side == Side::Here { 0 } else { 1 };
            let original = if index == 0 {
                &originals.0
            } else {
                &originals.1
            };
            compatible(&self.inventory(index)?.files, &original.files, files)?;
            for (relative, bytes) in files {
                self.check_roots()?;
                let target = self.roots[index].join(relative);
                let parent = target.parent().ok_or(Error::InvalidInventory)?;
                ensure_parents(&self.roots[index], parent)?;
                let permissions = match fs::symlink_metadata(&target) {
                    Ok(metadata) => {
                        if !metadata.is_file()
                            || metadata.file_type().is_symlink()
                            || metadata.nlink() != 1
                        {
                            return Err(Error::InvalidInventory);
                        }
                        let current = read_regular(&target, LIMIT)?;
                        if &current == bytes {
                            continue;
                        }
                        if original.files.get(relative) != Some(&current) {
                            return Err(Error::Stale);
                        }
                        metadata.permissions()
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        if original.files.contains_key(relative) {
                            return Err(Error::Stale);
                        }
                        fs::Permissions::from_mode(0o600)
                    }
                    Err(error) => return Err(failure(error)),
                };
                self.write_durable(&target, bytes, permissions)?;
            }
            Ok(())
        }
        fn complete(&mut self) -> Result<(), Error> {
            let (here, there) = self.originals()?;
            let target = Plan::preview(here, there)?.merged;
            let current = self.snapshots()?;
            if current.0.files != target || current.1.files != target {
                return Err(Error::Stale);
            }
            self.write_durable(
                &self.state.join("complete.v1"),
                b"complete\n",
                fs::Permissions::from_mode(0o600),
            )
        }
    }
    fn compatible(
        current: &BTreeMap<String, Vec<u8>>,
        original: &BTreeMap<String, Vec<u8>>,
        merged: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), Error> {
        if original.keys().any(|path| !current.contains_key(path))
            || current.iter().any(|(path, bytes)| {
                original.get(path) != Some(bytes) && merged.get(path) != Some(bytes)
            })
        {
            return Err(Error::Stale);
        }
        Ok(())
    }
    fn ensure_parents(root: &Path, parent: &Path) -> Result<(), Error> {
        let mut path = root.to_path_buf();
        for part in parent.strip_prefix(root).map_err(failure)?.components() {
            path.push(part);
            if !io(path.try_exists())? {
                io(fs::DirBuilder::new().mode(0o700).create(&path))?;
            }
            identity(&path)?;
            sync(&path)?;
            sync(path.parent().ok_or(Error::InvalidInventory)?)?;
        }
        Ok(())
    }
    fn read_regular(path: &Path, limit: u64) -> Result<Vec<u8>, Error> {
        let metadata = io(fs::symlink_metadata(path))?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.nlink() != 1
            || metadata.len() > limit
        {
            return Err(Error::InvalidInventory);
        }
        let mut file = io(File::open(path))?;
        let opened = io(file.metadata())?;
        if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
            return Err(Error::Stale);
        }
        let mut bytes = Vec::new();
        io((&mut file).take(limit + 1).read_to_end(&mut bytes))?;
        if bytes.len() as u64 > limit {
            return Err(failure("Inventory byte limit"));
        }
        Ok(bytes)
    }
    fn walk(
        root: &Path,
        directory: &Path,
        files: &mut BTreeMap<String, Vec<u8>>,
        revisions: &mut BTreeMap<String, String>,
        budget: &mut u64,
        entries: &mut usize,
        depth: usize,
    ) -> Result<(), Error> {
        if depth > 64 {
            return Err(failure("Inventory entry/depth limit"));
        }
        for entry in io(fs::read_dir(directory))? {
            *entries = entries
                .checked_sub(1)
                .ok_or_else(|| failure("Inventory entry limit"))?;
            let entry = io(entry)?;
            let path = entry.path();
            let metadata = io(fs::symlink_metadata(&path))?;
            // Nested mounts cannot participate in state-to-target atomic rename.
            if metadata.dev() != io(fs::symlink_metadata(root))?.dev() {
                return Err(failure("Nested filesystem is unsupported for atomic merge"));
            }
            if metadata.file_type().is_symlink() {
                return Err(Error::InvalidInventory);
            }
            if metadata.is_dir() {
                walk(root, &path, files, revisions, budget, entries, depth + 1)?;
            } else {
                let relative = path
                    .strip_prefix(root)
                    .map_err(failure)?
                    .to_str()
                    .ok_or(Error::InvalidInventory)?
                    .to_owned();
                let bytes = read_regular(&path, *budget)?;
                *budget = budget
                    .checked_sub(bytes.len() as u64)
                    .ok_or(Error::InvalidInventory)?;
                revisions.insert(
                    relative.clone(),
                    format!(
                        "{}:{}:{}:{}:{}:{}:{}:{}",
                        metadata.dev(),
                        metadata.ino(),
                        metadata.mode(),
                        metadata.len(),
                        metadata.mtime(),
                        metadata.mtime_nsec(),
                        metadata.ctime(),
                        metadata.ctime_nsec()
                    ),
                );
                files.insert(relative, bytes);
            }
        }
        Ok(())
    }
    fn field(bytes: &mut Vec<u8>, value: &[u8]) {
        bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
        bytes.extend_from_slice(value);
    }
    fn encode_snapshot(bytes: &mut Vec<u8>, snapshot: &Snapshot) {
        field(bytes, snapshot.scope.as_bytes());
        field(bytes, snapshot.revision.as_bytes());
        bytes.extend_from_slice(&(snapshot.files.len() as u64).to_le_bytes());
        for (path, contents) in &snapshot.files {
            field(bytes, path.as_bytes());
            field(bytes, contents);
        }
    }
    fn number(cursor: &mut &[u8]) -> Result<u64, Error> {
        let bytes: [u8; 8] = cursor
            .get(..8)
            .ok_or(Error::InvalidInventory)?
            .try_into()
            .map_err(failure)?;
        *cursor = &cursor[8..];
        Ok(u64::from_le_bytes(bytes))
    }
    fn take(cursor: &mut &[u8]) -> Result<Vec<u8>, Error> {
        let length = usize::try_from(number(cursor)?).map_err(failure)?;
        let value = cursor
            .get(..length)
            .ok_or(Error::InvalidInventory)?
            .to_vec();
        *cursor = &cursor[length..];
        Ok(value)
    }
    fn string(cursor: &mut &[u8]) -> Result<String, Error> {
        String::from_utf8(take(cursor)?).map_err(failure)
    }
    fn decode_snapshot(cursor: &mut &[u8]) -> Result<Snapshot, Error> {
        let scope = string(cursor)?;
        let revision = string(cursor)?;
        let length = number(cursor)?;
        if length > 100_000 {
            return Err(Error::InvalidInventory);
        }
        let mut files = BTreeMap::new();
        for _ in 0..length {
            let path = string(cursor)?;
            let contents = take(cursor)?;
            if files.insert(path, contents).is_some() {
                return Err(Error::InvalidInventory);
            }
        }
        Ok(Snapshot {
            scope,
            revision,
            files,
        })
    }
}

/// Shared whole-background lease. Cancellation closes admission; only the last
/// queued/future/output/resource cleanup reservation can report return. Settings
/// and every tracked child must use this SAME Work instance.
pub mod completion {
    use std::{
        future::Future,
        pin::Pin,
        sync::{Arc, Mutex},
        task::{Context, Poll},
    };
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Readback {
        pub here_scope: String,
        pub here_revision: String,
        pub there_scope: String,
        pub there_revision: String,
        pub merged_sha256: String,
    }
    #[derive(Clone, Debug)]
    pub struct Snapshot {
        pub closed: bool,
        pub pending: usize,
        pub opaque: bool,
        pub readback: Option<Readback>,
    }
    impl Snapshot {
        pub fn returned(&self) -> bool {
            self.closed && self.pending == 0 && !self.opaque
        }
    }
    type Notify = Box<dyn FnOnce(Snapshot) + Send>;
    #[derive(Default)]
    struct State {
        closed: bool,
        pending: usize,
        opaque: bool,
        readback: Option<Readback>,
        notify: Option<Notify>,
    }
    #[derive(Clone, Default)]
    pub struct Work(Arc<Mutex<State>>);
    impl Work {
        pub fn unknown() -> Self {
            let work = Self::default();
            work.opaque_child();
            work
        }
        pub fn snapshot(&self) -> Snapshot {
            let state = self.0.lock().unwrap();
            Snapshot {
                closed: state.closed,
                pending: state.pending,
                opaque: state.opaque,
                readback: state.readback.clone(),
            }
        }
        pub fn same(&self, other: &Self) -> bool {
            Arc::ptr_eq(&self.0, &other.0)
        }
        pub fn reserve(&self) -> Option<Reservation> {
            let mut state = self.0.lock().unwrap();
            if state.closed {
                return None;
            }
            state.pending = state.pending.checked_add(1)?;
            Some(Reservation(self.clone()))
        }
        pub fn opaque_child(&self) {
            self.0.lock().unwrap().opaque = true;
        }
        pub fn readback(&self, proof: Readback) {
            self.0.lock().unwrap().readback = Some(proof);
        }
        pub fn close(&self) {
            self.0.lock().unwrap().closed = true;
            self.dispatch();
        }
        pub fn when_returned(&self, notify: Notify) {
            let mut state = self.0.lock().unwrap();
            // One unmount waiter per receipt. Repeated cancellation cannot
            // replace its binding with a new generation's callback.
            if state.notify.is_some() {
                return;
            }
            state.notify = Some(notify);
            drop(state);
            self.dispatch();
        }
        fn dispatch(&self) {
            let ready = {
                let mut state = self.0.lock().unwrap();
                if !state.closed || state.pending != 0 || state.opaque {
                    None
                } else {
                    let snapshot = Snapshot {
                        closed: true,
                        pending: 0,
                        opaque: false,
                        readback: state.readback.clone(),
                    };
                    state.notify.take().map(|notify| (notify, snapshot))
                }
            };
            if let Some((notify, snapshot)) = ready {
                notify(snapshot);
            }
        }
    }
    pub struct Reservation(Work);
    impl Reservation {
        /// Count idle UI-owned resources too. Value cleanup always precedes the
        /// reservation release, including unwind and cancellation.
        pub fn owned<T>(self, value: T) -> Returned<T> {
            Returned {
                value: Some(value),
                reservation: Some(self),
            }
        }
        pub fn queued<T>(self, payload: T) -> Queued<T> {
            Queued {
                payload: Some(payload),
                reservation: Some(self),
            }
        }
        pub fn track<F: Future<Output = ()>>(self, future: F) -> Joined<F> {
            Joined {
                future: Some(Box::pin(future)),
                reservation: Some(self),
            }
        }
    }
    pub struct Queued<T> {
        payload: Option<T>,
        reservation: Option<Reservation>,
    }
    impl<T> Queued<T> {
        pub fn join<F: Future<Output = ()>>(mut self, build: impl FnOnce(T) -> F) -> Joined<F> {
            let future = build(self.payload.take().expect("queued payload owned once"));
            self.reservation
                .take()
                .expect("queued reservation owned once")
                .track(future)
        }
        pub fn job<F: Future>(mut self, build: impl FnOnce(T) -> F) -> Job<F> {
            let future = build(self.payload.take().expect("queued payload owned once"));
            Job {
                future: Some(Box::pin(future)),
                reservation: self.reservation.take(),
            }
        }
    }
    impl<T> Drop for Queued<T> {
        fn drop(&mut self) {
            drop(self.payload.take());
            drop(self.reservation.take());
        }
    }
    impl Drop for Reservation {
        fn drop(&mut self) {
            let mut state = self.0 .0.lock().unwrap();
            state.pending = state
                .pending
                .checked_sub(1)
                .expect("completion reservation is owned once");
            drop(state);
            self.0.dispatch();
        }
    }
    pub struct Joined<F: Future<Output = ()>> {
        future: Option<Pin<Box<F>>>,
        reservation: Option<Reservation>,
    }
    impl<F: Future<Output = ()>> Future for Joined<F> {
        type Output = ();
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            let this = self.get_mut();
            match this
                .future
                .as_mut()
                .expect("joined future polled after return")
                .as_mut()
                .poll(cx)
            {
                Poll::Pending => Poll::Pending,
                Poll::Ready(()) => {
                    drop(this.future.take());
                    drop(this.reservation.take());
                    Poll::Ready(())
                }
            }
        }
    }
    impl<F: Future<Output = ()>> Drop for Joined<F> {
        fn drop(&mut self) {
            // Also enforce cleanup ordering for cancellation before first poll.
            drop(self.future.take());
            drop(self.reservation.take());
        }
    }
    // Returned resource ownership is counted too. A dropped parent continuation
    // cannot release the guard while a worker/output still owns its session.
    pub struct Returned<T> {
        value: Option<T>,
        reservation: Option<Reservation>,
    }
    impl<T> AsRef<T> for Returned<T> {
        /// Borrow without releasing the ownership/cleanup reservation.
        fn as_ref(&self) -> &T {
            self.value.as_ref().expect("owned value already consumed")
        }
    }
    impl<T> AsMut<T> for Returned<T> {
        fn as_mut(&mut self) -> &mut T {
            self.value.as_mut().expect("owned value already consumed")
        }
    }
    impl<T> Returned<T> {
        /// Transfer into a reservation acquired BEFORE dequeueing the resource.
        /// Both leases overlap through construction; there is no uncounted gap.
        pub fn into_queued<U>(
            mut self,
            reservation: Reservation,
            map: impl FnOnce(T) -> U,
        ) -> Queued<U> {
            reservation.queued(map(self
                .value
                .take()
                .expect("owned value transferred once")))
        }
        /// Drop the resource inside this callback, or transfer it under another
        /// reservation acquired before the callback. Never store it uncounted.
        pub fn consume(mut self, consume: impl FnOnce(T)) {
            consume(self.value.take().expect("returned output consumed once"));
        }
    }
    impl<T> Drop for Returned<T> {
        fn drop(&mut self) {
            drop(self.value.take());
            drop(self.reservation.take());
        }
    }
    pub struct Job<F: Future> {
        future: Option<Pin<Box<F>>>,
        reservation: Option<Reservation>,
    }
    impl<F: Future> Future for Job<F> {
        type Output = Returned<F::Output>;
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let this = self.get_mut();
            match this
                .future
                .as_mut()
                .expect("job polled after return")
                .as_mut()
                .poll(cx)
            {
                Poll::Pending => Poll::Pending,
                Poll::Ready(output) => {
                    drop(this.future.take());
                    Poll::Ready(Returned {
                        value: Some(output),
                        reservation: this.reservation.take(),
                    })
                }
            }
        }
    }
    impl<F: Future> Drop for Job<F> {
        fn drop(&mut self) {
            drop(self.future.take());
            drop(self.reservation.take());
        }
    }
}
