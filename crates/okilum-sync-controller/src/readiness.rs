//! Short-lived scoped observations; none of these APIs mutate folder mode.
use crate::pairing::{Descriptor, Registration, State};
use anyhow::{ensure, Result};
use serde::Deserialize;
use serde_json::Value;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HubStatus {
    pub state: String,
    pub sequence: u64,
    pub global_total_items: Option<u64>,
    pub local_total_items: Option<u64>,
    pub global_bytes: Option<u64>,
    pub global_directories: Option<u64>,
    pub global_symlinks: Option<u64>,
    pub local_bytes: Option<u64>,
    pub need_total_items: Option<u64>,
    pub receive_only_total_items: Option<u64>,
    pub errors: Option<u64>,
    pub pull_errors: Option<u64>,
    pub error: Option<String>,
    pub invalid: Option<String>,
    pub watch_error: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Observation {
    pub protocol: u32,
    pub observation_id: Uuid,
    pub adapter_generation: Uuid,
    pub hub_started_at: String,
    pub expires_at: u64,
    pub owner_id: Uuid,
    pub vault_id: Uuid,
    pub registration_id: Uuid,
    pub device_id: String,
    pub hub_device_id: String,
    pub folder_id: String,
    pub paused: bool,
    pub connected: bool,
    pub remote_state: String,
    pub hub: HubStatus,
}
/// Intentionally not serializable: reopening the controller requires fresh
/// observations rather than extending authority through wall-clock rollback.
pub struct Receipt {
    observation: Observation,
    received: Instant,
    deadline: Instant,
}
impl Receipt {
    /// Validate a decoded observation from the authenticated pairing service.
    /// This creates no authority to mutate Syncthing.
    pub fn new(
        observation: Observation,
        registration: &Registration,
        descriptor: &Descriptor,
    ) -> Result<Self> {
        let now = unix_now()?;
        observation.validate(registration, descriptor, now)?;
        let received = Instant::now();
        let deadline = received + Duration::from_secs(observation.expires_at - now);
        Ok(Self {
            observation,
            received,
            deadline,
        })
    }
    pub fn observation(&self) -> &Observation {
        &self.observation
    }
    fn fresh(&self, now: u64, instant: Instant) -> bool {
        instant >= self.received
            && instant < self.deadline
            && self.observation.expires_at > now
            && self.observation.expires_at <= now.saturating_add(30)
    }
}
impl Observation {
    fn validate(
        &self,
        registration: &Registration,
        descriptor: &Descriptor,
        now: u64,
    ) -> Result<()> {
        ensure!(
            registration.state == State::HubReady
                && self.protocol == 1
                && !self.owner_id.is_nil()
                && !self.observation_id.is_nil()
                && !self.adapter_generation.is_nil()
                && !self.hub_started_at.is_empty()
                && self.hub_started_at.len() <= 128
                && self.registration_id == registration.id
                && self.vault_id == registration.vault
                && self.device_id == registration.device_id
                && self.hub_device_id == descriptor.hub_device_id
                && self.folder_id == descriptor.folder_id,
            "readiness scope mismatch"
        );
        ensure!(
            self.expires_at > now && self.expires_at <= now.saturating_add(30),
            "readiness expired or clock differs"
        );
        Ok(())
    }
    fn settled(&self) -> bool {
        let h = &self.hub;
        !self.paused
            && self.connected
            && self.remote_state == "valid"
            && h.state == "idle"
            && h.need_total_items == Some(0)
            && h.receive_only_total_items == Some(0)
            && h.errors == Some(0)
            && h.pull_errors == Some(0)
            && h.error.as_deref() == Some("")
            && h.invalid.as_deref() == Some("")
            && h.watch_error.as_deref() == Some("")
            && h.global_total_items.is_some()
            && h.global_total_items == h.local_total_items
            && h.global_bytes.is_some()
            && h.global_bytes == h.local_bytes
    }
}
/// The caller must scan the authenticated client between the two observations.
/// A true result is evidence for this interval, not atomic protection against
/// later external writes; the folder controller must recheck before mutation.
pub fn interval_complete(first: &Receipt, second: &Receipt, client: &Value) -> Result<bool> {
    let now = unix_now()?;
    let instant = Instant::now();
    let a = &first.observation;
    let b = &second.observation;
    if !first.fresh(now, instant)
        || !second.fresh(now, instant)
        || !a.settled()
        || !b.settled()
        || a.observation_id == b.observation_id
        || first.received > second.received
        || a.owner_id != b.owner_id
        || a.vault_id != b.vault_id
        || a.registration_id != b.registration_id
        || a.device_id != b.device_id
        || a.hub_device_id != b.hub_device_id
        || a.folder_id != b.folder_id
        || a.adapter_generation != b.adapter_generation
        || a.hub_started_at != b.hub_started_at
        || a.hub != b.hub
    {
        return Ok(false);
    }
    let sequence = client["remoteSequence"][&b.hub_device_id].as_u64();
    let received = sequence.is_some_and(|s| s >= b.hub.sequence)
        || b.hub.sequence == 0 && b.hub.global_total_items == Some(0) && sequence.is_none();
    // Pinned 1.29.5 includes 128 synthetic bytes per directory/symlink;
    // 2.1.6 counts file bytes only. Underflow/unknown counters fail closed.
    let hub_file_bytes = b
        .hub
        .global_directories
        .zip(b.hub.global_symlinks)
        .and_then(|(dirs, links)| dirs.checked_add(links))
        .and_then(|n| n.checked_mul(128))
        .and_then(|synthetic| b.hub.global_bytes?.checked_sub(synthetic));
    Ok(received
        && hub_file_bytes.is_some()
        && client["state"] == "idle"
        && [
            "needTotalItems",
            "receiveOnlyTotalItems",
            "errors",
            "pullErrors",
        ]
        .iter()
        .all(|k| client[*k].as_u64() == Some(0))
        && ["error", "invalid", "watchError"]
            .iter()
            .all(|k| client[*k].as_str() == Some(""))
        && client["localTotalItems"].as_u64() == b.hub.global_total_items
        && client["globalTotalItems"].as_u64() == b.hub.global_total_items
        && client["localBytes"].as_u64() == hub_file_bytes
        && client["globalBytes"].as_u64() == hub_file_bytes)
}
fn unix_now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

/// Authenticated observation provider. Kept injectable for isolated daemon tests.
pub trait Source {
    fn observe(&self) -> Result<Receipt>;
}
impl<F: Fn() -> Result<Receipt>> Source for F {
    fn observe(&self) -> Result<Receipt> {
        self()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> (Registration, Descriptor, Observation, Value) {
        let registration = Registration {
            id: Uuid::new_v4(),
            vault: Uuid::new_v4(),
            device_id: "client".into(),
            name: "fixture".into(),
            state: State::HubReady,
            last_error: None,
        };
        let descriptor = Descriptor {
            folder_id: "fixture".into(),
            hub_device_id: "hub".into(),
            hub_address: "tcp://127.0.0.1:22440".into(),
            ignores: vec![],
        };
        let counters = json!({"state":"idle","sequence":0,"globalTotalItems":0,"localTotalItems":0,"globalBytes":0,"globalDirectories":0,"globalSymlinks":0,"localBytes":0,"needTotalItems":0,"receiveOnlyTotalItems":0,"errors":0,"pullErrors":0,"error":"","invalid":"","watchError":""});
        let observation = Observation {
            protocol: 1,
            observation_id: Uuid::new_v4(),
            adapter_generation: Uuid::new_v4(),
            hub_started_at: "fixture-start".into(),
            expires_at: unix_now().unwrap() + 30,
            owner_id: Uuid::new_v4(),
            vault_id: registration.vault,
            registration_id: registration.id,
            device_id: registration.device_id.clone(),
            hub_device_id: descriptor.hub_device_id.clone(),
            folder_id: descriptor.folder_id.clone(),
            paused: false,
            connected: true,
            remote_state: "valid".into(),
            hub: serde_json::from_value(counters.clone()).unwrap(),
        };
        (registration, descriptor, observation, counters)
    }
    #[test]
    fn validates_scope_expiry_and_revocation() -> Result<()> {
        let (mut registration, descriptor, observation, _) = fixture();
        Receipt::new(observation.clone(), &registration, &descriptor)?;
        let mut wrong = observation.clone();
        wrong.vault_id = Uuid::new_v4();
        assert!(Receipt::new(wrong, &registration, &descriptor).is_err());
        let mut expired = observation.clone();
        expired.expires_at = unix_now()?;
        assert!(Receipt::new(expired, &registration, &descriptor).is_err());
        let mut future = observation.clone();
        future.expires_at = unix_now()? + 60;
        assert!(Receipt::new(future, &registration, &descriptor).is_err());
        registration.state = State::RemovalPending;
        assert!(Receipt::new(observation, &registration, &descriptor).is_err());
        Ok(())
    }
    #[test]
    fn empty_positive_control_and_invalidation() -> Result<()> {
        let (registration, descriptor, observation, client) = fixture();
        let first = Receipt::new(observation.clone(), &registration, &descriptor)?;
        let mut next = observation.clone();
        next.observation_id = Uuid::new_v4();
        let second = Receipt::new(next.clone(), &registration, &descriptor)?;
        assert!(interval_complete(&first, &second, &client)?);
        assert!(!interval_complete(&first, &first, &client)?);
        for kind in [
            "adapter",
            "daemon",
            "owner",
            "paused",
            "disconnected",
            "unknown",
            "index",
            "missing",
        ] {
            let mut changed = next.clone();
            match kind {
                "adapter" => changed.adapter_generation = Uuid::new_v4(),
                "daemon" => changed.hub_started_at = "restarted".into(),
                "owner" => changed.owner_id = Uuid::new_v4(),
                "paused" => changed.paused = true,
                "disconnected" => changed.connected = false,
                "unknown" => changed.remote_state = "unknown".into(),
                "index" => changed.hub.sequence += 1,
                _ => changed.hub.errors = None,
            }
            let second = Receipt::new(changed, &registration, &descriptor)?;
            assert!(
                !interval_complete(&first, &second, &client)?,
                "accepted {kind}"
            );
        }
        let mut stale = Receipt::new(next, &registration, &descriptor)?;
        stale.deadline = Instant::now() - Duration::from_secs(1);
        assert!(!interval_complete(&first, &stale, &client)?);
        Ok(())
    }
    #[test]
    fn pinned_versions_normalize_directory_and_symlink_bytes() -> Result<()> {
        let (registration, descriptor, mut observation, mut client) = fixture();
        observation.hub.sequence = 2;
        observation.hub.global_total_items = Some(2);
        observation.hub.local_total_items = Some(2);
        observation.hub.global_directories = Some(1);
        observation.hub.global_symlinks = Some(1);
        observation.hub.global_bytes = Some(256);
        observation.hub.local_bytes = Some(256);
        client["remoteSequence"] = json!({"hub":2});
        client["localTotalItems"] = json!(2);
        client["globalTotalItems"] = json!(2);
        let first = Receipt::new(observation.clone(), &registration, &descriptor)?;
        observation.observation_id = Uuid::new_v4();
        let second = Receipt::new(observation, &registration, &descriptor)?;
        assert!(interval_complete(&first, &second, &client)?);
        client["localBytes"] = json!(128);
        assert!(!interval_complete(&first, &second, &client)?);
        Ok(())
    }
    #[test]
    fn index_receipt_does_not_mean_file_receipt_or_no_local_edits() -> Result<()> {
        let (registration, descriptor, mut observation, mut client) = fixture();
        observation.hub.sequence = 12001;
        observation.hub.global_total_items = Some(12001);
        observation.hub.local_total_items = Some(12001);
        client["remoteSequence"] = json!({"hub":12001});
        client["localTotalItems"] = json!(12001);
        client["globalTotalItems"] = json!(12001);
        let first = Receipt::new(observation.clone(), &registration, &descriptor)?;
        observation.observation_id = Uuid::new_v4();
        let second = Receipt::new(observation, &registration, &descriptor)?;
        assert!(interval_complete(&first, &second, &client)?);
        client["needTotalItems"] = json!(1);
        assert!(!interval_complete(&first, &second, &client)?);
        client["needTotalItems"] = json!(0);
        client["receiveOnlyTotalItems"] = json!(1);
        assert!(!interval_complete(&first, &second, &client)?);
        client["receiveOnlyTotalItems"] = json!(0);
        client["remoteSequence"] = json!({"hub":9000});
        assert!(!interval_complete(&first, &second, &client)?);
        Ok(())
    }
}
