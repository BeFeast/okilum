//! Event-driven additions/appends, with conservative identity/continuity stops.
use anyhow::{bail, ensure, Context as _, Result};
use okilum_core::log::{
    tail::{FileIdentity, TailEvent},
    virtual_log::{
        watcher::Change, Reason, Selection, SelectionPlan, MAX_FILES, MAX_FILE_BYTES,
        MAX_TOTAL_BYTES,
    },
    LogFile, LogSnapshot, LogTail,
};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
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
}
#[derive(Clone)]
pub(super) struct Update {
    pub sources: Vec<Source>,
    pub candidates: Option<Vec<okilum_core::log::virtual_log::Candidate>>,
    pub more: bool,
}
struct Active {
    slot: usize,
    tail: LogTail,
}
pub(super) struct Session {
    known: HashSet<FileIdentity>,
    active: Vec<Active>,
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
impl Session {
    pub fn new(selection: &Selection) -> Result<Self> {
        ensure!(
            complete(selection),
            "Incomplete initial selection; use Snapshot/Rescan before Follow"
        );
        let mut this = Self {
            known: HashSet::new(),
            active: Vec::new(),
            frozen: Vec::new(),
            published: BTreeMap::new(),
        };
        for file in &selection.files {
            this.accept(file)?;
        }
        Ok(this)
    }
    fn accept(&mut self, file: &LogFile) -> Result<Source> {
        ensure!(
            self.known.len() < MAX_FILES,
            "Follow lifetime source limit reached; retained snapshot unchanged; Rescan explicitly"
        );
        ensure!(
            !self.known.contains(&file.identity()),
            "Repeated source identity; Rescan explicitly"
        );
        let slot = self.published.len();
        let source = Source {
            slot,
            path: file.path().to_owned(),
            snapshot: file.snapshot(),
        };
        if file.is_compressed() {
            self.frozen.push(file.path().to_owned());
            self.frozen.push(file.path().canonicalize()?);
        } else {
            self.active.push(Active {
                slot,
                tail: LogTail::from_loaded(file)?,
            });
        }
        self.known.insert(file.identity());
        self.published.insert(slot, source.clone());
        Ok(source)
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
                        "Compressed source changed; immutable history retained; Rescan explicitly"
                    );
                }
            }
        }
        let mut updates = Vec::new();
        for active in &mut self.active {
            running(cancel)?;
            let read = active.tail.begin_poll().read_pinned().context(
                "Source removed/replaced or unreadable; snapshot retained; Rescan explicitly",
            )?;
            running(cancel)?;
            let event = active.tail.apply(read);
            ensure!(
                !active.tail.is_missing(),
                "A source was removed; old identity/history retained; Rescan explicitly"
            );
            match event {
                TailEvent::Unchanged => {},
                TailEvent::Appended { reindexed: false, .. } => {
                    updates.push(Source { slot: active.slot, path: active.tail.path().to_owned(), snapshot: active.tail.snapshot() });
                }
                TailEvent::Reset { reason, .. } => bail!("{reason:?} source; Follow paused before identity/segment replacement. Snapshot retained; rotation continuity is unproven; Rescan explicitly"),
                TailEvent::WindowChanged | TailEvent::Appended { reindexed: true, .. } => bail!("Source record window changed; selection/marks retained in old snapshot; Rescan explicitly"),
                TailEvent::Stale => bail!("Stale source completion; snapshot retained; Rescan explicitly"),
                TailEvent::Deferred(error) => bail!("Follow memory bound: {error}; snapshot retained; Rescan explicitly"),
            }
        }
        running(cancel)?;
        let candidates = if changes.is_empty() {
            None
        } else {
            let selection = plan.select_excluding(&self.known, |_, _| {});
            ensure!(
                complete(&selection),
                "Membership refresh incomplete; snapshot retained; Rescan explicitly"
            );
            running(cancel)?;
            for file in &selection.files {
                // A compressed copy is a different inode: continuity is unproven.
                ensure!(!file.is_compressed(), "New compressed rotation/source requires Rescan; continuity is unproven; snapshot retained");
                updates.push(self.accept(file)?);
            }
            Some(selection.candidates)
        };
        let mut published = self.published.clone();
        for source in &updates {
            ensure!(
                source.snapshot.bytes().len() as u64 <= MAX_FILE_BYTES,
                "Follow per-source byte bound reached; snapshot retained; Rescan explicitly"
            );
            published.insert(source.slot, source.clone());
        }
        let bytes: u64 = published
            .values()
            .map(|s| s.snapshot.bytes().len() as u64)
            .sum();
        ensure!(
            bytes <= MAX_TOTAL_BYTES,
            "Follow retained byte bound reached; snapshot retained; Rescan explicitly"
        );
        running(cancel)?;
        self.published = published;
        Ok(Update {
            sources: updates,
            candidates,
            more: self
                .active
                .iter()
                .any(|a| a.tail.window().end < a.tail.window().file_len),
        })
    }
}

#[cfg(test)]
#[path = "follow/tests.rs"]
mod tests;
