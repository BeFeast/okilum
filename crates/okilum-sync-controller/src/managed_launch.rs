//! Linux managed launch and configuration admission (#2582).
//!
//! The runtime gate serializes only launch/enrollment/configuration operations;
//! it is never a substitute for the durable per-vault authority or a transport
//! quiescence receipt. Closed folders are fenced on disk BEFORE spawning.
#[cfg(target_os = "linux")]
use crate::lifecycle::ManagedInstance;
use crate::{daemon::DaemonIdentity, private};
#[cfg(any(target_os = "macos", windows))]
pub(crate) struct ManagedInstance {
    pub home: PathBuf,
    pub executable: PathBuf,
}
use anyhow::{ensure, Context, Result};
use okilum_write_authority::{Authority, DurableBinding, Phase, WriterLease};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    io::Read,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Weak},
    time::{Duration, Instant},
};
#[cfg(unix)]
use std::{
    fs::OpenOptions,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
};
use uuid::Uuid;
use xmltree::{Element, XMLNode};

#[cfg(target_os = "linux")]
use std::{
    collections::BTreeSet,
    fs,
    net::IpAddr,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
};
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{live_generation, NativeLaunch};

#[cfg(target_os = "macos")]
use crate::sidecar::macos::bootstrap::managed_launch_reference as native_launch_reference;
#[cfg(windows)]
use crate::sidecar::windows::bootstrap::managed_launch_reference as native_launch_reference;

mod quiescence;
pub use quiescence::{OrdinaryHold, ScopedHold};

