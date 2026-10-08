//! Portable managed-sidecar lifecycle contract. Platform bindings are injected;
//! this module is not wired into Reader startup or installer hooks.
//! The journal implementation must hold an exclusive per-user instance lock for
//! the controller's lifetime and durably persist intent outside the install tree.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(unix)]
pub mod journal;
pub mod macos;
pub mod supervisor;
pub mod windows;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub instance: Uuid,
    pub installation: Uuid,
    /// Current user's SID on Windows; numeric UID on macOS.
    pub owner: String,
    /// Dedicated supervisor, never Reader or a user-provided shell command.
    pub supervisor: String,
    /// Private durable state, separate from the installation and vault/index.
    pub state_directory: String,
    /// Existing device certificate identity. Re-enable cannot generate a new one.
    pub device_identity: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Intent {
    Enabled,
    Disabled,
    Removed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    pub binding: Binding,
    pub intent: Intent,
}

/// Implementations own the exclusive instance lock. `save` must atomically
/// replace and flush durable state before returning; failure prevents OS effects.
/// Windows implementations must enforce owner DACLs, not emulate chmod with a no-op.
pub trait LockedJournal {
    fn load(&self) -> Result<Option<Journal>>;
    fn save(&mut self, journal: &Journal) -> Result<()>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Registration {
    Absent,
    ApprovalRequired,
    Stopped,
    Running,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Disabled,
    ApprovalRequired,
    Running,
    Removed,
}

/// Every mutating method must revalidate ownership immediately at the native
/// boundary. A name or PID alone is never evidence that Tessera owns a service.
pub trait Platform {
    fn inspect(&mut self, binding: &Binding) -> Result<Registration>;
    /// Check signature/provenance, pinned sidecar version and saved device ID.
    /// Never generates identity, downloads a payload or launches a process.
    fn verify_payload(&mut self, binding: &Binding) -> Result<()>;
    fn register(&mut self, binding: &Binding) -> Result<()>;
    fn start(&mut self, binding: &Binding) -> Result<()>;
    /// Must confirm supervisor AND its owned child have exited before success.
    fn stop(&mut self, binding: &Binding) -> Result<()>;
    fn unregister(&mut self, binding: &Binding) -> Result<()>;
}

pub struct Controller<J, P> {
    journal: J,
    platform: P,
}
impl<J: LockedJournal, P: Platform> Controller<J, P> {
    pub fn new(journal: J, platform: P) -> Self {
        Self { journal, platform }
    }
    pub fn snapshot(&self) -> Result<Option<Journal>> {
        self.journal.load()
    }
    /// Called only after explicit Enable; preparation/signature verification is
    /// a separate opt-in stage. This cannot adopt an external runtime.
    pub fn enable(&mut self, binding: Binding) -> Result<State> {
        if let Some(saved) = self.journal.load()? {
            ensure!(
                saved.intent != Intent::Removed,
                "removed instance cannot be enabled"
            );
            ensure!(
                saved.binding == binding,
                "managed binding changed; recovery required"
            );
        }
        self.platform.verify_payload(&binding)?;
        self.journal.save(&Journal {
            binding,
            intent: Intent::Enabled,
        })?;
        self.reconcile()
    }
    pub fn disable(&mut self) -> Result<State> {
        self.set_intent(Intent::Disabled)
    }
    /// Local removal only. Hub revocation must remain a separate durable pending
    /// operation and is never awaited in an installer/uninstaller callback.
    pub fn remove(&mut self) -> Result<State> {
        self.set_intent(Intent::Removed)
    }
    fn set_intent(&mut self, intent: Intent) -> Result<State> {
        let Some(mut saved) = self.journal.load()? else {
            return Ok(State::Disabled);
        };
        if saved.intent != Intent::Removed {
            saved.intent = intent;
        }
        self.journal.save(&saved)?;
        self.reconcile()
    }
    pub fn reconcile(&mut self) -> Result<State> {
        let Some(saved) = self.journal.load()? else {
            return Ok(State::Disabled);
        };
        let binding = &saved.binding;
        let mut registration = self.platform.inspect(binding)?;
        if saved.intent == Intent::Enabled {
            // Recheck on restart too; a prepared journal cannot bypass signature
            // or identity checks after a payload is replaced on disk.
            self.platform.verify_payload(binding)?;
            if registration == Registration::Absent {
                self.platform.register(binding)?;
                registration = self.platform.inspect(binding)?;
            }
            if registration == Registration::ApprovalRequired {
                return Ok(State::ApprovalRequired);
            }
            ensure!(
                registration != Registration::Absent,
                "registration did not persist"
            );
            if registration == Registration::Stopped {
                self.platform.start(binding)?;
            }
            ensure!(
                self.platform.inspect(binding)? == Registration::Running,
                "service did not start"
            );
            Ok(State::Running)
        } else {
            if registration != Registration::Absent {
                self.platform.stop(binding)?;
                self.platform.unregister(binding)?;
            }
            ensure!(
                self.platform.inspect(binding)? == Registration::Absent,
                "removal needs reconciliation"
            );
            Ok(if saved.intent == Intent::Removed {
                State::Removed
            } else {
                State::Disabled
            })
        }
    }
}

pub(crate) fn safe_text(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && !value.chars().any(char::is_control),
        "invalid service field"
    );
    Ok(())
}
pub(crate) fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests;
