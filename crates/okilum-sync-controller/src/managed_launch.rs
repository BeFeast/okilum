//! Linux managed launch and configuration admission (#2582).
//!
//! The runtime gate serializes only launch/enrollment/configuration operations;
//! it is never a substitute for the durable per-vault authority or a transport
//! quiescence receipt. Closed folders are fenced on disk BEFORE spawning.
use crate::{daemon::DaemonIdentity, lifecycle::ManagedInstance, private};
use anyhow::{ensure, Context, Result};
use okilum_write_authority::{Authority, DurableBinding, Phase, WriterLease};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Read,
    net::{IpAddr, SocketAddr},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Weak},
    time::{Duration, Instant},
};
use uuid::Uuid;
use xmltree::{Element, XMLNode};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Identity(u64, u64, Option<u64>);
pub(crate) fn identity(path: &Path) -> Result<Identity> {
    let m = fs::symlink_metadata(path)?;
    ensure!(
        !m.file_type().is_symlink(),
        "managed path replaced by symlink"
    );
    Ok(Identity(
        m.dev(),
        m.ino(),
        m.created()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|t| u64::try_from(t.as_nanos()).ok()),
    ))
}
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
fn directory(home: &Path) -> Result<PathBuf> {
    Ok(home
        .parent()
        .context("managed home has no parent")?
        .join("managed-launch"))
}
fn lock_file(path: &Path, create: bool, wait: bool) -> Result<File> {
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
thread_local! { static GATES: RefCell<BTreeMap<PathBuf, Weak<File>>> = const { RefCell::new(BTreeMap::new()) }; }
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
    fn gate(&self) -> Result<Arc<File>> {
        let before = self.load()?;
        if let Some(lock) = GATES.with(|g| g.borrow().get(&self.directory).and_then(Weak::upgrade))
        {
            return Ok(lock);
        }
        let lock = lock_file(&self.directory.join("gate.lock"), false, true)?;
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
    let xml = Element::parse(private::read(&instance.home.join("config.xml"))?.as_slice())?;
    for e in xml
        .children
        .iter()
        .filter_map(XMLNode::as_element)
        .filter(|e| e.name == "folder")
    {
        let id = e.attributes.get("id").context("folder ID missing")?;
        let root = PathBuf::from(e.attributes.get("path").context("folder root missing")?)
            .canonicalize()?;
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
    _gate: Arc<File>,
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
        ensure!(
            !home
                .parent()
                .context("preparation missing")?
                .join("prepared.json")
                .try_exists()?,
            "managed launch binding missing; recovery required"
        );
        return Ok(None);
    }
    let r = existing(home)?;
    let gate = r.gate()?;
    let mut b = r.load()?;
    ensure!(
        b.home == home && b.certificate_hash == daemon.certificate_hash,
        "managed daemon binding differs"
    );
    let root = root.canonicalize()?;
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
    _gate: Arc<File>,
}
pub(crate) fn runtime_configuration(daemon: &DaemonIdentity) -> Result<RuntimeConfiguration> {
    let home = daemon.config_file.parent().context("daemon home missing")?;
    let reference = existing(home).context("external runtime has no managed hold ownership")?;
    let gate = reference.gate()?;
    let b = reference.load()?;
    ensure!(
        b.home == home && b.certificate_hash == daemon.certificate_hash,
        "managed runtime identity changed"
    );
    let api = daemon.connect()?;
    let config = api.config()?;
    let mut operations = Vec::new();
    for folder in config["folders"]
        .as_array()
        .context("folder inventory missing")?
    {
        let id = folder["id"].as_str().context("folder ID missing")?;
        let saved = b
            .folders
            .get(id)
            .context("managed folder binding missing")?;
        let root = Path::new(folder["path"].as_str().context("folder path missing")?);
        ensure!(
            root.canonicalize()? == saved.root && identity(root)? == saved.identity,
            "managed folder root changed"
        );
        operations.push(saved.authority.open()?.operation()?);
    }
    Ok(RuntimeConfiguration {
        _operations: operations,
        _gate: gate,
    })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Generation {
    pub id: Uuid,
    pub pid: u32,
    pub start_time: String,
    pub binding: Uuid,
    pub executable_hash: String,
    pub rest_address: SocketAddr,
    pub rest_start_time: Option<String>,
}
/// Bind authenticated REST to the exact kernel process, not merely to another
/// process with the same certificate/home. /proc/net alone is insufficient:
/// require that this child actually owns the listening socket inode.
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
fn rest_listener(pid: u32, address: SocketAddr) -> Result<Option<String>> {
    let (table, ip) = match address.ip() {
        IpAddr::V4(ip) => ("tcp", format!("{:08X}", u32::from_le_bytes(ip.octets()))),
        IpAddr::V6(ip) => (
            "tcp6",
            ip.octets()
                .chunks_exact(4)
                .map(|b| format!("{:08X}", u32::from_le_bytes(b.try_into().unwrap())))
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
/// Invoked only by the dedicated autonomous command after lifecycle verification.
/// Returns the child's exit status; the service manager owns retry policy.
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
    let mut xml = Element::parse(private::read(&b.home.join("config.xml"))?.as_slice())?;
    let mut leases: Vec<WriterLease> = Vec::new();
    let mut closed = Vec::new();
    for e in xml
        .children
        .iter_mut()
        .filter_map(XMLNode::as_mut_element)
        .filter(|e| e.name == "folder")
    {
        let id = e.attributes.get("id").context("folder ID missing")?.clone();
        let saved = b
            .folders
            .get(&id)
            .context("unbound managed folder; recovery required")?;
        let root = Path::new(e.attributes.get("path").context("folder root missing")?);
        ensure!(
            root.canonicalize()? == saved.root && identity(root)? == saved.identity,
            "managed folder replaced"
        );
        let a = saved.authority.open()?;
        // An admission refusal can be a hold OR uncertainty. Never infer Open
        // from a failed read. The actual admission closes check/effect races.
        match a.admit() {
            Ok(lease) => leases.push(lease),
            Err(_) => {
                // Validate durable state; corruption fails the entire launch.
                ensure!(
                    a.snapshot()?.phase != Phase::Open,
                    "uncertain launch admission"
                );
                set_paused(e);
                closed.push(id);
            }
        }
    }
    let mut bytes = Vec::new();
    xml.write(&mut bytes)?;
    private::write(&b.home.join("config.xml"), &bytes)?;
    // Never restore an old paused=false value, even after authority release.
    // Explicit admitted resume/reconciliation owns the later configuration.
    let mut command = Command::new(&b.executable);
    command
        .env_clear()
        .env("HOME", &b.home)
        .env("STMONITORED", "1")
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
            binding: b.id,
            executable_hash: b.executable_hash.clone(),
            rest_address,
            rest_start_time: None,
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
                    for id in &closed {
                        ensure!(
                            api.folder(id)?["paused"] == true,
                            "startup fence readback failed"
                        );
                    }
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
    let status = child.wait()?;
    ensure!(status.success(), "managed runtime crashed: {status}");
    Ok(())
}
fn set_paused(folder: &mut Element) {
    // Syncthing v2.1.6 serializes paused as a child element (not an attribute).
    folder.attributes.remove("paused");
    if folder.get_child("paused").is_none() {
        folder
            .children
            .push(XMLNode::Element(Element::new("paused")));
    }
    folder.get_mut_child("paused").unwrap().children = vec![XMLNode::Text("true".into())];
}

/// Observe a live authenticated launch generation. A stale file/PID, helper
/// crash, replaced package or reused process identity never yields a generation.
/// This is identity evidence only; it grants no folder-quiescence receipt.
pub fn live_generation(home: &Path) -> Result<Generation> {
    let r = existing(home)?;
    let b = r.load()?;
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
            && process_start(generation.pid)? == generation.start_time,
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
