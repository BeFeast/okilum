//! One-use macOS registration and daemon launch authority. Durable uncertainty
//! sacrifices availability, never grants another launch. There is deliberately
//! no retry from absence. Native package upgrades can grant a new generation
//! only after an authenticated owned-tree Stop and captured kernel exit.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retired: Option<Retired>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Retired {
    operation: Uuid,
    stop_revision: u64,
}
/// Constructed by the native connection only after its signed Stop reply and
/// captured kernel exit both succeed. Never reconstructed from PID or absence.
#[cfg(any(target_os = "macos", test))]
pub(crate) struct VerifiedExit {
    binding: Binding,
    token: super::StopToken,
}
#[cfg(any(target_os = "macos", test))]
impl VerifiedExit {
    #[cfg(target_os = "macos")]
    pub(crate) fn after_native_stop(binding: Binding, token: super::StopToken) -> Self {
        Self { binding, token }
    }
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
        if let Some(retired) = &self.retired {
            ensure!(
                self.phase == Phase::Claimed
                    && !retired.operation.is_nil()
                    && retired.stop_revision > 0,
                "invalid retired native launch"
            );
        }
        Ok(())
    }
}

pub(super) fn check_successor(current: &Envelope, next: &Envelope) -> Result<()> {
    let valid = match (current.launch.as_ref(), next.launch.as_ref()) {
        (None, None) => true,
        (Some(old), Some(new)) if old.generation == new.generation => {
            if old.retired == new.retired {
                old.phase == new.phase
                    || matches!(
                        (old.phase, new.phase),
                        (Phase::Ready, Phase::Dispatched) | (Phase::Dispatched, Phase::Claimed)
                    )
            } else if let (None, Some(retired), Some(stop)) =
                (&old.retired, &new.retired, current.stop())
            {
                old.phase == Phase::Claimed
                    && new.phase == Phase::Claimed
                    && current.intent() == Intent::Disabled
                    && next.intent() == Intent::Disabled
                    && next.stop().is_none()
                    && next.update().is_none()
                    && stop.reason == super::Reason::Disable
                    && stop.scope.generation == old.generation
                    && stop.operation_id == retired.operation
                    && stop.authorized_revision == retired.stop_revision
            } else {
                false
            }
        }
        (Some(old), Some(new)) => {
            old.phase == Phase::Claimed
                && old.retired.is_some()
                && new.phase == Phase::Ready
                && new.retired.is_none()
                && current.intent() == Intent::Disabled
                && current.stop().is_none()
                && current.update().is_none()
                && next.intent() == Intent::Enabled
                && next.stop().is_none()
                && next.update().is_none()
        }
        _ => false,
    };
    ensure!(
        valid,
        "recovery pending: launch authority cannot be replaced without verified retirement"
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
            retired: None,
        });
        first
    }
    pub fn launch_registration(&self) -> Option<&Registration> {
        self.launch.as_ref()
    }
    pub fn native_launch_retired(&self) -> bool {
        self.launch
            .as_ref()
            .is_some_and(|launch| launch.retired.is_some())
    }
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn complete_native_retirement(&self, proof: VerifiedExit) -> Result<Self> {
        ensure!(
            proof.binding == self.binding && proof.token.operation.reason == super::Reason::Disable,
            "native stop proof differs from the selected installation"
        );
        self.authorize(&proof.token, &proof.token.operation.scope)?;
        let registration = self
            .launch
            .as_ref()
            .context("native launch generation missing")?;
        ensure!(
            registration.phase == Phase::Claimed
                && registration.retired.is_none()
                && registration.generation == proof.token.operation.scope.generation,
            "native retirement requires its claimed generation"
        );
        let mut next = self.complete_stop(&proof.token, &proof.token.operation.scope)?;
        next.launch.as_mut().unwrap().retired = Some(Retired {
            operation: proof.token.operation.operation_id,
            stop_revision: proof.token.operation.authorized_revision,
        });
        Ok(next)
    }
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn enable_retired_native_launch(&self) -> Result<Self> {
        ensure!(
            self.native_launch_retired()
                && self.intent == Intent::Disabled
                && self.stop.is_none()
                && self.update.is_none(),
            "native restart requires a completed verified retirement"
        );
        let mut next = self.next()?;
        next.intent = Intent::Enabled;
        next.launch = Some(Registration {
            generation: Uuid::new_v4(),
            phase: Phase::Ready,
            retired: None,
        });
        Ok(next)
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
        self.dispatch_launch_checked(binding, None, deadline)
    }
    /// Explicit UI confirmation is bound to the generation it displayed. Check
    /// and consume that generation under the SAME transaction, never re-arm it.
    pub fn dispatch_confirmed_launch(
        &self,
        binding: &Binding,
        confirmed: Uuid,
        deadline: Instant,
    ) -> Result<Dispatch> {
        self.dispatch_launch_checked(binding, Some(confirmed), deadline)
    }
    fn dispatch_launch_checked(
        &self,
        binding: &Binding,
        confirmed: Option<Uuid>,
        deadline: Instant,
    ) -> Result<Dispatch> {
        let mut tx = self.begin(deadline)?;
        let saved = tx
            .current()?
            .context("recovery pending: missing launch journal")?;
        if let Some(confirmed) = confirmed {
            ensure!(
                saved
                    .launch_registration()
                    .is_some_and(|launch| launch.generation() == confirmed),
                "recovery pending: confirmed launch generation changed"
            );
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidecar::{
        authority::{StopToken, Stored},
        supervisor::ipc::Scope,
    };
    fn claimed() -> Envelope {
        let binding = Binding {
            installation: Uuid::new_v4(),
            instance: Uuid::new_v4(),
            owner: "fixture".into(),
            supervisor: "/signed/helper".into(),
            state_directory: "/private/state".into(),
            device_identity: "retained-device".into(),
        };
        Envelope::first_with_launch(binding.clone())
            .advance_launch(Phase::Ready, Phase::Dispatched, &binding)
            .unwrap()
            .advance_launch(Phase::Dispatched, Phase::Claimed, &binding)
            .unwrap()
    }
    fn stopping(saved: &Envelope) -> (Envelope, StopToken) {
        saved
            .disable(Scope {
                installation: saved.binding.installation,
                instance: saved.binding.instance,
                generation: saved.launch.as_ref().unwrap().generation,
            })
            .unwrap()
    }
    #[test]
    fn only_verified_retirement_can_grant_one_new_generation_without_changing_identity(
    ) -> Result<()> {
        let original = claimed();
        let (stopping, token) = stopping(&original);
        let retired = stopping.complete_native_retirement(VerifiedExit {
            binding: original.binding.clone(),
            token: token.clone(),
        })?;
        Envelope::check_successor(&Stored::Current(stopping.clone()), &retired)?;
        assert!(retired.enable()?.to_vec().is_err());
        let reopened = Envelope::from_slice(&retired.to_vec()?)?;
        let ready = reopened.enable_retired_native_launch()?;
        Envelope::check_successor(&Stored::Current(reopened.clone()), &ready)?;
        assert_eq!(ready.binding, original.binding);
        assert_eq!(ready.journal_epoch, original.journal_epoch);
        assert_ne!(
            ready.launch.as_ref().unwrap().generation,
            original.launch.as_ref().unwrap().generation
        );
        assert!(ready
            .advance_launch(Phase::Dispatched, Phase::Claimed, &original.binding)
            .is_err());
        let dispatched =
            ready.advance_launch(Phase::Ready, Phase::Dispatched, &original.binding)?;
        Envelope::check_successor(&Stored::Current(ready.clone()), &dispatched)?;
        let claimed =
            dispatched.advance_launch(Phase::Dispatched, Phase::Claimed, &original.binding)?;
        Envelope::check_successor(&Stored::Current(dispatched), &claimed)?;
        assert!(claimed
            .advance_launch(Phase::Dispatched, Phase::Claimed, &original.binding)
            .is_err());
        assert!(claimed.authorize(&token, &token.operation.scope).is_err());
        assert!(retired
            .complete_native_retirement(VerifiedExit {
                binding: original.binding.clone(),
                token
            })
            .is_err());
        Ok(())
    }
    #[test]
    fn absence_disable_or_generic_stop_completion_never_grant_native_restart() -> Result<()> {
        let original = claimed();
        assert!(original.enable_retired_native_launch().is_err());
        assert!(original
            .set_intent(Intent::Disabled)?
            .enable_retired_native_launch()
            .is_err());
        let (stopping, token) = stopping(&original);
        let completed = stopping.complete_stop(&token, &token.operation.scope)?;
        assert!(completed.enable_retired_native_launch().is_err());
        let mut forged = completed.clone();
        forged.launch = Some(Registration {
            generation: Uuid::new_v4(),
            phase: Phase::Ready,
            retired: None,
        });
        forged.intent = Intent::Enabled;
        forged.revision += 1;
        assert!(Envelope::check_successor(&Stored::Current(completed), &forged).is_err());
        let mut wrong = token.clone();
        wrong.operation.scope.generation = Uuid::new_v4();
        assert!(stopping
            .complete_native_retirement(VerifiedExit {
                binding: original.binding.clone(),
                token: wrong
            })
            .is_err());
        let mut foreign = original.binding.clone();
        foreign.device_identity = "other-device".into();
        assert!(stopping
            .complete_native_retirement(VerifiedExit {
                binding: foreign,
                token
            })
            .is_err());
        Ok(())
    }
}
