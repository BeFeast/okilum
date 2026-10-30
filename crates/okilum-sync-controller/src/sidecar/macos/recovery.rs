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
/// Safe recovery guidance. This describes uncertainty; it grants no permission
/// to launch, delete state, unregister a service or regenerate device identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryDetail {
    StateUnavailable,
    GenerationMissing,
    Prepared,
    LaunchUnconfirmed,
    PeerUnavailable,
    LifecyclePending,
    Running,
}
impl RecoveryDetail {
    pub fn observe(saved: &Envelope, observed: ManagedStatus) -> Self {
        if saved.intent() != Intent::Enabled || saved.stop().is_some() || saved.update().is_some() {
            return Self::LifecyclePending;
        }
        let Some(launch) = saved.launch_registration() else {
            return Self::GenerationMissing;
        };
        match launch.phase() {
            Phase::Ready => Self::Prepared,
            Phase::Dispatched => Self::LaunchUnconfirmed,
            Phase::Claimed if observed == ManagedStatus::Running => Self::Running,
            Phase::Claimed => Self::PeerUnavailable,
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::StateUnavailable => "Sync state could not be verified. Keep the existing Sync data and configuration. Restore access to the original state, then check again.",
            Self::GenerationMissing => "The saved Sync state has no verified launch generation. Keep it for recovery; starting a new setup could duplicate an existing process.",
            Self::Prepared => "Sync preparation is saved, but launch has not been confirmed. Enable will be available after the verified runtime is installed. Your preparation is retained.",
            Self::LaunchUnconfirmed => "A Sync launch was requested, but no verified running generation is confirmed. Check again after the helper responds; do not create another setup or delete Sync state.",
            Self::PeerUnavailable => "The previously launched Sync helper is not responding with a verified identity. Check again after access is restored. A second Sync process will not be started.",
            Self::LifecyclePending => "Sync has a pending stop, update or disabled state. Keep its data and configuration until that operation is safely reconciled.",
            Self::Running => "The verified Sync helper is responding. Your existing Sync identity is retained.",
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
    #[test]
    fn guidance_distinguishes_uncertainty_without_claiming_a_second_launch_is_safe() {
        assert_eq!(
            RecoveryDetail::observe(&saved("Ready"), ManagedStatus::RecoveryPending),
            RecoveryDetail::Prepared
        );
        assert_eq!(
            RecoveryDetail::observe(&saved("Dispatched"), ManagedStatus::RecoveryPending),
            RecoveryDetail::LaunchUnconfirmed
        );
        assert_eq!(
            RecoveryDetail::observe(&saved("Claimed"), ManagedStatus::RecoveryPending),
            RecoveryDetail::PeerUnavailable
        );
        assert_eq!(
            RecoveryDetail::observe(&saved("Claimed"), ManagedStatus::Running),
            RecoveryDetail::Running
        );
        // A prior Running presentation cannot override current durable authority.
        for phase in ["Ready", "Dispatched"] {
            assert_ne!(
                RecoveryDetail::observe(&saved(phase), ManagedStatus::Running),
                RecoveryDetail::Running
            );
        }
        let legacy = Envelope::first(saved("Ready").binding().clone());
        assert_eq!(
            RecoveryDetail::observe(&legacy, ManagedStatus::Running),
            RecoveryDetail::GenerationMissing
        );
        let disabled = saved("Claimed").set_intent(Intent::Disabled).unwrap();
        assert_eq!(
            RecoveryDetail::observe(&disabled, ManagedStatus::Running),
            RecoveryDetail::LifecyclePending
        );
        assert!(!RecoveryDetail::StateUnavailable.description().contains('/'));
    }
}
