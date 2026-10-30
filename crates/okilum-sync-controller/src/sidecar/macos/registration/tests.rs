use super::*;
use crate::sidecar::{authority::Envelope, store::UnixStore, supervisor::ipc::Scope, Intent};
use std::{
    cell::{Cell, RefCell},
    fs,
    os::unix::fs::PermissionsExt,
    rc::Rc,
};
use uuid::Uuid;

struct Proof {
    store: UnixStore,
    refuse: Rc<Cell<bool>>,
    live: Rc<RefCell<Option<Scope>>>,
    checks: Cell<usize>,
    refuse_after_first: bool,
}
impl RegistrationProof for Proof {
    fn verify(&self, _: &Binding, deadline: Instant) -> Result<()> {
        // Native payload verification also reads selection using this store.
        // Every boundary must permit a fresh lock: no nested coordinator lock.
        let _tx = self.store.begin(deadline)?;
        self.checks.set(self.checks.get() + 1);
        ensure!(
            !(self.refuse.get() || self.refuse_after_first && self.checks.get() > 1),
            "fixture bundle/payload verification refused"
        );
        Ok(())
    }
    fn observe(&self, _: &Binding, deadline: Instant) -> Result<Discovered> {
        let _tx = self.store.begin(deadline)?;
        Ok(match self.live.borrow().clone() {
            Some(scope) => Discovered::Live(scope),
            None => Discovered::Unreachable("fixture peer unavailable or signature refused".into()),
        })
    }
}
struct Service {
    store: UnixStore,
    binding: Binding,
    calls: Rc<Cell<usize>>,
    status: Rc<Cell<Status>>,
    change_on_second_status: Rc<Cell<bool>>,
    status_calls: Cell<usize>,
    before_register: Option<Box<dyn FnMut()>>,
    fail_register: Rc<Cell<bool>>,
    claim: Rc<Cell<bool>>,
    live: Rc<RefCell<Option<Scope>>>,
}
impl RegistrationPort for Service {
    fn status(&mut self, deadline: Instant) -> Result<Status> {
        let _tx = self.store.begin(deadline)?;
        self.status_calls.set(self.status_calls.get() + 1);
        if self.change_on_second_status.get() && self.status_calls.get() == 2 {
            self.status.set(Status::Enabled);
        }
        Ok(self.status.get())
    }
    fn register(&mut self, deadline: Instant) -> Result<()> {
        if let Some(other) = &mut self.before_register {
            other();
        }
        self.calls.set(self.calls.get() + 1);
        ensure!(!self.fail_register.get(), "fixture registration failed");
        self.status.set(Status::Enabled);
        if self.claim.get() {
            // Simulate launchd's helper claiming immediately during register.
            // This is the positive control for release-before-native-effect.
            let generation = self.store.claim_launch(&self.binding, deadline)?;
            *self.live.borrow_mut() = Some(Scope {
                installation: self.binding.installation,
                instance: self.binding.instance,
                generation,
            });
        } else {
            let _tx = self.store.begin(deadline)?;
        }
        Ok(())
    }
}
type Guard = RegistrationGuard<crate::sidecar::store::UnixDir, Proof, Service>;
struct World {
    _root: tempfile::TempDir,
    store: UnixStore,
    binding: Binding,
    calls: Rc<Cell<usize>>,
    status: Rc<Cell<Status>>,
    refuse: Rc<Cell<bool>>,
    change: Rc<Cell<bool>>,
    fail: Rc<Cell<bool>>,
    claim: Rc<Cell<bool>>,
    live: Rc<RefCell<Option<Scope>>>,
}
fn soon() -> Instant {
    Instant::now() + Duration::from_secs(2)
}
impl World {
    fn new(ready: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let store = UnixStore::open_existing(root.path()).unwrap();
        let binding = Binding {
            installation: Uuid::new_v4(),
            instance: Uuid::new_v4(),
            owner: rustix::process::geteuid().as_raw().to_string(),
            supervisor: "/fixture/helper".into(),
            state_directory: root.path().to_str().unwrap().into(),
            device_identity: "fixture-device".into(),
        };
        let first = if ready {
            Envelope::first_with_launch(binding.clone())
        } else {
            Envelope::first(binding.clone())
        };
        store.begin(soon()).unwrap().commit(first).unwrap();
        Self {
            _root: root,
            store,
            binding,
            calls: Rc::new(Cell::new(0)),
            status: Rc::new(Cell::new(Status::NotRegistered)),
            refuse: Rc::new(Cell::new(false)),
            change: Rc::new(Cell::new(false)),
            fail: Rc::new(Cell::new(false)),
            claim: Rc::new(Cell::new(false)),
            live: Rc::new(RefCell::new(None)),
        }
    }
    fn guard(&self) -> Guard {
        let proof = Proof {
            store: self.store.clone(),
            refuse: self.refuse.clone(),
            live: self.live.clone(),
            checks: Cell::new(0),
            refuse_after_first: false,
        };
        let service = Service {
            store: self.store.clone(),
            binding: self.binding.clone(),
            calls: self.calls.clone(),
            status: self.status.clone(),
            status_calls: Cell::new(0),
            before_register: None,
            change_on_second_status: self.change.clone(),
            fail_register: self.fail.clone(),
            claim: self.claim.clone(),
            live: self.live.clone(),
        };
        Guard::new(
            self.binding.clone(),
            self.store.clone(),
            proof,
            service,
            Duration::from_secs(2),
        )
        .unwrap()
    }
}
#[test]
fn native_boundaries_release_the_journal_and_one_helper_can_claim() {
    let world = World::new(true);
    world.claim.set(true);
    let mut guard = world.guard();
    assert_eq!(guard.register_prepared().unwrap(), ManagedStatus::Running);
    assert_eq!(world.calls.get(), 1);
    assert_eq!(guard.register_prepared().unwrap(), ManagedStatus::Running);
    assert_eq!(
        world.calls.get(),
        1,
        "existing signed generation was registered twice"
    );
    assert!(world.store.claim_launch(&world.binding, soon()).is_err());
}
#[test]
fn registration_without_live_signed_generation_never_grants_second_dispatch() {
    let world = World::new(true);
    let mut guard = world.guard();
    assert_eq!(
        guard.register_prepared().unwrap(),
        ManagedStatus::RecoveryPending
    );
    assert_eq!(
        guard.register_prepared().unwrap(),
        ManagedStatus::RecoveryPending
    );
    assert_eq!(world.calls.get(), 1);
    // Even if ServiceManagement later forgets the registration, the intent is spent.
    world.status.set(Status::NotRegistered);
    assert!(guard.register_prepared().is_err());
    assert_eq!(world.calls.get(), 1);
}
#[test]
fn existing_registration_in_every_native_state_is_inert_without_live_peer() {
    for status in [Status::Enabled, Status::RequiresApproval, Status::NotFound] {
        let world = World::new(true);
        world.status.set(status);
        assert_eq!(
            world.guard().register_prepared().unwrap(),
            ManagedStatus::RecoveryPending
        );
        assert_eq!(world.calls.get(), 0);
        let tx = world.store.begin(soon()).unwrap();
        assert_eq!(
            tx.current().unwrap().unwrap().revision(),
            1,
            "observation consumed an intent"
        );
    }
}
#[test]
fn failed_native_registration_and_changed_status_consume_dispatch_without_retry() {
    for changed in [false, true] {
        let world = World::new(true);
        world.fail.set(!changed);
        world.change.set(changed);
        let mut guard = world.guard();
        assert!(guard.register_prepared().is_err());
        world.status.set(Status::NotRegistered);
        assert!(world.guard().register_prepared().is_err());
        assert_eq!(world.calls.get(), usize::from(!changed));
        assert_eq!(world.guard().status(), ManagedStatus::RecoveryPending);
    }
}
#[test]
fn missing_legacy_corrupt_disabled_or_changed_binding_has_no_native_effect() {
    for damage in 0..5 {
        let world = World::new(damage != 0);
        match damage {
            0 => {} // Existing journal has no launch generation.
            1 => {
                fs::remove_file(world._root.path().join("sidecar.json")).unwrap();
            }
            2 => {
                fs::write(world._root.path().join("sidecar.json"), b"{").unwrap();
            }
            3 => {
                let mut tx = world.store.begin(soon()).unwrap();
                let saved = tx.current().unwrap().unwrap().clone();
                tx.commit(saved.set_intent(Intent::Disabled).unwrap())
                    .unwrap();
            }
            _ => {}
        }
        let mut guard = world.guard();
        if damage == 4 {
            guard.binding.instance = Uuid::new_v4();
        }
        assert!(guard.register_prepared().is_err());
        assert_eq!(world.calls.get(), 0);
        assert_eq!(guard.status(), ManagedStatus::RecoveryPending);
    }
}
#[test]
fn outer_transaction_is_refused_with_no_native_effect_and_verification_failure_is_inert() {
    let world = World::new(true);
    let held = world.store.begin(soon()).unwrap();
    let mut bounded = world.guard();
    bounded.budget = Duration::from_millis(30);
    assert!(bounded.register_prepared().is_err());
    assert_eq!(world.calls.get(), 0);
    drop(held);
    world.refuse.set(true);
    assert!(world.guard().register_prepared().is_err());
    assert_eq!(world.calls.get(), 0);
    world.refuse.set(false);
    world.claim.set(true);
    assert_eq!(
        world.guard().register_prepared().unwrap(),
        ManagedStatus::Running
    );
}

