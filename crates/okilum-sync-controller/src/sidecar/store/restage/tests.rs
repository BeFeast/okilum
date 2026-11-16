use super::*;
use crate::sidecar::{authority::Stored, supervisor::ipc::Scope};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc, sync::Arc, time::Duration};
use uuid::Uuid;

#[derive(Default)]
struct Memory {
    files: BTreeMap<String, Vec<u8>>,
    locked: bool,
    events: Vec<String>,
    fail: Option<(usize, bool)>,
    task: Option<Binding>,
    running: bool,
    selected: String,
    ready: bool,
    cleaned: bool,
    cancel_on_stop: bool,
}
impl Memory {
    fn before(&mut self, name: &str) -> Result<()> {
        self.events.push(name.into());
        ensure!(
            self.fail != Some((self.events.len(), false)),
            "crash before {name}"
        );
        Ok(())
    }
    fn after(&self) -> Result<()> {
        ensure!(
            self.fail != Some((self.events.len(), true)),
            "crash after effect"
        );
        Ok(())
    }
}
#[derive(Clone)]
struct Dir(Rc<RefCell<Memory>>);
struct Lock(Rc<RefCell<Memory>>);
impl Drop for Lock {
    fn drop(&mut self) {
        self.0.borrow_mut().locked = false;
    }
}
impl StateDir for Dir {
    type Lock = Lock;
    fn lock(self: &Arc<Self>, _: Instant) -> Result<Lock> {
        let mut m = self.0.borrow_mut();
        ensure!(!m.locked, "nested state lock");
        m.locked = true;
        Ok(Lock(self.0.clone()))
    }
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.0.borrow().files.get(name).cloned())
    }
    fn write(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let mut m = self.0.borrow_mut();
        m.before(&format!("write {name}"))?;
        m.files.insert(name.into(), bytes.into());
        m.after()
    }
    fn remove(&self, name: &str) -> Result<()> {
        let mut m = self.0.borrow_mut();
        m.before(&format!("remove {name}"))?;
        m.files.remove(name);
        m.after()
    }
}
struct Fake(Rc<RefCell<Memory>>);
fn scope(binding: &Binding) -> Scope {
    Scope {
        installation: binding.installation,
        instance: binding.instance,
        generation: Uuid::from_u128(3),
    }
}
impl Platform for Fake {
    fn inspect(&mut self, binding: &Binding) -> Result<Registration> {
        let m = self.0.borrow();
        ensure!(
            m.task.as_ref().is_none_or(|b| b == binding),
            "foreign task definition"
        );
        Ok(if m.running {
            Registration::Running
        } else if m.task.is_some() {
            Registration::Stopped
        } else {
            Registration::Absent
        })
    }
    fn verify_payload(&mut self, _: &Binding) -> Result<()> {
        Ok(())
    }
    fn register(&mut self, b: &Binding) -> Result<()> {
        let mut m = self.0.borrow_mut();
        m.before("register")?;
        ensure!(m.task.is_none(), "task collision");
        m.task = Some(b.clone());
        m.after()
    }
    fn start(&mut self, _: &Binding) -> Result<()> {
        let mut m = self.0.borrow_mut();
        m.before("start")?;
        m.running = true;
        m.after()
    }
    fn stop(&mut self, _: &Binding) -> Result<()> {
        panic!("no scheduler or PID kill")
    }
    fn supervisor_scope(&mut self, b: &Binding) -> Result<Option<Scope>> {
        Ok(self.0.borrow().running.then(|| scope(b)))
    }
    fn stop_supervisor(&mut self, b: &Binding, token: &StopToken) -> Result<()> {
        let mut m = self.0.borrow_mut();
        ensure!(!m.locked, "Stop must release the state lock");
        let saved = Envelope::from_slice(&m.files[super::super::NAME])?;
        saved.authorize(token, &scope(b))?;
        m.before("authenticated Stop and owned-tree exit")?;
        m.running = false;
        if m.cancel_on_stop {
            // disable_current_app: cancel the marker, then commit its Disable.
            // With the helper gone, that Disable equals the planned `stopped`.
            let mut plan: serde_json::Value = serde_json::from_slice(&m.files[MARKER])?;
            plan["cancelled"] = true.into();
            m.files.insert(MARKER.into(), serde_json::to_vec(&plan)?);
            let next = saved.set_intent(Intent::Disabled)?;
            m.files.insert(super::super::NAME.into(), next.to_vec()?);
        }
        m.after()
    }
    fn unregister(&mut self, _: &Binding) -> Result<()> {
        let mut m = self.0.borrow_mut();
        if m.task.is_none() {
            return Ok(());
        }
        ensure!(!m.running, "cannot unregister a live helper");
        m.before("unregister")?;
        m.task = None;
        m.after()
    }
}
impl Host for Fake {
    type Native = Fake;
    fn platform(&mut self, _: &Binding) -> Result<Fake> {
        Ok(Fake(self.0.clone()))
    }
    fn select_helper(&mut self, b: &Binding, _: &str) -> Result<()> {
        let mut m = self.0.borrow_mut();
        m.before("stage verified helper")?;
        m.selected = b.supervisor.clone();
        m.after()
    }
    fn ready(&mut self, _: &Binding, _: Instant) -> Result<()> {
        let mut m = self.0.borrow_mut();
        m.before("authenticated ready identity")?;
        ensure!(m.running, "helper not running");
        m.ready = true;
        m.after()
    }
    fn cleanup(&mut self, _: &Binding) -> Result<()> {
        let mut m = self.0.borrow_mut();
        ensure!(m.ready, "cleanup before readiness");
        m.before("cleanup old helper")?;
        m.cleaned = true;
        m.after()
    }
}
fn fixture() -> (Store<Dir>, Fake, Envelope) {
    let b = Binding {
        installation: Uuid::from_u128(1),
        instance: Uuid::from_u128(2),
        owner: "owner".into(),
        supervisor: "/state/helper/old/helper.exe".into(),
        state_directory: "/state".into(),
        device_identity: "retained device".into(),
    };
    let first = Envelope::first(b.clone());
    let m = Rc::new(RefCell::new(Memory {
        files: BTreeMap::from([(super::super::NAME.into(), first.to_vec().unwrap())]),
        task: Some(b.clone()),
        running: true,
        selected: b.supervisor,
        ..Default::default()
    }));
    (Store::new(Dir(m.clone())), Fake(m), first)
}
fn run(store: &Store<Dir>, host: &mut Fake) -> Result<bool> {
    store.restage_helper(
        host,
        "/state/helper/new/helper.exe".into(),
        "new".into(),
        Instant::now() + Duration::from_secs(5),
    )
}
fn current(host: &Fake) -> Envelope {
    Envelope::from_slice(&host.0.borrow().files[super::super::NAME]).unwrap()
}
/// The real Windows Disable path: cancel the marker, then the Controller's
/// commit. With no live helper the Controller records `set_intent(Disabled)`.
fn user_disable(store: &Store<Dir>, host: &Fake) -> Result<Envelope> {
    let deadline = Instant::now() + Duration::from_secs(1);
    store.cancel_helper_restage(deadline)?;
    let disabled = current(host).set_intent(Intent::Disabled)?;
    store.begin(deadline)?.commit(disabled.clone())?;
    Ok(disabled)
}
fn planned(host: &Fake, step: &str) -> Envelope {
    let plan: serde_json::Value = serde_json::from_slice(&host.0.borrow().files[MARKER]).unwrap();
    serde_json::from_value(plan[step].clone()).unwrap()
}

