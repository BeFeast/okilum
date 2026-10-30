//! Public enrollment API must compile and preserve authority on both Unix hosts.
//! The service is injected: these tests never contact a hub or start Syncthing.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use anyhow::Result;
use okilum_sync_controller::{
    enrollment::{Enrollment, PairingService},
    pairing::{Approval, Registration, Session, State, Status},
};
use std::{
    cell::RefCell,
    fs,
    os::unix::fs::{symlink, MetadataExt, PermissionsExt},
    path::PathBuf,
};

struct LostReply {
    state: PathBuf,
    requests: RefCell<Vec<Vec<u8>>>,
}
impl LostReply {
    fn new(state: PathBuf) -> Self {
        Self {
            state,
            requests: RefCell::new(vec![]),
        }
    }
}
impl PairingService for LostReply {
    fn origin(&self) -> &str {
        "https://sync.example.test"
    }
    fn start(&self, session: &Session) -> Result<Approval> {
        // The request's credentials must already be durably stored before even
        // the first network call. Capture only in memory, never in test output.
        assert!(self.state.join("pairing.json").is_file());
        self.requests
            .borrow_mut()
            .push(serde_json::to_vec(session)?);
        anyhow::bail!("injected lost start reply")
    }
    fn exchange(&self, _: &Session) -> Result<Option<Registration>> {
        panic!("storage tests must not exchange a grant")
    }
    fn status(&self, _: &Session) -> Result<Status> {
        panic!("storage tests must not query a grant")
    }
    fn remove(&self, _: &Session) -> Result<State> {
        panic!("storage tests must not revoke a grant")
    }
}

fn begin(enrollment: &Enrollment, service: &LostReply) -> Result<Approval> {
    enrollment.begin(service, &["AAAAAAA"; 8].join("-"), "Fixture Mac")
}

#[test]
fn inspecting_an_unenabled_connection_does_not_prepare_state() -> Result<()> {
    let root = tempfile::tempdir()?;
    let state = root.path().join("pairing");
    let enrollment = Enrollment::new(state.clone());
    assert!(enrollment.snapshot()?.is_none());
    assert!(!state.exists());
    // Positive control: an explicit begin reaches the service after saving state.
    let service = LostReply::new(state.clone());
    assert!(begin(&enrollment, &service).is_err());
    assert_eq!(service.requests.borrow().len(), 1);
    assert!(enrollment.snapshot()?.is_some());
    Ok(())
}

#[test]
fn lost_start_reply_reuses_private_request_and_credentials_after_restart() -> Result<()> {
    let root = tempfile::tempdir()?;
    let state = root.path().join("pairing");
    let service = LostReply::new(state.clone());
    assert!(begin(&Enrollment::new(state.clone()), &service).is_err());
    let reopened = Enrollment::new(state.clone());
    let request_id = reopened.snapshot()?.unwrap().request_id;
    assert!(begin(&reopened, &service).is_err());
    assert_eq!(reopened.snapshot()?.unwrap().request_id, request_id);
    let requests = service.requests.borrow();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0] == requests[1],
        "retry replaced request credentials"
    );
    for (path, mode) in [(&state, 0o700), (&state.join("pairing.json"), 0o600)] {
        let metadata = fs::symlink_metadata(path)?;
        assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
        assert_eq!(metadata.permissions().mode() & 0o777, mode);
    }
    Ok(())
}

#[test]
fn unsafe_or_corrupt_storage_never_reaches_the_service() -> Result<()> {
    let root = tempfile::tempdir()?;
    let actual = root.path().join("actual");
    fs::create_dir(&actual)?;
    fs::set_permissions(&actual, fs::Permissions::from_mode(0o700))?;
    let alias = root.path().join("alias");
    symlink(&actual, &alias)?;
    for state in [&actual, &alias] {
        let service = LostReply::new(state.clone());
        if state == &actual {
            fs::set_permissions(state, fs::Permissions::from_mode(0o755))?;
        }
        assert!(begin(&Enrollment::new(state.clone()), &service).is_err());
        assert!(service.requests.borrow().is_empty());
        assert!(!actual.join("pairing.json").exists());
        fs::set_permissions(&actual, fs::Permissions::from_mode(0o700))?;
    }
    let service = LostReply::new(actual.clone());
    assert!(begin(&Enrollment::new(actual.clone()), &service).is_err());
    assert_eq!(service.requests.borrow().len(), 1);
    fs::write(actual.join("pairing.json"), b"{broken")?;
    let before = service.requests.borrow().len();
    assert!(begin(&Enrollment::new(actual.clone()), &service).is_err());
    assert_eq!(service.requests.borrow().len(), before);
    assert_eq!(fs::read(actual.join("pairing.json"))?, b"{broken");
    Ok(())
}