#[test]
fn a_competing_guard_during_the_native_call_cannot_dispatch_again() {
    let world = World::new(true);
    let mut first = world.guard();
    let mut competing = world.guard();
    let attempted = Rc::new(Cell::new(false));
    let control = attempted.clone();
    first.service.before_register = Some(Box::new(move || {
        // ServiceManagement still says NotRegistered at this exact boundary.
        // The journal lock is free, but its dispatch is already consumed.
        assert!(competing.register_prepared().is_err());
        control.set(true);
    }));
    assert_eq!(
        first.register_prepared().unwrap(),
        ManagedStatus::RecoveryPending
    );
    assert!(attempted.get(), "competing attempt was not exercised");
    assert_eq!(world.calls.get(), 1);
}

#[test]
fn payload_or_bundle_change_after_dispatch_consumes_permission_without_native_call() {
    let world = World::new(true);
    let mut guard = world.guard();
    guard.proof.refuse_after_first = true;
    assert!(guard.register_prepared().is_err());
    assert_eq!(world.calls.get(), 0);
    assert!(world.guard().register_prepared().is_err());
    assert_eq!(world.calls.get(), 0);
}

fn generation(world: &World) -> Uuid {
    world
        .store
        .begin(soon())
        .unwrap()
        .current()
        .unwrap()
        .unwrap()
        .launch_registration()
        .unwrap()
        .generation()
}

