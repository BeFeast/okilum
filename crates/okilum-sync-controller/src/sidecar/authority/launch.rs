//! One-use macOS registration and daemon launch authority. Durable uncertainty
//! sacrifices availability, never grants another launch. There is deliberately
//! no re-arm operation: proving safe recovery is a separate procedure (#588).
use super::{Envelope, Intent};
use crate::sidecar::{
    store::{StateDir, Store},
    Binding,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Instant;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    /// Explicit fresh opt-in, before any OS registration was attempted.
    Ready,
    /// Persisted before calling ServiceManagement, even if that call fails.
    Dispatched,
    /// Persisted before spawn. A helper restart must not spawn again.
    Claimed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    generation: Uuid,
    phase: Phase,
}
impl Registration {
    pub fn generation(&self) -> Uuid {
        self.generation
    }
    pub fn phase(&self) -> Phase {
        self.phase
    }
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(
            !self.generation.is_nil(),
            "recovery pending: nil launch generation"
        );
        Ok(())
    }
}

pub(super) fn check_successor(
    old: Option<&Registration>,
    new: Option<&Registration>,
) -> Result<()> {
    ensure!(
        match (old, new) {
            (None, None) => true,
            (Some(old), Some(new)) =>
                old.generation == new.generation
                    && (old.phase == new.phase
                        || matches!(
                            (old.phase, new.phase),
                            (Phase::Ready, Phase::Dispatched) | (Phase::Dispatched, Phase::Claimed)
                        )),
            _ => false,
        },
        "recovery pending: launch authority cannot be replaced or re-armed"
    );
    Ok(())
}
impl Envelope {
    /// Only for a NEW explicit enrollment at the one native account state root.
    /// Legacy migration and repeated Enable must never call this to recover.
    pub fn first_with_launch(binding: Binding) -> Self {
        let mut first = Self::first(binding);
        first.launch = Some(Registration {
            generation: Uuid::new_v4(),
            phase: Phase::Ready,
        });
        first
    }
    pub fn launch_registration(&self) -> Option<&Registration> {
        self.launch.as_ref()
    }
    fn advance_launch(&self, expected: Phase, next: Phase, binding: &Binding) -> Result<Self> {
        ensure!(
            self.binding() == binding,
            "recovery pending: launch binding changed"
        );
        ensure!(
            self.intent() == Intent::Enabled && self.stop().is_none(),
            "launch requires enabled intent without a pending stop"
        );
        ensure!(
            self.update().is_none(),
            "recovery pending: update needs explicit recovery"
        );
        let saved = self
            .launch
            .as_ref()
            .context("recovery pending: missing launch generation")?;
        ensure!(
            saved.phase == expected,
            "recovery pending: launch permission already consumed"
        );
        let mut result = self.next()?;
        result.launch.as_mut().unwrap().phase = next;
        Ok(result)
    }
}

/// Not Clone/Copy/Deserialize: obtained only AFTER a successful flushed commit.
/// The caller consumes it for its single native registration attempt. Dropping
/// it never rolls back permission, even if no native call was made.
#[must_use]
pub struct Dispatch {
    generation: Uuid,
}
impl Dispatch {
    pub fn execute<T>(self, effect: impl FnOnce(Uuid) -> Result<T>) -> Result<T> {
        effect(self.generation)
    }
}
impl<D: StateDir> Store<D> {
    pub fn dispatch_launch(&self, binding: &Binding, deadline: Instant) -> Result<Dispatch> {
        let mut tx = self.begin(deadline)?;
        let saved = tx
            .current()?
            .context("recovery pending: missing launch journal")?;
        let next = saved.advance_launch(Phase::Ready, Phase::Dispatched, binding)?;
        let generation = next.launch.as_ref().unwrap().generation;
        tx.commit(next)?;
        // The lock drops before the effect; the launched helper needs it to claim.
        Ok(Dispatch { generation })
    }
    /// The returned generation is usable only by THIS successful call. Reopening
    /// a consumed record after a failed flush does not reconstruct permission.
    pub fn claim_launch(&self, binding: &Binding, deadline: Instant) -> Result<Uuid> {
        let mut tx = self.begin(deadline)?;
        let saved = tx
            .current()?
            .context("recovery pending: missing launch journal")?;
        let next = saved.advance_launch(Phase::Dispatched, Phase::Claimed, binding)?;
        let generation = next.launch.as_ref().unwrap().generation;
        tx.commit(next)?;
        Ok(generation)
    }
}
