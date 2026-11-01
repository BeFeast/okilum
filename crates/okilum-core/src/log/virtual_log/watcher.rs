//! Event-driven invalidation. Install before selection, then drain queued changes.
//! No timer, polling scan or Access-triggered self-invalidation.
use anyhow::{ensure, Result};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TrySendError},
        Arc,
    },
};

pub const MAX_DIRECTORIES: usize = 512;
const MAX_PENDING: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// Reconcile membership and update the existing tail sessions. Never discard
    /// removed snapshots silently: use Removed to announce their disappearance.
    Changed(Vec<PathBuf>),
    Removed(Vec<PathBuf>),
    /// Coverage is uncertain; caller must surface this and explicitly rescan.
    Incomplete(String),
}

pub fn classify(event: notify::Result<Event>) -> Option<Change> {
    match event {
        Err(error) => Some(Change::Incomplete(error.to_string())),
        Ok(event) if event.need_rescan() => Some(Change::Incomplete(
            "Filesystem events lost; rescan required".into(),
        )),
        Ok(event) => match event.kind {
            EventKind::Access(_) => None,
            EventKind::Remove(_) => Some(Change::Removed(event.paths)),
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Any | EventKind::Other => {
                Some(Change::Changed(event.paths))
            }
        },
    }
}

pub struct VirtualLogWatcher {
    watcher: RecommendedWatcher,
    receiver: Receiver<Change>,
    overflow: Arc<AtomicBool>,
    directories: BTreeSet<PathBuf>,
    root: PathBuf,
}

impl VirtualLogWatcher {
    pub fn start(root: &Path, wake: impl Fn() + Send + 'static) -> Result<Self> {
        let root = root.canonicalize()?;
        ensure!(root.is_dir(), "Virtual log watch root must be a directory");
        let (sender, receiver) = mpsc::sync_channel(MAX_PENDING);
        let overflow = Arc::new(AtomicBool::new(false));
        let lost = overflow.clone();
        let watcher = notify::recommended_watcher(move |event| {
            if let Some(change) = classify(event) {
                if matches!(sender.try_send(change), Err(TrySendError::Full(_))) {
                    lost.store(true, Ordering::Release);
                }
                wake();
            }
        })?;
        let mut this = Self {
            watcher,
            receiver,
            overflow,
            directories: BTreeSet::new(),
            root,
        };
        this.refresh_directories()?;
        Ok(this)
    }

    /// Called after a directory change, before rebuilding selection. Each directory
    /// is watched non-recursively; a scope limit is an error, never silent success.
    pub fn refresh_directories(&mut self) -> Result<()> {
        // Watch the root first, closing the new-subdirectory race during enumeration.
        if !self.directories.contains(&self.root) {
            self.watcher
                .watch(&self.root, RecursiveMode::NonRecursive)?;
            self.directories.insert(self.root.clone());
        }
        let mut desired = BTreeSet::new();
        for (visited, entry) in walkdir::WalkDir::new(&self.root)
            .follow_links(false)
            .into_iter()
            .enumerate()
        {
            ensure!(
                visited < super::MAX_ENTRIES,
                "Virtual log watcher enumeration limit reached"
            );
            let entry = entry?;
            if !entry.file_type().is_dir() {
                continue;
            }
            ensure!(
                desired.len() < MAX_DIRECTORIES,
                "Virtual log watcher directory limit reached"
            );
            let path = entry.path().to_owned();
            desired.insert(path.clone());
            if !self.directories.contains(&path) {
                self.watcher.watch(&path, RecursiveMode::NonRecursive)?;
                self.directories.insert(path);
            }
        }
        let removed: Vec<_> = self.directories.difference(&desired).cloned().collect();
        for path in removed {
            self.watcher.unwatch(&path)?;
            self.directories.remove(&path);
        }
        Ok(())
    }

    /// Non-blocking, bounded drain on a caller's filesystem notification turn.
    pub fn drain(&self) -> Vec<Change> {
        let mut changes: Vec<_> = self.receiver.try_iter().take(MAX_PENDING).collect();
        if self.overflow.swap(false, Ordering::AcqRel) {
            changes.push(Change::Incomplete(
                "Virtual log event queue overflow; rescan required".into(),
            ));
        }
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, RemoveKind};

    #[test]
    fn mutation_positive_control_then_reads_are_silent_and_removal_is_visible() {
        let path = PathBuf::from("logs/new.jsonl");
        assert_eq!(
            classify(Ok(
                Event::new(EventKind::Create(CreateKind::File)).add_path(path.clone())
            )),
            Some(Change::Changed(vec![path.clone()]))
        );
        assert_eq!(
            classify(Ok(
                Event::new(EventKind::Access(AccessKind::Read)).add_path(path.clone())
            )),
            None
        );
        assert_eq!(
            classify(Ok(
                Event::new(EventKind::Remove(RemoveKind::File)).add_path(path.clone())
            )),
            Some(Change::Removed(vec![path]))
        );
    }
}