#[test]
fn confirmed_enable_consumes_only_the_displayed_generation() {
    let world = World::new(true);
    let confirmed = generation(&world);
    let mut guard = world.guard();
    assert!(guard.enable_confirmed(Uuid::new_v4()).is_err());
    assert_eq!(world.calls.get(), 0);
    assert_eq!(
        world
            .store
            .begin(soon())
            .unwrap()
            .current()
            .unwrap()
            .unwrap()
            .revision(),
        1
    );
    // Positive control: rejecting a stale click did not spend the valid intent.
    world.claim.set(true);
    assert_eq!(
        guard.enable_confirmed(confirmed).unwrap(),
        ManagedStatus::Running
    );
    assert_eq!(world.calls.get(), 1);
    assert_eq!(
        guard.enable_confirmed(confirmed).unwrap(),
        ManagedStatus::Running
    );
    assert_eq!(world.calls.get(), 1);
}

#[test]
fn confirmed_enable_cannot_rearm_after_native_failure_or_lost_peer() {
    for fail in [false, true] {
        let world = World::new(true);
        let confirmed = generation(&world);
        world.fail.set(fail);
        let result = world.guard().enable_confirmed(confirmed);
        if fail {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap(), ManagedStatus::RecoveryPending);
        }
        world.status.set(Status::NotRegistered);
        world.fail.set(false);
        assert!(world.guard().enable_confirmed(confirmed).is_err());
        assert_eq!(world.calls.get(), 1);
    }
}

#[test]
fn a_competing_confirmed_click_cannot_create_a_second_daemon() {
    let world = World::new(true);
    let confirmed = generation(&world);
    let mut first = world.guard();
    let mut second = world.guard();
    let exercised = Rc::new(Cell::new(false));
    let witness = exercised.clone();
    first.service.before_register = Some(Box::new(move || {
        assert!(second.enable_confirmed(confirmed).is_err());
        witness.set(true);
    }));
    world.claim.set(true);
    assert_eq!(
        first.enable_confirmed(confirmed).unwrap(),
        ManagedStatus::Running
    );
    assert!(exercised.get());
    assert_eq!(world.calls.get(), 1);
}
