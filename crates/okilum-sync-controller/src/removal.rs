//! Cooperative removal across independent journals. Local stop is attempted
//! even if folder cleanup or remote revocation fails. Canonical files are retained.
use crate::{
    enrollment::{Enrollment, PairingService, Removal},
    folder::FolderController,
    runtime::{Runtime, Selection},
};
use anyhow::Result;

#[derive(Debug, Default)]
pub struct Outcome {
    pub local_stopped: bool,
    pub revoked: bool,
    pub folder_cleaned: bool,
    pub external_sync_retained: bool,
    pub errors: Vec<String>,
}
impl Outcome {
    /// An owned daemon can retain dormant private config after being permanently
    /// disabled. Reuse requires cleanup because the external daemon keeps running.
    pub fn complete(&self) -> bool {
        self.local_stopped && self.revoked && (!self.external_sync_retained || self.folder_cleaned)
    }
}

pub fn remove(
    runtime: &Runtime,
    enrollment: &Enrollment,
    folder: &FolderController,
    service: &impl PairingService,
) -> Outcome {
    let mut outcome = Outcome::default();
    // Read failure is not permission to assume ownership of an external daemon.
    let (snapshot, snapshot_known) = match runtime.snapshot() {
        Ok(snapshot) => (snapshot, true),
        Err(error) => {
            outcome.errors.push(error.to_string());
            (None, false)
        }
    };
    outcome.external_sync_retained = !snapshot_known
        || snapshot
            .as_ref()
            .is_some_and(|s| matches!(s.selection, Selection::Reuse(_)));
    // All durable denies precede network effects. Do not short-circuit: even a
    // damaged grant/folder journal must not prevent stopping an owned service.
    // An unreadable runtime must not block independent denies or revocation,
    // but cannot establish local shutdown or external-daemon ownership.
    let mut denied = snapshot_known;
    for result in [
        runtime.request_remove(),
        enrollment.request_remove(),
        folder.request_remove(),
    ] {
        if let Err(error) = result {
            denied = false;
            outcome.errors.push(error.to_string());
        }
    }
    match folder.exists() {
        Ok(false) => outcome.folder_cleaned = true,
        Ok(true) => match folder.remove() {
            Ok(status) => outcome.folder_cleaned = status.removed,
            Err(error) => outcome.errors.push(error.to_string()),
        },
        Err(error) => outcome.errors.push(error.to_string()),
    }
    match runtime.disable() {
        Ok(_) => outcome.local_stopped = denied && (snapshot.is_some() || outcome.folder_cleaned),
        Err(error) => outcome.errors.push(error.to_string()),
    }
    // Network revocation follows local stop. Retain the pairing journal so a
    // later retry can revoke without re-registering the stopped user service.
    match revoke(enrollment, service) {
        Ok(revoked) => outcome.revoked = revoked,
        Err(error) => outcome.errors.push(error.to_string()),
    }
    outcome
}
fn revoke(enrollment: &Enrollment, service: &impl PairingService) -> Result<bool> {
    if enrollment.snapshot()?.is_none() {
        return Ok(true);
    }
    Ok(matches!(
        enrollment.remove(service)?.removal,
        Some(Removal::Revoked | Removal::CancelledWithoutGrant)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        pairing::{Approval, Registration, Session, State, Status},
        private,
    };
    use std::{cell::Cell, fs};

    struct LostExchange {
        removals: Cell<usize>,
    }
    impl PairingService for LostExchange {
        fn origin(&self) -> &str {
            "https://removal.example.test"
        }
        fn start(&self, _: &Session) -> Result<Approval> {
            anyhow::bail!("lost start response")
        }
        fn exchange(&self, _: &Session) -> Result<Option<Registration>> {
            anyhow::bail!("lost grant response")
        }
        fn status(&self, _: &Session) -> Result<Status> {
            panic!("removal must not resume pairing")
        }
        fn remove(&self, _: &Session) -> Result<State> {
            self.removals.set(self.removals.get() + 1);
            Ok(State::Revoked)
        }
    }

    #[test]
    fn corrupt_runtime_does_not_block_revocation_or_delete_local_notes() -> Result<()> {
        let root = tempfile::tempdir()?;
        let state = root.path().join("runtime");
        private::directory(&state)?;
        private::write(&state.join("runtime.json"), b"{broken")?;
        let runtime = Runtime::new(state.clone(), root.path().join("units"));
        assert!(runtime.snapshot().is_err());
        let note = root.path().join("local-note.md");
        fs::write(&note, b"canonical note remains")?;
        let enrollment = Enrollment::new(root.path().join("enrollment"));
        let service = LostExchange {
            removals: Cell::new(0),
        };
        assert!(enrollment
            .begin(&service, &["AAAAAAA"; 8].join("-"), "Removal fixture")
            .is_err());
        // A lost exchange may have minted a grant and therefore requires revoke.
        assert!(enrollment.poll(&service).is_err());
        assert_eq!(service.removals.get(), 0);
        let folder = FolderController::new(root.path().join("folder"));
        let outcome = remove(&runtime, &enrollment, &folder, &service);
        assert_eq!(service.removals.get(), 1, "revocation must be attempted");
        assert!(outcome.revoked);
        assert!(!outcome.local_stopped && !outcome.complete());
        assert!(outcome.external_sync_retained);
        assert!(!outcome.errors.is_empty());
        assert_eq!(
            enrollment.snapshot()?.unwrap().removal,
            Some(Removal::Revoked)
        );
        assert_eq!(fs::read(&note)?, b"canonical note remains");
        assert_eq!(fs::read(state.join("runtime.json"))?, b"{broken");
        let retry = remove(&runtime, &enrollment, &folder, &service);
        assert!(retry.revoked && !retry.complete());
        assert_eq!(service.removals.get(), 1, "terminal revoke is idempotent");
        Ok(())
    }
}
