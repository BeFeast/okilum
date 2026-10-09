use super::{
    testing::{envelope, writes, Store},
    *,
};
use crate::sidecar::authority::Reason;
use std::{cell::RefCell, rc::Rc};
fn binding() -> Binding {
    Binding {
        instance: Uuid::nil(),
        installation: Uuid::from_u128(1),
        owner: "S-1-5-21-1234".into(),
        supervisor: r"C:\Users\Oleg\AppData\Local\Tessera\Sync\runtime\2.1.6\supervisor.exe".into(),
        state_directory: r"C:\Users\Oleg\AppData\Local\Tessera\Sync\state".into(),
        device_identity: "existing-certificate-fingerprint".into(),
    }
}
fn mac_binding() -> Binding {
    Binding {
        owner: "501".into(),
        supervisor: "/Applications/Tessera.app/Contents/MacOS/tessera-sync-supervisor".into(),
        state_directory: "/Users/oleg/Library/Application Support/Tessera/Sync".into(),
        ..binding()
    }
}
type Hook = Box<dyn FnMut(&StopToken)>;
struct Os {
    registration: Registration,
    mutations: Vec<&'static str>,
    fail_stop: bool,
    fail_register_after_effect: bool,
    foreign: bool,
    invalid_payload: bool,
    /// Advertise an authenticated supervisor generation while Running.
    ipc: bool,
    generation: u128,
    tokens: Vec<StopToken>,
    /// Runs inside the IPC Stop, i.e. while the controller must hold no lock.
    on_stop: Option<Hook>,
}
impl Default for Os {
    fn default() -> Self {
        Self {
            registration: Registration::Absent,
            mutations: vec![],
            fail_stop: false,
            fail_register_after_effect: false,
            foreign: false,
            invalid_payload: false,
            ipc: false,
            generation: 7,
            tokens: vec![],
            on_stop: None,
        }
    }
}
#[derive(Clone, Default)]
struct Fake(Rc<RefCell<Os>>);
impl Platform for Fake {
    fn inspect(&mut self, _: &Binding) -> Result<Registration> {
        ensure!(!self.0.borrow().foreign, "foreign service");
        Ok(self.0.borrow().registration)
    }
    fn verify_payload(&mut self, _: &Binding) -> Result<()> {
        ensure!(!self.0.borrow().invalid_payload, "tampered payload");
        Ok(())
    }
    fn register(&mut self, _: &Binding) -> Result<()> {
        let mut os = self.0.borrow_mut();
        os.mutations.push("register");
        os.registration = Registration::Stopped;
        ensure!(!os.fail_register_after_effect, "registration reply lost");
        Ok(())
    }
    fn start(&mut self, _: &Binding) -> Result<()> {
        let mut os = self.0.borrow_mut();
        os.mutations.push("start");
        os.registration = Registration::Running;
        Ok(())
    }
    fn stop(&mut self, _: &Binding) -> Result<()> {
        let mut os = self.0.borrow_mut();
        os.mutations.push("stop");
        ensure!(!os.fail_stop, "child still alive");
        os.registration = Registration::Stopped;
        Ok(())
    }
    fn supervisor_scope(&mut self, _: &Binding) -> Result<Option<Scope>> {
        let os = self.0.borrow();
        Ok(
            (os.ipc && os.registration == Registration::Running).then(|| Scope {
                installation: binding().installation,
                instance: binding().instance,
                generation: Uuid::from_u128(os.generation),
            }),
        )
    }
    fn stop_supervisor(&mut self, _: &Binding, token: &StopToken) -> Result<()> {
        {
            let mut os = self.0.borrow_mut();
            os.mutations.push("stop_ipc");
            os.tokens.push(token.clone());
        }
        let hook = self.0.borrow_mut().on_stop.take();
        if let Some(mut hook) = hook {
            hook(token);
            self.0.borrow_mut().on_stop = Some(hook);
        }
        let mut os = self.0.borrow_mut();
        ensure!(!os.fail_stop, "child still alive");
        os.registration = Registration::Stopped;
        Ok(())
    }
    fn unregister(&mut self, _: &Binding) -> Result<()> {
        let mut os = self.0.borrow_mut();
        os.mutations.push("unregister");
        os.registration = Registration::Absent;
        Ok(())
    }
}
fn ipc_os() -> Fake {
    let os = Fake::default();
    os.0.borrow_mut().ipc = true;
    os
}
fn mutations(os: &Fake) -> Vec<&'static str> {
    os.0.borrow().mutations.clone()
}

