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
