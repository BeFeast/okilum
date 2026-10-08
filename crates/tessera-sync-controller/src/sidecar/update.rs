//! Bounded, replayable runtime selection. Native updater hooks supply an already
//! staged runtime; no download, identity generation or vault writes happen here.
use super::{Binding, Intent};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    pub version: String,
    pub digest: String,
    pub location: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Stop,
    Select,
    Start,
    Check,
    Rollback,
    Complete,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    pub binding: Binding,
    pub previous: Runtime,
    pub candidate: Runtime,
    pub intent: Intent,
    pub phase: Phase,
    pub rolled_back: bool,
}
/// Shares the lifecycle's exclusive lock. Disable/Remove must update this intent
/// under that same lock before any updater step; Remove is terminal.
pub trait Store {
    fn load(&self) -> Result<Option<Update>>;
    fn save(&mut self, update: &Update) -> Result<()>;
}
pub trait Host {
    /// Signature/hash/version and original device identity; no effects.
    fn verify(&mut self, binding: &Binding, runtime: &Runtime) -> Result<()>;
    /// Stop and reap the instance regardless of which runtime was last selected.
    fn stop(&mut self, binding: &Binding) -> Result<()>;
    /// Atomic durable selection. May be replayed after a lost successful reply.
    fn select(&mut self, binding: &Binding, runtime: &Runtime) -> Result<()>;
    /// Idempotent start, using the verified selection and existing identity.
    fn start(&mut self, binding: &Binding, runtime: &Runtime) -> Result<()>;
    /// Confirm actual REST version AND device identity, not merely process exit.
    fn healthy(&mut self, binding: &Binding, runtime: &Runtime) -> Result<()>;
}
pub fn begin<S: Store, H: Host>(store: &mut S, host: &mut H, mut update: Update) -> Result<()> {
    if let Some(saved) = store.load()? {
        if saved.phase != Phase::Complete {
            ensure!(
                saved.binding == update.binding
                    && saved.previous == update.previous
                    && saved.candidate == update.candidate,
                "another update is pending"
            );
            return Ok(());
        }
        ensure!(saved.binding == update.binding, "update binding changed");
        let selected = if saved.rolled_back {
            &saved.previous
        } else {
            &saved.candidate
        };
        ensure!(
            *selected == update.previous,
            "previous runtime differs from committed selection"
        );
        update.intent = saved.intent;
    }
    ensure!(update.previous != update.candidate, "runtime is unchanged");
    ensure!(
        update.intent != Intent::Removed,
        "removed instance cannot update"
    );
    update.phase = Phase::Stop;
    update.rolled_back = false;
    host.verify(&update.binding, &update.previous)?;
    host.verify(&update.binding, &update.candidate)?;
    store.save(&update)
}
/// Perform one bounded phase. Host calls enforce hook deadlines; failure leaves
/// a durable phase to retry at next startup. Never loop or retry within a hook.
pub fn advance<S: Store, H: Host>(store: &mut S, host: &mut H) -> Result<Phase> {
    let Some(mut update) = store.load()? else {
        return Ok(Phase::Complete);
    };
    if update.phase == Phase::Complete {
        return Ok(Phase::Complete);
    }
    if update.intent != Intent::Enabled && update.phase != Phase::Rollback {
        update.phase = Phase::Rollback;
        store.save(&update)?;
        return Ok(Phase::Rollback);
    }
    let binding = &update.binding;
    match update.phase {
        Phase::Stop => {
            host.stop(binding)?;
            update.phase = Phase::Select;
        }
        Phase::Select => {
            if host.verify(binding, &update.candidate).is_err() {
                update.phase = Phase::Rollback;
            } else {
                host.select(binding, &update.candidate)?;
                update.phase = Phase::Start;
            }
        }
        Phase::Start => {
            if host
                .verify(binding, &update.candidate)
                .and_then(|()| host.start(binding, &update.candidate))
                .is_err()
            {
                update.phase = Phase::Rollback;
            } else {
                update.phase = Phase::Check;
            }
        }
        Phase::Check => {
            update.phase = if host.healthy(binding, &update.candidate).is_ok() {
                Phase::Complete
            } else {
                Phase::Rollback
            };
        }
        Phase::Rollback => {
            host.stop(binding)?;
            host.verify(binding, &update.previous)?;
            host.select(binding, &update.previous)?;
            if update.intent == Intent::Enabled {
                host.start(binding, &update.previous)?;
                host.healthy(binding, &update.previous)?;
            }
            update.rolled_back = true;
            update.phase = Phase::Complete;
        }
        Phase::Complete => unreachable!(),
    }
    store.save(&update)?;
    Ok(update.phase)
}
#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    struct Memory {
        value: Update,
        fail: bool,
        absent: bool,
    }
    impl Store for Memory {
        fn load(&self) -> Result<Option<Update>> {
            Ok((!self.absent).then(|| self.value.clone()))
        }
        fn save(&mut self, value: &Update) -> Result<()> {
            ensure!(!self.fail, "disk failure");
            self.value = value.clone();
            self.absent = false;
            Ok(())
        }
    }
    #[derive(Default)]
    struct Fake {
        selected: String,
        running: bool,
        bad_health: bool,
        lost_select: bool,
        starts: usize,
    }
    impl Host for Fake {
        fn verify(&mut self, binding: &Binding, _: &Runtime) -> Result<()> {
            ensure!(binding.device_identity == "same-device", "identity changed");
            Ok(())
        }
        fn stop(&mut self, _: &Binding) -> Result<()> {
            self.running = false;
            Ok(())
        }
        fn select(&mut self, _: &Binding, rt: &Runtime) -> Result<()> {
            self.selected = rt.version.clone();
            if std::mem::take(&mut self.lost_select) {
                anyhow::bail!("lost reply");
            }
            Ok(())
        }
        fn start(&mut self, _: &Binding, rt: &Runtime) -> Result<()> {
            ensure!(self.selected == rt.version, "wrong selection");
            self.running = true;
            self.starts += 1;
            Ok(())
        }
        fn healthy(&mut self, _: &Binding, rt: &Runtime) -> Result<()> {
            ensure!(
                !(self.bad_health && rt.version == "new"),
                "wrong REST identity/version"
            );
            Ok(())
        }
    }
    fn fixture() -> (Memory, Fake) {
        let rt = |version: &str| Runtime {
            version: version.into(),
            digest: format!("digest-{version}"),
            location: format!("/private/runtime/{version}"),
        };
        (
            Memory {
                fail: false,
                absent: false,
                value: Update {
                    binding: Binding {
                        instance: Uuid::new_v4(),
                        installation: Uuid::new_v4(),
                        owner: "user".into(),
                        supervisor: "/private/supervisor".into(),
                        state_directory: "/private/state".into(),
                        device_identity: "same-device".into(),
                    },
                    previous: rt("old"),
                    candidate: rt("new"),
                    intent: Intent::Enabled,
                    phase: Phase::Stop,
                    rolled_back: false,
                },
            },
            Fake {
                selected: "old".into(),
                running: true,
                ..Default::default()
            },
        )
    }
    fn finish(store: &mut Memory, host: &mut Fake) {
        for _ in 0..8 {
            if advance(store, host).unwrap() == Phase::Complete {
                return;
            }
        }
        panic!("update did not settle");
    }
    #[test]
    fn failed_prepare_write_has_no_runtime_effects() {
        let (mut store, mut host) = fixture();
        store.fail = true;
        store.absent = true;
        let update = store.value.clone();
        assert!(begin(&mut store, &mut host, update).is_err());
        assert!(host.running);
        assert_eq!(host.selected, "old");
        assert_eq!(host.starts, 0);
    }
    #[test]
    fn lost_selection_reply_replays_without_changing_identity() {
        let (mut store, mut host) = fixture();
        advance(&mut store, &mut host).unwrap();
        host.lost_select = true;
        assert!(advance(&mut store, &mut host).is_err());
        assert_eq!(store.value.phase, Phase::Select);
        finish(&mut store, &mut host);
        assert_eq!(host.selected, "new");
        assert!(host.running);
        assert_eq!(store.value.binding.device_identity, "same-device");
    }
    #[test]
    fn failed_health_rolls_back_to_previous_runtime() {
        let (mut store, mut host) = fixture();
        host.bad_health = true;
        finish(&mut store, &mut host);
        assert_eq!(host.selected, "old");
        assert!(host.running);
        assert!(store.value.rolled_back);
    }
    #[test]
    fn disable_or_remove_at_any_phase_cannot_restart() {
        for intent in [Intent::Disabled, Intent::Removed] {
            for phase in [
                Phase::Stop,
                Phase::Select,
                Phase::Start,
                Phase::Check,
                Phase::Rollback,
            ] {
                let (mut store, mut host) = fixture();
                store.value.intent = intent;
                store.value.phase = phase;
                finish(&mut store, &mut host);
                assert!(!host.running);
                assert_eq!(host.starts, 0);
            }
        }
    }
    #[test]
    fn lost_final_write_can_replay_rollback() {
        let (mut store, mut host) = fixture();
        store.value.phase = Phase::Rollback;
        store.fail = true;
        assert!(advance(&mut store, &mut host).is_err());
        assert_eq!(store.value.phase, Phase::Rollback);
        store.fail = false;
        finish(&mut store, &mut host);
        assert_eq!(host.selected, "old");
    }
    #[test]
    fn empty_store_is_inert_but_explicit_begin_is_persisted() {
        let (mut store, mut host) = fixture();
        store.absent = true;
        assert_eq!(advance(&mut store, &mut host).unwrap(), Phase::Complete);
        assert!(store.absent);
        assert_eq!(host.starts, 0);
        let update = store.value.clone();
        begin(&mut store, &mut host, update).unwrap();
        assert!(!store.absent);
        finish(&mut store, &mut host);
        assert_eq!(host.selected, "new");
    }
    #[test]
    fn pending_update_cannot_be_overwritten_or_reenabled_by_retry() {
        let (mut store, mut host) = fixture();
        store.value.phase = Phase::Rollback;
        store.value.intent = Intent::Removed;
        let mut retry = store.value.clone();
        retry.intent = Intent::Enabled;
        begin(&mut store, &mut host, retry.clone()).unwrap();
        assert_eq!(store.value.intent, Intent::Removed);
        assert_eq!(store.value.phase, Phase::Rollback);
        retry.candidate.version = "another-version".into();
        assert!(begin(&mut store, &mut host, retry).is_err());
        assert_eq!(store.value.candidate.version, "new");
    }
}