struct RemovalRecovery {
    start: LostReply,
    offline: std::cell::Cell<bool>,
    exchanges: std::cell::Cell<usize>,
    removals: RefCell<Vec<Vec<u8>>>,
}
impl PairingService for RemovalRecovery {
    fn origin(&self) -> &str {
        self.start.origin()
    }
    fn start(&self, session: &Session) -> Result<Approval> {
        self.start.start(session)
    }
    fn exchange(&self, _: &Session) -> Result<Option<Registration>> {
        self.exchanges.set(self.exchanges.get() + 1);
        anyhow::bail!("injected lost exchange response")
    }
    fn status(&self, _: &Session) -> Result<Status> {
        panic!("removal recovery must not resume pairing")
    }
    fn remove(&self, session: &Session) -> Result<State> {
        self.removals
            .borrow_mut()
            .push(serde_json::to_vec(session)?);
        if self.offline.get() {
            anyhow::bail!("fixture service offline");
        }
        Ok(State::Revoked)
    }
}

#[test]
fn lost_exchange_offline_removal_and_restart_preserve_credentials_and_unrelated_data() -> Result<()>
{
    use okilum_sync_controller::enrollment::{Intent, Removal};
    let root = tempfile::tempdir()?;
    let state = root.path().join("pairing");
    let unrelated = root.path().join("existing-syncthing-config.xml");
    let note = root.path().join("personal-note.md");
    fs::write(&unrelated, b"existing config must survive")?;
    fs::write(&note, b"unrelated note must survive")?;
    let service = RemovalRecovery {
        start: LostReply::new(state.clone()),
        offline: std::cell::Cell::new(true),
        exchanges: std::cell::Cell::new(0),
        removals: RefCell::new(vec![]),
    };
    let enrollment = Enrollment::new(state.clone());
    assert!(enrollment
        .begin(&service, &["AAAAAAA"; 8].join("-"), "Fixture Mac")
        .is_err());
    let request = enrollment.snapshot()?.unwrap().request_id;
    assert!(enrollment.poll(&service).is_err());
    assert_eq!(
        service.exchanges.get(),
        1,
        "positive control: uncertain exchange never fired"
    );
    enrollment.request_remove()?;
    drop(enrollment);
    let reopened = Enrollment::new(state.clone());
    assert_eq!(reopened.snapshot()?.unwrap().intent, Intent::Remove);
    assert!(reopened.poll(&service).is_err());
    assert_eq!(
        reopened.snapshot()?.unwrap().removal,
        Some(Removal::Pending)
    );
    drop(reopened);
    service.offline.set(false);
    let recovered = Enrollment::new(state);
    assert_eq!(recovered.poll(&service)?.removal, Some(Removal::Revoked));
    assert_eq!(recovered.snapshot()?.unwrap().request_id, request);
    assert_eq!(recovered.poll(&service)?.removal, Some(Removal::Revoked));
    assert_eq!(
        service.removals.borrow().len(),
        2,
        "terminal receipt must not revoke twice"
    );
    for removal in service.removals.borrow().iter() {
        assert!(
            removal == &service.start.requests.borrow()[0],
            "removal changed durable credentials"
        );
    }
    assert!(recovered
        .begin(&service, &["AAAAAAA"; 8].join("-"), "Fixture Mac")
        .is_err());
    assert_eq!(
        service.start.requests.borrow().len(),
        1,
        "removed request was silently reintroduced"
    );
    assert_eq!(service.exchanges.get(), 1, "removal must never re-exchange");
    assert_eq!(fs::read(&unrelated)?, b"existing config must survive");
    assert_eq!(fs::read(&note)?, b"unrelated note must survive");
    Ok(())
}
