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
}
impl<D: StateDir, T: OwnedTree> StoreRuntime<D, T> {
    pub fn new(store: Store<D>, tree: T) -> Self {
        Self {
            store,
            tree,
            deadline: None,
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
    #[cfg(all(test, unix))]
    pub(crate) fn tree_mut(&mut self) -> &mut T {
        &mut self.tree
    }
}
impl<D: StateDir, T: OwnedTree> OwnedRuntime for StoreRuntime<D, T> {
    /// The transaction itself: it owns the lock, and dropping it releases it.
    type Lease = Transaction<D>;
    fn status(&mut self) -> Result<Status> {
        self.tree.status()
    }
    fn authorize_stop(
        &mut self,
        binding: &Binding,
        scope: &Scope,
        token: &StopToken,
    ) -> Result<Transaction<D>> {
        let deadline = self.deadline.take().context("no exchange deadline set")?;
        let transaction = self.store.begin(deadline)?;
        let envelope = transaction
            .current()?
            .context("no v2 journal authorizes a stop")?;
        ensure!(
            envelope.binding() == binding,
            "journal binding differs from the supervisor's"
        );
        envelope.authorize(token, scope)?;
        Ok(transaction)
    }
    fn stop_owned(&mut self, _lease: &Transaction<D>) -> Result<Status> {
        self.tree.stop()
    }
}

#[cfg(all(test, unix))]
mod tests;