#[test]
fn install_and_unopened_reader_are_inert_with_enable_positive_control() {
    let store = Store::default();
    let os = Fake::default();
    let mut c = Controller::new(store.clone(), os.clone());
    assert_eq!(c.snapshot().unwrap(), Stored::Absent);
    assert_eq!(c.reconcile().unwrap(), State::Disabled);
    c.disable().unwrap();
    c.remove().unwrap();
    assert!(mutations(&os).is_empty());
    assert_eq!(writes(&store), 0);
    assert_eq!(c.enable(binding()).unwrap(), State::Running);
    assert_eq!(mutations(&os), ["register", "start"]);
    c.disable().unwrap();
    let mut changed = binding();
    changed.device_identity = "replacement".into();
    assert!(c.enable(changed).is_err());
    c.enable(binding()).unwrap();
    assert_eq!(
        envelope(&store).binding().device_identity,
        binding().device_identity
    );
}
#[test]
fn failed_durable_write_prevents_os_effects() {
    let store = Store::default();
    store.0.borrow_mut().fail = true;
    let os = Fake::default();
    let mut c = Controller::new(store.clone(), os.clone());
    assert!(c.enable(binding()).is_err());
    assert!(mutations(&os).is_empty());
    // A failed prepare also blocks Disable's stop and unregister. Positive
    // control: the same call performs them once the write succeeds.
    store.0.borrow_mut().fail = false;
    c.enable(binding()).unwrap();
    store.0.borrow_mut().fail = true;
    os.0.borrow_mut().mutations.clear();
    assert!(c.disable().is_err());
    assert!(mutations(&os).is_empty());
    store.0.borrow_mut().fail = false;
    c.disable().unwrap();
    assert_eq!(mutations(&os), ["stop", "unregister"]);
}
#[test]
fn lost_registration_reply_recovers_without_replacing_identity_or_registering_twice() {
    let store = Store::default();
    let os = Fake::default();
    os.0.borrow_mut().fail_register_after_effect = true;
    let mut c = Controller::new(store.clone(), os.clone());
    assert!(c.enable(binding()).is_err());
    assert_eq!(envelope(&store).intent(), Intent::Enabled);
    let mut resumed = Controller::new(store, os.clone());
    assert_eq!(resumed.reconcile().unwrap(), State::Running);
    assert_eq!(mutations(&os), ["register", "start"]);
}
#[test]
fn interrupted_stop_stays_disabled_and_never_unregisters_live_child() {
    let store = Store::default();
    let os = Fake::default();
    let mut c = Controller::new(store.clone(), os.clone());
    c.enable(binding()).unwrap();
    os.0.borrow_mut().fail_stop = true;
    assert!(c.disable().is_err());
    assert_eq!(envelope(&store).intent(), Intent::Disabled);
    assert!(!mutations(&os).contains(&"unregister"));
    os.0.borrow_mut().fail_stop = false;
    let mut resumed = Controller::new(store, os.clone());
    assert_eq!(resumed.reconcile().unwrap(), State::Disabled);
    assert_eq!(
        mutations(&os),
        ["register", "start", "stop", "stop", "unregister"]
    );
}
#[test]
fn terminal_removal_and_tampered_payload_cannot_restart() {
    let store = Store::default();
    let os = Fake::default();
    let mut c = Controller::new(store, os.clone());
    c.enable(binding()).unwrap();
    c.remove().unwrap();
    assert!(c.enable(binding()).is_err());
    assert_eq!(c.reconcile().unwrap(), State::Removed);
    let mut changed = binding();
    changed.device_identity = "new identity".into();
    assert!(c.enable(changed).is_err());
    let store = Store::default();
    let os = Fake::default();
    let mut c = Controller::new(store, os.clone());
    c.enable(binding()).unwrap();
    os.0.borrow_mut().invalid_payload = true;
    os.0.borrow_mut().mutations.clear();
    assert!(c.reconcile().is_err());
    assert!(mutations(&os).is_empty());
}
#[test]
fn foreign_service_is_never_stopped_or_adopted_but_the_intent_is_kept() {
    let store = Store::default();
    let os = Fake::default();
    let mut c = Controller::new(store.clone(), os.clone());
    c.enable(binding()).unwrap();
    os.0.borrow_mut().foreign = true;
    os.0.borrow_mut().mutations.clear();
    assert!(c.remove().is_err());
    assert!(mutations(&os).is_empty());
    assert_eq!(envelope(&store).intent(), Intent::Removed);
    assert!(envelope(&store).stop().is_none());
}

