//! The supervisor's side of a Stop: authorize under the instance lock, keep that
//! lock through the bounded owned-tree stop and reap, release it before the reply
//! (the server drops the lease first). Platform integrations supply only the
//! [`OwnedTree`]; the store, token checks and lock discipline are shared.
use super::ipc::{OwnedRuntime, Scope, Status};
use crate::sidecar::{
    authority::StopToken,
    store::{StateDir, Store, Transaction},
    Binding,
};
use anyhow::{ensure, Context, Result};
use std::time::Instant;

/// Captured process and job handles of the supervised tree, never a PID lookup.
pub trait OwnedTree {
    fn status(&mut self) -> Result<Status>;
    /// Bounded stop AND reap. Stopped means every owned descendant exited;
    /// Stopping means a timeout and must not permit unregister or removal.
    fn stop(&mut self) -> Result<Status>;
}

pub struct StoreRuntime<D: StateDir, T> {
    store: Store<D>,
    tree: T,
    /// Absolute deadline of the current exchange, shared with its transport, so
    /// waiting for the lock cannot extend the exchange budget.
    deadline: Option<Instant>,
    /// A stop can partially affect the tree even when it returns an error. This
    /// generation must never advertise Running again after beginning that effect.
    stopped: Option<Status>,
}

/// The lock and deadline belong to the same authorization. A later exchange
/// cannot refresh the budget of an already authorized stop.
pub struct StopLease<D: StateDir> {
    _transaction: Transaction<D>,
    deadline: Instant,
}
impl<D: StateDir, T: OwnedTree> StoreRuntime<D, T> {
    pub fn new(store: Store<D>, tree: T) -> Self {
        Self {
            store,
            tree,
            deadline: None,
            stopped: None,
        }
    }
    /// Call before each `Server::serve_one` with that connection's deadline. A
    /// Stop with no deadline is denied rather than allowed to wait unboundedly.
    pub fn begin_exchange(&mut self, deadline: Instant) {
        self.deadline = Some(deadline);
    }
    pub fn tree(&self) -> &T {
        &self.tree
    }
    /// The supervisor's own cleanup after its runtime died. Not an authorized Stop: it
    /// takes no lock, answers nobody and exists only so the owned tree is flushed
    /// before the supervisor exits.
    pub fn flush_tree(&mut self) -> Result<Status> {
        self.stop_tree()
    }
    #[cfg(all(test, unix))]
    pub(crate) fn tree_mut(&mut self) -> &mut T {
        &mut self.tree
    }

    fn stop_tree(&mut self) -> Result<Status> {
        if self.stopped == Some(Status::Stopped) {
            return Ok(Status::Stopped);
        }
        self.stopped = Some(Status::Stopping);
        let status = self.tree.stop()?;
        ensure!(status != Status::Running, "stop did not transition runtime");
        self.stopped = Some(status);
        Ok(status)
    }

    fn authorize_stop_with_clock(
        &mut self,
        binding: &Binding,
        scope: &Scope,
        token: &StopToken,
        now: impl Fn() -> Instant,
    ) -> Result<StopLease<D>> {
        let deadline = self.deadline.take().context("no exchange deadline set")?;
        ensure!(now() < deadline, "supervisor exchange deadline expired");
        let transaction = self.store.begin(deadline)?;
        let envelope = transaction
            .current()?
            .context("no v2 journal authorizes a stop")?;
        ensure!(
            envelope.binding() == binding,
            "journal binding differs from the supervisor's"
        );
        envelope.authorize(token, scope)?;
        // A free lock may succeed after its deadline, and reading the journal
        // also consumes budget. Neither authorizes an effect after expiry.
        ensure!(now() < deadline, "supervisor exchange deadline expired");
        Ok(StopLease {
            _transaction: transaction,
            deadline,
        })
    }

    fn stop_owned_with_clock(
        &mut self,
        lease: &StopLease<D>,
        now: impl Fn() -> Instant,
    ) -> Result<Status> {
        ensure!(
            now() < lease.deadline,
            "supervisor exchange deadline expired"
        );
        let status = self.stop_tree()?;
        // A late result is not a successful exchange, even if a subsequent
        // authorized retry can observe that the tree really did finish.
        ensure!(
            now() < lease.deadline,
            "supervisor exchange deadline expired"
        );
        Ok(status)
    }
}
impl<D: StateDir, T: OwnedTree> OwnedRuntime for StoreRuntime<D, T> {
    /// The transaction owns the lock; the lease also retains the original
    /// exchange deadline. Dropping the lease releases the lock.
    type Lease = StopLease<D>;
    fn status(&mut self) -> Result<Status> {
        if self.stopped == Some(Status::Stopped) {
            return Ok(Status::Stopped);
        }
        let status = self.tree.status()?;
        if status != Status::Running {
            self.stopped = Some(status);
        }
        Ok(self.stopped.unwrap_or(status))
    }
    fn authorize_stop(
        &mut self,
        binding: &Binding,
        scope: &Scope,
        token: &StopToken,
    ) -> Result<StopLease<D>> {
        self.authorize_stop_with_clock(binding, scope, token, Instant::now)
    }
    fn stop_owned(&mut self, lease: &StopLease<D>) -> Result<Status> {
        self.stop_owned_with_clock(lease, Instant::now)
    }
}

#[cfg(all(test, unix))]
mod tests;
