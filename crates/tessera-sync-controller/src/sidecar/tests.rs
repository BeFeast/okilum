use super::*;
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
#[derive(Default)]
struct Memory {
    saved: Option<Journal>,
    writes: usize,
    fail: bool,
}
#[derive(Clone, Default)]
struct Store(Rc<RefCell<Memory>>);
impl LockedJournal for Store {
    fn load(&self) -> Result<Option<Journal>> {
        Ok(self.0.borrow().saved.clone())
    }
    fn save(&mut self, value: &Journal) -> Result<()> {
        let mut memory = self.0.borrow_mut();
        ensure!(!memory.fail, "simulated flush failure");
        memory.saved = Some(value.clone());
        memory.writes += 1;
        Ok(())
    }
}
struct Os {
    registration: Registration,
    mutations: Vec<&'static str>,
    fail_stop: bool,
    fail_register_after_effect: bool,
    foreign: bool,
    invalid_payload: bool,
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
    fn unregister(&mut self, _: &Binding) -> Result<()> {
        let mut os = self.0.borrow_mut();
        os.mutations.push("unregister");
        os.registration = Registration::Absent;
        Ok(())
    }
}
#[test]
fn install_and_unopened_reader_are_inert_with_enable_positive_control() {
    let store = Store::default();
    let os = Fake::default();
    let mut c = Controller::new(store.clone(), os.clone());
    assert!(c.snapshot().unwrap().is_none());
    assert_eq!(c.reconcile().unwrap(), State::Disabled);
    c.disable().unwrap();
    c.remove().unwrap();
    assert!(os.0.borrow().mutations.is_empty());
    assert_eq!(store.0.borrow().writes, 0);
    assert_eq!(c.enable(binding()).unwrap(), State::Running);
    assert_eq!(os.0.borrow().mutations, ["register", "start"]);
    c.disable().unwrap();
    let mut changed = binding();
    changed.device_identity = "replacement".into();
    assert!(c.enable(changed).is_err());
    c.enable(binding()).unwrap();
    assert_eq!(
        store
            .0
            .borrow()
            .saved
            .as_ref()
            .unwrap()
            .binding
            .device_identity,
        binding().device_identity
    );
}
#[test]
fn failed_durable_write_prevents_os_effects() {
    let store = Store::default();
    store.0.borrow_mut().fail = true;
    let os = Fake::default();
    let mut c = Controller::new(store, os.clone());
    assert!(c.enable(binding()).is_err());
    assert!(os.0.borrow().mutations.is_empty());
}
#[test]
fn lost_registration_reply_recovers_without_replacing_identity_or_registering_twice() {
    let store = Store::default();
    let os = Fake::default();
    os.0.borrow_mut().fail_register_after_effect = true;
    let mut c = Controller::new(store.clone(), os.clone());
    assert!(c.enable(binding()).is_err());
    assert_eq!(
        store.0.borrow().saved.as_ref().unwrap().intent,
        Intent::Enabled
    );
    let mut resumed = Controller::new(store, os.clone());
    assert_eq!(resumed.reconcile().unwrap(), State::Running);
    assert_eq!(os.0.borrow().mutations, ["register", "start"]);
}
#[test]
fn interrupted_stop_stays_disabled_and_never_unregisters_live_child() {
    let store = Store::default();
    let os = Fake::default();
    let mut c = Controller::new(store.clone(), os.clone());
    c.enable(binding()).unwrap();
    os.0.borrow_mut().fail_stop = true;
    assert!(c.disable().is_err());
    assert_eq!(
        store.0.borrow().saved.as_ref().unwrap().intent,
        Intent::Disabled
    );
    assert!(!os.0.borrow().mutations.contains(&"unregister"));
    os.0.borrow_mut().fail_stop = false;
    let mut resumed = Controller::new(store, os.clone());
    assert_eq!(resumed.reconcile().unwrap(), State::Disabled);
    assert_eq!(
        os.0.borrow().mutations,
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
    assert!(os.0.borrow().mutations.is_empty());
}
#[test]
fn foreign_service_is_never_stopped_or_adopted() {
    let store = Store::default();
    let os = Fake::default();
    let mut c = Controller::new(store, os.clone());
    c.enable(binding()).unwrap();
    os.0.borrow_mut().foreign = true;
    os.0.borrow_mut().mutations.clear();
    assert!(c.remove().is_err());
    assert!(os.0.borrow().mutations.is_empty());
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
    assert_eq!(
        store.0.borrow().saved.as_ref().unwrap().intent,
        Intent::Enabled
    );
    assert_eq!(controller.platform.0.mutations, ["register"]);
    controller.platform.0.status = macos::Status::Enabled;
    assert_eq!(controller.reconcile().unwrap(), State::Running);
    assert_eq!(controller.disable().unwrap(), State::Disabled);
    let before = controller.platform.0.mutations.clone();
    assert_eq!(controller.reconcile().unwrap(), State::Disabled);
    assert_eq!(controller.platform.0.mutations, before);
}
