//! Portable managed-sidecar lifecycle contract. Platform bindings are injected;
//! this module is not wired into Reader startup or installer hooks.
//! Authority is a revision-bound journal taken in short exclusive transactions,
//! durably persisted outside the install tree (see `authority` and `store`).
use anyhow::{ensure, Context, Result};
use authority::{Envelope, StopToken, Stored};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use supervisor::ipc::Scope;
use uuid::Uuid;

pub mod authority;
#[cfg(unix)]
pub mod journal;
pub mod macos;
#[cfg(unix)]
pub mod store;
pub mod supervisor;
pub mod update;
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
    /// Verified native generation of the live supervisor, taken from captured
    /// handles and an authenticated endpoint, never a PID or public file. None
    /// when this platform has no authenticated IPC channel; the controller then
    /// stops natively under its lock instead of sending a token.
    fn supervisor_scope(&mut self, binding: &Binding) -> Result<Option<Scope>>;
    /// Send protocol-v2 Stop carrying `token` and confirm the supervisor AND its
    /// owned child exited. The controller holds no lock here: the server takes it.
    fn stop_supervisor(&mut self, binding: &Binding, token: &StopToken) -> Result<()>;
    fn unregister(&mut self, binding: &Binding) -> Result<()>;
}

/// Short exclusive transactions over the revision-bound journal. The lock is
/// held for one transaction only and is never held across an IPC wait.
pub trait Authority {
    type Tx<'a>: Tx
    where
        Self: 'a;
    /// Take the exclusive instance lock or fail at the absolute `deadline`.
    fn begin(&mut self, deadline: Instant) -> Result<Self::Tx<'_>>;
}
pub trait Tx {
    fn stored(&self) -> Result<&Stored>;
    /// Atomic, flushed replacement that must be the exact successor of `stored`;
    /// failure permits no native effect.
    fn commit(&mut self, next: Envelope) -> Result<()>;
    /// Explicit, idempotent migration of a legacy journal; never arms a stop.
    fn migrate(&mut self) -> Result<Envelope>;
}
/// One absolute budget per control operation, shared by every lock wait in it.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(30);

