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
    let snapshot = match runtime.snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            outcome.errors.push(error.to_string());
            return outcome;
        }
    };
    outcome.external_sync_retained = snapshot
        .as_ref()
        .is_some_and(|s| matches!(s.selection, Selection::Reuse(_)));
    // All durable denies precede network effects. Do not short-circuit: even a
    // damaged grant/folder journal must not prevent stopping an owned service.
    let mut denied = true;
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
