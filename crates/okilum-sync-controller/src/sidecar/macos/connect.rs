//! Explicit Connect after preparation: the one registration dispatch, the wait for
//! its helper generation and the authenticated REST readback. No native effects
//! live here, so offline OS fakes run the same timing on Linux test hosts (#2589).
use super::{recovery::ManagedStatus, Status};
use crate::{
    daemon::{DaemonIdentity, CLIENT_VERSION},
    private,
};
use anyhow::{ensure, Context, Result};
use std::{
    io::Read,
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub enum ConnectOutcome {
    /// Both supervisor generation and REST device identity have been authenticated.
    Ready {
        config_path: PathBuf,
        device_id: String,
    },
    /// Durable prepared identity only. Caller must NOT set live configurations yet.
    RequiresApproval {
        config_path: PathBuf,
        device_id: String,
    },
}

/// Bounds of one explicit Connect. Preparation (inventory, signatures, a fresh
/// identity) ends at registration. The one dispatch and the wait for its helper
/// to authenticate have their own bound: on macOS the first launch of a freshly
/// registered agent and its runtime outlasted the whole former Connect budget,
/// and only a second Connect found it running (#2589).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectBudget {
    preparation: Duration,
    startup: Duration,
}
impl ConnectBudget {
    pub const MAX_PREPARATION: Duration = Duration::from_secs(60);
    pub const MAX_STARTUP: Duration = Duration::from_secs(180);
    /// The explicit Connect of Settings and `okilum sync connect`.
    pub const CONNECT: Self = Self {
        preparation: Self::MAX_PREPARATION,
        startup: Self::MAX_STARTUP,
    };
    pub fn new(preparation: Duration, startup: Duration) -> Result<Self> {
        ensure!(
            !preparation.is_zero() && preparation <= Self::MAX_PREPARATION,
            "Connect requires a bounded preparation budget of at most 60 seconds"
        );
        ensure!(
            !startup.is_zero() && startup <= Self::MAX_STARTUP,
            "Connect requires a bounded startup budget of at most 180 seconds"
        );
        Ok(Self {
            preparation,
            startup,
        })
    }
    pub fn preparation(self) -> Duration {
        self.preparation
    }
    pub fn startup(self) -> Duration {
        self.startup
    }
}

/// One REST readback of the prepared identity.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Readback {
    /// The exact identity and version answered on the literal loopback endpoint.
    Verified,
    /// Nothing answered yet. The helper reports Running as soon as it has spawned
    /// its runtime, before Syncthing listens. Only the startup deadline ends this.
    Unanswered,
}

/// The same observation mapping is used by production and offline OS adapters.
/// Service approval is permission, not proof of a running authenticated runtime.
pub(crate) trait ConnectRegistration {
    fn register_prepared(&mut self) -> Result<ManagedStatus>;
    fn native_status(&mut self) -> Result<Status>;
    /// Bounded by the whole startup deadline, never by a renewed per-poll budget.
    fn managed_status(&mut self, deadline: Instant) -> ManagedStatus;
    fn wait(&mut self) {
        std::thread::sleep(Duration::from_millis(250));
    }
}

pub(crate) fn complete_explicit_connect(
    registration: &mut impl ConnectRegistration,
    identity: DaemonIdentity,
    budget: ConnectBudget,
    mut readback: impl FnMut(&DaemonIdentity, Instant) -> Result<Readback>,
) -> Result<ConnectOutcome> {
    // The wait starts with this Connect's registration, never with what
    // preparation left over: the first launch is the slow one (#2589).
    let deadline = Instant::now()
        .checked_add(budget.startup())
        .context("startup deadline overflow")?;
    // RegistrationGuard consumes Ready exactly once. Waiting longer only observes
    // that dispatched/claimed generation; it never re-arms or registers again.
    let mut status = registration.register_prepared()?;
    let mut rest_unanswered = false;
    loop {
        ensure!(
            Instant::now() < deadline,
            "recovery pending: {} before the deadline",
            if rest_unanswered {
                "managed REST unavailable"
            } else {
                "Sync did not become authenticated"
            }
        );
        // The wait is long; turning Sync off ends it instead of outliving consent.
        ensure!(
            okilum_sync::NetworkAccess::current().allowed(),
            "Sync is disabled"
        );
        let native = registration.native_status()?;
        if native == Status::RequiresApproval {
            return Ok(ConnectOutcome::RequiresApproval {
                config_path: identity.config_file,
                device_id: identity.device_id,
            });
        }
        // Missing/removed registration cannot become Ready from a stale peer.
        ensure!(
            native == Status::Enabled,
            "recovery pending: Sync registration unavailable"
        );
        if status == ManagedStatus::Running {
            match readback(&identity, deadline)? {
                Readback::Verified => {
                    ensure!(
                        Instant::now() < deadline,
                        "REST readback exceeded Connect deadline"
                    );
                    return Ok(ConnectOutcome::Ready {
                        config_path: identity.config_file,
                        device_id: identity.device_id,
                    });
                }
                Readback::Unanswered => rest_unanswered = true,
            }
        }
        registration.wait();
        status = registration.managed_status(deadline);
    }
}

/// GET-only, deadline-bound readback. The private saved credential never leaves a
/// literal loopback endpoint, and redirects/proxies are disabled. Any answer must
/// be the exact identity and version; only no answer at all is left to the wait.
pub(crate) fn verify_rest(identity: &DaemonIdentity, deadline: Instant) -> Result<Readback> {
    identity.verify_local()?;
    let xml = xmltree::Element::parse(private::read(&identity.config_file)?.as_slice())?;
    let gui = xml.get_child("gui").context("GUI configuration missing")?;
    let address: std::net::SocketAddr = gui
        .get_child("address")
        .and_then(xmltree::Element::get_text)
        .context("REST address missing")?
        .parse()?;
    ensure!(
        address.ip().is_loopback() && address.port() != 0,
        "REST requires literal loopback"
    );
    let key = gui
        .get_child("apikey")
        .and_then(xmltree::Element::get_text)
        .context("REST credential missing")?;
    let mut header = reqwest::header::HeaderValue::from_str(&key)?;
    header.set_sensitive(true);
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    for (path, field, expected) in [
        ("status", "myID", identity.device_id.as_str()),
        ("version", "version", CLIENT_VERSION),
    ] {
        ensure!(
            okilum_sync::NetworkAccess::current().allowed(),
            "Sync is disabled"
        );
        let remaining = deadline.saturating_duration_since(Instant::now());
        ensure!(!remaining.is_zero(), "REST readback deadline elapsed");
        let Ok(response) = client
            .get(format!("http://{address}/rest/system/{path}"))
            .header("X-API-Key", header.clone())
            .timeout(remaining)
            .send()
        else {
            return Ok(Readback::Unanswered);
        };
        ensure!(
            response.status().is_success(),
            "managed REST authentication/readback failed"
        );
        let mut bytes = Vec::new();
        response.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= 1024 * 1024 && Instant::now() < deadline,
            "REST readback exceeded bounds"
        );
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        ensure!(
            value[field].as_str() == Some(expected),
            "managed REST identity/version changed"
        );
    }
    identity.verify_local()?;
    Ok(Readback::Verified)
}

#[cfg(test)]
#[path = "connect_tests.rs"]
mod tests;
