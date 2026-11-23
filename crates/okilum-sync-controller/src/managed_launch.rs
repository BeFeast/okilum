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
/// Folders fenced by a unique Syncthing marker name because their saved root
/// was missing, replaced or elsewhere, at launch or while running. `CheckPath`
/// requires `<root>/<markerName>` and a non-default marker is never created,
/// so no directory that appears at the path (a foreign `.stfolder` included)
/// can be scanned or synced. Only the saved root identity with an admitted
/// authority restores the recorded original.
const FENCED: &str = "fenced-folders.json";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Fence {
    /// The folder's own marker, restored at unfence.
    marker: String,
    /// The launcher also paused it (a folder without index history), so the
    /// unfence resumes it; a user's or hold's pause is never recorded here.
    #[serde(default)]
    paused: bool,
}
const DEFAULT_MARKER: &str = ".stfolder";
const FENCE_PREFIX: &str = ".okilum-missing-root-";
/// Every verified managed root carries its own marker inside Syncthing's
/// internal `.stfolder` directory (never scanned or synced, `fs.IsInternal`),
/// and `markerName` points at it while Okilum manages the runtime. A foreign
/// directory that takes the path, even one with its own `.stfolder`, lacks
/// that file, so `CheckPath` fails before any scan, batch commit or pull.
/// Stable per folder: reused across launches, fences and unfences.
#[cfg(target_os = "linux")]
const ROOT_MARKER_PREFIX: &str = ".stfolder/okilum-root-";
/// Reuse `candidate` if it is this root's marker and the file is there;
/// otherwise create a fresh one. The caller holds the root's writer lease.
#[cfg(target_os = "linux")]
fn valid_root_marker(marker: &str) -> bool {
    marker
        .strip_prefix(ROOT_MARKER_PREFIX)
        .is_some_and(|id| id.len() == 36 && Uuid::parse_str(id).is_ok())
}
#[cfg(target_os = "linux")]
fn root_marker_present(root: &Path, marker: &str) -> bool {
    valid_root_marker(marker)
        && std::fs::symlink_metadata(root.join(marker)).is_ok_and(|m| m.is_file())
}
/// Create exactly `marker` (a validated root-marker name) in `root`. The caller
/// holds the root's writer lease. Idempotent.
#[cfg(target_os = "linux")]
fn create_root_marker(root: &Path, marker: &str) -> Result<()> {
    ensure!(valid_root_marker(marker), "invalid managed root marker");
    let directory = root.join(DEFAULT_MARKER);
    match std::fs::symlink_metadata(&directory) {
        Ok(meta) => ensure!(meta.is_dir(), "Syncthing marker directory replaced"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(&directory)?,
        Err(e) => return Err(e.into()),
    }
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join(marker))
    {
        Ok(mut file) => {
            std::io::Write::write_all(&mut file, b"Okilum managed folder root marker\n")?;
            file.sync_all()?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => ensure!(
            root_marker_present(root, marker),
            "managed root marker is not a regular file"
        ),
        Err(e) => return Err(e.into()),
    }
    File::open(&directory)?.sync_all()?;
    Ok(())
}
/// Reuse `candidate` if it is this root's marker and the file is there;
/// otherwise create a fresh one. The caller holds the root's writer lease.
#[cfg(target_os = "linux")]
fn ensure_root_marker(root: &Path, candidate: Option<&str>) -> Result<String> {
    if let Some(marker) = candidate.filter(|m| root_marker_present(root, m)) {
        return Ok(marker.to_owned());
    }
    let marker = new_root_marker();
    create_root_marker(root, &marker)?;
    Ok(marker)
}
/// A fresh root-marker name for a folder Okilum is about to create (paused).
#[cfg(target_os = "linux")]
pub(crate) fn new_root_marker() -> String {
    format!("{ROOT_MARKER_PREFIX}{}", Uuid::new_v4())
}
/// After a paused `add_paused_folder` that named a root marker (and on every
/// retry of that step): make sure the named file exists before the folder can
/// ever be resumed. Folders with any other marker are left alone. The caller
/// holds the root's writer lease.
#[cfg(target_os = "linux")]
pub(crate) fn materialize_root_marker(root: &Path, folder: &serde_json::Value) -> Result<()> {
    match folder["markerName"].as_str() {
        Some(marker) if valid_root_marker(marker) && !root_marker_present(root, marker) => {
            create_root_marker(root, marker)
        }
        _ => Ok(()),
    }
}
/// Ownership witnesses compare the whole folder. The launch command may have
/// moved a verified folder onto its root marker (or a missing one onto a
/// fence) since the witness was recorded; forgive exactly those markers.
/// Any other marker drift still fails the caller's comparison.
#[cfg(target_os = "linux")]
pub(crate) fn forgive_managed_marker(
    expected: &mut serde_json::Value,
    actual: &serde_json::Value,
    root: &Path,
) {
    if let Some(marker) = actual["markerName"].as_str() {
        if root_marker_present(root, marker) || marker.starts_with(FENCE_PREFIX) {
            expected["markerName"] = actual["markerName"].clone();
        }
    }
}
fn read_fenced(r: &Reference) -> Result<BTreeMap<String, Fence>> {
    let path = r.directory.join(FENCED);
    if !path.try_exists()? {
        return Ok(BTreeMap::new());
    }
    Ok(serde_json::from_slice(&private::read(&path)?)?)
}
fn write_fenced(r: &Reference, fenced: &BTreeMap<String, Fence>) -> Result<()> {
    let path = r.directory.join(FENCED);
    if fenced.is_empty() {
        if path.try_exists()? {
            std::fs::remove_file(path)?;
        }
        return Ok(());
    }
    private::write(&path, &serde_json::to_vec(fenced)?)
}
#[cfg(target_os = "linux")]
fn set_unpaused(folder: &mut Element) {
    folder.attributes.remove("paused");
    folder
        .children
        .retain(|node| !matches!(node, XMLNode::Element(e) if e.name == "paused"));
    let mut paused = Element::new("paused");
    paused.children.push(XMLNode::Text("false".into()));
    folder.children.push(XMLNode::Element(paused));
}
fn marker_of(folder: &Element) -> Option<String> {
    folder
        .get_child("markerName")
        .and_then(Element::get_text)
        .map(|text| text.into_owned())
}
fn set_marker(folder: &mut Element, marker: &str) {
    folder
        .children
        .retain(|node| !matches!(node, XMLNode::Element(e) if e.name == "markerName"));
    let mut element = Element::new("markerName");
    element.children.push(XMLNode::Text(marker.into()));
    folder.children.push(XMLNode::Element(element));
}
#[cfg(target_os = "linux")]
fn read_ids(r: &Reference, name: &str) -> Result<Vec<String>> {
    let path = r.directory.join(name);
    if !path.try_exists()? {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_slice(&private::read(&path)?)?)
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
/// Index evidence has its own short lock, never the launch gate: supervision
/// must not make a first-Sync or configuration operation, which refuses a busy
/// gate instead of waiting, fail just because evidence was being recorded.
/// Held only for local file work and bounded REST reads; never while waiting
/// for the gate (the gate holder may wait for it, never the reverse).
fn evidence_lock(r: &Reference) -> Result<LockFile> {
    lock_file(&r.directory.join("indexed.lock"), true, true)
}
fn revoke_index_evidence(r: &Reference, folder: &str) -> Result<()> {
    let path = r.directory.join(INDEXED);
    if !path.try_exists()? {
        return Ok(());
    }
    let _evidence = evidence_lock(r)?;
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
    /// Admitted while their root was absent (with their original marker);
    /// checked again after readiness. Linux only: other platforms fail a
    /// missing root closed, so nothing returns during their startup.
    #[cfg(target_os = "linux")]
    returning: Vec<(String, Folder, Fence)>,
    /// Present but closed (held) folders that keep their fence marker: their
    /// records must survive readiness so supervision can unfence them later.
    /// Linux only: fence records are read only there.
    #[cfg(target_os = "linux")]
    held_fenced: BTreeMap<String, Fence>,
}
fn fence_folders(r: &Reference, b: &Binding) -> Result<FencedFolders> {
    let mut xml = Element::parse(read_configuration(&b.home.join("config.xml"))?.as_slice())?;
    let indexed: Vec<String> =
        if cfg!(target_os = "linux") && r.directory.join(INDEXED).try_exists()? {
            serde_json::from_slice(&private::read(&r.directory.join(INDEXED))?)?
        } else {
            Vec::new()
        };
    let fenced: BTreeMap<String, Fence> = if cfg!(target_os = "linux") {
        read_fenced(r)?
    } else {
        BTreeMap::new()
    };
    // Every original marker this launch may overwrite or restore stays on disk
    // until the configuration that depends on it is durable (#2582 review): the
    // previous records plus the folders fenced now. Readiness shrinks it.
    let mut persist = fenced.clone();
    let mut leases: Vec<WriterLease> = Vec::new();
    let mut closed = Vec::new();
    let mut paused_at_start = Vec::new();
    #[cfg(target_os = "linux")]
    let mut returning = Vec::new();
    #[cfg(target_os = "linux")]
    let mut held_fenced = BTreeMap::new();
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
            let paused =
                e.get_child("paused").and_then(Element::get_text).as_deref() == Some("true");
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
            // Fence before the child starts: no directory that appears at this
            // path can be scanned or synced until the saved root is verified.
            // The recorded original wins; a leftover fence marker with no
            // record (never written by this version) falls back to Syncthing's
            // default rather than being mistaken for the original.
            let original = fenced.get(&id).cloned().unwrap_or_else(|| Fence {
                marker: marker_of(e)
                    .filter(|m| !m.starts_with(FENCE_PREFIX))
                    .unwrap_or_else(|| DEFAULT_MARKER.into()),
                paused: false,
            });
            set_marker(e, &format!("{FENCE_PREFIX}{}", Uuid::new_v4()));
            persist.insert(id.clone(), original.clone());
            #[cfg(target_os = "linux")]
            returning.push((id, saved.unwrap().clone(), original));
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
        // The saved root is back (identity verified above): restore a fence
        // marker left by an earlier missing-root launch. A hold still closes
        // the folder below; its later explicit resume needs the real marker.
        // Only a folder that still carries a fence marker may have its fence
        // pause undone; a stale record (its unfence already restored the marker)
        // must never resume a folder the user paused since.
        let current = marker_of(e);
        let still_fenced = current
            .as_deref()
            .is_some_and(|m| m.starts_with(FENCE_PREFIX));
        let fence = fenced.get(&id).cloned();
        // A scoped transport hold compares this folder's whole configuration on
        // resume: give it back exactly the marker it had, never a new one.
        let scoped = cfg!(target_os = "linux") && quiescence::unfinished(&r.directory, &id)?;
        if scoped && still_fenced {
            if let Some(fence) = &fence {
                set_marker(e, &fence.marker);
            }
        }
        let a = saved.authority.open()?;
        // An admission refusal can be a hold OR uncertainty. Never infer Open
        // from a failed read. The actual admission closes check/effect races.
        match a.admit() {
            Ok(lease) => {
                #[cfg(target_os = "linux")]
                if !scoped {
                    // The verified root runs behind its own marker, never `.stfolder`.
                    let candidate = if still_fenced {
                        fence.as_ref().map(|f| f.marker.clone())
                    } else {
                        current.clone()
                    };
                    set_marker(e, &ensure_root_marker(&saved.root, candidate.as_deref())?);
                    // Undo only a pause the fence itself added (never a user's).
                    if still_fenced && fence.as_ref().is_some_and(|f| f.paused) {
                        set_unpaused(e);
                    }
                }
                leases.push(lease);
            }
            Err(_) => {
                #[cfg(target_os = "linux")]
                if still_fenced && !scoped {
                    if let Some(fence) = &fence {
                        held_fenced.insert(id.clone(), fence.clone());
                    }
                }
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
    // Record the originals before config.xml carries the unique markers: a
    // launcher killed in between must not lose the marker it replaced.
    write_fenced(r, &persist)?;
    #[cfg(unix)]
    private::write(&b.home.join("config.xml"), &bytes)?;
    #[cfg(windows)]
    private::write_generated(&b.home.join("config.xml"), &bytes)?;
    Ok(FencedFolders {
        leases,
        closed,
        paused_at_start,
        #[cfg(target_os = "linux")]
        returning,
        #[cfg(target_os = "linux")]
        held_fenced,
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
        closed,
        paused_at_start,
        returning,
        held_fenced,
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
                    // A root that returned during startup with the saved
                    // identity is unfenced now, under the startup leases;
                    // anything else stays fenced by its unique marker.
                    let mut still_fenced = held_fenced.clone();
                    for (id, saved, original) in &returning {
                        let root = Path::new(saved.root.as_path());
                        let admitted = root.try_exists()?
                            && root.canonicalize()? == saved.root
                            && identity(root)? == saved.identity;
                        // Same Open admission as supervision: a drain that began
                        // during startup (pinned by the startup lease) keeps it
                        // fenced.
                        let lease = if admitted {
                            saved.authority.open().and_then(|a| a.admit()).ok()
                        } else {
                            None
                        };
                        if let Some(_lease) = lease {
                            let marker = ensure_root_marker(&saved.root, Some(&original.marker))?;
                            api.patch_folder(id, &serde_json::json!({"markerName": marker}))?;
                        } else {
                            ensure!(
                                api.folder(id)?["markerName"]
                                    .as_str()
                                    .is_some_and(|m| m.starts_with(FENCE_PREFIX)),
                                "missing-root fence readback failed"
                            );
                            still_fenced.insert(id.clone(), original.clone());
                        }
                    }
                    write_fenced(r, &still_fenced)?;
                    for id in &closed {
                        ensure!(
                            api.folder(id)?["paused"] == true,
                            "startup fence readback failed"
                        );
                    }
                    // Evidence for a later launch whose root has gone missing.
                    // Monotonic per instance: a paused or fenced folder may
                    // report no sequence, so readiness only adds IDs;
                    // creating an instance is what revokes them.
                    let configured: Vec<String> = api.config()?["folders"]
                        .as_array()
                        .context("folder inventory missing")?
                        .iter()
                        .filter_map(|folder| folder["id"].as_str().map(str::to_owned))
                        .collect();
                    let _evidence = evidence_lock(r)?;
                    let mut indexed: Vec<String> = read_ids(r, INDEXED)?
                        .into_iter()
                        .filter(|id| configured.contains(id))
                        .collect();
                    for id in &configured {
                        if !indexed.contains(id)
                            && api
                                .status(id)
                                .is_ok_and(|status| status["sequence"].as_i64().unwrap_or(0) > 0)
                        {
                            indexed.push(id.clone());
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

/// One supervision round of the live, owned runtime (#2582 review). It runs
/// every 2 s for the whole life of the launch command and covers every managed
/// folder, not only the ones fenced at launch:
/// - records index evidence as soon as a folder instance has index history,
///   so a folder added or first filled after launch needs no restart;
/// - fences a folder again whenever its saved root is absent, replaced or
///   configured at another path, recording the current marker first. A folder
///   without index history is also paused at runtime (`model.restartFolder`
///   never recreates a root, unlike `newFolder` at sequence zero), and the
///   record notes that pause so only it is undone later;
/// - unfences only when the saved root is back with its identity and its
///   authority admits a writer (held for the PATCH).
///
/// Configuration admission refuses a busy gate instead of waiting, so a round
/// takes the gate only when it found a transition, and re-reads under it.
#[cfg(target_os = "linux")]
fn supervise(r: &Reference, daemon: &DaemonIdentity) -> Result<()> {
    struct Live {
        id: String,
        path: PathBuf,
        marker: String,
        paused: bool,
    }
    let api = daemon.connect()?;
    let live = |api: &okilum_sync::Syncthing| -> Result<Vec<Live>> {
        Ok(api.config()?["folders"]
            .as_array()
            .context("folder inventory missing")?
            .iter()
            .filter_map(|folder| {
                Some(Live {
                    id: folder["id"].as_str()?.to_owned(),
                    path: PathBuf::from(folder["path"].as_str()?),
                    marker: folder["markerName"]
                        .as_str()
                        .unwrap_or(DEFAULT_MARKER)
                        .to_owned(),
                    paused: folder["paused"] == true,
                })
            })
            .collect())
    };
    let history = |api: &okilum_sync::Syncthing, id: &str| {
        api.status(id)
            .is_ok_and(|status| status["sequence"].as_i64().unwrap_or(0) > 0)
    };
    // Exactly the saved root, at the saved path, with the saved identity.
    // Any error counts as a mismatch: fencing is the safe answer.
    let intact = |path: &Path, saved: &Folder| -> bool {
        path == saved.root
            && saved.root.try_exists().unwrap_or(false)
            && saved
                .root
                .canonicalize()
                .is_ok_and(|root| root == saved.root)
            && identity(&saved.root).is_ok_and(|found| found == saved.identity)
    };
    // Unlocked pre-check: most rounds find nothing and never touch the gate.
    let b = r.load()?;
    let indexed = read_ids(r, INDEXED)?;
    let fenced = read_fenced(r)?;
    let folders = live(&api)?;
    let fresh = folders
        .iter()
        .any(|f| !indexed.contains(&f.id) && history(&api, &f.id));
    let lost = folders.iter().any(|f| {
        !f.marker.starts_with(FENCE_PREFIX)
            && b.folders
                .get(&f.id)
                .is_some_and(|saved| !intact(&f.path, saved))
    });
    let returned = folders.iter().any(|f| {
        fenced.contains_key(&f.id)
            && b.folders
                .get(&f.id)
                .is_some_and(|saved| intact(&f.path, saved))
    });
    if fresh {
        // Evidence never takes the launch gate (see `evidence_lock`). Re-read
        // under the evidence lock: creating an instance revokes under it too.
        let _evidence = evidence_lock(r)?;
        let mut indexed = read_ids(r, INDEXED)?;
        let before = indexed.len();
        for f in &live(&api)? {
            if !indexed.contains(&f.id) && history(&api, &f.id) {
                indexed.push(f.id.clone());
            }
        }
        if indexed.len() != before {
            private::write(&r.directory.join(INDEXED), &serde_json::to_vec(&indexed)?)?;
        }
    }
    if !lost && !returned {
        return Ok(());
    }
    // Fence transitions alone take the gate: a root went missing or came back.
    let Ok(_gate) = r.gate() else {
        return Ok(()); // a configuration operation owns the gate; retry later
    };
    let b = r.load()?;
    let folders = live(&api)?;
    let mut fenced = read_fenced(r)?;
    for f in &folders {
        let Some(saved) = b.folders.get(&f.id) else {
            continue; // not a managed folder of this runtime
        };
        if f.marker.starts_with(FENCE_PREFIX) {
            if !fenced.contains_key(&f.id) || !intact(&f.path, saved) {
                continue; // still fenced (or fenced without a record: keep it)
            }
            let fence = fenced[&f.id].clone();
            let Ok(_lease) = saved.authority.open().and_then(|a| a.admit()) else {
                continue; // a hold or drain keeps it fenced
            };
            if quiescence::unfinished(&r.directory, &f.id)? {
                continue; // a scoped hold's resume restores its own configuration
            }
            let marker = ensure_root_marker(&saved.root, Some(&fence.marker))?;
            let mut change = serde_json::json!({"markerName": marker});
            if fence.paused {
                change["paused"] = false.into();
            }
            api.patch_folder(&f.id, &change)?;
            fenced.remove(&f.id);
            write_fenced(r, &fenced)?;
        } else if intact(&f.path, saved) {
            // Unfenced earlier, but that round failed to drop the record: drop it
            // now and never touch the pause state (the user may have paused it).
            if fenced.remove(&f.id).is_some() {
                write_fenced(r, &fenced)?;
            }
        } else {
            // Lost while running (unplugged, moved, replaced): fence it again.
            let pause = !f.paused && !history(&api, &f.id);
            fenced.insert(
                f.id.clone(),
                Fence {
                    marker: f.marker.clone(),
                    paused: pause,
                },
            );
            // The record is durable before the runtime carries the fence.
            write_fenced(r, &fenced)?;
            let mut change =
                serde_json::json!({"markerName": format!("{FENCE_PREFIX}{}", Uuid::new_v4())});
            if pause {
                change["paused"] = true.into();
            }
            api.patch_folder(&f.id, &change)?;
        }
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

#[cfg(all(test, target_os = "linux"))]
mod issue2582_root_marker_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn issue2582_witness_forgives_only_this_roots_own_marker() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("vault");
        std::fs::create_dir(&root)?;
        let marker = ensure_root_marker(&root, None)?;
        assert!(root_marker_present(&root, &marker));
        // A witness recorded before root markers existed.
        let witness = json!({"id": "v", "markerName": ".stfolder", "path": root});
        let check = |live: serde_json::Value| {
            let mut expected = witness.clone();
            forgive_managed_marker(&mut expected, &live, &root);
            expected == live
        };
        assert!(check(
            json!({"id": "v", "markerName": marker, "path": root})
        ));
        assert!(check(
            json!({"id": "v", "markerName": ".okilum-missing-root-1", "path": root})
        ));
        // Anything else still fails the witness comparison.
        assert!(!check(
            json!({"id": "v", "markerName": ".other", "path": root})
        ));
        let absent = new_root_marker();
        assert!(!check(
            json!({"id": "v", "markerName": absent, "path": root})
        ));
        assert!(!check(
            json!({"id": "v", "markerName": ".stfolder/okilum-root-../../x", "path": root})
        ));
        std::os::unix::fs::symlink(root.join(&marker), root.join(&absent))?;
        assert!(
            !check(json!({"id": "v", "markerName": absent, "path": root})),
            "a symlinked marker is not this root's marker"
        );
        // Other fields are never forgiven.
        assert!(!check(
            json!({"id": "w", "markerName": marker, "path": root})
        ));
        Ok(())
    }

    #[test]
    fn issue2582_materialize_creates_exactly_the_named_marker_and_is_idempotent() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("vault");
        std::fs::create_dir(&root)?;
        let marker = new_root_marker();
        let folder = json!({"markerName": marker});
        materialize_root_marker(&root, &folder)?;
        assert!(root_marker_present(&root, &marker));
        materialize_root_marker(&root, &folder)?;
        assert_eq!(std::fs::read_dir(root.join(".stfolder"))?.count(), 1);
        // Other markers are left alone; invalid names are never created.
        materialize_root_marker(&root, &json!({"markerName": ".stfolder"}))?;
        materialize_root_marker(&root, &json!({"markerName": ".stfolder/okilum-root-x"}))?;
        assert_eq!(std::fs::read_dir(root.join(".stfolder"))?.count(), 1);
        Ok(())
    }
}
