//! Short exclusive transactions over the revision-bound envelope (design:
//! docs/sync-sidecar-stop-operations.md). The directory lock is held only for one
//! transaction and released before any IPC wait; every commit is an atomic,
//! flushed replacement of `sidecar.json` and must be the exact next revision.
//! Windows needs its own DACL-checked store.
use super::{
    authority::{Envelope, Stored},
    journal::Directory,
    Journal,
};
use anyhow::{ensure, Context, Result};
use std::{
    fs::TryLockError,
    path::Path,
    time::{Duration, Instant},
};

const NAME: &str = "sidecar.json";
/// Legacy `update.json`. Read only for migration; the v2 envelope owns update
/// state afterwards and the leftover file is kept as recovery evidence.
const LEGACY_UPDATE: &str = "update.json";

pub struct Store {
    dir: Directory,
}
impl Store {
    /// No directory, journal or lock is created. Preparation supplies a private
    /// directory outside installation, vault and index, after explicit Enable.
    pub fn open_existing(path: &Path) -> Result<Self> {
        let dir = Directory::open(path)?;
        dir.check_location()?;
        Ok(Self { dir })
    }
    /// Take the exclusive instance lock or fail at `deadline`. Callers pass the
    /// absolute deadline of the whole exchange; there is no retry beyond it.
    pub fn begin(&mut self, deadline: Instant) -> Result<Transaction<'_>> {
        loop {
            match self.dir.handle.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    ensure!(!left.is_zero(), "sidecar state is busy");
                    std::thread::sleep(left.min(Duration::from_millis(2)));
                }
                Err(TryLockError::Error(e)) => return Err(e.into()),
            }
        }
        let mut transaction = Transaction {
            dir: &self.dir,
            state: Stored::Absent,
            failed: false,
        };
        transaction.state = transaction.read_state()?;
        Ok(transaction)
    }
}

pub struct Transaction<'a> {
    dir: &'a Directory,
    state: Stored,
    failed: bool,
}
impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        let _ = self.dir.handle.unlock();
    }
}
impl Transaction<'_> {
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
        let Some(data) = self.dir.read_bytes(NAME)? else {
            ensure!(
                self.dir.read_bytes(LEGACY_UPDATE)?.is_none(),
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
        let update = self
            .dir
            .read_bytes(LEGACY_UPDATE)?
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
        match self.dir.write_bytes(NAME, &next.to_vec()?) {
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

impl super::Authority for Store {
    type Tx<'a> = Transaction<'a>;
    fn begin(&mut self, deadline: Instant) -> Result<Transaction<'_>> {
        Store::begin(self, deadline)
    }
}
impl super::Tx for Transaction<'_> {
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

#[cfg(test)]
mod tests;
