//! LAN-first computer pairing, independent of service enrollment and Syncthing
//! lifecycle. Opening this controller does not listen or start a daemon.
mod existing;
mod transfer;
mod wire;
use crate::private;
use anyhow::{ensure, Context, Result};
pub use existing::{
    attach_existing, configuration_paths, inspect_existing, AttachedVault, ExistingVault,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr, TcpListener, UdpSocket},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const LIFETIME: u64 = 600;
const MAX_PEERS: usize = 64;

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
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub offer_url: Option<String>,
    pub pending: Option<Pending>,
    pub peers: Vec<Peer>,
    pub catalog: Vec<Vault>,
    pub attached: Vec<ExistingVault>,
    pub status: String,
}

#[derive(Clone, Serialize, Deserialize)]
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
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Database {
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
    transfers: Vec<transfer::Transfer>,
}
struct Shared {
    state: PathBuf,
    configs: Mutex<Vec<PathBuf>>,
    db: Mutex<Database>,
    listener: Mutex<Option<Listener>>,
    stopped: AtomicBool,
    access: okilum_sync::NetworkAccess,
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
    Redeem { request: Request },
    Catalog,
    Share { vault: String, device: String },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    nonce: Uuid,
    target: Vec<u8>,
    operation: Operation,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    nonce: Uuid,
    result: Reply,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Pending,
    Rejected,
    Approved { paired: Paired, catalog: Vec<Vault> },
    Catalog { catalog: Vec<Vault> },
    Shared { connection: transfer::Connection },
    Error { message: String },
}

