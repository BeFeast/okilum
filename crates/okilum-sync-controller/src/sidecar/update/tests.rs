use super::*;
use crate::sidecar::{
    authority::{Envelope, Reason, Stored},
    testing::{envelope, writes, Store},
    Journal,
};
use anyhow::ensure;
use std::{cell::RefCell, rc::Rc};
use uuid::Uuid;

const BUDGET: Duration = Duration::from_secs(5);

fn binding() -> Binding {
    Binding {
        instance: Uuid::from_u128(2),
        installation: Uuid::from_u128(1),
        owner: "user".into(),
        supervisor: "/private/supervisor".into(),
        state_directory: "/private/state".into(),
        device_identity: "same-device".into(),
    }
}
fn rt(version: &str) -> Runtime {
    Runtime {
        version: version.into(),
        digest: format!("digest-{version}"),
        location: format!("/private/runtime/{version}"),
    }
}
/// A store holding an enrolled instance with an update in `phase`.
fn store_at(intent: Intent, phase: Phase) -> Store {
    let store = Store::default();
    let legacy = Update {
        binding: binding(),
        previous: rt("old"),
        candidate: rt("new"),
        intent: Intent::Enabled,
        phase,
        rolled_back: false,
    };
    let journal = Journal {
        binding: binding(),
        intent,
    };
    store.0.borrow_mut().stored =
        Stored::Current(Envelope::migrate(journal, Some(legacy)).unwrap());
    store
}
fn enrolled() -> Store {
    let store = Store::default();
    store.0.borrow_mut().stored = Stored::Current(Envelope::first(binding()));
    store
}

type Hook = Box<dyn FnMut(&StopToken)>;
struct Fake {
    selected: String,
    running: bool,
    bad_health: bool,
    lost_select: bool,
    starts: usize,
    verifies: usize,
    ipc: bool,
    tokens: Vec<StopToken>,
    on_stop: Option<Hook>,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            selected: "old".into(),
            running: true,
            bad_health: false,
            lost_select: false,
            starts: 0,
            verifies: 0,
            ipc: false,
            tokens: vec![],
            on_stop: None,
        }
    }
}
impl Host for Fake {
    fn verify(&mut self, binding: &Binding, _: &Runtime) -> Result<()> {
        self.verifies += 1;
        ensure!(binding.device_identity == "same-device", "identity changed");
        Ok(())
    }
    fn supervisor_scope(&mut self, binding: &Binding) -> Result<Option<Scope>> {
        Ok((self.ipc && self.running).then(|| Scope {
            installation: binding.installation,
            instance: binding.instance,
            generation: Uuid::from_u128(7),
        }))
    }
    fn stop(&mut self, _: &Binding) -> Result<()> {
        self.running = false;
        Ok(())
    }
    fn stop_supervisor(&mut self, _: &Binding, token: &StopToken) -> Result<()> {
        self.tokens.push(token.clone());
        if let Some(hook) = &mut self.on_stop {
            hook(token);
        }
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
fn step(store: &mut Store, host: &mut Fake) -> Result<Phase> {
    advance(store, host, BUDGET)
}
fn finish(store: &mut Store, host: &mut Fake) {
    for _ in 0..8 {
        if step(store, host).unwrap() == Phase::Complete {
            return;
        }
    }
    panic!("update did not settle");
}
fn phase(store: &Store) -> Phase {
    envelope(store).update().unwrap().phase
}

#[test]
fn failed_prepare_write_has_no_runtime_effects() {
    let mut store = enrolled();
    let mut host = Fake::default();
    store.0.borrow_mut().fail = true;
    assert!(begin(&mut store, &mut host, BUDGET, rt("old"), rt("new")).is_err());
    assert!(host.running);
    assert_eq!((host.selected.as_str(), host.starts), ("old", 0));
    // Positive control: the same call persists once the write succeeds.
    store.0.borrow_mut().fail = false;
    begin(&mut store, &mut host, BUDGET, rt("old"), rt("new")).unwrap();
    assert_eq!(phase(&store), Phase::Stop);
}

#[test]
fn lost_selection_reply_replays_without_changing_identity() {
    let mut store = enrolled();
    let mut host = Fake::default();
    begin(&mut store, &mut host, BUDGET, rt("old"), rt("new")).unwrap();
    step(&mut store, &mut host).unwrap();
    host.lost_select = true;
    assert!(step(&mut store, &mut host).is_err());
    assert_eq!(phase(&store), Phase::Select);
    finish(&mut store, &mut host);
    assert_eq!(host.selected, "new");
    assert!(host.running);
    assert_eq!(envelope(&store).binding().device_identity, "same-device");
}

#[test]
fn failed_health_rolls_back_to_previous_runtime() {
    let mut store = enrolled();
    let mut host = Fake {
        bad_health: true,
        ..Fake::default()
    };
    begin(&mut store, &mut host, BUDGET, rt("old"), rt("new")).unwrap();
    finish(&mut store, &mut host);
    assert_eq!(host.selected, "old");
    assert!(host.running);
    assert!(envelope(&store).update().unwrap().rolled_back);
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
            let mut store = store_at(intent, phase);
            let mut host = Fake::default();
            finish(&mut store, &mut host);
            assert!(!host.running, "{intent:?} {phase:?}");
            assert_eq!(host.starts, 0, "{intent:?} {phase:?}");
            assert_eq!(envelope(&store).intent(), intent);
        }
    }
    // Positive control: an Enabled instance in the same phase does start.
    let mut store = store_at(Intent::Enabled, Phase::Start);
    let mut host = Fake::default();
    finish(&mut store, &mut host);
    assert!(host.running && host.starts == 1);
}

