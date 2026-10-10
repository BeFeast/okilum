//! Short exclusive transactions over the revision-bound envelope (design:
//! docs/sync-sidecar-stop-operations.md). The instance lock is held only for one
//! transaction and released before any IPC wait; every commit is an atomic,
//! flushed replacement of `sidecar.json` and must be the exact next revision.
//! The platform supplies a [`StateDir`] (private directory, lock, file I/O); the
//! transaction logic here is shared. A transaction owns its lock, so the native
//! supervisor can hold it from `authorize_stop` through the owned-tree stop.
use super::{
    authority::{Envelope, Stored},
    update::Update,
    Journal,
};
use anyhow::{ensure, Context, Result};
use std::{sync::Arc, time::Instant};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{UnixDir, UnixStore};
#[cfg(target_os = "windows")]
mod windows_dir;
#[cfg(target_os = "windows")]
pub use windows_dir::{WindowsDir, WindowsStore};

mod hint;
mod selection;
pub use hint::Hint;
pub(crate) use selection::version_label_ok;
pub use selection::Selection;

pub(crate) const NAME: &str = "sidecar.json";
/// Legacy `update.json`. Read only for migration; the v2 envelope owns update
/// state afterwards and the leftover file is kept as recovery evidence.
pub(crate) const LEGACY_UPDATE: &str = "update.json";

/// A verified private state directory. Implementations never create state, check
/// owner and mode (Unix) or the protected owner-only DACL (Windows) on every
/// access, refuse links and reparse points, and bound file sizes.
pub trait StateDir: Sized + 'static {
    /// Exclusive instance lock; released on drop. Re-acquiring it through any
    /// handle in this process or another must fail or wait, never nest.
    type Lock;
    /// Wait for the lock until the absolute `deadline`, then fail as busy.
    fn lock(self: &Arc<Self>, deadline: Instant) -> Result<Self::Lock>;
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>>;
    /// Atomic replace that is flushed before returning. An error after the
    /// replacement became visible is still an error.
    fn write(&self, name: &str, data: &[u8]) -> Result<()>;
    /// Delete a private regular file; absent is success, a foreign one is refused.
    fn remove(&self, name: &str) -> Result<()>;
}

pub struct Store<D: StateDir> {
    dir: Arc<D>,
}
impl<D: StateDir> Clone for Store<D> {
    fn clone(&self) -> Self {
        Self {
            dir: self.dir.clone(),
        }
    }
}
impl<D: StateDir> Store<D> {
    pub fn new(dir: D) -> Self {
        Self { dir: Arc::new(dir) }
    }
    /// Take the exclusive instance lock or fail at `deadline`. Callers pass the
    /// absolute deadline of the whole exchange; there is no retry beyond it.
    pub fn begin(&self, deadline: Instant) -> Result<Transaction<D>> {
        let lock = self.dir.lock(deadline)?;
        let mut transaction = Transaction {
            dir: self.dir.clone(),
            state: Stored::Absent,
            failed: false,
            _lock: lock,
        };
        transaction.state = transaction.read_state()?;
        Ok(transaction)
    }
}

pub struct Transaction<D: StateDir> {
    dir: Arc<D>,
    state: Stored,
    failed: bool,
    /// Last field: the lock outlives everything above and is released on drop.
    _lock: D::Lock,
}
impl<D: StateDir> Transaction<D> {
    /// Unreadable after a failed commit: the disk may or may not hold the new
    /// revision, so nothing may act on this snapshot. Begin a new transaction.
    pub fn state(&self) -> Result<&Stored> {
        ensure!(!self.failed, "journal commit failed; reload required");
        Ok(&self.state)
    }
    pub fn current(&self) -> Result<Option<&Envelope>> {
        Ok(match self.state()? {
            Stored::Current(envelope) => Some(envelope),
            _ => None,
        })
    }

    fn read_state(&self) -> Result<Stored> {
        let Some(data) = self.dir.read(NAME)? else {
            ensure!(
                self.dir.read(LEGACY_UPDATE)?.is_none(),
                "update has no lifecycle journal"
            );
            return Ok(Stored::Absent);
        };
        let value: serde_json::Value =
            serde_json::from_slice(&data).context("invalid sidecar journal; recovery required")?;
        if value.get("schema").is_some() {
            return Ok(Stored::Current(Envelope::from_slice(&data)?));
        }
        let journal: Journal =
            serde_json::from_value(value).context("invalid sidecar journal; recovery required")?;
        let update: Option<Update> = self
            .dir
            .read(LEGACY_UPDATE)?
            .map(|data| serde_json::from_slice(&data))
            .transpose()
            .context("invalid update journal; recovery required")?;
        Ok(Stored::Legacy { journal, update })
    }

    /// Replace the journal with `next`, which must be the exact successor of the
    /// state read in this transaction. On error nothing may act on the result:
    /// the replacement may even be visible if only the directory flush failed.
    pub fn commit(&mut self, next: Envelope) -> Result<()> {
        ensure!(!self.failed, "journal commit failed; reload required");
        Envelope::check_successor(&self.state, &next)?;
        self.write(next)
    }
    fn write(&mut self, next: Envelope) -> Result<()> {
        match self.dir.write(NAME, &next.to_vec()?) {
            Ok(()) => {
                self.state = Stored::Current(next);
                Ok(())
            }
            Err(e) => {
                self.failed = true;
                Err(e)
            }
        }
    }

    /// Explicit migration of a legacy journal: one flushed replacement of
    /// `sidecar.json`, no native effect, no stop fabricated. Idempotent once
    /// migrated; the caller arms a verified operation in a later revision.
    pub fn migrate(&mut self) -> Result<Envelope> {
        match self.state()?.clone() {
            Stored::Absent => anyhow::bail!("no journal to migrate"),
            Stored::Current(envelope) => Ok(envelope),
            Stored::Legacy { journal, update } => {
                let envelope = Envelope::migrate(journal, update)?;
                self.commit(envelope.clone())?;
                Ok(envelope)
            }
        }
    }
    /// Explicit operator repair after revision exhaustion; the caller has
    /// verified the supervisor stopped. The new epoch revokes every old token.
    pub fn repair(&mut self) -> Result<Envelope> {
        ensure!(!self.failed, "journal commit failed; reload required");
        let Stored::Current(current) = &self.state else {
            anyhow::bail!("only a current journal can be repaired");
        };
        let repaired = current.repair_epoch();
        self.write(repaired.clone())?;
        Ok(repaired)
    }
}

impl<D: StateDir> super::Authority for Store<D> {
    type Tx<'a> = Transaction<D>;
    fn begin(&mut self, deadline: Instant) -> Result<Transaction<D>> {
        Store::begin(self, deadline)
    }
}
impl<D: StateDir> super::Tx for Transaction<D> {
    fn stored(&self) -> Result<&Stored> {
        self.state()
    }
    fn commit(&mut self, next: Envelope) -> Result<()> {
        Transaction::commit(self, next)
    }
    fn migrate(&mut self) -> Result<Envelope> {
        Transaction::migrate(self)
    }
}

#[cfg(all(test, unix))]
mod tests;
#[cfg(all(test, target_os = "windows"))]
mod windows_tests;
