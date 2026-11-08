//! LAN-first computer pairing, independent of service enrollment and Syncthing
//! lifecycle. Opening this controller does not listen or start a daemon.
mod addresses;
mod create_ignores;
mod removal;
pub use removal::SyncComputer;
pub mod shared_ignores;
pub use shared_ignores::IgnorePreview;
mod existing;
mod history;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PolicyWarning {
    pub path: PathBuf,
    pub message: String,
}
pub use history::{HistoryWarning, RetentionDays, RetentionSetting};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod inventory;
mod lan;
mod origin;
mod source;
mod transfer;
pub use addresses::{PeerLink, Relay};
pub use transfer::received_vault_names;
mod wire;
use crate::private;
use anyhow::{ensure, Context, Result};
pub use existing::{
    attach_existing, configuration_paths, inspect_existing, AttachedVault, ExistingVault,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const LIFETIME: u64 = 3600;
const PAIRING_REPLY_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_PEERS: usize = 64;

mod overview;
pub use overview::{ComputerOverview, Overview, VaultOverview};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pending {
    pub request_id: Uuid,
    pub name: String,
    pub device_id: String,
    pub fingerprint: String,
    pub incoming: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Peer {
    pub id: Uuid,
    pub name: String,
    pub device_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Vault {
    pub id: String,
    pub label: String,
}
/// Serializable for the desktop's local `okilum sync` control channel (#2589).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub computers: Vec<SyncComputer>,
    pub pending_removals: usize,
    pub policy_warnings: Vec<PolicyWarning>,
    pub history_warnings: Vec<history::HistoryWarning>,
    pub history_retention: Vec<RetentionSetting>,
    pub offer_url: Option<String>,
    pub pending: Option<Pending>,
    pub peers: Vec<Peer>,
    pub catalog: Vec<Vault>,
    pub attached: Vec<ExistingVault>,
    pub shared: Vec<PathBuf>,
    pub status: String,
    /// An owned first receive still needs guarded bidirectional promotion.
    pub preparing: bool,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    id: Uuid,
    key: [u8; 32],
    expires: u64,
    endpoint: SocketAddr,
    public: Vec<u8>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: Uuid,
    name: String,
    public: Vec<u8>,
    #[serde(default)]
    syncthing_device: Option<String>,
}
impl Request {
    fn validate(&self, signer: &[u8]) -> Result<()> {
        validate_name(&self.name)?;
        ensure!(
            !self.id.is_nil() && signer.len() == 32 && self.public == signer,
            "request identity proof differs"
        );
        if let Some(device) = &self.syncthing_device {
            ensure!(
                device.len() == 63
                    && device.split('-').count() == 8
                    && device.split('-').all(|part| part.len() == 7
                        && part
                            .bytes()
                            .all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b))),
                "invalid Syncthing device identity"
            );
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Invitation {
    offer: Offer,
    request: Option<Request>,
    approved: Option<Uuid>,
    rejected: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Paired {
    peer: Peer,
    public: Vec<u8>,
    key: [u8; 32],
    endpoint: SocketAddr,
    allowed: Vec<String>,
    #[serde(default)]
    syncthing_device: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    addresses: Vec<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Database {
    #[serde(default)]
    local_removed: bool,
    #[serde(default)]
    revoked_peers: BTreeMap<Uuid, Paired>,
    #[serde(default)]
    removals: Vec<removal::Removal>,
    seed: [u8; 32],
    name: String,
    endpoint: Option<SocketAddr>,
    invitation: Option<Invitation>,
    outbound: Option<(Offer, Request)>,
    peers: BTreeMap<Uuid, Paired>,
    attached: Vec<ExistingVault>,
    #[serde(default)]
    catalog: Vec<Vault>,
    #[serde(default)]
    catalog_peer: Option<Uuid>,
    #[serde(default)]
    transfers: Vec<transfer::Transfer>,
    #[serde(default)]
    share_grants: Vec<transfer::ShareGrant>,
    #[serde(default)]
    sources: Vec<source::Source>,
}
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
type InspectionHook = Arc<dyn Fn(Uuid) + Send + Sync>;

struct Shared {
    policy_revisions: Mutex<BTreeMap<PathBuf, String>>,
    policy_warnings: Mutex<Vec<PolicyWarning>>,
    history_warnings: Mutex<Vec<history::HistoryWarning>>,
    state: PathBuf,
    configs: Mutex<Vec<PathBuf>>,
    db: Mutex<Database>,
    /// One accept loop per advertised endpoint: the current invitation's and
    /// every other endpoint a paired computer pins on this computer (#2589).
    listeners: Mutex<BTreeMap<SocketAddr, Listener>>,
    /// Open paired-channel connections across all listeners.
    connections: Arc<AtomicUsize>,
    stopped: AtomicBool,
    access: okilum_sync::NetworkAccess,
    observation_generation: Uuid,
    transfer_lock: Mutex<()>,
    connection_watch: Mutex<addresses::ConnectionWatch>,
    /// Invitation whose approval reply this process has written to the joiner.
    /// In memory only; a headless inviter waits on it before exiting.
    delivered_approval: Mutex<Option<Uuid>>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    inventory: Mutex<inventory::Cache>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    inventory_key: [u8; 32],
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    first_sync: Mutex<BTreeMap<Uuid, Arc<Mutex<inventory::merge::Session>>>>,
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    inspection_hook: Mutex<Option<InspectionHook>>,
    // Lifetime-wide exclusive ownership, including multiple application windows.
    _lock: std::fs::File,
}
struct Listener {
    stop: Arc<AtomicBool>,
    socket: TcpListener,
}
impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        use std::os::fd::AsRawFd;
        // shutdown affects both duplicated descriptors and wakes accept.
        unsafe {
            libc::shutdown(self.socket.as_raw_fd(), libc::SHUT_RDWR);
        }
    }
}
#[derive(Clone)]
pub struct Controller {
    shared: Arc<Shared>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    PollRemovals {},
    AcknowledgeRemoval {
        id: Uuid,
    },
    RemoveComputer {
        id: Uuid,
        device: String,
        vaults: std::collections::BTreeSet<String>,
    },
    Redeem {
        request: Request,
    },
    Catalog {},
    Share {
        vault: String,
        device: String,
    },
    Observe {
        vault: String,
        device: String,
    },
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    Inspect {
        request: Box<inventory::Request>,
    },
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    FirstSync {
        request: Box<inventory::merge::Request>,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    nonce: Uuid,
    target: Vec<u8>,
    operation: Operation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transport: Option<addresses::Transport>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    nonce: Uuid,
    result: Reply,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transport: Option<addresses::Transport>,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Removals {
        requests: Vec<removal::Request>,
    },
    RemovalRecorded {
        id: Uuid,
    },
    Pending,
    Rejected,
    Approved {
        paired: Paired,
        catalog: Vec<Vault>,
    },
    Catalog {
        catalog: Vec<Vault>,
    },
    Shared {
        connection: transfer::Connection,
    },
    Observed {
        observation: transfer::Observation,
    },
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    Inspection {
        response: inventory::Response,
    },
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    FirstSync {
        response: inventory::merge::Response,
    },
    NotReady,
    Error {
        message: String,
    },
}

/// Whether this computer's saved journal pairs it with another computer or
/// attaches a vault (#2589). Read-only and lock-free: it never creates the
/// journal and works while its owner runs. The app uses it to decide whether to
/// serve paired computers before anyone opens Settings → Sync.
pub fn has_paired_computers_or_vaults(state: &Path) -> Result<bool> {
    let file = state.join("direct.json");
    if !file.try_exists()? {
        return Ok(false);
    }
    let db: Database = serde_json::from_slice(&private::read(&file)?)?;
    Ok(!db.peers.is_empty() || !db.attached.is_empty())
}

impl Controller {
    pub fn open(state: PathBuf, configs: Vec<PathBuf>) -> Result<Self> {
        #[cfg(target_os = "macos")]
        if let Some(profile) = crate::sidecar::macos::isolation::selected()? {
            profile.require_controller(&state, &configs)?;
        }
        let _lock = private::lock(&state)?;
        let file = state.join("direct.json");
        let db = if file.try_exists()? {
            serde_json::from_slice(&private::read(&file)?)?
        } else {
            Database {
                seed: wire::random()?,
                name: String::new(),
                endpoint: None,
                invitation: None,
                outbound: None,
                peers: BTreeMap::new(),
                attached: vec![],
                catalog: vec![],
                catalog_peer: None,
                local_removed: false,
                revoked_peers: BTreeMap::new(),
                removals: vec![],
                transfers: vec![],
                share_grants: vec![],
                sources: vec![],
            }
        };
        // Persist identity once, before creating an offer or exposing its public key.
        private::write(&file, &serde_json::to_vec(&db)?)?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let inventory_key = inventory::persistent_key(&state)?;
        Ok(Self {
            shared: Arc::new(Shared {
                state,
                configs: Mutex::new(configs),
                db: Mutex::new(db),
                listeners: Mutex::new(BTreeMap::new()),
                connections: Arc::new(AtomicUsize::new(0)),
                stopped: AtomicBool::new(false),
                access: okilum_sync::NetworkAccess::current(),
                observation_generation: Uuid::new_v4(),
                transfer_lock: Mutex::new(()),
                policy_revisions: Mutex::new(BTreeMap::new()),
                policy_warnings: Mutex::new(Vec::new()),
                history_warnings: Mutex::new(Vec::new()),
                connection_watch: Mutex::new(Default::default()),
                delivered_approval: Mutex::new(None),
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                inventory: Mutex::new(Default::default()),
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                inventory_key,
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                first_sync: Mutex::new(BTreeMap::new()),
                #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
                inspection_hook: Mutex::new(None),
                _lock,
            }),
        })
    }
    fn check(&self) -> Result<()> {
        ensure!(
            !self.shared.stopped.load(Ordering::Acquire) && self.shared.access.allowed(),
            "Sync is disabled"
        );
        Ok(())
    }
    /// Current verified selection, including a runtime supplied after bootstrap.
    /// Discovery defaults are not a replacement for this controller snapshot.
    pub fn configuration_paths(&self) -> Result<Vec<PathBuf>> {
        self.check()?;
        Ok(self.shared.configs.lock().unwrap().clone())
    }
    pub fn set_configurations(&self, configs: Vec<PathBuf>) -> Result<()> {
        self.check()?;
        #[cfg(target_os = "macos")]
        if let Some(profile) = crate::sidecar::macos::isolation::selected()? {
            profile.require_configurations(&configs)?;
        }
        let mut current = self.shared.configs.lock().unwrap();
        if *current != configs {
            *current = configs;
            *self.shared.connection_watch.lock().unwrap() = Default::default();
        }
        Ok(())
    }
    /// Nonblocking capability revocation. It closes the application listeners and
    /// makes retained clones unable to send. It never stops an external daemon.
    pub fn shutdown(&self) {
        self.shared.stopped.store(true, Ordering::Release);
        if let Ok(mut listeners) = self.shared.listeners.try_lock() {
            listeners.clear();
        }
    }
    /// The name this computer last offered or requested pairing under.
    pub fn computer_name(&self) -> Result<String> {
        self.check()?;
        Ok(self.shared.db.lock().unwrap().name.clone())
    }
    /// Whether the joiner has been sent this invitation's approval. `approve`
    /// only records the peer; the joiner learns it on its next poll, so the
    /// inviting process must keep listening until this is true.
    pub fn approval_delivered(&self) -> Result<bool> {
        self.check()?;
        let approved = self
            .shared
            .db
            .lock()
            .unwrap()
            .invitation
            .as_ref()
            .filter(|i| i.approved.is_some())
            .map(|i| i.offer.id);
        Ok(approved.is_some() && *self.shared.delivered_approval.lock().unwrap() == approved)
    }
    pub fn snapshot(&self) -> Result<Snapshot> {
        self.check()?;
        let db = self.shared.db.lock().unwrap();
        let mut snapshot = snapshot(&db)?;
        snapshot.policy_warnings = self.shared.policy_warnings.lock().unwrap().clone();
        snapshot.history_warnings = self.shared.history_warnings.lock().unwrap().clone();
        let removed = db.local_removed;
        let held = db.transfers.iter().any(transfer::Transfer::legacy_hold);
        drop(db);
        if !removed && !held && snapshot.pending.is_none() && snapshot.offer_url.is_none() {
            if let Some(status) = &self.shared.connection_watch.lock().unwrap().status {
                snapshot.status = status.clone();
            }
        }
        Ok(snapshot)
    }
    pub fn attach_existing(
        &self,
        inspected: &ExistingVault,
        consent: bool,
    ) -> Result<ExistingVault> {
        self.check()?;
        ensure!(consent, "explicit attach-existing consent required");
        let configs = self.shared.configs.lock().unwrap().clone();
        let fresh = inspect_existing(&configs, &inspected.path)?;
        ensure!(
            existing::same_binding(&fresh, inspected),
            "inspected replica changed"
        );
        let state = self.shared.state.canonicalize()?;
        ensure!(
            !state.starts_with(&fresh.path),
            "operational state must be outside the vault"
        );
        let mut db = self.shared.db.lock().unwrap();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let upgraded_grants = {
            let mut grants = db.share_grants.clone();
            for grant in grants.iter_mut().filter(|g| g.vault == fresh.folder_id) {
                grant.refresh_source(&fresh, true)?;
            }
            grants
        };
        if let Some(old) = db
            .attached
            .iter_mut()
            .find(|v| v.folder_id == fresh.folder_id)
        {
            ensure!(
                existing::same_binding_with_consent(old, &fresh),
                "folder ID already associated with another replica"
            );
            // Only this explicit, freshly inspected consent can upgrade a legacy
            // path-only association. Automatic refresh/catalog never acquires it.
            *old = fresh.clone();
        } else {
            db.attached.push(fresh.clone());
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            db.share_grants = upgraded_grants;
        }
        // Explicit consent also exposes this vault to computers already approved
        // before enrollment, matching Share with my computers for managed vaults.
        for peer in db.peers.values_mut() {
            if !peer.allowed.contains(&fresh.folder_id) {
                peer.allowed.push(fresh.folder_id.clone());
            }
        }
        self.save(&db)?;
        Ok(fresh)
    }
    pub fn begin_pairing(&self, name: &str) -> Result<Snapshot> {
        self.check()?;
        validate_name(name)?;
        let ip = lan::address().context("cannot select a LAN address for pairing")?;
        self.begin_pairing_at(name, "0.0.0.0:0".parse()?, ip)
    }
    pub fn begin_pairing_at(
        &self,
        name: &str,
        bind: SocketAddr,
        advertised: IpAddr,
    ) -> Result<Snapshot> {
        self.check()?;
        validate_name(name)?;
        ensure!(
            !advertised.is_unspecified() && !advertised.is_multicast(),
            "invalid advertised LAN address"
        );
        self.listen(bind, advertised)?;
        let mut db = self.shared.db.lock().unwrap();
        ensure!(db.peers.len() < MAX_PEERS, "paired computer limit reached");
        // Add computer is retryable, including after reopening Settings. Keep
        // an in-flight request's capability so its approval remains valid.
        let time = now()?;
        if db.invitation.as_ref().is_some_and(|i| {
            i.offer.expires > time
                && !i.rejected
                && i.approved.is_none()
                && Some(i.offer.endpoint) == db.endpoint
        }) {
            return snapshot(&db);
        }
        db.name = name.to_owned();
        db.invitation = Some(Invitation {
            offer: Offer {
                id: Uuid::new_v4(),
                key: wire::random()?,
                expires: now()? + LIFETIME,
                endpoint: db.endpoint.context("listener missing")?,
                public: wire::public(&db.seed)?,
            },
            request: None,
            approved: None,
            rejected: false,
        });
        self.save(&db)?;
        snapshot(&db)
    }
    /// The listener a new invitation advertises. A computer whose paired
    /// computers already pin an endpoint here invites at that same endpoint
    /// (#2589): a fresh random port per invitation left earlier computers
    /// pinned to a port nobody listened on again. Only an explicit bind port, or
    /// no usable pinned endpoint, opens a new listener.
    fn listen(&self, bind: SocketAddr, advertised: IpAddr) -> Result<()> {
        self.check()?;
        if bind.port() == 0 {
            // Serving the other pins is best effort here; refresh reports failures.
            let _ = self.serve_pinned();
        }
        let (current, pinned) = {
            let db = self.shared.db.lock().unwrap();
            (db.endpoint, served(&db))
        };
        let mut listeners = self.shared.listeners.lock().unwrap();
        if current.is_some_and(|current| listeners.contains_key(&current)) {
            return Ok(());
        }
        let reused = if bind.port() == 0 {
            // Prefer the advertised address; another live pin keeps one endpoint.
            let live: Vec<_> = pinned
                .into_iter()
                .filter(|endpoint| listeners.contains_key(endpoint))
                .collect();
            live.iter()
                .find(|endpoint| endpoint.ip() == advertised)
                .or_else(|| live.first())
                .copied()
        } else {
            None
        };
        let endpoint = match reused {
            Some(endpoint) => endpoint,
            None => self.start(&mut listeners, bind, advertised)?,
        };
        drop(listeners);
        self.shared.db.lock().unwrap().endpoint = Some(endpoint);
        Ok(())
    }
    /// Listen again on every endpoint a paired computer pins on this computer.
    /// An address no longer assigned to this computer is skipped. Any other
    /// failure, such as a port now taken by another program, is returned after
    /// the remaining endpoints were tried: never permission to replace a pin.
    fn serve_pinned(&self) -> Result<()> {
        self.check()?;
        let (pinned, current) = {
            let db = self.shared.db.lock().unwrap();
            (served(&db), db.endpoint)
        };
        let mut listeners = self.shared.listeners.lock().unwrap();
        let mut failure = None;
        for endpoint in pinned {
            if listeners.contains_key(&endpoint) {
                continue;
            }
            // Rebind the exact advertised address and port.
            match self.start(&mut listeners, endpoint, endpoint.ip()) {
                Ok(_) => {}
                Err(error) if not_local(&error) => {}
                // Only the current endpoint is fatal. An older pin whose port is
                // taken must not fail the whole refresh: the caller would drop
                // this controller and every healthy listener with it (#2589).
                Err(error) if Some(endpoint) == current => {
                    failure.get_or_insert(error);
                }
                Err(error) => {
                    eprintln!(
                        "okilum-sync: older paired endpoint {endpoint} not served: {error:#}"
                    );
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }
    /// Start one accept loop on `bind`, advertised at `advertised` and the bound
    /// port, and return that advertised endpoint.
    fn start(
        &self,
        listeners: &mut BTreeMap<SocketAddr, Listener>,
        bind: SocketAddr,
        advertised: IpAddr,
    ) -> Result<SocketAddr> {
        self.check()?;
        let listener = TcpListener::bind(bind)
            .or_else(|wildcard_error| {
                if bind.ip().is_unspecified() && bind.port() == 0 {
                    // Some host policies disallow wildcard listeners but permit the
                    // actual LAN interface. Still ask the OS for an unused port.
                    TcpListener::bind(SocketAddr::new(advertised, 0)).with_context(|| {
                        format!("wildcard pairing listener failed: {wildcard_error}")
                    })
                } else {
                    Err(wildcard_error.into())
                }
            })
            .context("cannot start LAN pairing listener")?;
        listener.set_nonblocking(true)?;
        let endpoint = SocketAddr::new(advertised, listener.local_addr()?.port());
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let socket = listener.try_clone()?;
        let weak = Arc::downgrade(&self.shared);
        let connections = self.shared.connections.clone();
        thread::Builder::new()
            .name("okilum-pairing".into())
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    let Some(shared) = weak.upgrade() else {
                        break;
                    };
                    if shared.stopped.load(Ordering::Acquire) || !shared.access.allowed() {
                        break;
                    }
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                    shared.inventory.lock().unwrap().prune();
                    drop(shared);
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            if let Some(shared) = weak.upgrade() {
                                // Bounded workers keep slow inspections/frame reads off the
                                // accept loop. Inspection admission separately limits each peer.
                                if wire::limits(&stream).is_ok()
                                    && connections
                                        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                                            (n < 16).then_some(n + 1)
                                        })
                                        .is_ok()
                                {
                                    struct Connection(Arc<AtomicUsize>);
                                    impl Drop for Connection {
                                        fn drop(&mut self) {
                                            self.0.fetch_sub(1, Ordering::AcqRel);
                                        }
                                    }
                                    let connection = Connection(connections.clone());
                                    let _ = thread::Builder::new()
                                        .name("okilum-direct".into())
                                        .spawn(move || {
                                            let _connection = connection;
                                            let controller = Controller { shared };
                                            let _ = controller.serve(&mut stream);
                                        });
                                }
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(20))
                        }
                        Err(_) => break,
                    }
                }
            })?;
        listeners.insert(endpoint, Listener { stop, socket });
        Ok(endpoint)
    }
    pub fn redeem(&self, url: &str, name: &str) -> Result<Snapshot> {
        self.check()?;
        validate_name(name)?;
        let offer = decode_url(url)?;
        ensure!(offer.expires > now()?, "pairing invitation expired");
        {
            let db = self.shared.db.lock().unwrap();
            if let Some((current, request)) = &db.outbound {
                ensure!(
                    current == &offer && request.name == name,
                    "cancel the current request before connecting again"
                );
                // A timeout may follow remote receipt. Retry the durable identity,
                // including its approved Syncthing binding, rather than replacing it.
                drop(db);
                return self.refresh_pairing();
            }
        }
        let configs = self.configuration_paths()?;
        let inventory = crate::daemon::discover(&configs);
        inventory.folders()?;
        let syncthing_device = if inventory.candidates.len() == 1 {
            let identity = &inventory.candidates[0].identity;
            identity.connect()?;
            Some(identity.device_id.clone())
        } else {
            None
        };
        let mut db = self.shared.db.lock().unwrap();
        ensure!(
            offer.public != wire::public(&db.seed)?,
            "cannot pair this computer with itself"
        );
        ensure!(db.peers.len() < MAX_PEERS, "paired computer limit reached");
        let request = Request {
            id: Uuid::new_v4(),
            name: name.to_owned(),
            public: wire::public(&db.seed)?,
            syncthing_device,
        };
        db.name = name.to_owned();
        db.outbound = Some((offer, request));
        self.save(&db)?;
        drop(db);
        self.refresh_pairing()
    }
    pub fn refresh(&self) -> Result<Snapshot> {
        self.check()?;
        // Opening the journal stays offline. Explicit refresh restores every
        // source endpoint already pinned by a durable paired capability (#2589),
        // not only the latest invitation's. A receiving peer's remote endpoint
        // must never become a local listener.
        self.serve_pinned()?;
        self.refresh_pairing()?;
        self.reconcile_removals()?;
        if self.shared.db.lock().unwrap().local_removed {
            return self.snapshot();
        }
        self.reconcile_peer_addresses()?;
        self.reconcile_create_ignores()?;
        self.reconcile_history()?;
        self.reconcile_transfers()?;
        self.refresh_connection_status()?;
        self.refresh_attached()?;
        let mut snapshot = self.snapshot()?;
        snapshot.computers = self.sync_computers()?;
        Ok(snapshot)
    }
    /// Refresh the pairing journal/channel without daemon inventory, binary
    /// verification or transfer reconciliation. Used while a pairing step is open.
    pub fn refresh_pairing(&self) -> Result<Snapshot> {
        self.check()?;
        self.serve_pinned()?;
        let outbound = self.shared.db.lock().unwrap().outbound.clone();
        if let Some((offer, request)) = outbound {
            if offer.expires <= now()? {
                let mut db = self.shared.db.lock().unwrap();
                if db
                    .outbound
                    .as_ref()
                    .is_some_and(|(o, r)| o.id == offer.id && r.id == request.id)
                {
                    db.outbound = None;
                    self.save(&db)?;
                }
                anyhow::bail!("pairing invitation expired");
            }
            let request_id = request.id;
            let reply = self.call(
                offer.endpoint,
                offer.id,
                &offer.key,
                &offer.public,
                Operation::Redeem { request },
            )?;
            let mut db = self.shared.db.lock().unwrap();
            ensure!(
                db.outbound
                    .as_ref()
                    .is_some_and(|(o, r)| { o.id == offer.id && r.id == request_id }),
                "pairing request cancelled or changed"
            );
            match reply {
                Reply::Pending => {}
                Reply::Rejected => {
                    db.outbound = None;
                    self.save(&db)?;
                    anyhow::bail!("pairing request rejected");
                }
                Reply::Approved { paired, catalog } => {
                    ensure!(
                        paired.public == offer.public && paired.endpoint == offer.endpoint,
                        "pairing approval scope changed"
                    );
                    db.catalog = catalog;
                    db.catalog_peer = Some(paired.peer.id);
                    db.peers.insert(paired.peer.id, paired);
                    db.outbound = None;
                    self.save(&db)?;
                }
                Reply::Error { message } => anyhow::bail!("{message}"),
                _ => anyhow::bail!("unexpected pairing response"),
            }
        }
        self.snapshot()
    }
    pub fn approve(&self, request_id: Uuid) -> Result<Snapshot> {
        self.check()?;
        let mut db = self.shared.db.lock().unwrap();
        let invitation = db.invitation.as_ref().context("no invitation")?;
        ensure!(
            invitation.offer.expires > now()?
                && !invitation.rejected
                && invitation.approved.is_none(),
            "invitation expired or consumed"
        );
        let request = invitation
            .request
            .clone()
            .context("no requesting computer")?;
        ensure!(
            request.id == request_id,
            "request changed; compare computer again"
        );
        ensure!(db.peers.len() < MAX_PEERS, "paired computer limit reached");
        let id = Uuid::new_v4();
        let paired = Paired {
            peer: Peer {
                id,
                name: request.name,
                device_id: wire::fingerprint(&request.public),
            },
            public: request.public,
            syncthing_device: request.syncthing_device,
            addresses: vec![],
            key: wire::random()?,
            endpoint: invitation.offer.endpoint,
            allowed: db.attached.iter().map(|v| v.folder_id.clone()).collect(),
        };
        db.peers.insert(id, paired);
        db.invitation.as_mut().unwrap().approved = Some(id);
        self.save(&db)?;
        snapshot(&db)
    }
    pub fn reject(&self, request_id: Uuid) -> Result<Snapshot> {
        self.check()?;
        let mut db = self.shared.db.lock().unwrap();
        let invitation = db.invitation.as_mut().context("no invitation")?;
        ensure!(
            invitation
                .request
                .as_ref()
                .is_some_and(|r| r.id == request_id)
                && invitation.approved.is_none(),
            "request unavailable"
        );
        invitation.rejected = true;
        self.save(&db)?;
        snapshot(&db)
    }
    pub fn cancel(&self) -> Result<Snapshot> {
        self.check()?;
        let mut db = self.shared.db.lock().unwrap();
        if let Some(invitation) = &mut db.invitation {
            invitation.rejected = true;
        }
        db.outbound = None;
        self.save(&db)?;
        self.shared.listeners.lock().unwrap().clear();
        snapshot(&db)
    }
    pub fn catalog(&self, peer_id: Uuid) -> Result<Vec<Vault>> {
        self.check()?;
        let paired = self
            .shared
            .db
            .lock()
            .unwrap()
            .peers
            .get(&peer_id)
            .cloned()
            .context("unknown paired computer")?;
        match self.call(
            paired.endpoint,
            peer_id,
            &paired.key,
            &paired.public,
            Operation::Catalog {},
        )? {
            Reply::Catalog { catalog } => {
                let mut db = self.shared.db.lock().unwrap();
                db.catalog = catalog.clone();
                db.catalog_peer = Some(peer_id);
                self.save(&db)?;
                Ok(catalog)
            }
            Reply::Error { message } => anyhow::bail!("{message}"),
            _ => anyhow::bail!("unexpected catalog response"),
        }
    }
    fn call(
        &self,
        endpoint: SocketAddr,
        scope: Uuid,
        credential: &[u8; 32],
        public: &[u8],
        operation: Operation,
    ) -> Result<Reply> {
        self.check()?;
        let read_only = operation.read_only();
        let exchange_transport = !read_only && !operation.removal_only() && !operation.first_sync();
        let first_sync = operation.first_sync();
        let redeem = matches!(operation, Operation::Redeem { .. });
        let seed = self.shared.db.lock().unwrap().seed;
        let message = Message {
            nonce: Uuid::new_v4(),
            target: public.to_vec(),
            operation,
            transport: if exchange_transport {
                self.local_transport()?
            } else {
                None
            },
        };
        let frame = wire::seal(scope, credential, &seed, &message)?;
        let mut stream =
            wire::connect(endpoint).map_err(|error| transfer::not_connected(endpoint, error))?;
        self.check()?;
        if redeem || first_sync {
            // The other computer may be busy (daemon inventory, disk). A pairing
            // request waits longer than the 3 s frame bound before giving up (#2589).
            stream.set_read_timeout(Some(if first_sync {
                Duration::from_secs(90)
            } else {
                PAIRING_REPLY_TIMEOUT
            }))?;
        }
        wire::write(&mut stream, &frame)?;
        let waiting_since = std::time::Instant::now();
        let response = match wire::read(&mut stream) {
            Ok(response) => response,
            // The durable request is already recorded locally. A slow human or
            // an older inviter can outlive one socket; poll with the SAME signed
            // request, never replace its identity or require another invitation.
            Err(error)
                if redeem && wire::approval_wait_interrupted(&error, waiting_since.elapsed()) =>
            {
                return Ok(Reply::Pending);
            }
            Err(error) => return Err(error),
        };
        let (response, _): (Response, _) = wire::open(response, credential, Some(public))?;
        ensure!(response.nonce == message.nonce, "stale pairing response");
        self.check()?;
        let mut reply = response.result;
        if let Some(transport) = response.transport {
            ensure!(
                !first_sync,
                "first Sync cannot exchange transport configuration"
            );
            ensure!(
                !read_only,
                "inspection cannot exchange transport configuration"
            );
            match &mut reply {
                Reply::Approved { paired, .. } => addresses::remember(paired, &transport)?,
                Reply::Error { .. } | Reply::Pending | Reply::Rejected => {}
                _ => {
                    let mut db = self.shared.db.lock().unwrap();
                    let peer = db
                        .peers
                        .get_mut(&scope)
                        .context("unknown paired computer")?;
                    self.accept_transport(peer, &transport)?;
                    self.save(&db)?;
                }
            }
        }
        Ok(reply)
    }
    fn serve(&self, stream: &mut std::net::TcpStream) -> Result<()> {
        self.check()?;
        let envelope: wire::Envelope = wire::read(stream)?;
        let scope = envelope.scope;
        let mut db = self.shared.db.lock().unwrap();
        let invitation = db.invitation.as_ref().filter(|i| i.offer.id == scope);
        let (credential, expected) = if let Some(i) = invitation {
            ensure!(i.offer.expires > now()?, "invitation expired");
            (i.offer.key, i.request.as_ref().map(|r| r.public.clone()))
        } else {
            let peer = db
                .peers
                .get(&scope)
                .or_else(|| db.revoked_peers.get(&scope))
                .context("unknown pairing capability")?;
            (peer.key, Some(peer.public.clone()))
        };
        let (message, signer): (Message, _) =
            wire::open(envelope, &credential, expected.as_deref())?;
        ensure!(
            (!message.operation.read_only() && !message.operation.first_sync())
                || message.transport.is_none(),
            "inspection cannot exchange transport configuration"
        );
        ensure!(
            message.target == wire::public(&db.seed)?,
            "wrong intended computer"
        );
        self.check()?;
        if !db.peers.contains_key(&scope) && db.revoked_peers.contains_key(&scope) {
            ensure!(
                removal::revoked_operation_allowed(&db, scope, &message.operation)
                    && message.transport.is_none(),
                "Pairing authorization was removed"
            );
        }
        // Only an approved, authenticated capability may update a pinned peer.
        let peer_id = if db.peers.contains_key(&scope) {
            Some(scope)
        } else {
            db.invitation
                .as_ref()
                .filter(|i| i.offer.id == scope)
                .and_then(|i| i.approved)
        };
        if let (Some(id), Some(transport)) = (peer_id, &message.transport) {
            let peer = db.peers.get_mut(&id).context("approved peer missing")?;
            ensure!(
                peer.syncthing_device.is_some(),
                "Syncthing identity was not approved"
            );
            self.accept_transport(peer, transport)?;
            self.save(&db)?;
        }
        let seed = db.seed;
        #[cfg(feature = "first-sync-fixture")]
        let first_sync_fixture = message.operation.first_sync();
        let result = match message.operation {
            Operation::RemoveComputer { id, device, vaults } => {
                drop(db);
                self.receive_removal(scope, id, device, vaults, None)
            }
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Operation::Inspect { request } => {
                drop(db);
                self.inspect_request(scope, &signer, *request)
                    .map(|response| Reply::Inspection { response })
            }
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Operation::FirstSync { request } => {
                drop(db);
                self.first_sync_request(scope, &signer, *request)
                    .map(|response| Reply::FirstSync { response })
            }
            operation => {
                let reply = self.dispatch(&mut db, scope, operation, signer);
                // The reply's transport query contacts the local Syncthing daemon.
                // Never hold the pairing database across it (#2589): the UI and other
                // requests must not wait for a slow daemon.
                drop(db);
                reply
            }
        };
        let reply = result.unwrap_or_else(|error| {
            #[cfg(feature = "first-sync-fixture")]
            if first_sync_fixture {
                eprintln!("FIRST_SYNC_FIXTURE_REFUSAL: {error:#}");
            }
            if let Some(message) = removal::public_refusal(&error) {
                return Reply::Error {
                    message: message.into(),
                };
            }
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            if let Some(message) = inventory::public_refusal(&error) {
                return Reply::Error {
                    message: message.into(),
                };
            }
            let _ = error;
            Reply::Error {
                message: "operation unavailable; check approved vault, replica and destination"
                    .into(),
            }
        });
        self.check()?;
        let approval = matches!(reply, Reply::Approved { .. });
        let response = wire::seal(
            scope,
            &credential,
            &seed,
            &Response {
                nonce: message.nonce,
                result: reply,
                transport: if message.transport.is_some() {
                    // Address exchange is optional and repeats on every refresh. An
                    // unreachable daemon must not turn a pending pairing into a
                    // silent socket timeout on the other computer (#2589).
                    self.local_transport().unwrap_or_else(|error| {
                        eprintln!("okilum-sync: pairing reply without transport: {error:#}");
                        None
                    })
                } else {
                    None
                },
            },
        )?;
        wire::write(stream, &response)?;
        if approval {
            *self.shared.delivered_approval.lock().unwrap() = Some(scope);
        }
        Ok(())
    }
    fn dispatch(
        &self,
        db: &mut Database,
        scope: Uuid,
        operation: Operation,
        signer: Vec<u8>,
    ) -> Result<Reply> {
        match operation {
            Operation::PollRemovals {} => {
                ensure!(
                    db.peers.contains_key(&scope) || db.revoked_peers.contains_key(&scope),
                    "unknown removal capability"
                );
                Ok(Reply::Removals {
                    requests: removal::requests(db, scope),
                })
            }
            Operation::AcknowledgeRemoval { id } => {
                removal::acknowledge(db, scope, id)?;
                self.save(db)?;
                Ok(Reply::RemovalRecorded { id })
            }
            Operation::RemoveComputer { .. } => {
                anyhow::bail!("removal requires serialized lock-free dispatch")
            }
            Operation::Redeem { request } => {
                request.validate(&signer)?;
                let invitation = db.invitation.as_mut().context("no invitation")?;
                ensure!(
                    invitation.offer.id == scope && invitation.offer.expires > now()?,
                    "invalid invitation scope"
                );
                if invitation.rejected {
                    return Ok(Reply::Rejected);
                }
                if let Some(old) = &invitation.request {
                    ensure!(
                        old.id == request.id
                            && old.public == request.public
                            && old.name == request.name
                            && old.syncthing_device == request.syncthing_device,
                        "invitation already claimed"
                    );
                } else {
                    invitation.request = Some(request);
                }
                let approved = invitation.approved;
                self.save(db)?;
                if let Some(id) = approved {
                    let local = db.peers.get(&id).context("approved peer missing")?;
                    let remote = Paired {
                        peer: Peer {
                            id,
                            name: db.name.clone(),
                            device_id: wire::fingerprint(&wire::public(&db.seed)?),
                        },
                        public: wire::public(&db.seed)?,
                        key: local.key,
                        endpoint: local.endpoint,
                        allowed: vec![],
                        syncthing_device: None,
                        addresses: vec![],
                    };
                    Ok(Reply::Approved {
                        paired: remote,
                        catalog: catalog_for(db, &local.allowed),
                    })
                } else {
                    Ok(Reply::Pending)
                }
            }
            Operation::Catalog {} => {
                let peer = db.peers.get(&scope).context("computer not approved")?;
                Ok(Reply::Catalog {
                    catalog: catalog_for(db, &peer.allowed),
                })
            }
            Operation::Share { vault, device } => {
                let peer = db.peers.get(&scope).context("computer not approved")?;
                ensure!(peer.allowed.contains(&vault), "vault not authorized");
                ensure!(
                    peer.syncthing_device.as_deref() == Some(device.as_str()),
                    "Syncthing identity was not approved for this paired computer"
                );
                let addresses = peer.addresses.clone();
                let source = db
                    .attached
                    .iter()
                    .find(|v| v.folder_id == vault)
                    .context("authorized vault unavailable")?
                    .clone();
                let configs = self.shared.configs.lock().unwrap().clone();
                let fresh = inspect_existing(&configs, &source.path)?;
                ensure!(
                    existing::same_binding(&source, &fresh),
                    "authorized vault identity changed"
                );
                ensure!(
                    fresh.folder_type == "sendreceive",
                    "source folder is not bidirectional; existing mode retained"
                );
                if let Some(grant) = db
                    .share_grants
                    .iter()
                    .find(|g| g.peer == scope && g.vault == vault)
                {
                    grant.validate(&fresh, &device)?;
                } else {
                    // Durable authorization precedes every remote membership mutation.
                    db.share_grants
                        .push(transfer::ShareGrant::new(scope, &fresh, &device));
                    self.save(db)?;
                }
                Ok(Reply::Shared {
                    connection: transfer::share(&fresh, &device, &addresses)?,
                })
            }
            Operation::Observe { vault, device } => {
                let peer = db.peers.get(&scope).context("computer not approved")?;
                ensure!(
                    peer.allowed.contains(&vault)
                        && peer.syncthing_device.as_deref() == Some(device.as_str()),
                    "readiness identity was not approved"
                );
                let grant = db
                    .share_grants
                    .iter()
                    .find(|g| g.peer == scope && g.vault == vault)
                    .context("readiness has no explicit share grant")?;
                let configs = self.shared.configs.lock().unwrap().clone();
                match grant.observe(&configs, self.shared.observation_generation) {
                    Ok(observation) => Ok(Reply::Observed { observation }),
                    Err(error) if transfer::offline(&error) => Ok(Reply::NotReady),
                    Err(error) => Err(error),
                }
            }
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Operation::Inspect { .. } => anyhow::bail!("inspection requires lock-free dispatch"),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Operation::FirstSync { .. } => anyhow::bail!("first Sync requires lock-free dispatch"),
        }
    }
    fn save(&self, db: &Database) -> Result<()> {
        private::write(
            &self.shared.state.join("direct.json"),
            &serde_json::to_vec(db)?,
        )
    }
    fn refresh_attached(&self) -> Result<()> {
        // Daemon inspection is read-only; persist upgraded private identity witnesses.
        // Offline replicas retain their durable association.
        let configs = self.shared.configs.lock().unwrap().clone();
        let mut db = self.shared.db.lock().unwrap();
        let mut attached = db.attached.clone();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let mut grants = db.share_grants.clone();
        let mut changed = false;
        for attached in &mut attached {
            if let Ok(fresh) = inspect_existing(&configs, &attached.path) {
                ensure!(
                    existing::same_binding(attached, &fresh),
                    "attached replica identity changed"
                );
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                for grant in grants.iter_mut().filter(|g| g.vault == fresh.folder_id) {
                    changed |= grant.refresh_source(&fresh, false)?;
                }
                changed |= *attached != fresh;
                *attached = fresh;
            }
        }
        if changed {
            db.attached = attached;
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            {
                db.share_grants = grants;
            }
            self.save(&db)?;
        }
        Ok(())
    }
}
impl Operation {
    fn first_sync(&self) -> bool {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if matches!(self, Self::FirstSync { .. }) {
            return true;
        }
        false
    }
    fn removal_only(&self) -> bool {
        matches!(
            self,
            Self::RemoveComputer { .. } | Self::PollRemovals {} | Self::AcknowledgeRemoval { .. }
        )
    }
    fn read_only(&self) -> bool {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if matches!(self, Self::Inspect { .. }) {
            return true;
        }
        false
    }
}
/// Every distinct endpoint this computer serves its paired computers on, the
/// current one first (#2589). Approval pins the inviter's own listener, so each
/// such pin shares the address this computer last listened on; a computer that
/// never listened serves nothing. A computer this one joined (its catalog or a
/// vault received from it) is remote even on the same address: a receiving
/// peer's endpoint never becomes a local listener, and no pin is invented.
fn served(db: &Database) -> Vec<SocketAddr> {
    let Some(own) = db.endpoint else {
        return vec![];
    };
    let joined =
        |id: &Uuid| db.catalog_peer == Some(*id) || db.transfers.iter().any(|t| t.peer() == *id);
    let mut endpoints: Vec<_> = db
        .peers
        .iter()
        .chain(db.revoked_peers.iter())
        .filter(|&(id, peer)| {
            peer.endpoint == own || (peer.endpoint.ip() == own.ip() && !joined(id))
        })
        .map(|(_, peer)| peer.endpoint)
        .collect();
    endpoints.sort_by_key(|endpoint| (*endpoint != own, *endpoint));
    endpoints.dedup();
    endpoints.truncate(MAX_PEERS);
    endpoints
}
/// The address is not assigned to this computer (any more).
fn not_local(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::AddrNotAvailable)
    })
}
fn snapshot(db: &Database) -> Result<Snapshot> {
    let current = now()?;
    let offer = db
        .invitation
        .as_ref()
        .filter(|i| !i.rejected && i.approved.is_none() && i.offer.expires > current);
    let pending = if let Some(i) = offer {
        i.request.as_ref().map(|r| pending(r, true))
    } else {
        db.outbound
            .as_ref()
            .filter(|(o, _)| o.expires > current)
            .map(|(_, r)| pending(r, false))
    };
    Ok(Snapshot {
        computers: vec![],
        pending_removals: removal::pending(db),
        policy_warnings: Vec::new(),
        history_warnings: Vec::new(),
        history_retention: history_settings(db),
        offer_url: offer.map(|i| encode_url(&i.offer)).transpose()?,
        pending,
        peers: db.peers.values().map(|p| p.peer.clone()).collect(),
        catalog: db.catalog.clone(),
        attached: db
            .attached
            .iter()
            .map(|attached| {
                let mut named = attached.clone();
                if let Some(label) = transfer::receiver_label(db, attached) {
                    named.label = label.into();
                }
                named
            })
            .collect(),
        shared: db
            .sources
            .iter()
            .filter(|s| s.active)
            .map(|s| s.path.clone())
            .collect(),
        preparing: db.transfers.iter().any(transfer::Transfer::preparing),
        status: if db.local_removed {
            "This computer was removed from Sync. Its files are kept."
        } else if db
            .outbound
            .as_ref()
            .is_some_and(|(o, _)| o.expires <= current)
        {
            "Pairing invitation expired"
        } else if db.outbound.is_some() {
            "Waiting for confirmation on the other computer"
        } else if offer.is_some() {
            "Waiting for the new computer"
        } else if db.transfers.iter().any(transfer::Transfer::legacy_hold) {
            "Initial receive held: ownership witness unavailable"
        } else if db.transfers.iter().any(transfer::Transfer::preparing) {
            "Receiving vault; checking safe bidirectional readiness"
        } else if !db.transfers.is_empty() {
            "Bidirectional Sync active"
        } else {
            "Ready"
        }
        .into(),
    })
}
fn pending(r: &Request, incoming: bool) -> Pending {
    let fingerprint = wire::fingerprint(&r.public);
    Pending {
        request_id: r.id,
        name: r.name.clone(),
        device_id: r
            .syncthing_device
            .clone()
            .unwrap_or_else(|| fingerprint.clone()),
        fingerprint,
        incoming,
    }
}
fn catalog_contains(db: &Database, peer: Uuid, vault: &str) -> bool {
    db.catalog_peer == Some(peer) && db.catalog.iter().any(|v| v.id == vault)
}
fn catalog_for(db: &Database, allowed: &[String]) -> Vec<Vault> {
    db.attached
        .iter()
        .filter(|v| allowed.contains(&v.folder_id))
        .map(|v| Vault {
            id: v.folder_id.clone(),
            label: v.label.clone(),
        })
        .collect()
}
fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
/// Validate a computer name without starting controller or runtime IO.
pub fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 100
            && name.trim() == name
            && !name.chars().any(char::is_control),
        "invalid computer name"
    );
    Ok(())
}
/// Display the actual source computer address without exposing the capability.
pub fn pairing_computer(url: &str) -> Result<String> {
    Ok(decode_url(url)?.endpoint.ip().to_string())
}
/// Validate pasted input before platform runtime bootstrap. Redemption rechecks
/// expiry because approval or bootstrap may take time. This performs no IO.
pub fn validate_pairing_input(url: &str, name: &str) -> Result<()> {
    validate_name(name)?;
    let offer = decode_url(url)?;
    ensure!(offer.expires > now()?, "pairing invitation expired");
    Ok(())
}
fn encode_url(offer: &Offer) -> Result<String> {
    Ok(format!(
        "okilum-sync://pair/{}#{}",
        offer.endpoint,
        wire::hex(&serde_json::to_vec(offer)?)
    ))
}
fn decode_url(input: &str) -> Result<Offer> {
    ensure!(
        input.len() <= 8192 && input.trim() == input,
        "invalid pairing URL"
    );
    let url = url::Url::parse(input)?;
    ensure!(
        url.scheme() == "okilum-sync"
            && url.host_str() == Some("pair")
            && url.query().is_none()
            && url.username().is_empty()
            && url.password().is_none(),
        "invalid pairing URL"
    );
    let encoded = url.fragment().context("pairing capability missing")?;
    ensure!(
        encoded.len().is_multiple_of(2) && encoded.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid pairing capability"
    );
    let bytes = (0..encoded.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&encoded[i..i + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let offer: Offer = serde_json::from_slice(&bytes)?;
    ensure!(
        url.path() == format!("/{}", offer.endpoint)
            && offer.public.len() == 32
            && !offer.id.is_nil()
            && !offer.endpoint.ip().is_unspecified()
            && !offer.endpoint.ip().is_multicast()
            && offer.endpoint.port() != 0,
        "pairing URL scope differs"
    );
    ensure!(
        offer.expires <= now()? + LIFETIME,
        "invalid pairing invitation lifetime"
    );
    Ok(offer)
}

impl Controller {
    fn reconcile_create_ignores(&self) -> Result<()> {
        self.check()?;
        let _guard = self.shared.transfer_lock.lock().unwrap();
        let mut warnings = Vec::new();
        self.reconcile_source_ignores(&mut warnings);
        self.reconcile_transfer_ignores(&mut warnings);
        self.reconcile_source_shared_ignores(&mut warnings);
        self.reconcile_transfer_shared_ignores(&mut warnings);
        *self.shared.policy_warnings.lock().unwrap() = warnings;
        Ok(())
    }
}

fn history_settings(db: &Database) -> Vec<RetentionSetting> {
    let mut settings = transfer::history_settings(db);
    settings.extend(source::history_settings(db));
    settings
}

#[cfg(test)]
mod tests {
    use super::*;
    fn controller(root: &Path, name: &str) -> Result<Controller> {
        Controller::open(root.join(name), vec![])
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn sync_2419_pairing_poll_does_not_wait_for_managed_runtime_inspection() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let prepared = directory.path().join("prepared");
        std::fs::create_dir(&prepared)?;
        std::fs::write(prepared.join("prepared.json"), b"invalid runtime receipt")?;
        let controller = Controller::open(directory.path().join("direct"), vec![])?;
        controller.begin_pairing_at("Desktop", "127.0.0.1:0".parse()?, "127.0.0.1".parse()?)?;
        controller.set_configurations(vec![prepared.join("syncthing/config.xml")])?;
        let polled = controller.refresh_pairing()?;
        assert!(
            polled.offer_url.is_some(),
            "invitation observation uses the live journal"
        );
        assert!(
            controller.refresh().is_err(),
            "positive control: full refresh still checks the runtime"
        );
        controller.shutdown();
        assert!(
            controller.refresh_pairing().is_err(),
            "fast polling honors revocation"
        );
        Ok(())
    }
    #[test]
    fn direct_request_identity_and_unknown_operations_fail_before_claim() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = controller(root.path(), "request-validation")?;
        let url = offer(&source)?;
        let offer = decode_url(&url)?;
        let signer = wire::public(&wire::random()?)?;
        let valid = Request {
            id: Uuid::new_v4(),
            name: "Олег — מחשב 💻".into(),
            public: signer.clone(),
            syncthing_device: Some(["AAAAAAA"; 8].join("-")),
        };
        valid.validate(&signer)?;
        let restored: Request = serde_json::from_slice(&serde_json::to_vec(&valid)?)?;
        assert_eq!(restored.name, valid.name);
        let mut malformed = vec![];
        let mut changed = valid.clone();
        changed.id = Uuid::nil();
        malformed.push(changed);
        let mut changed = valid.clone();
        changed.public = vec![0; 32];
        malformed.push(changed);
        let mut changed = valid.clone();
        changed.syncthing_device = Some("untrusted-device".into());
        malformed.push(changed);
        let mut changed = valid.clone();
        changed.name = "bad\nname".into();
        malformed.push(changed);
        let mut db = source.shared.db.lock().unwrap();
        for request in malformed {
            assert!(source
                .dispatch(
                    &mut db,
                    offer.id,
                    Operation::Redeem { request },
                    signer.clone()
                )
                .is_err());
            assert!(
                db.invitation.as_ref().unwrap().request.is_none(),
                "invalid request claimed invitation"
            );
            assert!(db.peers.is_empty());
        }
        assert!(matches!(
            source.dispatch(
                &mut db,
                offer.id,
                Operation::Redeem { request: valid },
                signer
            )?,
            Reply::Pending
        ));
        assert!(
            db.invitation.as_ref().unwrap().request.is_some(),
            "positive valid claim control"
        );
        drop(db);
        source.shutdown();
        assert!(matches!(
            serde_json::from_value::<Operation>(serde_json::json!({"operation":"catalog"}))?,
            Operation::Catalog {}
        ));
        assert!(serde_json::from_value::<Operation>(
            serde_json::json!({"operation":"catalog","unexpected":true})
        )
        .is_err());
        assert!(serde_json::from_value::<Operation>(
            serde_json::json!({"operation":"folder_delete","vault":"private"})
        )
        .is_err());
        assert!(serde_json::from_value::<Request>(
            serde_json::json!({"id":Uuid::new_v4(),"name":"Name","public":vec![0;32],"extra":true})
        )
        .is_err());
        Ok(())
    }
    #[test]
    fn pairing_preflight_checks_scope_expiry_and_names_without_controller() -> Result<()> {
        let mut offer = Offer {
            id: Uuid::new_v4(),
            key: [7; 32],
            expires: now()? + 300,
            endpoint: "127.0.0.1:12345".parse()?,
            public: vec![3; 32],
        };
        let url = encode_url(&offer)?;
        validate_pairing_input(&url, "New computer")?;
        assert!(validate_pairing_input("not a URL", "New computer").is_err());
        assert!(validate_pairing_input(&url.replace("12345#", "12346#"), "New computer").is_err());
        assert!(validate_pairing_input(&url, "bad\nname").is_err());
        assert!(validate_pairing_input(&url, &"é".repeat(51)).is_err());
        offer.expires = now()? - 1;
        let error = validate_pairing_input(&encode_url(&offer)?, "New computer").unwrap_err();
        assert!(error.to_string().contains("expired"));
        Ok(())
    }
    #[test]
    fn attachment_rejects_catalog_from_another_peer_before_daemon_access() -> Result<()> {
        let root = tempfile::tempdir()?;
        let controller = controller(root.path(), "catalog-binding")?;
        let selected = Uuid::new_v4();
        let other = Uuid::new_v4();
        {
            let mut db = controller.shared.db.lock().unwrap();
            db.catalog = vec![Vault {
                id: "same-id".into(),
                label: "Selected vault".into(),
            }];
            db.catalog_peer = Some(selected);
            assert!(
                catalog_contains(&db, selected, "same-id"),
                "positive selected catalog control"
            );
            assert!(!catalog_contains(&db, other, "same-id"));
        }
        let error = controller
            .attach(other, "same-id", root.path())
            .unwrap_err();
        assert!(error.to_string().contains("selected computer"));
        {
            let mut db = controller.shared.db.lock().unwrap();
            db.catalog_peer = None;
            assert!(
                !catalog_contains(&db, selected, "same-id"),
                "legacy catalog cannot infer authority"
            );
        }
        controller.shutdown();
        Ok(())
    }
    #[test]
    fn configuration_snapshot_tracks_bootstrap_selection_and_capability_revocation() -> Result<()> {
        let root = tempfile::tempdir()?;
        let access = okilum_sync::NetworkAccess::new(true);
        let _scope = access.enter();
        let initial = root.path().join("external/config.xml");
        let current = Controller::open(root.path().join("direct"), vec![initial.clone()])?;
        let retained = current.clone();
        assert_eq!(current.configuration_paths()?, vec![initial]);
        let managed = root.path().join("managed/config.xml");
        current.set_configurations(vec![managed.clone()])?;
        let mut snapshot = retained.configuration_paths()?;
        assert_eq!(snapshot, vec![managed.clone()]);
        snapshot.clear();
        assert_eq!(current.configuration_paths()?, vec![managed]);
        access.revoke();
        assert!(retained.configuration_paths().is_err());
        assert!(retained.set_configurations(vec![]).is_err());
        Ok(())
    }
    #[test]
    fn configuration_snapshot_refuses_a_stopped_controller_and_retained_clone() -> Result<()> {
        let root = tempfile::tempdir()?;
        let current = controller(root.path(), "direct")?;
        let retained = current.clone();
        assert!(retained.configuration_paths()?.is_empty());
        current.shutdown();
        assert!(current.configuration_paths().is_err());
        assert!(retained.configuration_paths().is_err());
        Ok(())
    }
    fn offer(controller: &Controller) -> Result<String> {
        Ok(controller
            .begin_pairing_at(
                "Source computer",
                "127.0.0.1:0".parse()?,
                "127.0.0.1".parse()?,
            )?
            .offer_url
            .unwrap())
    }
    #[test]
    fn real_encrypted_pairing_requires_old_confirmation_and_bootstraps_private_catalog(
    ) -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = controller(root.path(), "source")?;
        let target = controller(root.path(), "target")?;
        // Only a local synthetic catalog fixture; no daemon/config/files touched.
        source
            .shared
            .db
            .lock()
            .unwrap()
            .attached
            .push(ExistingVault {
                daemon: crate::daemon::DaemonIdentity {
                    config_file: root.path().join("not-a-daemon.xml"),
                    device_id: "fixture".into(),
                    certificate_hash: "fixture".into(),
                },
                version: "v2.1.7".into(),
                folder_id: "private-vault".into(),
                path: root.path().join("private-local-path"),
                label: "Private vault".into(),
                paused: false,
                folder_type: "sendreceive".into(),
                peers: vec![],
                root_identity: None,
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                stable_root_identity: None,
            });
        let url = offer(&source)?;
        assert!(!url.contains("private-local-path"));
        let pending = target.redeem(&url, "New computer")?;
        assert!(pending.catalog.is_empty());
        assert!(!pending.pending.unwrap().incoming);
        let request = source.snapshot()?.pending.unwrap();
        assert!(request.incoming);
        assert_eq!(request.name, "New computer");
        assert!(source.approve(Uuid::new_v4()).is_err());
        let attacker = controller(root.path(), "attacker")?;
        assert!(
            attacker.redeem(&url, "Other computer").is_err(),
            "claimed invitation accepted another identity"
        );
        source.approve(request.request_id)?;
        let approved = target.refresh()?;
        assert!(approved.pending.is_none());
        assert_eq!(
            approved.catalog,
            vec![Vault {
                id: "private-vault".into(),
                label: "Private vault".into()
            }]
        );
        assert_eq!(approved.peers.len(), 1);
        let peer = approved.peers[0].id;
        assert_eq!(target.catalog(peer)?, approved.catalog);
        assert_eq!(
            target.refresh()?.catalog,
            approved.catalog,
            "refresh lost catalog"
        );
        assert!(
            source.approve(request.request_id).is_err(),
            "approval was not single use"
        );
        assert!(target.attach(peer, "private-vault", root.path()).is_err());
        source.shutdown();
        target.shutdown();
        attacker.shutdown();
        Ok(())
    }
    #[test]
    fn connect_retry_preserves_claimed_identity_through_approval_and_restart() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = controller(root.path(), "source")?;
        let target = controller(root.path(), "target")?;
        let url = offer(&source)?;
        target.redeem(&url, "New computer")?;
        let request = source.snapshot()?.pending.unwrap();
        assert!(
            request.incoming,
            "positive control: source received request"
        );
        assert!(target.redeem(&url, "Changed name").is_err());
        let retry = target.redeem(&url, "New computer")?;
        assert_eq!(retry.pending.unwrap().request_id, request.request_id);
        drop(target);
        let target = controller(root.path(), "target")?;
        source.approve(request.request_id)?;
        let approved = target.redeem(&url, "New computer")?;
        assert!(approved.pending.is_none());
        assert_eq!(approved.peers.len(), 1);
        assert_eq!(source.snapshot()?.peers.len(), 1);
        source.shutdown();
        target.shutdown();
        Ok(())
    }
    #[test]
    fn cancelled_request_cannot_accept_an_inflight_approval() -> Result<()> {
        let root = tempfile::tempdir()?;
        let target = controller(root.path(), "target")?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let seed = wire::random()?;
        let offer = Offer {
            id: Uuid::new_v4(),
            key: wire::random()?,
            expires: now()? + LIFETIME,
            endpoint: listener.local_addr()?,
            public: wire::public(&seed)?,
        };
        let url = encode_url(&offer)?;
        let (received, receipt) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let server = thread::spawn(move || -> Result<()> {
            let (mut stream, _) = listener.accept()?;
            wire::limits(&stream)?;
            let (message, _): (Message, _) =
                wire::open(wire::read(&mut stream)?, &offer.key, None)?;
            ensure!(
                matches!(message.operation, Operation::Redeem { .. }),
                "expected redeem"
            );
            received.send(())?;
            released.recv_timeout(Duration::from_secs(3))?;
            let reply = Reply::Approved {
                paired: Paired {
                    peer: Peer {
                        id: Uuid::new_v4(),
                        name: "Source computer".into(),
                        device_id: wire::fingerprint(&offer.public),
                    },
                    public: offer.public,
                    key: wire::random()?,
                    endpoint: offer.endpoint,
                    allowed: vec![],
                    syncthing_device: None,
                    addresses: vec![],
                },
                catalog: vec![Vault {
                    id: "private".into(),
                    label: "Private".into(),
                }],
            };
            let response = wire::seal(
                offer.id,
                &offer.key,
                &seed,
                &Response {
                    nonce: message.nonce,
                    result: reply,
                    transport: None,
                },
            )?;
            wire::write(&mut stream, &response)
        });
        let retained = target.clone();
        let redeem = thread::spawn(move || retained.redeem(&url, "New computer"));
        receipt.recv_timeout(Duration::from_secs(3))?;
        assert!(
            target.snapshot()?.pending.is_some(),
            "positive control: request in flight"
        );
        target.cancel()?;
        release.send(())?;
        server.join().unwrap()?;
        assert!(redeem.join().unwrap().is_err());
        let cancelled = target.snapshot()?;
        assert!(cancelled.pending.is_none());
        assert!(cancelled.peers.is_empty());
        assert!(cancelled.catalog.is_empty());
        target.shutdown();
        Ok(())
    }
    #[test]
    fn expired_outbound_is_terminal_and_can_be_replaced() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = controller(root.path(), "source")?;
        let target = controller(root.path(), "target")?;
        let url = offer(&source)?;
        target.redeem(&url, "New computer")?;
        assert!(
            target.snapshot()?.pending.is_some(),
            "positive control: live request"
        );
        {
            let mut db = target.shared.db.lock().unwrap();
            db.outbound.as_mut().unwrap().0.expires = now()? - 1;
            target.save(&db)?;
        }
        assert_eq!(target.snapshot()?.status, "Pairing invitation expired");
        assert!(target.refresh().is_err());
        assert!(target.shared.db.lock().unwrap().outbound.is_none());
        drop(target);
        let target = controller(root.path(), "target")?;
        assert!(target.shared.db.lock().unwrap().outbound.is_none());
        assert!(target.snapshot()?.pending.is_none());
        source.cancel()?;
        let replacement = offer(&source)?;
        assert!(target
            .redeem(&replacement, "New computer")?
            .pending
            .is_some());
        source.shutdown();
        target.shutdown();
        Ok(())
    }
    #[test]
    fn approval_wait_survives_closed_reply_and_human_delay() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = controller(root.path(), "source")?;
        let target = controller(root.path(), "target")?;
        let url = offer(&source)?;
        let mut decoded = decode_url(&url)?;
        // A forwarding inviter drops the first response after accepting the
        // request, modelling an older peer's short socket deadline.
        let proxy = TcpListener::bind("127.0.0.1:0")?;
        let destination = decoded.endpoint;
        decoded.endpoint = proxy.local_addr()?;
        let url = encode_url(&decoded)?;
        let proxy_thread = thread::spawn(move || -> Result<()> {
            let (mut first, _) = proxy.accept()?;
            let envelope: wire::Envelope = wire::read(&mut first)?;
            let mut remote = wire::connect(destination)?;
            wire::write(&mut remote, &envelope)?;
            let _: wire::Envelope = wire::read(&mut remote)?;
            // The response is deliberately lost, but the claimed identity stays.
            thread::sleep(Duration::from_millis(3100));
            drop(first);
            let (mut retry, _) = proxy.accept()?;
            let envelope: wire::Envelope = wire::read(&mut retry)?;
            let mut remote = wire::connect(destination)?;
            wire::write(&mut remote, &envelope)?;
            let response: wire::Envelope = wire::read(&mut remote)?;
            wire::write(&mut retry, &response)?;
            Ok(())
        });
        let waiting = target.redeem(&url, "Joining computer")?;
        let request = waiting
            .pending
            .context("lost reply must remain pending")?
            .request_id;
        assert_eq!(source.snapshot()?.pending.unwrap().request_id, request);
        // Human approval takes longer than the original three-second timeout.
        thread::sleep(Duration::from_millis(3100));
        let waiting = target.refresh_pairing()?;
        assert_eq!(waiting.pending.unwrap().request_id, request);
        proxy_thread.join().unwrap()?;
        // Continue against the original endpoint after the controlled proxy.
        {
            let mut db = target.shared.db.lock().unwrap();
            db.outbound.as_mut().unwrap().0.endpoint = destination;
            target.save(&db)?;
        }
        source.approve(request)?;
        let approved = target.refresh_pairing()?;
        assert!(approved.pending.is_none());
        assert_eq!(approved.peers.len(), 1);
        source.shutdown();
        target.shutdown();
        Ok(())
    }
    #[test]
    fn issue2589_saved_pairing_is_read_without_opening_or_creating_the_journal() -> Result<()> {
        let root = tempfile::tempdir()?;
        let (source_state, target_state) = (root.path().join("source"), root.path().join("target"));
        assert!(!has_paired_computers_or_vaults(&source_state)?);
        assert!(!source_state.exists(), "the check never creates Sync state");
        let source = controller(root.path(), "source")?;
        let target = controller(root.path(), "target")?;
        assert!(
            !has_paired_computers_or_vaults(&source_state)?,
            "no pair yet"
        );
        let url = offer(&source)?;
        target.redeem(&url, "New computer")?;
        assert!(
            !has_paired_computers_or_vaults(&target_state)?,
            "an unanswered request pairs nothing"
        );
        source.approve(source.snapshot()?.pending.unwrap().request_id)?;
        assert_eq!(target.refresh_pairing()?.peers.len(), 1, "positive control");
        // Both owners still hold their journal locks.
        assert!(has_paired_computers_or_vaults(&source_state)?);
        assert!(has_paired_computers_or_vaults(&target_state)?);
        source.shutdown();
        target.shutdown();
        Ok(())
    }
    #[test]
    fn explicit_refresh_restores_only_the_durable_source_endpoint() -> Result<()> {
        let root = tempfile::tempdir()?;
        let access = okilum_sync::NetworkAccess::new(true);
        let _scope = access.enter();
        let source = controller(root.path(), "source")?;
        let target = controller(root.path(), "target")?;
        let url = offer(&source)?;
        let endpoint = decode_url(&url)?.endpoint;
        target.redeem(&url, "Restart fixture")?;
        source.approve(source.snapshot()?.pending.unwrap().request_id)?;
        let peer = target.refresh()?.peers[0].id;
        assert!(
            target.catalog(peer)?.is_empty(),
            "encrypted positive control"
        );
        let worker = Arc::downgrade(&source.shared);
        source.shutdown();
        drop(source);
        // A worker finishing its last request may briefly retain the journal.
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while worker.strong_count() != 0 && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(worker.strong_count(), 0, "source worker did not terminate");
        // No daemon or Syncthing configuration is used by this fixture.
        let source = controller(root.path(), "source")?;
        assert!(source.shared.listeners.lock().unwrap().is_empty());
        assert!(wire::connect(endpoint).is_err(), "open must stay offline");
        assert!(target.catalog(peer).is_err());
        let occupied = TcpListener::bind(endpoint)?;
        assert!(source.refresh().is_err(), "no alternate-port fallback");
        assert!(source.shared.listeners.lock().unwrap().is_empty());
        assert_eq!(source.shared.db.lock().unwrap().endpoint, Some(endpoint));
        drop(occupied);
        source.refresh()?;
        assert!(
            target.catalog(peer)?.is_empty(),
            "restored encrypted catalog"
        );
        assert_eq!(source.shared.db.lock().unwrap().endpoint, Some(endpoint));
        target.refresh()?;
        assert!(target.shared.listeners.lock().unwrap().is_empty());
        source.cancel()?;
        access.revoke();
        assert!(
            source.refresh().is_err(),
            "revoked access restored a listener"
        );
        assert!(target.refresh().is_err());
        assert!(source.shared.listeners.lock().unwrap().is_empty());
        assert!(wire::connect(endpoint).is_err());
        source.shutdown();
        target.shutdown();
        Ok(())
    }
    /// Pair `target` through an invitation `source` creates on `bind`; returns
    /// the target's peer id and the endpoint the target pins.
    fn pair_at(
        source: &Controller,
        target: &Controller,
        name: &str,
        bind: SocketAddr,
    ) -> Result<(Uuid, SocketAddr)> {
        let url = source
            .begin_pairing_at("Source computer", bind, "127.0.0.1".parse()?)?
            .offer_url
            .unwrap();
        target.redeem(&url, name)?;
        source.approve(source.snapshot()?.pending.unwrap().request_id)?;
        let paired = target.refresh_pairing()?;
        assert!(paired.pending.is_none(), "positive control: {name} paired");
        let peer = paired.peers.last().unwrap().id;
        Ok((peer, decode_url(&url)?.endpoint))
    }
    /// Stop a controller like a quit app, then open its journal again offline.
    fn reopen(root: &Path, name: &str, old: Controller) -> Result<Controller> {
        let worker = Arc::downgrade(&old.shared);
        old.shutdown();
        drop(old);
        // A worker finishing its last request may briefly retain the journal.
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while worker.strong_count() != 0 && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(worker.strong_count(), 0, "{name} worker did not terminate");
        controller(root, name)
    }
    fn listening(controller: &Controller) -> Vec<SocketAddr> {
        controller
            .shared
            .listeners
            .lock()
            .unwrap()
            .keys()
            .copied()
            .collect()
    }
    #[test]
    fn issue2589_refresh_serves_every_endpoint_paired_computers_pin() -> Result<()> {
        let root = tempfile::tempdir()?;
        let access = okilum_sync::NetworkAccess::new(true);
        let _scope = access.enter();
        let source = controller(root.path(), "source")?;
        let muninn = controller(root.path(), "muninn")?;
        let mac = controller(root.path(), "mac")?;
        let (muninn_peer, first) = pair_at(&source, &muninn, "muninn", "127.0.0.1:0".parse()?)?;
        // The live divergence: a later process invited the Mac on another port.
        // Choose it while `first` is still taken, so the two cannot coincide.
        let port = TcpListener::bind("127.0.0.1:0")?.local_addr()?;
        let source = reopen(root.path(), "source", source)?;
        let (mac_peer, second) = pair_at(&source, &mac, "Mac", port)?;
        assert_ne!(first, second, "positive control: the pins diverge");
        assert_eq!(source.shared.db.lock().unwrap().endpoint, Some(second));
        let source = reopen(root.path(), "source", source)?;
        assert!(listening(&source).is_empty(), "open stays offline");
        assert!(muninn.catalog(muninn_peer).is_err());
        source.refresh()?;
        let mut both = vec![first, second];
        both.sort();
        assert_eq!(listening(&source), both);
        assert!(
            muninn.catalog(muninn_peer)?.is_empty(),
            "the first computer reaches the inviter again without pairing again"
        );
        assert!(mac.catalog(mac_peer)?.is_empty());
        assert_eq!(
            source.shared.db.lock().unwrap().endpoint,
            Some(second),
            "refresh keeps every pin and the invitation endpoint"
        );
        // An occupied older pin is skipped without failing the refresh (the app
        // would otherwise drop its controller and every healthy listener), it is
        // never moved to another port, and the current pin is still served.
        let source = reopen(root.path(), "source", source)?;
        let occupied = TcpListener::bind(first)?;
        source.refresh()?;
        assert_eq!(listening(&source), [second], "no alternate-port fallback");
        assert!(mac.catalog(mac_peer)?.is_empty());
        drop(occupied);
        source.refresh()?;
        assert!(muninn.catalog(muninn_peer)?.is_empty());
        // The current endpoint stays fatal: Settings reports it.
        let source = reopen(root.path(), "source", source)?;
        let occupied = TcpListener::bind(second)?;
        assert!(
            source.refresh().is_err(),
            "the invitation endpoint must be served"
        );
        drop(occupied);
        source.refresh()?;
        // A receiving computer never listens on its inviter's endpoint, even
        // on a shared address after it invites a computer of its own. Its own
        // port is chosen while the inviter still holds both pins.
        let own = decode_url(&offer(&mac)?)?.endpoint;
        let source = reopen(root.path(), "source", source)?;
        muninn.refresh()?;
        mac.refresh()?;
        assert!(listening(&muninn).is_empty());
        assert_eq!(listening(&mac), [own]);
        assert!(wire::connect(first).is_err() && wire::connect(second).is_err());
        source.shutdown();
        muninn.shutdown();
        mac.shutdown();
        Ok(())
    }
    #[test]
    fn issue2589_new_invitation_reuses_the_endpoint_paired_computers_pin() -> Result<()> {
        let root = tempfile::tempdir()?;
        let access = okilum_sync::NetworkAccess::new(true);
        let _scope = access.enter();
        let source = controller(root.path(), "source")?;
        let muninn = controller(root.path(), "muninn")?;
        let mac = controller(root.path(), "mac")?;
        let (muninn_peer, first) = pair_at(&source, &muninn, "muninn", "127.0.0.1:0".parse()?)?;
        // A new process (the app after a restart, or `okilum sync invite`)
        // asks for any port; it gets the one muninn pins.
        let source = reopen(root.path(), "source", source)?;
        let (mac_peer, second) = pair_at(&source, &mac, "Mac", "127.0.0.1:0".parse()?)?;
        assert_eq!(second, first, "the new invitation opened another port");
        assert!(muninn.catalog(muninn_peer)?.is_empty());
        assert!(mac.catalog(mac_peer)?.is_empty());
        assert_eq!(served(&source.shared.db.lock().unwrap()), [first]);
        // Cancel closes the listener; the next invitation listens there again.
        source.cancel()?;
        assert!(listening(&source).is_empty());
        assert_eq!(decode_url(&offer(&source)?)?.endpoint, first);
        assert!(muninn.catalog(muninn_peer)?.is_empty());
        source.shutdown();
        muninn.shutdown();
        mac.shutdown();
        Ok(())
    }
    #[test]
    fn issue2589_refresh_skips_a_pinned_address_this_computer_no_longer_has() -> Result<()> {
        let root = tempfile::tempdir()?;
        let access = okilum_sync::NetworkAccess::new(true);
        let _scope = access.enter();
        let source = controller(root.path(), "source")?;
        let target = controller(root.path(), "target")?;
        let (peer, _) = pair_at(&source, &target, "Target", "127.0.0.1:0".parse()?)?;
        let source = reopen(root.path(), "source", source)?;
        // As after the address changed: TEST-NET-1 is assigned to no host.
        let gone: SocketAddr = "192.0.2.10:45000".parse()?;
        {
            let mut db = source.shared.db.lock().unwrap();
            db.endpoint = Some(gone);
            db.peers.get_mut(&peer).unwrap().endpoint = gone;
        }
        assert_eq!(served(&source.shared.db.lock().unwrap()), [gone]);
        source.refresh()?;
        assert!(listening(&source).is_empty());
        assert_eq!(
            source.shared.db.lock().unwrap().peers[&peer].endpoint,
            gone,
            "a pin is never replaced"
        );
        source.shutdown();
        target.shutdown();
        Ok(())
    }
    #[test]
    fn revocation_closes_actual_listener_and_retained_clone_cannot_send() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = controller(root.path(), "source")?;
        let target = controller(root.path(), "target")?;
        let url = offer(&source)?;
        target.redeem(&url, "Positive control")?;
        assert!(
            source.snapshot()?.pending.is_some(),
            "positive network control did not fire"
        );
        let endpoint = decode_url(&url)?.endpoint;
        let retained = source.clone();
        source.shutdown();
        assert!(retained.snapshot().is_err());
        assert!(retained.begin_pairing("Disabled").is_err());
        assert!(
            wire::connect(endpoint).is_err(),
            "disabled listener still accepts TCP"
        );
        assert!(target.refresh().is_err());
        target.shutdown();
        Ok(())
    }
    #[test]
    fn expiry_rejection_and_wrong_identity_never_return_catalog() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = controller(root.path(), "source")?;
        let target = controller(root.path(), "target")?;
        let url = offer(&source)?;
        target.redeem(&url, "New computer")?;
        let request = source.snapshot()?.pending.unwrap();
        source.reject(request.request_id)?;
        assert!(target.refresh().is_err());
        assert!(target.snapshot()?.catalog.is_empty());
        source.cancel()?;
        let url = offer(&source)?;
        let mut expired = decode_url(&url)?;
        expired.expires = now()? - 1;
        assert!(target.redeem(&encode_url(&expired)?, "Expired").is_err());
        let mut wrong = decode_url(&url)?;
        wrong.public = vec![1; 32];
        assert!(target
            .redeem(&encode_url(&wrong)?, "Wrong intended computer")
            .is_err());
        assert!(target.snapshot()?.catalog.is_empty());
        source.shutdown();
        target.shutdown();
        Ok(())
    }
    #[test]
    fn encrypted_frame_binds_scope_signer_and_content() -> Result<()> {
        let seed = wire::random()?;
        let credential = wire::random()?;
        let id = Uuid::new_v4();
        let message = Message {
            nonce: Uuid::new_v4(),
            target: wire::public(&seed)?,
            operation: Operation::Catalog {},
            transport: None,
        };
        let frame = wire::seal(id, &credential, &seed, &message)?;
        let (decoded, _): (Message, _) =
            wire::open(frame, &credential, Some(&wire::public(&seed)?))?;
        assert_eq!(decoded.nonce, message.nonce);
        let mut wrong_scope = wire::seal(id, &credential, &seed, &message)?;
        wrong_scope.scope = Uuid::new_v4();
        assert!(wire::open::<Message>(wrong_scope, &credential, None).is_err());
        let frame = wire::seal(id, &credential, &seed, &message)?;
        assert!(wire::open::<Message>(frame, &wire::random()?, None).is_err());
        let frame = wire::seal(id, &credential, &seed, &message)?;
        assert!(wire::open::<Message>(frame, &credential, Some(&[1; 32])).is_err());
        Ok(())
    }
}