pub struct Controller<A, P> {
    authority: A,
    platform: P,
    budget: Duration,
}
impl<A: Authority, P: Platform> Controller<A, P> {
    pub fn new(authority: A, platform: P) -> Self {
        Self {
            authority,
            platform,
            budget: DEFAULT_BUDGET,
        }
    }
    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }
    /// Read-only: a legacy journal stays legacy and nothing is created.
    pub fn snapshot(&mut self) -> Result<Stored> {
        let deadline = Instant::now() + self.budget;
        Ok(self.authority.begin(deadline)?.stored()?.clone())
    }
    /// The envelope for mutation. A legacy journal is migrated first (one flushed
    /// replacement, no effect); an absent one yields None.
    fn current(tx: &mut impl Tx) -> Result<Option<Envelope>> {
        current_envelope(tx)
    }
    /// Called only after explicit Enable; preparation/signature verification is
    /// a separate opt-in stage. This cannot adopt an external runtime.
    pub fn enable(&mut self, binding: Binding) -> Result<State> {
        let deadline = Instant::now() + self.budget;
        {
            let mut tx = self.authority.begin(deadline)?;
            let next = match Self::current(&mut tx)? {
                None => {
                    self.platform.verify_payload(&binding)?;
                    Envelope::first(binding)
                }
                Some(saved) => {
                    ensure!(
                        saved.intent() != Intent::Removed,
                        "removed instance cannot be enabled"
                    );
                    ensure!(
                        *saved.binding() == binding,
                        "managed binding changed; recovery required"
                    );
                    self.platform.verify_payload(&binding)?;
                    saved.enable()?
                }
            };
            tx.commit(next)?;
        }
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
    /// Prepare under the lock: the new intent and, when an authenticated
    /// supervisor generation is live, a fresh stop token in one replacement.
    /// Nothing native happens here; `reconcile` carries the effects.
    fn set_intent(&mut self, intent: Intent) -> Result<State> {
        let deadline = Instant::now() + self.budget;
        {
            let mut tx = self.authority.begin(deadline)?;
            let Some(saved) = Self::current(&mut tx)? else {
                return Ok(State::Disabled);
            };
            let live = Self::live_scope(&mut self.platform, saved.binding());
            let next = match &live {
                Ok(Some(scope)) if intent == Intent::Removed => saved.remove(scope.clone())?.0,
                Ok(Some(scope)) => saved.disable(scope.clone())?.0,
                // No live generation, or it could not be read: still record the
                // intent so it is replayed, then surface the error below.
                _ => saved.set_intent(intent)?,
            };
            tx.commit(next)?;
            live?;
        }
        self.reconcile()
    }
    /// Verified supervisor generation, or None when nothing is running or the
    /// platform has no authenticated IPC channel.
    fn live_scope(platform: &mut P, binding: &Binding) -> Result<Option<Scope>> {
        if platform.inspect(binding)? == Registration::Running {
            platform.supervisor_scope(binding)
        } else {
            Ok(None)
        }
    }

    pub fn reconcile(&mut self) -> Result<State> {
        let deadline = Instant::now() + self.budget;
        let mut tx = self.authority.begin(deadline)?;
        let Some(saved) = Self::current(&mut tx)? else {
            return Ok(State::Disabled);
        };
        let binding = saved.binding().clone();
        let mut registration = self.platform.inspect(&binding)?;
        if saved.intent() == Intent::Enabled {
            // Effects run under the lock, so a concurrent Disable cannot interleave.
            // Recheck on restart too; a prepared journal cannot bypass signature
            // or identity checks after a payload is replaced on disk.
            self.platform.verify_payload(&binding)?;
            if registration == Registration::Absent {
                self.platform.register(&binding)?;
                registration = self.platform.inspect(&binding)?;
            }
            if registration == Registration::ApprovalRequired {
                return Ok(State::ApprovalRequired);
            }
            ensure!(
                registration != Registration::Absent,
                "registration did not persist"
            );
            if registration == Registration::Stopped {
                self.platform.start(&binding)?;
            }
            ensure!(
                self.platform.inspect(&binding)? == Registration::Running,
                "service did not start"
            );
            return Ok(State::Running);
        }
        let terminal = if saved.intent() == Intent::Removed {
            State::Removed
        } else {
            State::Disabled
        };
        if registration == Registration::Absent {
            return Ok(terminal);
        }
        let scope = if registration == Registration::Running {
            self.platform.supervisor_scope(&binding)?
        } else {
            None
        };
        let Some(scope) = scope else {
            // Nothing running, or no authenticated IPC channel: stop natively
            // and unregister under this one lock so authority cannot change.
            self.platform.stop(&binding)?;
            self.platform.unregister(&binding)?;
            return Self::finish_removal(&mut self.platform, &binding, terminal);
        };
        // A stored operation for this generation is a retry and is reused as is;
        // anything else is armed fresh (crash recovery, new generation).
        let token = match saved.stop() {
            Some(op) if op.scope == scope => {
                let token = StopToken {
                    journal_epoch: saved.journal_epoch(),
                    operation: op.clone(),
                };
                saved.authorize(&token, &scope)?;
                token
            }
            _ => {
                let (next, token) = saved.arm_recovery(scope.clone())?;
                tx.commit(next)?;
                token
            }
        };
        // Release the lock before IPC: the supervisor takes it to authorize.
        drop(tx);
        self.platform.stop_supervisor(&binding, &token)?;
        let mut tx = self.authority.begin(deadline)?;
        let current = Self::current(&mut tx)?.context("journal disappeared during stop")?;
        // A stale reply (Enable, new intent, new operation) has no authority to
        // unregister, even when its Intent and Binding still match.
        current.authorize(&token, &scope)?;
        ensure!(
            *current.binding() == binding,
            "managed binding changed; recovery required"
        );
        self.platform.unregister(&binding)?;
        tx.commit(current.complete_stop(&token, &scope)?)?;
        Self::finish_removal(&mut self.platform, &binding, terminal)
    }
    fn finish_removal(platform: &mut P, binding: &Binding, terminal: State) -> Result<State> {
        ensure!(
            platform.inspect(binding)? == Registration::Absent,
            "removal needs reconciliation"
        );
        Ok(terminal)
    }
}

/// The envelope for mutation. A legacy journal is migrated first (one flushed
/// replacement, no effect); an absent one yields None.
pub(crate) fn current_envelope(tx: &mut impl Tx) -> Result<Option<Envelope>> {
    Ok(match tx.stored()?.clone() {
        Stored::Absent => None,
        Stored::Legacy { .. } => Some(tx.migrate()?),
        Stored::Current(envelope) => Some(envelope),
    })
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
mod testing;
#[cfg(test)]
mod tests;