#[test]
fn every_crash_before_and_after_publication_or_native_effect_resumes_enabled() -> Result<()> {
    let (store, mut host, _) = fixture();
    assert!(run(&store, &mut host)?);
    let events = host.0.borrow().events.clone();
    assert!(events
        .iter()
        .any(|e| e == "authenticated Stop and owned-tree exit"));
    assert!(events.iter().any(|e| e == "register"));
    for point in 1..=events.len() {
        for after in [false, true] {
            let (store, mut host, original) = fixture();
            host.0.borrow_mut().fail = Some((point, after));
            assert!(
                run(&store, &mut host).is_err(),
                "unfired crash {point}: {}",
                events[point - 1]
            );
            host.0.borrow_mut().fail = None;
            // Simulate a fresh launch with the same durable store/native state.
            let reopened = Store::new(Dir(host.0.clone()));
            run(&reopened, &mut host)?;
            let now = current(&host);
            assert_eq!(now.intent(), Intent::Enabled, "crash {point} after={after}");
            let mut expected = original.binding().clone();
            expected.supervisor = "/state/helper/new/helper.exe".into();
            assert_eq!(now.binding(), &expected);
            assert_eq!(now.journal_epoch(), original.journal_epoch());
            let m = host.0.borrow();
            assert!(m.running && m.ready && m.cleaned);
            assert_eq!(m.task.as_ref(), Some(&expected));
            assert!(!m.files.contains_key(MARKER));
        }
    }
    Ok(())
}
#[test]
fn newer_user_disable_at_every_crash_point_never_reenables_sync() -> Result<()> {
    let (store, mut host, _) = fixture();
    run(&store, &mut host)?;
    let count = host.0.borrow().events.len();
    for point in 2..count {
        let (store, mut host, _) = fixture();
        host.0.borrow_mut().fail = Some((point, true));
        assert!(run(&store, &mut host).is_err());
        host.0.borrow_mut().fail = None;
        host.0.borrow_mut().running = false;
        // Explicit Disable may repeat the updater's Disabled intent, and may even
        // equal a planned step byte for byte; its cancel still revokes reconnect.
        let disabled = user_disable(&store, &host)?;
        assert!(!run(&store, &mut host)?);
        assert_eq!(current(&host), disabled);
        assert!(!host.0.borrow().running);
        assert_eq!(host.0.borrow().selected, disabled.binding().supervisor);
    }
    Ok(())
}
#[test]
fn user_disable_identical_to_planned_step_is_still_respected() -> Result<()> {
    let (store, mut host, _) = fixture();
    run(&store, &mut host)?;
    let stop = host
        .0
        .borrow()
        .events
        .iter()
        .position(|e| e == "authenticated Stop and owned-tree exit")
        .context("no Stop")?;
    let (store, mut host, _) = fixture();
    // The old helper exited, then the app died before `stopped` was committed.
    host.0.borrow_mut().fail = Some((stop + 1, true));
    assert!(run(&store, &mut host).is_err());
    host.0.borrow_mut().fail = None;
    assert!(!host.0.borrow().running);
    let stopped = planned(&host, "stopped");
    let disabled = user_disable(&store, &host)?;
    assert_eq!(disabled, stopped, "the collision this marker flag exists for");
    assert!(!run(&store, &mut host)?);
    assert_eq!(current(&host), disabled);
    assert!(!store.helper_restage_pending()?);
    let m = host.0.borrow();
    assert!(!m.running && !m.cleaned);
    assert_eq!(m.selected, disabled.binding().supervisor);
    Ok(())
}
#[test]
fn concurrent_disable_during_authenticated_stop_revokes_delayed_reply() -> Result<()> {
    let (store, mut host, _) = fixture();
    host.0.borrow_mut().cancel_on_stop = true;
    assert!(run(&store, &mut host).is_err());
    let disabled = current(&host);
    assert_eq!(disabled.intent(), Intent::Disabled);
    assert!(!run(&store, &mut host)?);
    assert_eq!(current(&host), disabled);
    assert!(!host.0.borrow().events.iter().any(|e| e == "register"));
    Ok(())
}
#[test]
fn disabled_removed_and_pending_updates_never_begin_a_restage() -> Result<()> {
    for intent in [Intent::Disabled, Intent::Removed] {
        let (store, mut host, first) = fixture();
        store
            .begin(Instant::now() + Duration::from_secs(1))?
            .commit(first.set_intent(intent)?)?;
        assert!(!run(&store, &mut host)?);
        assert!(!store.helper_restage_pending()?);
    }
    // Binding immutability is also enforced by the real generic store.
    let (store, mut host, first) = fixture();
    let mut forged = serde_json::to_value(first.enable()?)?;
    forged["binding"]["supervisor"] = "foreign.exe".into();
    let forged = Envelope::from_slice(&serde_json::to_vec(&forged)?)?;
    assert!(Envelope::check_successor(&Stored::Current(first), &forged).is_err());
    assert!(store
        .begin(Instant::now() + Duration::from_secs(1))?
        .commit(forged)
        .is_err());
    assert!(run(&store, &mut host)?);
    Ok(())
}