#[test]
fn lost_final_write_can_replay_rollback() {
    let mut store = store_at(Intent::Enabled, Phase::Rollback);
    let mut host = Fake::default();
    store.0.borrow_mut().fail = true;
    assert!(step(&mut store, &mut host).is_err());
    assert_eq!(phase(&store), Phase::Rollback);
    store.0.borrow_mut().fail = false;
    finish(&mut store, &mut host);
    assert_eq!(host.selected, "old");
}

#[test]
fn absent_store_is_inert_and_begin_needs_enrollment() {
    let mut store = Store::default();
    let mut host = Fake::default();
    assert_eq!(step(&mut store, &mut host).unwrap(), Phase::Complete);
    assert!(begin(&mut store, &mut host, BUDGET, rt("old"), rt("new")).is_err());
    assert_eq!(writes(&store), 0);
    assert_eq!((host.verifies, host.starts), (0, 0));
    let mut store = enrolled();
    begin(&mut store, &mut host, BUDGET, rt("old"), rt("new")).unwrap();
    assert_eq!(writes(&store), 1);
    finish(&mut store, &mut host);
    assert_eq!(host.selected, "new");
}

#[test]
fn pending_update_cannot_be_overwritten_or_reenabled_by_retry() {
    let mut store = store_at(Intent::Removed, Phase::Rollback);
    let mut host = Fake::default();
    let before = writes(&store);
    begin(&mut store, &mut host, BUDGET, rt("old"), rt("new")).unwrap();
    assert_eq!(writes(&store), before, "a retry writes nothing");
    assert_eq!(envelope(&store).intent(), Intent::Removed);
    assert_eq!(phase(&store), Phase::Rollback);
    assert!(begin(
        &mut store,
        &mut host,
        BUDGET,
        rt("old"),
        rt("another-version")
    )
    .is_err());
    assert_eq!(envelope(&store).update().unwrap().candidate.version, "new");
    // A completed update must name the committed selection, and Removed cannot update.
    let mut done = store_at(Intent::Enabled, Phase::Complete);
    assert!(begin(&mut done, &mut host, BUDGET, rt("old"), rt("next")).is_err());
    begin(&mut done, &mut host, BUDGET, rt("new"), rt("next")).unwrap();
    assert_eq!(phase(&done), Phase::Stop);
    let mut removed = store_at(Intent::Removed, Phase::Complete);
    assert!(begin(&mut removed, &mut host, BUDGET, rt("new"), rt("next")).is_err());
}