#[test]
fn ipc_stop_carries_the_stored_token_without_holding_the_lock() {
    let store = Store::default();
    let os = ipc_os();
    let mut c = Controller::new(store.clone(), os.clone());
    c.enable(binding()).unwrap();
    let seen = Rc::new(RefCell::new(vec![]));
    let (probe, log) = (store.clone(), seen.clone());
    os.0.borrow_mut().on_stop = Some(Box::new(move |token| {
        let held = probe.0.borrow().locked;
        let stored = match &probe.0.borrow().stored {
            Stored::Current(e) => e.stop().cloned(),
            _ => None,
        };
        log.borrow_mut()
            .push((held, stored == Some(token.operation.clone())));
    }));
    assert_eq!(c.disable().unwrap(), State::Disabled);
    // Not locked during IPC, and the token was already durable (prepare first).
    assert_eq!(*seen.borrow(), [(false, true)]);
    assert_eq!(
        mutations(&os),
        ["register", "start", "stop_ipc", "unregister"]
    );
    let done = envelope(&store);
    assert_eq!((done.revision(), done.intent()), (3, Intent::Disabled));
    assert!(
        done.stop().is_none(),
        "operation consumed in a new revision"
    );
    let token = os.0.borrow().tokens[0].clone();
    assert_eq!(token.operation.reason, Reason::Disable);
    assert_eq!(token.operation.authorized_revision, 2);
    assert!(done.authorize(&token, &token.operation.scope).is_err());
}
#[test]
fn aba_stale_controller_cannot_unregister_after_enable_and_disable_again() {
    let store = Store::default();
    let os = ipc_os();
    let mut a = Controller::new(store.clone(), os.clone());
    a.enable(binding()).unwrap();
    // While A waits on IPC, B (same instance) Enables and Disables again, and
    // its own stop is still pending: same Intent and Binding, different operation.
    let (store_b, os_b) = (store.clone(), os.clone());
    os.0.borrow_mut().on_stop = Some(Box::new(move |_| {
        let mut b = Controller::new(store_b.clone(), os_b.clone());
        b.enable(binding()).unwrap();
        os_b.0.borrow_mut().fail_stop = true;
        assert!(b.disable().is_err());
        os_b.0.borrow_mut().fail_stop = false;
    }));
    assert!(a.disable().is_err());
    assert!(!mutations(&os).contains(&"unregister"));
    let pending = envelope(&store);
    assert_eq!(
        (pending.revision(), pending.intent()),
        (4, Intent::Disabled)
    );
    let stale = os.0.borrow().tokens[0].clone();
    let fresh = os.0.borrow().tokens[1].clone();
    assert_eq!(stale.operation.authorized_revision, 2);
    assert_eq!(fresh.operation.authorized_revision, 4);
    assert_ne!(stale.operation.operation_id, fresh.operation.operation_id);
    assert!(pending.authorize(&stale, &stale.operation.scope).is_err());
    pending.authorize(&fresh, &fresh.operation.scope).unwrap();
}
#[test]
fn retry_reuses_the_stored_token_and_a_new_generation_arms_a_new_one() {
    let store = Store::default();
    let os = ipc_os();
    let mut c = Controller::new(store.clone(), os.clone());
    c.enable(binding()).unwrap();
    os.0.borrow_mut().fail_stop = true;
    assert!(c.disable().is_err());
    assert_eq!(envelope(&store).revision(), 2);
    os.0.borrow_mut().fail_stop = false;
    assert_eq!(c.reconcile().unwrap(), State::Disabled);
    let tokens = os.0.borrow().tokens.clone();
    assert_eq!(tokens.len(), 2);
    assert_eq!(
        tokens[0], tokens[1],
        "retry sends the stored token unchanged"
    );
    // A supervisor restart between attempts: the old operation names a dead
    // generation, so recovery arms a fresh one in a new revision.
    let store = Store::default();
    let os = ipc_os();
    let mut c = Controller::new(store.clone(), os.clone());
    c.enable(binding()).unwrap();
    os.0.borrow_mut().fail_stop = true;
    assert!(c.disable().is_err());
    {
        let mut inner = os.0.borrow_mut();
        inner.fail_stop = false;
        inner.generation = 8;
    }
    assert_eq!(c.reconcile().unwrap(), State::Disabled);
    let tokens = os.0.borrow().tokens.clone();
    assert_ne!(
        tokens[0].operation.operation_id,
        tokens[1].operation.operation_id
    );
    assert_eq!(tokens[1].operation.scope.generation, Uuid::from_u128(8));
    assert_eq!(tokens[1].operation.authorized_revision, 3);
}
#[test]
fn enable_invalidates_a_pending_stop() {
    let store = Store::default();
    let os = ipc_os();
    let mut c = Controller::new(store.clone(), os.clone());
    c.enable(binding()).unwrap();
    os.0.borrow_mut().fail_stop = true;
    assert!(c.disable().is_err());
    os.0.borrow_mut().fail_stop = false;
    let old = os.0.borrow().tokens[0].clone();
    let pending = envelope(&store);
    // Positive control: the token is valid until the Enable commits.
    pending.authorize(&old, &old.operation.scope).unwrap();
    assert_eq!(c.enable(binding()).unwrap(), State::Running);
    let now = envelope(&store);
    assert!(now.stop().is_none() && now.intent() == Intent::Enabled);
    assert!(now.revision() > pending.revision());
    assert!(now.authorize(&old, &old.operation.scope).is_err());
}
#[test]
fn lock_contention_leaves_the_work_pending_without_effects() {
    let store = Store::default();
    let os = Fake::default();
    let mut c = Controller::new(store.clone(), os.clone());
    c.enable(binding()).unwrap();
    os.0.borrow_mut().mutations.clear();
    store.0.borrow_mut().locked = true; // another holder
    for result in [c.disable(), c.reconcile(), c.enable(binding())] {
        assert!(result.unwrap_err().to_string().contains("busy"));
    }
    assert!(mutations(&os).is_empty());
    store.0.borrow_mut().locked = false;
    assert_eq!(c.disable().unwrap(), State::Disabled);
    assert_eq!(mutations(&os), ["stop", "unregister"]);
}
#[test]
fn legacy_journal_is_read_only_until_the_first_mutating_reconcile_migrates_it() {
    for intent in [Intent::Enabled, Intent::Disabled, Intent::Removed] {
        let store = Store::default();
        let legacy = Journal {
            binding: binding(),
            intent,
        };
        store.0.borrow_mut().stored = Stored::Legacy {
            journal: legacy.clone(),
            update: None,
        };
        let os = Fake::default();
        let mut c = Controller::new(store.clone(), os.clone());
        assert!(matches!(c.snapshot().unwrap(), Stored::Legacy { .. }));
        assert_eq!(writes(&store), 0, "snapshot never mints a revision");
        let state = c.reconcile().unwrap();
        let migrated = envelope(&store);
        assert_eq!((migrated.revision(), migrated.intent()), (1, intent));
        assert!(migrated.stop().is_none(), "migration fabricates no stop");
        assert_eq!(writes(&store), 1);
        match intent {
            Intent::Enabled => {
                assert_eq!(state, State::Running);
                assert_eq!(mutations(&os), ["register", "start"]);
            }
            _ => assert!(mutations(&os).is_empty()),
        }
    }
}

