//! Durable desktop pairing. The private journal precedes every operation that
//! can issue or revoke authority, so a lost response never loses its credential.
use crate::{
    pairing::{Approval, Descriptor, Registration, Service, Session, State, Status},
    private,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub trait PairingService {
    fn origin(&self) -> &str;
    fn start(&self, session: &Session) -> Result<Approval>;
    fn exchange(&self, session: &Session) -> Result<Option<Registration>>;
    fn status(&self, session: &Session) -> Result<Status>;
    fn remove(&self, session: &Session) -> Result<State>;
}
impl PairingService for Service {
    fn origin(&self) -> &str {
        self.origin()
    }
    fn start(&self, s: &Session) -> Result<Approval> {
        self.start(s)
    }
    fn exchange(&self, s: &Session) -> Result<Option<Registration>> {
        self.exchange(s)
    }
    fn status(&self, s: &Session) -> Result<Status> {
        self.status(s)
    }
    fn remove(&self, s: &Session) -> Result<State> {
        self.remove(s)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Intent {
    Pair,
    Remove,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Removal {
    Pending,
    Revoked,
    CancelledWithoutGrant,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    origin: String,
    session: Session,
    intent: Intent,
    exchange_started: bool,
    vault: Option<Uuid>,
    descriptor: Option<Descriptor>,
    registration: Option<Registration>,
    removal: Option<Removal>,
}
/// Public status never exposes either credential. Local setup cancellation is
/// deliberately distinct from confirmed revocation by the service.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub request_id: Uuid,
    pub intent: Intent,
    pub registration: Option<Registration>,
    pub descriptor: Option<Descriptor>,
    pub removal: Option<Removal>,
}
pub struct Enrollment {
    state: PathBuf,
}
impl Enrollment {
    pub fn new(state: PathBuf) -> Self {
        Self { state }
    }
    /// Explicit Enable only. A retry must use the original service and identity.
    pub fn begin(
        &self,
        service: &impl PairingService,
        device: &str,
        name: &str,
    ) -> Result<Approval> {
        let _lock = private::lock(&self.state)?;
        let mut journal = if self.file().try_exists()? {
            self.load(service)?
        } else {
            let j = Journal {
                origin: service.origin().to_owned(),
                session: Session::new(device.to_owned(), name.to_owned())?,
                intent: Intent::Pair,
                exchange_started: false,
                vault: None,
                descriptor: None,
                registration: None,
                removal: None,
            };
            self.save(&j)?;
            j
        };
        ensure!(
            journal.session.device_id() == device && journal.session.name() == name,
            "pairing identity differs from saved request"
        );
        ensure!(
            journal.intent == Intent::Pair && journal.removal.is_none(),
            "pairing was removed; fresh approval required"
        );
        let approval = service.start(&journal.session)?;
        // Rejected/expired requests remain durable; do not silently mint another.
        if approval.request.state == "cancelled" {
            journal.intent = Intent::Remove;
            self.save(&journal)?;
        }
        Ok(approval)
    }
    /// Recover an already minted grant before attempting the expiring exchange.
    /// A status/network failure must not erase credentials or scope bindings.
    pub fn poll(&self, service: &impl PairingService) -> Result<Snapshot> {
        let _lock = private::lock(&self.state)?;
        let mut j = self.load(service)?;
        if j.intent == Intent::Remove {
            return self.remove_locked(service, &mut j);
        }
        if j.exchange_started {
            if let Ok(status) = service.status(&j.session) {
                self.accept(&mut j, status)?;
                return Ok(snapshot(&j));
            }
            // Once bound, expiration cannot be repaired by another exchange.
            ensure!(
                j.registration.is_none(),
                "grant status unavailable; retry without re-enrollment"
            );
        }
        let prior_uncertain_exchange = j.exchange_started;
        j.exchange_started = true;
        self.save(&j)?;
        if let Some(registration) = service.exchange(&j.session)? {
            bind(&mut j, &registration)?;
            j.registration = Some(registration);
            self.save(&j)?;
            let status = service.status(&j.session)?;
            self.accept(&mut j, status)?;
        } else if !prior_uncertain_exchange {
            // A completed unapproved exchange issued nothing. Keep uncertainty
            // from any older lost response: that request could still be running.
            j.exchange_started = false;
            self.save(&j)?;
        }
        Ok(snapshot(&j))
    }
    /// Persists cancellation first. Even a failed exchange may have issued a
    /// grant; failed revocation stays pending and is retried after restart.
    pub fn remove(&self, service: &impl PairingService) -> Result<Snapshot> {
        let _lock = private::lock(&self.state)?;
        let mut j = self.load(service)?;
        j.intent = Intent::Remove;
        self.save(&j)?;
        self.remove_locked(service, &mut j)
    }
    pub fn snapshot(&self) -> Result<Option<Snapshot>> {
        if !self.file().try_exists()? {
            return Ok(None);
        }
        let j: Journal = serde_json::from_slice(&private::read(&self.file())?)?;
        Ok(Some(snapshot(&j)))
    }
    fn remove_locked(&self, service: &impl PairingService, j: &mut Journal) -> Result<Snapshot> {
        if j.removal == Some(Removal::Revoked) || j.removal == Some(Removal::CancelledWithoutGrant)
        {
            return Ok(snapshot(j));
        }
        if !j.exchange_started {
            j.removal = Some(Removal::CancelledWithoutGrant);
        } else {
            j.removal = Some(Removal::Pending);
            self.save(j)?;
            // Keep Pending on network failure or unknown grant. Never exchange
            // after Remove merely to manufacture a revocation receipt.
            if service.remove(&j.session)? == State::Revoked {
                j.removal = Some(Removal::Revoked);
            }
        }
        self.save(j)?;
        Ok(snapshot(j))
    }
    fn accept(&self, j: &mut Journal, status: Status) -> Result<()> {
        bind(j, &status.registration)?;
        if let Some(d) = status.descriptor {
            d.validate()?;
            ensure!(
                j.descriptor.as_ref().is_none_or(|old| old == &d),
                "vault connection scope changed"
            );
            j.descriptor = Some(d);
        }
        if matches!(
            status.registration.state,
            State::RemovalPending | State::Revoked
        ) {
            j.intent = Intent::Remove;
            j.removal = Some(if status.registration.state == State::Revoked {
                Removal::Revoked
            } else {
                Removal::Pending
            });
        }
        j.registration = Some(status.registration);
        self.save(j)
    }
    fn file(&self) -> PathBuf {
        self.state.join("pairing.json")
    }
    fn load(&self, service: &impl PairingService) -> Result<Journal> {
        let j: Journal = serde_json::from_slice(&private::read(&self.file())?)
            .context("read durable pairing")?;
        ensure!(
            j.origin == service.origin(),
            "pairing service differs from saved origin"
        );
        Ok(j)
    }
    fn save(&self, j: &Journal) -> Result<()> {
        private::write(&self.file(), &serde_json::to_vec(j)?)
    }
    /// A fresh scoped observation; secrets remain inside the enrollment journal.
    pub fn readiness(&self, service: &Service) -> Result<crate::readiness::Receipt> {
        let _lock = private::lock(&self.state)?;
        let j = self.load(service)?;
        ensure!(
            j.intent == Intent::Pair && j.removal.is_none(),
            "registration is being removed"
        );
        let registration = j
            .registration
            .as_ref()
            .context("registration unavailable")?;
        let descriptor = j.descriptor.as_ref().context("descriptor unavailable")?;
        service.readiness(&j.session, registration, descriptor)
    }
    pub fn state_directory(&self) -> &Path {
        &self.state
    }
}
fn bind(j: &mut Journal, r: &Registration) -> Result<()> {
    ensure!(
        r.id == j.session.id() && r.device_id == j.session.device_id() && !r.vault.is_nil(),
        "grant identity differs"
    );
    ensure!(
        j.vault.is_none_or(|old| old == r.vault),
        "grant vault scope changed"
    );
    j.vault = Some(r.vault);
    Ok(())
}
fn snapshot(j: &Journal) -> Snapshot {
    Snapshot {
        request_id: j.session.id(),
        intent: j.intent,
        registration: j.registration.clone(),
        descriptor: if j.intent == Intent::Pair {
            j.descriptor.clone()
        } else {
            None
        },
        removal: j.removal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pairing::Request;
    use std::cell::{Cell, RefCell};
    struct Fake {
        origin: String,
        registration: RefCell<Option<Registration>>,
        vault: Cell<Uuid>,
        lost_exchange: Cell<bool>,
        offline: Cell<bool>,
        exchanges: Cell<u32>,
        unapproved: Cell<bool>,
        descriptor: RefCell<Descriptor>,
    }
    impl Fake {
        fn new() -> Self {
            Self {
                origin: "https://sync.example.test".into(),
                registration: RefCell::new(None),
                vault: Cell::new(Uuid::new_v4()),
                lost_exchange: Cell::new(false),
                offline: Cell::new(false),
                exchanges: Cell::new(0),
                unapproved: Cell::new(false),
                descriptor: RefCell::new(Descriptor {
                    folder_id: "fixture".into(),
                    hub_device_id: ["BBBBBBB"; 8].join("-"),
                    hub_address: "tcp://127.0.0.1:22000".into(),
                    ignores: vec![],
                }),
            }
        }
    }
    impl PairingService for Fake {
        fn origin(&self) -> &str {
            &self.origin
        }
        fn start(&self, s: &Session) -> Result<Approval> {
            Ok(Approval {
                approval_url: format!("{}/#sync={}", self.origin, s.id()),
                request: Request {
                    id: s.id(),
                    device_id: s.device_id().into(),
                    name: s.name().into(),
                    code: "12345678".into(),
                    state: "pending".into(),
                    expires: 123,
                },
            })
        }
        fn exchange(&self, s: &Session) -> Result<Option<Registration>> {
            self.exchanges.set(self.exchanges.get() + 1);
            if self.unapproved.get() {
                return Ok(None);
            }
            let r = Registration {
                id: s.id(),
                vault: self.vault.get(),
                device_id: s.device_id().into(),
                name: s.name().into(),
                state: State::HubReady,
                last_error: None,
            };
            *self.registration.borrow_mut() = Some(r.clone());
            ensure!(
                !self.lost_exchange.get(),
                "lost response after grant committed"
            );
            Ok(Some(r))
        }
        fn status(&self, _: &Session) -> Result<Status> {
            ensure!(!self.offline.get(), "offline");
            let mut r = self.registration.borrow().clone().context("no grant")?;
            r.vault = self.vault.get();
            Ok(Status {
                registration: r,
                descriptor: Some(self.descriptor.borrow().clone()),
            })
        }
        fn remove(&self, _: &Session) -> Result<State> {
            ensure!(!self.offline.get(), "offline");
            self.registration
                .borrow_mut()
                .as_mut()
                .context("no grant")?
                .state = State::Revoked;
            Ok(State::Revoked)
        }
    }
    fn begin(e: &Enrollment, f: &Fake) -> Result<()> {
        e.begin(f, &["AAAAAAA"; 8].join("-"), "Laptop")?;
        Ok(())
    }
    #[test]
    fn lost_exchange_recovers_without_reexchange_after_restart() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let e = Enrollment::new(dir.path().join("pair"));
        let f = Fake::new();
        assert!(e.snapshot()?.is_none());
        assert!(!e.state_directory().exists());
        begin(&e, &f)?;
        f.lost_exchange.set(true);
        assert!(e.poll(&f).is_err());
        let reopened = Enrollment::new(e.state.clone());
        assert!(reopened.poll(&f)?.descriptor.is_some());
        assert_eq!(f.exchanges.get(), 1);
        f.vault.set(Uuid::new_v4());
        assert!(reopened.poll(&f).is_err());
        assert_eq!(f.exchanges.get(), 1);
        Ok(())
    }
    #[test]
    fn offline_remove_is_durable_and_terminal_even_after_lost_exchange() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let e = Enrollment::new(dir.path().join("pair"));
        let f = Fake::new();
        begin(&e, &f)?;
        f.lost_exchange.set(true);
        assert!(e.poll(&f).is_err());
        f.offline.set(true);
        assert!(e.remove(&f).is_err());
        assert_eq!(e.snapshot()?.unwrap().removal, Some(Removal::Pending));
        let reopened = Enrollment::new(e.state.clone());
        assert!(begin(&reopened, &f).is_err());
        f.offline.set(false);
        let result = reopened.poll(&f)?;
        assert_eq!(result.removal, Some(Removal::Revoked));
        assert!(result.descriptor.is_none());
        assert_eq!(f.exchanges.get(), 1);
        Ok(())
    }
    #[test]
    fn preexchange_cancel_is_not_reported_as_hub_revocation() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let e = Enrollment::new(dir.path().join("pair"));
        let f = Fake::new();
        begin(&e, &f)?;
        assert_eq!(e.remove(&f)?.removal, Some(Removal::CancelledWithoutGrant));
        assert_eq!(e.poll(&f)?.removal, Some(Removal::CancelledWithoutGrant));
        assert_eq!(f.exchanges.get(), 0);
        assert!(begin(&e, &f).is_err());
        Ok(())
    }
    #[test]
    fn unapproved_poll_can_cancel_but_never_erases_an_older_uncertain_exchange() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let e = Enrollment::new(dir.path().join("clean"));
        let f = Fake::new();
        f.unapproved.set(true);
        begin(&e, &f)?;
        e.poll(&f)?;
        assert_eq!(e.remove(&f)?.removal, Some(Removal::CancelledWithoutGrant));
        let e = Enrollment::new(dir.path().join("uncertain"));
        f.unapproved.set(false);
        f.lost_exchange.set(true);
        begin(&e, &f)?;
        assert!(e.poll(&f).is_err());
        // Model an earlier still-running call: current status has no grant,
        // while the fresh exchange reports unapproved. That is not proof that
        // the timed-out call can no longer issue authority.
        *f.registration.borrow_mut() = None;
        f.unapproved.set(true);
        e.poll(&f)?;
        assert!(e.remove(&f).is_err());
        assert_eq!(e.snapshot()?.unwrap().removal, Some(Removal::Pending));
        Ok(())
    }
    #[test]
    fn service_and_connection_scope_cannot_be_rebound() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let e = Enrollment::new(dir.path().join("pair"));
        let mut f = Fake::new();
        begin(&e, &f)?;
        e.poll(&f)?;
        f.descriptor.borrow_mut().folder_id = "other".into();
        assert!(e.poll(&f).is_err());
        f.origin = "https://other.example.test".into();
        assert!(e.poll(&f).is_err());
        assert_eq!(f.exchanges.get(), 1);
        Ok(())
    }
}
