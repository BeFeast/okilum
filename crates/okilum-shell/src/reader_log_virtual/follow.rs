//! Bounded observed Follow. Errors are terminal; the UI keeps its last receipt.
use anyhow::{bail, ensure, Context as _, Result};
use okilum_core::log::{
    tail::{FileIdentity, IdentityLease, ResetReason, TailEvent},
    virtual_log::{
        rotation_matches, watcher::Change, Reason, Selection, SelectionPlan, MAX_FILES,
        MAX_FILE_BYTES, MAX_TOTAL_BYTES,
    },
    LogFile, LogSnapshot, LogTail,
};
use std::{
    collections::{BTreeMap, HashSet},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

#[derive(Clone)]
pub(super) struct Source {
    pub slot: usize,
    pub path: PathBuf,
    pub snapshot: Arc<LogSnapshot>,
    pub label: Option<&'static str>,
}
#[derive(Clone)]
pub(super) struct Update {
    pub sources: Vec<Source>,
    pub candidates: Option<Vec<okilum_core::log::virtual_log::Candidate>>,
    pub more: bool,
    pub notice: Option<String>,
}
struct Active {
    slot: Option<usize>,
    tail: LogTail,
}
struct Retired {
    _identity: IdentityLease,
    slot: Option<usize>,
    path: PathBuf,
    // Rotation has a distinct old descriptor. Truncation reuses the live inode,
    // so its earlier logical segment must remain frozen instead.
    tail: Option<LogTail>,
    label: &'static str,
}
pub(super) struct Session {
    known: HashSet<FileIdentity>,
    active: Vec<Active>,
    retired: Vec<Retired>,
    aliases: Vec<IdentityLease>,
    frozen: Vec<PathBuf>,
    published: BTreeMap<usize, Source>,
}
fn running(cancel: &AtomicBool) -> Result<()> {
    ensure!(
        !cancel.load(Ordering::Acquire),
        "Follow stopped; retained snapshot unchanged"
    );
    Ok(())
}
fn complete(selection: &Selection) -> bool {
    !selection.incomplete
        && !selection.candidates.iter().any(|c| {
            matches!(
                c.reason,
                Reason::SizeLimit
                    | Reason::FileLimit
                    | Reason::IncompleteWindow
                    | Reason::Unreadable(_)
            )
        })
}
fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hash);
    hash.finish()
}
fn extends(old: &LogSnapshot, new: &LogSnapshot) -> bool {
    old.window().offset == new.window().offset
        && old.index().format() == new.index().format()
        && new.bytes().starts_with(old.bytes())
}
impl Session {
    pub fn new(selection: &Selection) -> Result<Self> {
        ensure!(
            complete(selection),
            "Incomplete initial selection; use Snapshot/Rescan before Follow"
        );
        let mut this = Self {
            known: HashSet::new(),
            active: Vec::new(),
            retired: Vec::new(),
            aliases: Vec::new(),
            frozen: Vec::new(),
            published: BTreeMap::new(),
        };
        for file in &selection.files {
            this.accept(file)?;
        }
        Ok(this)
    }
    fn capacity(&self) -> Result<()> {
        ensure!(
            self.known.len() < MAX_FILES && self.published.len() < MAX_FILES,
            "Follow lifetime identity/segment limit reached; snapshot retained; Rescan explicitly"
        );
        Ok(())
    }
    fn accept(&mut self, file: &LogFile) -> Result<Source> {
        self.capacity()?;
        ensure!(
            !self.known.contains(&file.identity()),
            "Repeated source identity; Rescan explicitly"
        );
        let slot = self.published.len();
        let source = Source {
            slot,
            path: file.path().to_owned(),
            snapshot: file.snapshot(),
            label: None,
        };
        if file.is_compressed() {
            self.freeze(file.path())?;
        } else {
            self.active.push(Active {
                slot: Some(slot),
                tail: LogTail::from_loaded(file)?,
            });
        }
        self.known.insert(file.identity());
        self.published.insert(slot, source.clone());
        Ok(source)
    }
    fn freeze(&mut self, path: &Path) -> Result<()> {
        self.frozen.push(path.to_owned());
        self.frozen.push(path.canonicalize()?);
        Ok(())
    }
    fn stage(
        &mut self,
        slot: Option<usize>,
        path: &Path,
        snapshot: Arc<LogSnapshot>,
        label: Option<&'static str>,
        plan: &SelectionPlan,
        updates: &mut Vec<Source>,
    ) -> Result<Option<usize>> {
        ensure!(
            snapshot.bytes().len() as u64 <= MAX_FILE_BYTES
                && snapshot.window().offset == 0
                && !snapshot.window().oversized,
            "Follow record/byte window bound reached; snapshot retained; Rescan explicitly"
        );
        let slot = if let Some(slot) = slot {
            let old = &self.published[&slot];
            ensure!(extends(&old.snapshot, &snapshot), "Observed prefix changed; selection/marks retained in old snapshot; Rescan explicitly");
            if Arc::ptr_eq(&old.snapshot, &snapshot) && old.label == label {
                return Ok(Some(slot));
            }
            slot
        } else {
            // Do not admit a replacement from only the first bounded catch-up chunk.
            if snapshot.window().end < snapshot.window().file_len {
                return Ok(None);
            }
            match plan.snapshot_reason(&snapshot) {
                Reason::Matched => {},
                Reason::NoMatchingRecord | Reason::OutsideTimeRange | Reason::NoTimestamps => return Ok(None),
                reason => bail!("Cannot establish replacement membership: {reason:?}; snapshot retained; Rescan explicitly"),
            }
            ensure!(
                self.published.len() < MAX_FILES,
                "Follow retained segment limit reached; snapshot retained; Rescan explicitly"
            );
            self.published.len()
        };
        let source = Source {
            slot,
            path: path.to_owned(),
            snapshot,
            label,
        };
        self.published.insert(slot, source.clone());
        updates.push(source);
        Ok(Some(slot))
    }
    fn discover(
        &mut self,
        files: &[LogFile],
        candidates: &mut [okilum_core::log::virtual_log::Candidate],
        updates: &mut Vec<Source>,
        notices: &mut Vec<String>,
    ) -> Result<bool> {
        let mut retry_path = false;
        let archives: Vec<_> = if files.is_empty() {
            Vec::new()
        } else {
            self.retired
                .iter()
                .filter_map(|r| {
                    let source = self.published.get(&r.slot?)?;
                    Some((
                        source.path.clone(),
                        source.snapshot.clone(),
                        fingerprint(source.snapshot.bytes()),
                    ))
                })
                .collect()
        };
        for file in files {
            if self
                .active
                .iter()
                .any(|source| source.tail.path() == file.path())
            {
                // A replacement appeared after this turn's tail poll. Retry
                // that retained session instead of creating a second owner.
                retry_path = true;
                continue;
            }
            if self.known.contains(&file.identity()) {
                for candidate in &mut *candidates {
                    if candidate.path == file.path() {
                        candidate.reason = Reason::DuplicateSource;
                    }
                }
                continue;
            }
            let digest = if archives.is_empty() {
                0
            } else {
                fingerprint(file.bytes())
            };
            let same_archive = archives.iter().any(|(path, snapshot, hash)| {
                path.file_name() != file.path().file_name()
                    && path.parent() == file.path().parent()
                    && path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|base| rotation_matches(file.path(), base))
                    && *hash == digest
                    && snapshot.bytes() == file.bytes()
            });
            if same_archive {
                self.capacity()?;
                self.known.insert(file.identity());
                self.aliases.push(file.retain_identity());
                self.freeze(file.path())?;
                for candidate in &mut *candidates {
                    if candidate.path == file.path() {
                        candidate.reason = Reason::DuplicateSource;
                    }
                }
                notices.push(
                    "Byte-equivalent same-family archive coalesced with its observed segment"
                        .into(),
                );
            } else {
                ensure!(!file.is_compressed(), "New compressed archive continuity is unproven; snapshot retained; Rescan explicitly");
                updates.push(self.accept(file)?);
            }
        }
        Ok(retry_path)
    }
    pub fn refresh(
        &mut self,
        plan: &SelectionPlan,
        changes: &[Change],
        cancel: &AtomicBool,
    ) -> Result<Update> {
        running(cancel)?;
        for change in changes {
            match change {
                Change::Incomplete(error) => {
                    bail!("Watch coverage lost: {error}; snapshot retained; Rescan explicitly")
                }
                Change::Changed(paths) | Change::Removed(paths) => {
                    ensure!(
                        !paths.is_empty(),
                        "Unscoped filesystem event; snapshot retained; Rescan explicitly"
                    );
                    ensure!(
                        !self.frozen.iter().any(|frozen| paths
                            .iter()
                            .any(|path| path == frozen || frozen.starts_with(path))),
                        "Frozen archive changed; immutable history retained; Rescan explicitly"
                    );
                }
            }
        }
        let mut updates = Vec::new();
        let mut notices = Vec::new();
        // Keep borrowing and publication separate. Any error discards the session;
        // no part of this batch is sent to the UI before the whole turn succeeds.
        let mut active = std::mem::take(&mut self.active);
        let mut retired = std::mem::take(&mut self.retired);
        for source in &mut retired {
            let Some(tail) = &mut source.tail else {
                continue;
            };
            running(cancel)?;
            let read = tail
                .begin_poll()
                .read_open_identity()
                .context("Retained rotation descriptor unreadable; snapshot retained")?;
            running(cancel)?;
            match tail.apply(read) {
                TailEvent::Unchanged | TailEvent::Appended { reindexed: false, .. } => {
                    source.slot = self.stage(source.slot, &source.path, tail.snapshot(), Some(source.label), plan, &mut updates)?;
                }
                event => bail!("Retained rotation changed outside append continuity: {event:?}; snapshot retained; Rescan explicitly"),
            }
        }
        for source in &mut active {
            running(cancel)?;
            let path = source.tail.path().to_owned();
            let link = match std::fs::symlink_metadata(&path) {
                Ok(meta) => {
                    ensure!(
                        path.canonicalize()?.starts_with(plan.root()),
                        "Replacement escaped source root; snapshot retained; Rescan explicitly"
                    );
                    if !meta.file_type().is_symlink() {
                        ensure!(meta.is_file() && meta.len() <= MAX_FILE_BYTES, "Replacement/source exceeds Follow file bound; snapshot retained; Rescan explicitly");
                    }
                    meta.file_type().is_symlink()
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => return Err(error.into()),
            };
            let old_identity = source.tail.retain_identity();
            let request = source.tail.begin_poll();
            // Stable symlink appends remain supported; a retarget requires Rescan.
            let read = if link {
                request.read_pinned()
            } else {
                request.read()
            }
            .context("Source identity/read failed; snapshot retained; Rescan explicitly")?;
            running(cancel)?;
            match source.tail.apply(read) {
                TailEvent::Unchanged | TailEvent::Appended { reindexed: false, .. } => {
                    source.slot = self.stage(source.slot, &path, source.tail.snapshot(), None, plan, &mut updates)?;
                }
                TailEvent::Reset { reason, previous, .. } => {
                    ensure!(retired.len() < MAX_FILES, "Follow rotation lifetime bound reached; snapshot retained; Rescan explicitly");
                    let next_id = source.tail.identity();
                    ensure!(next_id == old_identity.identity() || !self.known.contains(&next_id), "Rotation points to an already tracked identity; snapshot retained; Rescan explicitly");
                    ensure!(self.known.contains(&next_id) || self.known.len() < MAX_FILES, "Follow identity limit reached; snapshot retained; Rescan explicitly");
                    let label = match reason { ResetReason::Rotated => "before rotation", ResetReason::Truncated => "before truncation" };
                    let old_slot = self.stage(source.slot, &path, previous.snapshot(), Some(label), plan, &mut updates)?;
                    let old_tail = if reason == ResetReason::Rotated { Some(old_identity.tail(&path, previous.snapshot())?) } else { None };
                    retired.push(Retired { _identity: old_identity, slot: old_slot, path: path.clone(), tail: old_tail, label });
                    self.known.insert(next_id);
                    source.slot = self.stage(None, &path, source.tail.snapshot(), None, plan, &mut updates)?;
                    notices.push(format!("Observed {reason:?}; earlier segment retained; unobserved gaps are not ruled out"));
                }
                TailEvent::WindowChanged | TailEvent::Appended { reindexed: true, .. } => bail!("Source record window changed; selection/marks retained in old snapshot; Rescan explicitly"),
                TailEvent::Stale => bail!("Stale source completion; snapshot retained; Rescan explicitly"),
                TailEvent::Deferred(error) => bail!("Follow memory bound: {error}; snapshot retained; Rescan explicitly"),
            }
            if source.slot.is_none() {
                notices.push("Replacement has not yet matched frozen query/time criteria; earlier history retained".into());
            }
            if source.tail.is_missing() {
                notices.push("Source missing; open identity/history retained while waiting for replacement; continuity unproven".into());
            }
        }
        self.active = active;
        self.retired = retired;
        running(cancel)?;
        let mut retry_path = false;
        let candidates = if changes.is_empty() {
            None
        } else {
            let mut selection = plan.select_excluding(&self.known, |_, _| {});
            ensure!(
                complete(&selection),
                "Membership refresh incomplete; snapshot retained; Rescan explicitly"
            );
            running(cancel)?;
            retry_path = self.discover(
                &selection.files,
                &mut selection.candidates,
                &mut updates,
                &mut notices,
            )?;
            Some(selection.candidates)
        };
        let bytes: u64 = self
            .published
            .values()
            .map(|s| s.snapshot.bytes().len() as u64)
            .sum::<u64>()
            + self
                .active
                .iter()
                .filter(|s| s.slot.is_none())
                .map(|s| s.tail.bytes().len() as u64)
                .sum::<u64>()
            + self
                .retired
                .iter()
                .filter(|s| s.slot.is_none())
                .filter_map(|s| s.tail.as_ref())
                .map(|t| t.bytes().len() as u64)
                .sum::<u64>();
        ensure!(
            bytes <= MAX_TOTAL_BYTES,
            "Follow retained byte bound reached; snapshot retained; Rescan explicitly"
        );
        running(cancel)?;
        let more = retry_path
            || self
                .active
                .iter()
                .map(|s| &s.tail)
                .chain(self.retired.iter().filter_map(|s| s.tail.as_ref()))
                .any(|tail| tail.window().end < tail.window().file_len);
        Ok(Update {
            sources: updates,
            candidates,
            more,
            notice: (!notices.is_empty()).then(|| notices.join(" · ")),
        })
    }
}

#[cfg(test)]
#[path = "follow/tests.rs"]
mod tests;