#[derive(Default)]
struct Tasks {
    task: Option<windows::Task>,
    mutations: Vec<&'static str>,
    collide: bool,
}
impl windows::TaskApi for Tasks {
    fn current_sid(&self) -> Result<String> {
        Ok(binding().owner)
    }
    fn read(&mut self, _: &str) -> Result<Option<windows::Task>> {
        Ok(self.task.as_ref().map(|t| windows::Task {
            owner_sid: t.owner_sid.clone(),
            definition: t.definition.clone(),
            running: t.running,
        }))
    }
    fn verify_payload(&mut self, _: &Binding) -> Result<()> {
        Ok(())
    }
    fn create(&mut self, _: &str, xml: &str) -> Result<()> {
        ensure!(
            !self.collide && self.task.is_none(),
            "TASK_CREATE collision"
        );
        self.mutations.push("create");
        self.task = Some(windows::Task {
            owner_sid: binding().owner,
            definition: xml.into(),
            running: false,
        });
        Ok(())
    }
    fn start_owned(&mut self, _: &str, _: &str, _: &str) -> Result<()> {
        self.mutations.push("start");
        self.task.as_mut().unwrap().running = true;
        Ok(())
    }
    fn stop_owned(&mut self, _: &str, _: &str, _: &str) -> Result<()> {
        self.mutations.push("stop");
        self.task.as_mut().unwrap().running = false;
        Ok(())
    }
    fn delete_owned(&mut self, _: &str, _: &str, _: &str) -> Result<()> {
        self.mutations.push("delete");
        self.task = None;
        Ok(())
    }
}
#[test]
fn task_scheduler_requires_interactive_least_privilege_and_preserves_foreign_definition() {
    let b = binding();
    let mut adapter = windows::TaskScheduler(Tasks::default());
    let xml = windows::definition(&b).unwrap();
    assert!(xml.contains("InteractiveToken") && xml.contains("LeastPrivilege"));
    assert!(!xml.contains("Password") && !xml.contains("api-key"));
    adapter.register(&b).unwrap();
    adapter.start(&b).unwrap();
    adapter.0.task.as_mut().unwrap().definition = xml.replace("LeastPrivilege", "HighestAvailable");
    assert!(adapter.stop(&b).is_err());
    assert!(adapter.unregister(&b).is_err());
    assert_eq!(adapter.0.mutations, ["create", "start"]);
    adapter.0.task.as_mut().unwrap().definition = xml;
    adapter.stop(&b).unwrap();
    adapter.unregister(&b).unwrap();
    assert_eq!(adapter.0.mutations, ["create", "start", "stop", "delete"]);
}
#[test]
fn task_registration_collision_and_owner_change_fail_closed() {
    let b = binding();
    let mut adapter = windows::TaskScheduler(Tasks {
        collide: true,
        ..Default::default()
    });
    assert!(adapter.register(&b).is_err());
    assert!(adapter.0.mutations.is_empty());
    adapter.0.collide = false;
    adapter.register(&b).unwrap();
    adapter.0.task.as_mut().unwrap().owner_sid = "S-1-5-21-9999".into();
    assert!(adapter.start(&b).is_err());
    assert_eq!(adapter.0.mutations, ["create"]);
}
#[test]
fn task_paths_are_escaped_without_shell_or_credentials() {
    let mut b = binding();
    b.state_directory = "C:\\Users\\A&B\\Sync\\".into();
    let xml = windows::definition(&b).unwrap();
    assert!(xml.contains("A&amp;B"));
    xmltree::Element::parse(xml.as_bytes()).unwrap();
    for bad in [
        r"C:\Users\..\outside",
        r"\\server\share\app.exe",
        "C:\\a\"b",
        "C:\\a\ncommand",
    ] {
        b.supervisor = bad.into();
        assert!(windows::definition(&b).is_err());
    }
}

