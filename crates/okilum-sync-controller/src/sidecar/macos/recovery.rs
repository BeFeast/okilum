//! Conservative presentation of managed macOS liveness. This is deliberately
//! not an absence detector or a capability to launch/recover anything.
use crate::sidecar::{
    authority::{launch::Phase, Envelope},
    discovery::Discovered,
    Intent,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedStatus {
    Running,
    RecoveryPending,
}
impl ManagedStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::RecoveryPending => "Recovery pending",
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::Running => "The verified Sync helper is responding. Sync setup remains unavailable here.",
            Self::RecoveryPending => "Sync needs recovery before it can start safely. No additional Sync process will be started. Sync setup is not available yet.",
        }
    }
    pub(crate) fn observe(saved: &Envelope, peer: &Discovered) -> Self {
        let Some(registration) = saved.launch_registration() else {
            return Self::RecoveryPending;
        };
        match peer {
            Discovered::Live(scope)
                if saved.intent() == Intent::Enabled
                    && saved.stop().is_none()
                    && saved.update().is_none()
                    && registration.phase() == Phase::Claimed
                    && scope.installation == saved.binding().installation
                    && scope.instance == saved.binding().instance
                    && scope.generation == registration.generation() =>
            {
                Self::Running
            }
            _ => Self::RecoveryPending,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidecar::{supervisor::ipc::Scope, Binding};
    use uuid::Uuid;
    fn saved(phase: &str) -> Envelope {
        let binding = Binding {
            installation: Uuid::from_u128(1),
            instance: Uuid::from_u128(2),
            owner: "501".into(),
            supervisor: "/app/helper".into(),
            state_directory: "/state".into(),
            device_identity: "fixture".into(),
        };
        let mut json = serde_json::to_value(Envelope::first_with_launch(binding)).unwrap();
        json["launch"]["phase"] = phase.into();
        Envelope::from_slice(&serde_json::to_vec(&json).unwrap()).unwrap()
    }
    fn live(saved: &Envelope) -> Discovered {
        Discovered::Live(Scope {
            installation: saved.binding().installation,
            instance: saved.binding().instance,
            generation: saved.launch_registration().unwrap().generation(),
        })
    }
    #[test]
    fn only_claimed_matching_live_generation_is_running() {
        let claimed = saved("Claimed");
        assert_eq!(
            ManagedStatus::observe(&claimed, &live(&claimed)),
            ManagedStatus::Running
        );
        for phase in ["Ready", "Dispatched"] {
            let saved = saved(phase);
            assert_eq!(
                ManagedStatus::observe(&saved, &live(&saved)),
                ManagedStatus::RecoveryPending
            );
        }
        for peer in [
            Discovered::Absent,
            Discovered::Unreachable("wrong signature or no answer".into()),
            live(&saved("Claimed")),
        ] {
            assert_eq!(
                ManagedStatus::observe(&claimed, &peer),
                ManagedStatus::RecoveryPending
            );
        }
        let legacy = Envelope::first(claimed.binding().clone());
        assert_eq!(
            ManagedStatus::observe(&legacy, &live(&claimed)),
            ManagedStatus::RecoveryPending
        );
        for intent in [Intent::Disabled, Intent::Removed] {
            assert_eq!(
                ManagedStatus::observe(&claimed.set_intent(intent).unwrap(), &live(&claimed)),
                ManagedStatus::RecoveryPending
            );
        }
        let Discovered::Live(scope) = live(&claimed) else {
            panic!()
        };
        let (stopping, _) = claimed.disable(scope).unwrap();
        assert_eq!(
            ManagedStatus::observe(&stopping, &live(&claimed)),
            ManagedStatus::RecoveryPending
        );
    }
}