pub(crate) type Identity = okilum_write_authority::filesystem_identity::Identity;
#[cfg(unix)]
pub(crate) fn identity(path: &Path) -> Result<Identity> {
    let handle = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    // Persist the same filesystem/subvolume/inode/birth witness as Reader.
    // Boot-local btrfs device numbers must not invalidate an existing pairing.
    okilum_write_authority::filesystem_identity::from_file(&handle)
}
#[cfg(unix)]
pub(crate) fn digest(path: &Path) -> Result<String> {
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let m = f.metadata()?;
    ensure!(
        m.is_file() && m.mode() & 0o022 == 0 && m.len() <= 100_000_000,
        "unsafe managed executable"
    );
    let mut h = Sha256::new();
    let mut block = [0; 65536];
    loop {
        let n = f.read(&mut block)?;
        if n == 0 {
            break;
        }
        h.update(&block[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{live_generation, NativeLaunch};
#[cfg(windows)]
pub(crate) fn identity(path: &Path) -> Result<Identity> {
    if path.is_dir() {
        okilum_core::windows_files::Directory::open(path)?.filesystem_identity()
    } else {
        let directory = okilum_core::windows_files::Directory::open(
            path.parent().context("file parent missing")?,
        )?;
        directory.child(path.file_name().context("file name missing")?)?;
        okilum_write_authority::filesystem_identity::read(path)
    }
}
#[cfg(windows)]
pub(crate) fn digest(path: &Path) -> Result<String> {
    let directory = okilum_core::windows_files::Directory::open(
        path.parent().context("executable parent missing")?,
    )?;
    let mut file = directory.open_file(path.file_name().context("executable name missing")?)?;
    ensure!(
        file.metadata()?.len() <= 100_000_000,
        "unsafe managed executable"
    );
    let mut hash = Sha256::new();
    let mut block = [0; 65536];
    loop {
        let n = file.read(&mut block)?;
        if n == 0 {
            break;
        }
        hash.update(&block[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
type LockFile = File;

fn read_configuration(path: &Path) -> Result<Vec<u8>> {
    #[cfg(unix)]
    {
        private::read(path)
    }
    #[cfg(windows)]
    {
        private::public_file(path)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Folder {
    root: PathBuf,
    identity: Identity,
    authority: DurableBinding,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    id: Uuid,
    home: PathBuf,
    home_identity: Identity,
    executable: PathBuf,
    executable_hash: String,
    certificate_hash: String,
    gate: Identity,
    process: Identity,
    folders: BTreeMap<String, Folder>,
}
/// Journal pins this reference; deleting the launch directory never creates a
/// new empty registry on an existing service's restart/reconcile.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    directory: PathBuf,
    identity: Identity,
    id: Uuid,
}
/// Linux only (#2582): whether a configured folder root is absent (moved,
/// deleted, unmounted). Launch admits such a folder only in the provably safe
/// state checked by `fence_folders`. Other platforms keep failing closed.
fn missing_root(root: &Path) -> Result<bool> {
    Ok(cfg!(target_os = "linux") && !root.try_exists()?)
}
/// An absent root cannot be canonicalized, so the configured path must name
/// the saved (canonical) root exactly. Any drift keeps the folder refused.
fn same_missing_root(configured: &Path, saved: &Path) -> bool {
    configured.components().eq(saved.components())
}
/// Folder IDs whose local index sequence was non-zero in the last live,
/// authenticated generation. Pinned v2.1.6 recreates the root of a folder with
/// sequence zero (`model.newFolder`), so only these may start while missing.
const INDEXED: &str = "indexed-folders.json";
/// Folders this launcher paused at runtime because their root was missing.
/// A runtime pause never recreates a root (`model.restartFolder`), unlike a
/// startup pause. Only a verified unpark (saved root identity, admitted
/// authority) or an explicit user pause decision removes an ID.
const PARKED: &str = "parked-folders.json";
fn read_ids(r: &Reference, name: &str) -> Result<Vec<String>> {
    let path = r.directory.join(name);
    if !path.try_exists()? {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_slice(&private::read(&path)?)?)
}
fn write_ids(r: &Reference, name: &str, ids: &[String]) -> Result<()> {
    let path = r.directory.join(name);
    if ids.is_empty() {
        if path.try_exists()? {
            std::fs::remove_file(path)?;
        }
        return Ok(());
    }
    private::write(&path, &serde_json::to_vec(ids)?)
}
/// The evidence belongs to one folder instance. Creating an instance
/// (`add_paused_folder`) revokes it first, under the configuration admission
/// that already holds the launch gate; the live runtime records it again once
/// that instance has index history. A removed and re-added ID never inherits
/// the old instance's history, while no-op reconciliation (the 2 s Sync
/// refresh) and a failed removal keep it.
pub(crate) fn folder_instance_changing(daemon: &DaemonIdentity, folder: &str) -> Result<()> {
    let home = daemon.config_file.parent().context("daemon home missing")?;
    if !directory(home)?.join(INDEXED).try_exists()? {
        return Ok(());
    }
    revoke_index_evidence(&existing(home)?, folder)
}
/// An explicit user pause decision owns the folder's pause state from now on:
/// the launcher must never unpark (resume) it. Called under the admission that
/// holds the launch gate.
pub(crate) fn folder_pause_decided(daemon: &DaemonIdentity, folder: &str) -> Result<()> {
    let home = daemon.config_file.parent().context("daemon home missing")?;
    if !directory(home)?.join(PARKED).try_exists()? {
        return Ok(());
    }
    let r = existing(home)?;
    let mut parked = read_ids(&r, PARKED)?;
    let before = parked.len();
    parked.retain(|id| id != folder);
    if parked.len() != before {
        write_ids(&r, PARKED, &parked)?;
    }
    Ok(())
}
fn set_unpaused(folder: &mut Element) {
    folder.attributes.remove("paused");
    folder
        .children
        .retain(|node| !matches!(node, XMLNode::Element(e) if e.name == "paused"));
    let mut paused = Element::new("paused");
    paused.children.push(XMLNode::Text("false".into()));
    folder.children.push(XMLNode::Element(paused));
}
fn revoke_index_evidence(r: &Reference, folder: &str) -> Result<()> {
    let path = r.directory.join(INDEXED);
    if !path.try_exists()? {
        return Ok(());
    }
    let mut indexed: Vec<String> = serde_json::from_slice(&private::read(&path)?)?;
    let before = indexed.len();
    indexed.retain(|id| id != folder);
    if indexed.len() != before {
        private::write(&path, &serde_json::to_vec(&indexed)?)?;
    }
    Ok(())
}
fn directory(home: &Path) -> Result<PathBuf> {
    Ok(home
        .parent()
        .context("managed home has no parent")?
        .join("managed-launch"))
}
#[cfg(unix)]
fn lock_file(path: &Path, create: bool, wait: bool) -> Result<LockFile> {
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let m = f.metadata()?;
    ensure!(
        m.is_file() && m.uid() == rustix::process::geteuid().as_raw() && m.mode() & 0o077 == 0,
        "managed lock ownership changed"
    );
    if wait {
        f.lock()?;
    } else {
        f.try_lock()
            .context("managed runtime is already running or busy")?;
    }
    Ok(f)
}
#[cfg(windows)]
fn lock_file(path: &Path, create: bool, wait: bool) -> Result<LockFile> {
    let file = private::managed_lock(path, create)?;
    if wait {
        file.lock()?;
    } else {
        file.try_lock()
            .context("managed runtime is already running or busy")?;
    }
    Ok(file)
}

thread_local! { static GATES: RefCell<BTreeMap<PathBuf, Weak<LockFile>>> = const { RefCell::new(BTreeMap::new()) }; }
impl Reference {
    fn load(&self) -> Result<Binding> {
        ensure!(
            identity(&self.directory)? == self.identity,
            "managed launch directory replaced"
        );
        let reference: Reference =
            serde_json::from_slice(&private::read(&self.directory.join("reference.json"))?)?;
        ensure!(&reference == self, "managed launch reference replaced");
        let b: Binding =
            serde_json::from_slice(&private::read(&self.directory.join("binding.json"))?)?;
        ensure!(
            b.id == self.id && !b.id.is_nil(),
            "managed launch binding replaced"
        );
        ensure!(
            b.home.canonicalize()? == b.home
                && identity(&b.home)? == b.home_identity
                && directory(&b.home)? == self.directory
                && identity(&self.directory.join("gate.lock"))? == b.gate
                && identity(&self.directory.join("process.lock"))? == b.process,
            "managed launch identity changed"
        );
        ensure!(
            format!(
                "{:x}",
                Sha256::digest(private::public_file(&b.home.join("cert.pem"))?)
            ) == b.certificate_hash,
            "managed certificate replaced"
        );
        Ok(b)
    }
    fn gate(&self) -> Result<Arc<LockFile>> {
        let before = self.load()?;
        if let Some(lock) = GATES.with(|g| g.borrow().get(&self.directory).and_then(Weak::upgrade))
        {
            return Ok(lock);
        }
        // A first-merge owner can retain this gate across paired requests.
        // Refuse contention rather than wait while an RPC holds the pairing DB:
        // the next request must remain able to finish or recover that merge.
        let lock = lock_file(&self.directory.join("gate.lock"), false, false)?;
        ensure!(
            identity(&self.directory.join("gate.lock"))? == before.gate,
            "managed gate replaced"
        );
        self.load()?;
        let lock = Arc::new(lock);
        GATES.with(|g| {
            g.borrow_mut()
                .insert(self.directory.clone(), Arc::downgrade(&lock));
        });
        Ok(lock)
    }
    pub(crate) fn validate(&self, instance: &ManagedInstance) -> Result<()> {
        let b = self.load()?;
        ensure!(
            b.home == instance.home
                && b.executable == instance.executable
                && digest(&instance.executable)? == b.executable_hash,
            "managed package identity changed"
        );
        Ok(())
    }
    /// Only the lifecycle's verified, stopped upgrade may repin package bytes.
    /// Retain the registry, folder authorities and reference identity unchanged.
    #[cfg(target_os = "linux")]
    pub(crate) fn upgrade_package(&self, instance: &ManagedInstance) -> Result<()> {
        let _gate = self.gate()?;
        let mut b = self.load()?;
        let _process = lock_file(&self.directory.join("process.lock"), false, false)?;
        ensure!(
            b.home == instance.home && b.executable == instance.executable,
            "managed package location changed"
        );
        b.executable_hash = digest(&instance.executable)?;
        self.save(&b)
    }
    fn save(&self, b: &Binding) -> Result<()> {
        private::write(
            &self.directory.join("binding.json"),
            &serde_json::to_vec(b)?,
        )
    }
}
/// Explicit fresh preparation or exact legacy-owned migration only. Never used
/// by the launch command or by configuration admission to repair missing state.
pub(crate) fn initialize(instance: &ManagedInstance) -> Result<Reference> {
    let dir = directory(&instance.home)?;
    private::directory(&dir)?;
    let _initialization = private::lock(&dir)?;
    let path = dir.join("reference.json");
    if path.try_exists()? {
        let r: Reference = serde_json::from_slice(&private::read(&path)?)?;
        r.validate(instance)?;
        return Ok(r);
    }
    ensure!(
        !dir.join("binding.json").try_exists()?,
        "partial managed launch registration requires recovery"
    );
    let _gate = lock_file(&dir.join("gate.lock"), true, true)?;
    let _process = lock_file(&dir.join("process.lock"), true, false)?;
    let r = Reference {
        directory: dir.clone(),
        identity: identity(&dir)?,
        id: Uuid::new_v4(),
    };
    let mut b = Binding {
        id: r.id,
        home: instance.home.clone(),
        home_identity: identity(&instance.home)?,
        executable: instance.executable.clone(),
        executable_hash: digest(&instance.executable)?,
        certificate_hash: format!(
            "{:x}",
            Sha256::digest(private::public_file(&instance.home.join("cert.pem"))?)
        ),
        gate: identity(&dir.join("gate.lock"))?,
        process: identity(&dir.join("process.lock"))?,
        folders: BTreeMap::new(),
    };
    // Existing folders enter only during explicit migration. Bind their actual
    // roots, including a closed authority; launch will fence those offline.
    let xml = Element::parse(read_configuration(&instance.home.join("config.xml"))?.as_slice())?;
    for e in xml
        .children
        .iter()
        .filter_map(XMLNode::as_element)
        .filter(|e| e.name == "folder")
    {
        let id = e.attributes.get("id").context("folder ID missing")?;
        let configured = PathBuf::from(e.attributes.get("path").context("folder root missing")?);
        let root = configured.canonicalize().with_context(|| {
            format!(
                "Sync folder {id} at {} is unavailable; reconnect its disk or remove this folder from Sync before retrying the managed runtime upgrade",
                configured.display()
            )
        })?;
        ensure!(
            !dir.starts_with(&root) && !instance.home.starts_with(&root),
            "managed launch state must be outside the vault"
        );
        let authority = Authority::for_root(&root)?.durable_binding()?;
        ensure!(
            b.folders
                .insert(
                    id.clone(),
                    Folder {
                        identity: identity(&root)?,
                        root,
                        authority
                    }
                )
                .is_none(),
            "duplicate folder ID"
        );
    }
    r.save(&b)?;
    private::write(&path, &serde_json::to_vec(&r)?)?;
    Ok(r)
}
pub(crate) fn existing(home: &Path) -> Result<Reference> {
    let r: Reference =
        serde_json::from_slice(&private::read(&directory(home)?.join("reference.json"))?)?;
    r.load()?;
    Ok(r)
}
/// Lifetime includes authority admission, all REST mutations and readback. The
/// lease is acquired by admission.rs AFTER this gate and before registration.
pub(crate) struct Configuration {
    _operation: okilum_write_authority::OperationGuard,
    _gate: Arc<LockFile>,
}
pub(crate) fn configuration(
    daemon: &DaemonIdentity,
    folder: &str,
    root: &Path,
) -> Result<Option<Configuration>> {
    let home = daemon.config_file.parent().context("daemon home missing")?;
    let dir = directory(home)?;
    if !dir.try_exists()? {
        // External instances have no managed launch/hold ownership. Still use
        // local admission; an unfinished hold refuses any configuration effect.
        #[cfg(target_os = "linux")]
        ensure!(
            !home
                .parent()
                .context("preparation missing")?
                .join("prepared.json")
                .try_exists()?,
            "managed launch binding missing; recovery required"
        );
        #[cfg(any(target_os = "macos", windows))]
        ensure!(
            native_launch_reference(home.parent().context("preparation missing")?,)?.is_none(),
            "managed launch binding missing; recovery required"
        );
        return Ok(None);
    }
    let r = existing(home)?;
    let gate = r.gate()?;
    ensure!(
        crate::daemon::managed_transport_identity(home.parent().context("preparation missing")?)?
            == *daemon,
        "managed preparation binding changed"
    );
    let mut b = r.load()?;
    ensure!(
        b.home == home && b.certificate_hash == daemon.certificate_hash,
        "managed daemon binding differs"
    );
    let root = root.canonicalize()?;
    ensure!(
        !r.directory.starts_with(&root) && !b.home.starts_with(&root),
        "managed launch state must be outside the vault"
    );
    let operation = if let Some(saved) = b.folders.get(folder) {
        ensure!(
            saved.root == root && identity(&root)? == saved.identity,
            "managed folder root replaced"
        );
        saved.authority.open()?.operation()?
    } else {
        // Register under a writer lease BEFORE publication. If drain wins this
        // race, nothing (including a new registry entry) is published.
        let authority = Authority::for_root(&root)?;
        let operation = authority.operation()?;
        b.folders.insert(
            folder.to_owned(),
            Folder {
                identity: identity(&root)?,
                root,
                authority: authority.durable_binding()?,
            },
        );
        r.save(&b)?;
        operation
    };
    Ok(Some(Configuration {
        _operation: operation,
        _gate: gate,
    }))
}

/// Device addresses and runtime-wide network policy can restart connections to
/// any configured folder. Retain admission for every affected vault; a closed
/// vault refuses the shared effect without stopping the running daemon.
pub(crate) struct RuntimeConfiguration {
    _operations: Vec<okilum_write_authority::OperationGuard>,
    _missing: Vec<WriterLease>,
    _gate: Option<Arc<LockFile>>,
}
pub(crate) fn runtime_configuration(daemon: &DaemonIdentity) -> Result<RuntimeConfiguration> {
    let home = daemon.config_file.parent().context("daemon home missing")?;
    #[cfg(any(target_os = "macos", windows))]
    if !directory(home)?.try_exists()? {
        // Legacy supervisors keep their existing pairing capabilities. They
        // have no first-merge fence and cannot grant publication ownership.
        ensure!(
            native_launch_reference(home.parent().context("preparation missing")?,)?.is_none(),
            "managed launch binding missing; recovery required"
        );
        let config = daemon.connect()?.config()?;
        let mut operations = Vec::new();
        for folder in config["folders"]
            .as_array()
            .context("folder inventory missing")?
        {
            let root = Path::new(folder["path"].as_str().context("folder root missing")?);
            // A missing root has no canonical publisher to admit (#2582).
            if !missing_root(root)? {
                operations.push(okilum_write_authority::operation(root)?);
            }
        }
        return Ok(RuntimeConfiguration {
            _operations: operations,
            _missing: Vec::new(),
            _gate: None,
        });
    }
    let reference = existing(home).context("external runtime has no managed hold ownership")?;
    let gate = reference.gate()?;
    ensure!(
        crate::daemon::managed_transport_identity(home.parent().context("preparation missing")?)?
            == *daemon,
        "managed preparation binding changed"
    );
    let b = reference.load()?;
    ensure!(
        b.home == home && b.certificate_hash == daemon.certificate_hash,
        "managed runtime identity changed"
    );
    let api = daemon.connect()?;
    let config = api.config()?;
    let mut operations = Vec::new();
    let mut missing = Vec::new();
    for folder in config["folders"]
        .as_array()
        .context("folder inventory missing")?
    {
        let id = folder["id"].as_str().context("folder ID missing")?;
        let root = Path::new(folder["path"].as_str().context("folder path missing")?);
        let saved = b
            .folders
            .get(id)
            .context("managed folder binding missing")?;
        if missing_root(root)? {
            // Runtime-wide effects restart connections; the folder's own
            // admission still holds the bound authority without its root.
            ensure!(
                same_missing_root(root, &saved.root),
                "managed folder root changed"
            );
            missing.push(saved.authority.admit_without_root()?);
            continue;
        }
        ensure!(
            root.canonicalize()? == saved.root && identity(root)? == saved.identity,
            "managed folder root changed"
        );
        operations.push(saved.authority.open()?.operation()?);
    }
    Ok(RuntimeConfiguration {
        _operations: operations,
        _missing: missing,
        _gate: Some(gate),
    })
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Generation {
    pub id: Uuid,
    pub pid: u32,
    pub start_time: String,
    pub owner_pid: u32,
    pub owner_start_time: String,
    pub binding: Uuid,
    pub executable_hash: String,
    pub rest_address: SocketAddr,
    pub rest_start_time: Option<String>,
    /// These folders had no runner at all in this process generation. This is
    /// recorded from the exact offline configuration before the child starts.
    #[serde(default)]
    pub paused_at_start: Vec<String>,
}
/// Bind authenticated REST to the exact kernel process, not merely to another
/// process with the same certificate/home. /proc/net alone is insufficient:
/// require that this child actually owns the listening socket inode.
#[cfg(target_os = "linux")]
fn owns_rest(pid: u32, address: SocketAddr) -> Result<()> {
    let sockets = fs::read_dir(format!("/proc/{pid}/fd"))?
        .map(|entry| -> Result<Option<String>> {
            let path = match fs::read_link(entry?.path()) {
                Ok(path) => path,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e.into()),
            };
            Ok(path
                .to_str()
                .and_then(|s| s.strip_prefix("socket:["))
                .and_then(|s| s.strip_suffix(']'))
                .map(str::to_owned))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<BTreeSet<_>>();
    let listener = rest_listener(pid, address)?;
    ensure!(
        listener.is_some_and(|inode| sockets.contains(&inode)),
        "REST listener is not owned by the admitted runtime generation"
    );
    Ok(())
}
#[cfg(target_os = "linux")]
fn rest_listener(pid: u32, address: SocketAddr) -> Result<Option<String>> {
    let (table, ip) = match address.ip() {
        IpAddr::V4(ip) => ("tcp", format!("{:08X}", u32::from_le_bytes(ip.octets()))),
        IpAddr::V6(ip) => (
            "tcp6",
            ip.octets()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| format!("{:08X}", u32::from_le_bytes(*b)))
                .collect::<String>(),
        ),
    };
    let local = format!("{ip}:{:04X}", address.port());
    let table = fs::read_to_string(format!("/proc/{pid}/net/{table}"))?;
    Ok(table.lines().skip(1).find_map(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        (fields.len() > 9 && fields[1] == local && fields[3] == "0A").then(|| fields[9].to_owned())
    }))
}

#[cfg(target_os = "linux")]
fn process_start(pid: u32) -> Result<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    Ok(stat
        .rsplit_once(')')
        .context("invalid process identity")?
        .1
        .split_whitespace()
        .nth(19)
        .context("process start time missing")?
        .into())
}
#[cfg(target_os = "linux")]
fn process_parent(pid: u32) -> Result<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    Ok(stat
        .rsplit_once(')')
        .context("invalid process identity")?
        .1
        .split_whitespace()
        .nth(1)
        .context("process parent missing")?
        .parse()?)
}
/// Invoked only by the dedicated autonomous command after lifecycle verification.
/// Returns the child's exit status; the service manager owns retry policy.
struct FencedFolders {
    leases: Vec<WriterLease>,
    closed: Vec<String>,
    paused_at_start: Vec<String>,
    /// Admitted while their root was absent; checked again after readiness.
    returning: Vec<(String, Folder)>,
}
fn fence_folders(r: &Reference, b: &Binding) -> Result<FencedFolders> {
    let mut xml = Element::parse(read_configuration(&b.home.join("config.xml"))?.as_slice())?;
    let indexed: Vec<String> =
        if cfg!(target_os = "linux") && r.directory.join(INDEXED).try_exists()? {
            serde_json::from_slice(&private::read(&r.directory.join(INDEXED))?)?
        } else {
            Vec::new()
        };
    let parked: Vec<String> = if cfg!(target_os = "linux") {
        read_ids(r, PARKED)?
    } else {
        Vec::new()
    };
    let mut leases: Vec<WriterLease> = Vec::new();
    let mut closed = Vec::new();
    let mut paused_at_start = Vec::new();
    let mut returning = Vec::new();
    for e in xml
        .children
        .iter_mut()
        .filter_map(XMLNode::as_mut_element)
        .filter(|e| e.name == "folder")
    {
        let id = e.attributes.get("id").context("folder ID missing")?.clone();
        let root = Path::new(e.attributes.get("path").context("folder root missing")?);
        if missing_root(root)? {
            // Moved, deleted or unmounted (#2582). Pausing cannot fence it:
            // pinned v2.1.6 recreates the root of every PAUSED folder at
            // startup (`model.initFolders`) and of an unpaused folder whose
            // local sequence is zero (`model.newFolder`). An unpaused, indexed
            // folder only reports `ErrPathMissing` and publishes nothing, so it
            // may start untouched, but only for exactly its saved root and
            // under a writer lease admitted without the root and retained
            // through startup, like a present root's lease. Every other
            // missing root keeps the whole launch fail-closed.
            let parked_here = parked.iter().any(|known| known == &id);
            // A folder this launcher parked is paused only at runtime; it starts
            // unpaused again so Syncthing does not recreate its root.
            let paused = !parked_here
                && e.get_child("paused").and_then(Element::get_text).as_deref() == Some("true");
            let saved = b
                .folders
                .get(&id)
                .filter(|saved| same_missing_root(root, &saved.root));
            let lease = match saved {
                Some(saved) if !paused && indexed.iter().any(|known| known == &id) => {
                    saved.authority.admit_without_root().ok()
                }
                _ => None,
            };
            let lease = lease.with_context(|| {
                format!(
                    "Sync folder {id} at {} is unavailable; reconnect its disk or remove this folder from Sync",
                    root.display()
                )
            })?;
            leases.push(lease);
            if parked_here {
                set_unpaused(e);
            }
            returning.push((id, saved.unwrap().clone()));
            continue;
        }
        let saved = b
            .folders
            .get(&id)
            .context("unbound managed folder; recovery required")?;
        ensure!(
            !r.directory.starts_with(&saved.root) && !b.home.starts_with(&saved.root),
            "managed launch state must be outside the vault"
        );
        ensure!(
            root.canonicalize()? == saved.root && identity(root)? == saved.identity,
            "managed folder replaced"
        );
        let a = saved.authority.open()?;
        // An admission refusal can be a hold OR uncertainty. Never infer Open
        // from a failed read. The actual admission closes check/effect races.
        match a.admit() {
            Ok(lease) => {
                // Its root is back with the saved identity and the authority
                // admits a writer: lift this launcher's own runtime park.
                if parked.iter().any(|known| known == &id) {
                    set_unpaused(e);
                }
                leases.push(lease);
            }
            Err(_) => {
                // Validate durable state; corruption fails the entire launch.
                ensure!(
                    a.snapshot()?.phase != Phase::Open,
                    "uncertain launch admission"
                );
                set_paused(e);
                closed.push(id.clone());
            }
        }
        if e.get_child("paused").and_then(Element::get_text).as_deref() == Some("true") {
            paused_at_start.push(id.clone());
        }
    }
    let mut bytes = Vec::new();
    xml.write(&mut bytes)?;
    #[cfg(unix)]
    private::write(&b.home.join("config.xml"), &bytes)?;
    #[cfg(windows)]
    private::write_generated(&b.home.join("config.xml"), &bytes)?;
    Ok(FencedFolders {
        leases,
        closed,
        paused_at_start,
        returning,
    })
}
#[cfg(target_os = "linux")]
pub(crate) fn run(r: &Reference, instance: &ManagedInstance) -> Result<()> {
    let _process = lock_file(&r.directory.join("process.lock"), false, false)?;
    let gate = r.gate()?;
    r.validate(instance)?;
    let b = r.load()?;
    ensure!(
        b.executable_hash == crate::linux_bootstrap::BINARY_SHA,
        "managed launch requires the checksum-pinned Linux package"
    );
    let daemon =
        crate::daemon::managed_transport_identity(b.home.parent().context("preparation missing")?)?;
    ensure!(
        daemon.config_file == b.home.join("config.xml")
            && daemon.certificate_hash == b.certificate_hash,
        "managed preparation binding changed"
    );
    let rest_address = daemon.rest_address()?;
    ensure!(
        rest_listener(std::process::id(), rest_address)?.is_none(),
        "an external or previous runtime still owns the managed REST endpoint"
    );
    let FencedFolders {
        leases,
        mut closed,
        paused_at_start,
        returning,
    } = fence_folders(r, &b)?;
    // Fixture-only interleaving point (#2582 review): the decision is taken and
    // its leases are held, the child is not started yet.
    #[cfg(feature = "first-sync-fixture")]
    if let Some(dir) = std::env::var_os("OKILUM_LAUNCH_FIXTURE_PAUSE") {
        let dir = PathBuf::from(dir);
        fs::write(
            dir.join("launch-admitted"),
            b"leases held, child not started",
        )?;
        while !dir.join("launch-go").exists() {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    // Never restore an old paused=false value, even after authority release.
    // Explicit admitted resume/reconciliation owns the later configuration.
    let mut command = Command::new(&b.executable);
    command
        .env_clear()
        .env("HOME", &b.home)
        .env("STMONITORED", "1")
        // Version archive stamps are local wall times; Okilum reads them in its
        // own zone, so the daemon must not fall back to another one (#2567).
        .envs(std::env::var_os("TZ").map(|tz| ("TZ", tz)))
        .args([
            "serve",
            "--no-browser",
            "--no-restart",
            "--no-upgrade",
            "--home",
        ])
        .arg(&b.home)
        .stdin(Stdio::null());
    let parent = std::process::id() as libc::pid_t;
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                libc::_exit(1);
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    // Always kill AND reap on a readiness/binding/readback failure.
    let startup = (|| -> Result<()> {
        ensure!(
            fs::metadata(format!("/proc/{}/exe", child.id()))?.ino()
                == fs::metadata(&b.executable)?.ino()
                && fs::metadata(format!("/proc/{}/exe", child.id()))?.dev()
                    == fs::metadata(&b.executable)?.dev(),
            "spawned runtime package changed"
        );
        let mut generation = Generation {
            id: Uuid::new_v4(),
            pid: child.id(),
            start_time: process_start(child.id())?,
            owner_pid: std::process::id(),
            owner_start_time: process_start(std::process::id())?,
            binding: b.id,
            executable_hash: b.executable_hash.clone(),
            rest_address,
            rest_start_time: None,
            paused_at_start,
        };
        private::write(
            &r.directory.join("generation.json"),
            &serde_json::to_vec(&generation)?,
        )?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            ensure!(
                child.try_wait()?.is_none(),
                "managed runtime exited during startup"
            );
            match daemon.connect() {
                Ok(api) => {
                    owns_rest(child.id(), rest_address)?;
                    generation.rest_start_time = Some(
                        api.identity()?["startTime"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .context("runtime REST generation missing")?
                            .to_owned(),
                    );
                    // A root admitted while absent may have returned during
                    // startup. Only exactly the saved root may run; anything
                    // else is paused now, before the leases are released.
                    // Still missing: park it with a runtime pause, which never
                    // recreates a root, so only a verified unpark lets it
                    // publish again.
                    let mut parked = Vec::new();
                    for (id, saved) in &returning {
                        let root = Path::new(saved.root.as_path());
                        if !root.try_exists()? {
                            api.patch_folder(id, &serde_json::json!({"paused":true}))?;
                            parked.push(id.clone());
                            closed.push(id.clone());
                        } else if root.canonicalize()? != saved.root
                            || identity(root)? != saved.identity
                        {
                            api.patch_folder(id, &serde_json::json!({"paused":true}))?;
                            closed.push(id.clone());
                        }
                    }
                    write_ids(r, PARKED, &parked)?;
                    for id in &closed {
                        ensure!(
                            api.folder(id)?["paused"] == true,
                            "startup fence readback failed"
                        );
                    }
                    // Evidence for a later launch whose root has gone missing. A
                    // folder whose status cannot be read keeps its last evidence.
                    let previous: Vec<String> = if r.directory.join(INDEXED).try_exists()? {
                        serde_json::from_slice(&private::read(&r.directory.join(INDEXED))?)?
                    } else {
                        Vec::new()
                    };
                    let mut indexed = Vec::new();
                    for folder in api.config()?["folders"]
                        .as_array()
                        .context("folder inventory missing")?
                    {
                        let id = folder["id"].as_str().context("folder ID missing")?;
                        let known = match api.status(id) {
                            Ok(status) => status["sequence"].as_i64().unwrap_or(0) > 0,
                            Err(_) => previous.iter().any(|p| p == id),
                        };
                        if known {
                            indexed.push(id.to_owned());
                        }
                    }
                    private::write(&r.directory.join(INDEXED), &serde_json::to_vec(&indexed)?)?;
                    break;
                }
                Err(e) if Instant::now() < deadline => {
                    let _ = e;
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(e.context("managed launch readiness failed")),
            }
        }
        r.validate(instance)?;
        private::write(
            &r.directory.join("generation.json"),
            &serde_json::to_vec(&generation)?,
        )?;
        Ok(())
    })();
    if let Err(e) = startup {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    drop(leases);
    drop(gate);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        // Best effort: a failed round never stops the runtime; the next round
        // retries. Runs only under the gate, so it never races configuration.
        let _ = supervise(r, &daemon);
        std::thread::sleep(Duration::from_secs(2));
    };
    ensure!(status.success(), "managed runtime crashed: {status}");
    Ok(())
}

/// One supervision round of the live, owned runtime (#2582 review):
/// - records index evidence as soon as a folder instance has index history,
///   so a folder added or first filled after launch needs no restart;
/// - unparks a parked folder only when its root is back with the saved
///   identity and its authority admits a writer (held for the PATCH).
/// Configuration admission refuses a busy gate instead of waiting, so a round
/// takes the gate only when it found a transition, and re-reads under it.
#[cfg(target_os = "linux")]
fn supervise(r: &Reference, daemon: &DaemonIdentity) -> Result<()> {
    let api = daemon.connect()?;
    let configured = |api: &okilum_sync::Syncthing| -> Result<Vec<String>> {
        Ok(api.config()?["folders"]
            .as_array()
            .context("folder inventory missing")?
            .iter()
            .filter_map(|folder| folder["id"].as_str().map(str::to_owned))
            .collect())
    };
    let history = |api: &okilum_sync::Syncthing, id: &str| {
        api.status(id)
            .is_ok_and(|status| status["sequence"].as_i64().unwrap_or(0) > 0)
    };
    // Unlocked pre-check: most rounds find nothing and never touch the gate.
    let indexed = read_ids(r, INDEXED)?;
    let parked = read_ids(r, PARKED)?;
    let fresh: Vec<String> = configured(&api)?
        .into_iter()
        .filter(|id| !indexed.contains(id) && history(&api, id))
        .collect();
    let returned = parked.iter().any(|id| {
        r.load()
            .ok()
            .and_then(|b| b.folders.get(id).map(|f| f.root.exists()))
            == Some(true)
    });
    if fresh.is_empty() && !returned {
        return Ok(());
    }
    let Ok(_gate) = r.gate() else {
        return Ok(()); // a configuration operation owns the gate; retry later
    };
    let b = r.load()?;
    let configured = configured(&api)?;
    let mut indexed = read_ids(r, INDEXED)?;
    let before = indexed.len();
    for id in &fresh {
        // Re-read under the gate: the instance may have been replaced meanwhile.
        if configured.contains(id) && !indexed.contains(id) && history(&api, id) {
            indexed.push(id.clone());
        }
    }
    if indexed.len() != before {
        private::write(&r.directory.join(INDEXED), &serde_json::to_vec(&indexed)?)?;
    }
    let parked = read_ids(r, PARKED)?;
    let mut remaining = Vec::new();
    for id in &parked {
        let Some(saved) = b.folders.get(id).filter(|_| configured.contains(id)) else {
            continue; // removed meanwhile: nothing to unpark
        };
        let root = saved.root.as_path();
        if !root.try_exists()? {
            remaining.push(id.clone());
            continue;
        }
        let same = root.canonicalize()? == saved.root && identity(root)? == saved.identity;
        let lease = if same {
            saved.authority.open().and_then(|a| a.admit()).ok()
        } else {
            None
        };
        match lease {
            Some(_lease) => {
                api.patch_folder(id, &serde_json::json!({"paused":false}))?;
            }
            None => remaining.push(id.clone()),
        }
    }
    if remaining != parked {
        write_ids(r, PARKED, &remaining)?;
    }
    Ok(())
}
fn set_paused(folder: &mut Element) {
    // Syncthing v2.1.6 serializes paused as a child element (not an attribute).
    folder.attributes.remove("paused");
    // Never leave a second paused=false for an XML decoder to select instead.
    folder
        .children
        .retain(|node| !matches!(node, XMLNode::Element(e) if e.name == "paused"));
    let mut paused = Element::new("paused");
    paused.children.push(XMLNode::Text("true".into()));
    folder.children.push(XMLNode::Element(paused));
}

/// Observe a live authenticated launch generation. A stale file/PID, helper
/// crash, replaced package or reused process identity never yields a generation.
/// This is identity evidence only; it grants no folder-quiescence receipt.
#[cfg(target_os = "linux")]
pub fn live_generation(home: &Path) -> Result<Generation> {
    let r = existing(home)?;
    let b = r.load()?;
    ensure!(
        b.home == home,
        "external home has no managed launch ownership"
    );
    private::read(&r.directory.join("process.lock"))?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(r.directory.join("process.lock"))?;
    ensure!(
        matches!(lock.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        "managed runtime has no verified live launch owner"
    );
    let generation: Generation =
        serde_json::from_slice(&private::read(&r.directory.join("generation.json"))?)?;
    ensure!(
        generation.binding == b.id
            && generation.executable_hash == b.executable_hash
            && process_start(generation.pid)? == generation.start_time
            && process_start(generation.owner_pid)? == generation.owner_start_time
            && process_parent(generation.pid)? == generation.owner_pid,
        "stale managed process generation"
    );
    let running = fs::metadata(format!("/proc/{}/exe", generation.pid))?;
    let expected = fs::metadata(&b.executable)?;
    ensure!(
        running.dev() == expected.dev()
            && running.ino() == expected.ino()
            && digest(&b.executable)? == b.executable_hash,
        "managed process executable replaced"
    );
    owns_rest(generation.pid, generation.rest_address)?;
    let daemon =
        crate::daemon::managed_transport_identity(home.parent().context("preparation missing")?)?;
    let started = generation
        .rest_start_time
        .as_ref()
        .context("managed generation is not ready")?;
    ensure!(
        daemon.rest_address()? == generation.rest_address
            && daemon.connect()?.identity()?["startTime"].as_str() == Some(started.as_str())
            && process_start(generation.pid)? == generation.start_time,
        "managed REST generation changed"
    );
    Ok(generation)
}