struct Mac {
    version: u32,
    status: macos::Status,
    running: bool,
    approval: bool,
    mutations: Vec<&'static str>,
    wrong_bundle: bool,
}
impl Default for Mac {
    fn default() -> Self {
        Self {
            version: 13,
            status: macos::Status::NotRegistered,
            running: false,
            approval: true,
            mutations: vec![],
            wrong_bundle: false,
        }
    }
}
impl macos::SmApi for Mac {
    fn major_version(&self) -> u32 {
        self.version
    }
    fn verify_bundle(&mut self, _: &Binding, _: &str) -> Result<()> {
        ensure!(!self.wrong_bundle, "bundle changed");
        Ok(())
    }
    fn verify_payload(&mut self, _: &Binding) -> Result<()> {
        Ok(())
    }
    fn status(&mut self, _: &str) -> Result<macos::Status> {
        Ok(self.status)
    }
    fn running(&mut self, _: &Binding) -> Result<bool> {
        Ok(self.running)
    }
    fn register(&mut self, _: &Binding, _: &str) -> Result<()> {
        self.mutations.push("register");
        self.status = if self.approval {
            macos::Status::RequiresApproval
        } else {
            macos::Status::Enabled
        };
        Ok(())
    }
    fn start_owned(&mut self, _: &Binding) -> Result<()> {
        self.mutations.push("start");
        self.running = true;
        Ok(())
    }
    fn stop_owned(&mut self, _: &Binding) -> Result<()> {
        self.mutations.push("stop");
        self.running = false;
        Ok(())
    }
    fn unregister(&mut self, _: &Binding, _: &str) -> Result<()> {
        self.mutations.push("unregister");
        self.status = macos::Status::NotRegistered;
        Ok(())
    }
}
#[test]
fn macos_approval_is_not_running_and_monterey_cannot_register() {
    let b = mac_binding();
    let mut adapter = macos::SmAppService(Mac::default());
    adapter.register(&b).unwrap();
    assert_eq!(adapter.inspect(&b).unwrap(), Registration::ApprovalRequired);
    assert!(adapter.start(&b).is_err());
    assert_eq!(adapter.0.mutations, ["register"]);
    adapter.0.status = macos::Status::Enabled;
    adapter.start(&b).unwrap();
    assert_eq!(adapter.inspect(&b).unwrap(), Registration::Running);
    adapter.stop(&b).unwrap();
    adapter.unregister(&b).unwrap();
    assert_eq!(adapter.inspect(&b).unwrap(), Registration::Absent);
    let mut old = macos::SmAppService(Mac {
        version: 12,
        ..Default::default()
    });
    assert!(old.register(&b).is_err());
    assert!(old.0.mutations.is_empty());
}
#[test]
fn missing_or_replaced_mac_bundle_is_not_silently_registered() {
    let mut adapter = macos::SmAppService(Mac {
        status: macos::Status::NotFound,
        ..Default::default()
    });
    assert!(adapter.register(&mac_binding()).is_err());
    adapter.0.status = macos::Status::NotRegistered;
    adapter.0.wrong_bundle = true;
    assert!(adapter.register(&mac_binding()).is_err());
    assert!(adapter.0.mutations.is_empty());
    let plist = macos::bundled_plist("Contents/MacOS/tessera-sync-supervisor").unwrap();
    xmltree::Element::parse(plist.as_bytes()).unwrap();
    assert!(!plist.contains("LaunchAgents/") && !plist.contains("state_directory"));
    assert!(macos::bundled_plist("Contents/MacOS/../Resources/helper").is_err());
}

#[test]
fn controller_persists_enabled_while_waiting_for_macos_system_approval() {
    let store = Store::default();
    let mut controller = Controller::new(store.clone(), macos::SmAppService(Mac::default()));
    assert_eq!(
        controller.enable(mac_binding()).unwrap(),
        State::ApprovalRequired
    );
    assert_eq!(envelope(&store).intent(), Intent::Enabled);
    assert_eq!(controller.platform.0.mutations, ["register"]);
    controller.platform.0.status = macos::Status::Enabled;
    assert_eq!(controller.reconcile().unwrap(), State::Running);
    assert_eq!(controller.disable().unwrap(), State::Disabled);
    let before = controller.platform.0.mutations.clone();
    assert_eq!(controller.reconcile().unwrap(), State::Disabled);
    assert_eq!(controller.platform.0.mutations, before);
}