impl Controller {
    pub fn open(state: PathBuf, configs: Vec<PathBuf>) -> Result<Self> {
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
                transfers: vec![],
            }
        };
        // Persist identity once, before creating an offer or exposing its public key.
        private::write(&file, &serde_json::to_vec(&db)?)?;
        Ok(Self {
            shared: Arc::new(Shared {
                state,
                configs: Mutex::new(configs),
                db: Mutex::new(db),
                listener: Mutex::new(None),
                stopped: AtomicBool::new(false),
                access: okilum_sync::NetworkAccess::current(),
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
        *self.shared.configs.lock().unwrap() = configs;
        Ok(())
    }
    /// Nonblocking capability revocation. It closes the application listener and
    /// makes retained clones unable to send. It never stops an external daemon.
    pub fn shutdown(&self) {
        self.shared.stopped.store(true, Ordering::Release);
        if let Ok(mut listener) = self.shared.listener.try_lock() {
            listener.take();
        }
    }
    pub fn snapshot(&self) -> Result<Snapshot> {
        self.check()?;
        snapshot(&self.shared.db.lock().unwrap())
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
        if let Some(old) = db.attached.iter().find(|v| v.folder_id == fresh.folder_id) {
            ensure!(
                existing::same_binding(old, &fresh),
                "folder ID already associated with another replica"
            );
        } else {
            db.attached.push(fresh.clone());
        }
        self.save(&db)?;
        Ok(fresh)
    }
    pub fn begin_pairing(&self, name: &str) -> Result<Snapshot> {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        // UDP connect selects a local route; no datagram or discovery is sent.
        socket.connect("192.0.2.1:9")?;
        let ip = socket.local_addr()?.ip();
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
        ensure!(
            db.invitation
                .as_ref()
                .is_none_or(|i| i.offer.expires <= now().unwrap_or(0)
                    || i.rejected
                    || i.approved.is_some()),
            "cancel the current invitation before creating another"
        );
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
    fn listen(&self, bind: SocketAddr, advertised: IpAddr) -> Result<()> {
        self.check()?;
        let mut current = self.shared.listener.lock().unwrap();
        if current.is_some() {
            return Ok(());
        }
        let listener = TcpListener::bind(bind).context("cannot start LAN pairing listener")?;
        listener.set_nonblocking(true)?;
        let endpoint = SocketAddr::new(advertised, listener.local_addr()?.port());
        self.shared.db.lock().unwrap().endpoint = Some(endpoint);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let socket = listener.try_clone()?;
        let weak = Arc::downgrade(&self.shared);
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
                    drop(shared);
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            if let Some(shared) = weak.upgrade() {
                                // One bounded connection at a time; no unbounded worker queue.
                                if wire::limits(&stream).is_ok() {
                                    let controller = Controller { shared };
                                    let _ = controller.serve(&mut stream);
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
        *current = Some(Listener { stop, socket });
        Ok(())
    }
    pub fn redeem(&self, url: &str, name: &str) -> Result<Snapshot> {
        self.check()?;
        validate_name(name)?;
        let offer = decode_url(url)?;
        ensure!(offer.expires > now()?, "pairing invitation expired");
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
        };
        db.name = name.to_owned();
        db.outbound = Some((offer, request));
        self.save(&db)?;
        drop(db);
        self.refresh()
    }
    pub fn refresh(&self) -> Result<Snapshot> {
        self.check()?;
        let outbound = self.shared.db.lock().unwrap().outbound.clone();
        if let Some((offer, request)) = outbound {
            ensure!(offer.expires > now()?, "pairing invitation expired");
            let reply = self.call(
                offer.endpoint,
                offer.id,
                &offer.key,
                &offer.public,
                Operation::Redeem { request },
            )?;
            let mut db = self.shared.db.lock().unwrap();
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
                    db.peers.insert(paired.peer.id, paired);
                    db.outbound = None;
                    self.save(&db)?;
                }
                Reply::Error { message } => anyhow::bail!("{message}"),
                _ => anyhow::bail!("unexpected pairing response"),
            }
        }
        self.refresh_attached()?;
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
        self.shared.listener.lock().unwrap().take();
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
            Operation::Catalog,
        )? {
            Reply::Catalog { catalog } => {
                let mut db = self.shared.db.lock().unwrap();
                db.catalog = catalog.clone();
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
        let seed = self.shared.db.lock().unwrap().seed;
        let message = Message {
            nonce: Uuid::new_v4(),
            target: public.to_vec(),
            operation,
        };
        let frame = wire::seal(scope, credential, &seed, &message)?;
        let mut stream = wire::connect(endpoint)?;
        self.check()?;
        wire::write(&mut stream, &frame)?;
        let response = wire::read(&mut stream)?;
        let (response, _): (Response, _) = wire::open(response, credential, Some(public))?;
        ensure!(response.nonce == message.nonce, "stale pairing response");
        self.check()?;
        Ok(response.result)
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
            let peer = db.peers.get(&scope).context("unknown pairing capability")?;
            (peer.key, Some(peer.public.clone()))
        };
        let (message, signer): (Message, _) =
            wire::open(envelope, &credential, expected.as_deref())?;
        ensure!(
            message.target == wire::public(&db.seed)?,
            "wrong intended computer"
        );
        self.check()?;
        let reply = self
            .dispatch(&mut db, scope, message.operation, signer)
            .unwrap_or_else(|_| Reply::Error {
                message: "operation unavailable; check approved vault, replica and destination"
                    .into(),
            });
        self.check()?;
        let response = wire::seal(
            scope,
            &credential,
            &db.seed,
            &Response {
                nonce: message.nonce,
                result: reply,
            },
        )?;
        wire::write(stream, &response)
    }
    fn dispatch(
        &self,
        db: &mut Database,
        scope: Uuid,
        operation: Operation,
        signer: Vec<u8>,
    ) -> Result<Reply> {
        match operation {
            Operation::Redeem { request } => {
                validate_name(&request.name)?;
                ensure!(
                    request.public == signer && request.id != Uuid::nil(),
                    "request identity proof differs"
                );
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
                            && old.name == request.name,
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
                    };
                    Ok(Reply::Approved {
                        paired: remote,
                        catalog: catalog_for(db, &local.allowed),
                    })
                } else {
                    Ok(Reply::Pending)
                }
            }
            Operation::Catalog => {
                let peer = db.peers.get(&scope).context("computer not approved")?;
                Ok(Reply::Catalog {
                    catalog: catalog_for(db, &peer.allowed),
                })
            }
            Operation::Share { vault, device } => {
                let peer = db.peers.get(&scope).context("computer not approved")?;
                ensure!(peer.allowed.contains(&vault), "vault not authorized");
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
                Ok(Reply::Shared {
                    connection: transfer::share(&fresh, &device)?,
                })
            }
        }
    }
    fn save(&self, db: &Database) -> Result<()> {
        private::write(
            &self.shared.state.join("direct.json"),
            &serde_json::to_vec(db)?,
        )
    }
    fn refresh_attached(&self) -> Result<()> {
        // Explicitly read-only; offline replicas retain their durable association.
        let configs = self.shared.configs.lock().unwrap().clone();
        let mut db = self.shared.db.lock().unwrap();
        for attached in &mut db.attached {
            if let Ok(fresh) = inspect_existing(&configs, &attached.path) {
                ensure!(
                    existing::same_binding(attached, &fresh),
                    "attached replica identity changed"
                );
                *attached = fresh;
            }
        }
        Ok(())
    }
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
        offer_url: offer.map(|i| encode_url(&i.offer)).transpose()?,
        pending,
        peers: db.peers.values().map(|p| p.peer.clone()).collect(),
        catalog: db.catalog.clone(),
        attached: db.attached.clone(),
        status: if db.outbound.is_some() {
            "Waiting for confirmation on the other computer"
        } else if offer.is_some() {
            "Waiting for the new computer"
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
        device_id: fingerprint.clone(),
        fingerprint,
        incoming,
    }
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
fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 100
            && name.trim() == name
            && !name.chars().any(char::is_control),
        "invalid computer name"
    );
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
        encoded.len() % 2 == 0 && encoded.bytes().all(|b| b.is_ascii_hexdigit()),
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

#[cfg(test)]
mod tests {
    use super::*;
    fn controller(root: &Path, name: &str) -> Result<Controller> {
        Controller::open(root.join(name), vec![])
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
            operation: Operation::Catalog,
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