#[test]
fn update_stop_sends_a_durable_token_without_the_lock_and_consumes_it() {
    let mut store = enrolled();
    let mut host = Fake {
        ipc: true,
        ..Fake::default()
    };
    begin(&mut store, &mut host, BUDGET, rt("old"), rt("new")).unwrap();
    let seen = Rc::new(RefCell::new(vec![]));
    let (probe, log) = (store.clone(), seen.clone());
    host.on_stop = Some(Box::new(move |token| {
        let memory = probe.0.borrow();
        let stored = match &memory.stored {
            Stored::Current(e) => e.stop().cloned(),
            _ => None,
        };
        log.borrow_mut()
            .push((memory.locked, stored == Some(token.operation.clone())));
    }));
    assert_eq!(step(&mut store, &mut host).unwrap(), Phase::Select);
    assert_eq!(*seen.borrow(), [(false, true)]);
    let id = envelope(&store).update().unwrap().update_id;
    assert_eq!(host.tokens[0].operation.reason, Reason::UpdateStop(id));
    let now = envelope(&store);
    assert!(now.stop().is_none(), "operation consumed in a new revision");
    assert!(now
        .authorize(&host.tokens[0], &host.tokens[0].operation.scope)
        .is_err());
    host.on_stop = None;
    finish(&mut store, &mut host);
    assert_eq!(host.selected, "new");
}

#[test]
fn disable_during_update_stop_stops_a_stale_updater_from_selecting_or_starting() {
    let mut store = enrolled();
    let mut host = Fake {
        ipc: true,
        ..Fake::default()
    };
    begin(&mut store, &mut host, BUDGET, rt("old"), rt("new")).unwrap();
    let hook_store = store.clone();
    host.on_stop = Some(Box::new(move |token| {
        let mut authority = hook_store.clone();
        let mut tx = authority.begin(Instant::now() + BUDGET).unwrap();
        let current = current_envelope(&mut tx).unwrap().unwrap();
        let (next, _) = current.disable(token.operation.scope.clone()).unwrap();
        tx.commit(next).unwrap();
    }));
    assert!(step(&mut store, &mut host).is_err());
    assert_eq!((host.selected.as_str(), host.starts), ("old", 0));
    assert_eq!(phase(&store), Phase::Stop);
    assert_eq!(envelope(&store).intent(), Intent::Disabled);
    host.on_stop = None;
    host.ipc = false;
    finish(&mut store, &mut host);
    assert_eq!((host.selected.as_str(), host.starts), ("old", 0));
    assert!(!host.running);
    assert_eq!(envelope(&store).intent(), Intent::Disabled);
}

#[test]
fn rollback_with_a_live_supervisor_uses_the_rollback_reason_and_obeys_intent() {
    for (intent, restarts) in [(Intent::Enabled, 1), (Intent::Disabled, 0)] {
        let mut store = store_at(intent, Phase::Rollback);
        let mut host = Fake {
            ipc: true,
            selected: "new".into(),
            ..Fake::default()
        };
        assert_eq!(step(&mut store, &mut host).unwrap(), Phase::Complete);
        let id = envelope(&store).update().unwrap().update_id;
        assert_eq!(host.tokens[0].operation.reason, Reason::UpdateRollback(id));
        assert_eq!(host.selected, "old");
        assert_eq!(host.starts, restarts);
        assert!(envelope(&store).update().unwrap().rolled_back);
        assert!(envelope(&store).stop().is_none());
    }
}

#[test]
fn stored_update_stop_is_retried_with_the_same_token() {
    let mut store = enrolled();
    let mut host = Fake {
        ipc: true,
        ..Fake::default()
    };
    begin(&mut store, &mut host, BUDGET, rt("old"), rt("new")).unwrap();
    // Lose the committed completion: the IPC stop worked but the relock failed.
    let hook_store = store.clone();
    host.on_stop = Some(Box::new(move |_| hook_store.0.borrow_mut().fail = true));
    assert!(step(&mut store, &mut host).is_err());
    assert_eq!(host.tokens.len(), 1, "the IPC stop was reached");
    assert!(
        envelope(&store).stop().is_some(),
        "the operation stays durable"
    );
    store.0.borrow_mut().fail = false;
    host.running = true; // the supervisor came back with the same generation
    host.on_stop = None;
    assert_eq!(step(&mut store, &mut host).unwrap(), Phase::Select);
    assert_eq!(host.tokens.len(), 2);
    assert_eq!(host.tokens[0], host.tokens[1]);
}
