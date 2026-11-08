//! #2582: separate processes and checksum-pinned disposable runtimes only.
#![cfg(target_os = "linux")]
#[path = "support/direct_daemon.rs"]
#[allow(
    dead_code,
    reason = "shared fixture also supplies the direct pairing suites"
)]
mod direct_daemon;
use anyhow::{ensure, Context, Result};
use direct_daemon::fixture_process;
use direct_daemon::{wait, Peer};
use okilum_sync_controller::{
    daemon::{DaemonIdentity, Preparation},
    direct::Controller,
    enrollment::{Intent, Snapshot},
    folder::FolderController,
    lifecycle::{Lifecycle, UserServices},
    managed_launch::live_generation,
    pairing::{Descriptor, Registration, State},
    runtime::{Runtime, Selection},
};
use okilum_write_authority::{Authority, Phase};
use serde_json::{json, Value};
use sha2::Digest;
use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
use uuid::Uuid;

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = fixture_process::stop(&mut self.0);
    }
}
fn worker(mode: &str, root: &Path) -> Result<OwnedChild> {
    Ok(OwnedChild(fixture_process::spawn(
        Command::new(std::env::current_exe()?)
            .args(["issue2582_child", "--exact", "--ignored", "--nocapture"])
            .env("OKILUM_ADMISSION_CHILD", mode)
            .env("OKILUM_ADMISSION_ROOT", root),
    )?))
}
#[test]
#[ignore = "only the fixture parent selects child operation"]
fn issue2582_child() -> Result<()> {
    let root = PathBuf::from(std::env::var_os("OKILUM_ADMISSION_ROOT").context("fixture root")?);
    match std::env::var("OKILUM_ADMISSION_CHILD")?.as_str() {
        "resume" => {
            FolderController::new(root.join("folder-state")).pause(false)?;
        }
        "holder" => {
            let authority = Authority::for_root(&root.join("selected"))?;
            let mut hold = authority.begin_drain("fixture-holder")?;
            let token = hold.token().clone();
            wait("held", || hold.try_hold(&token))?;
            fs::write(root.join("holder-ready"), "held")?;
            loop {
                thread::sleep(Duration::from_millis(50));
            }
        }
        "scoped-holder" => {
            let daemon: DaemonIdentity =
                serde_json::from_slice(&fs::read(root.join("scoped-daemon.json"))?)?;
            let authority = Authority::for_root(&root.join("selected"))?;
            let mut hold = authority.begin_drain("scoped-first-sync")?;
            let token = hold.token().clone();
            wait("local writers drained", || hold.try_hold(&token))?;
            let scope = okilum_sync_controller::managed_launch::ScopedHold::acquire(
                daemon, "selected", hold, false,
            )?;
            scope.validate()?;
            fs::write(
                root.join("holder-ready"),
                "transport and local writers quiescent",
            )?;
            loop {
                thread::sleep(Duration::from_millis(50));
            }
        }
        #[cfg(feature = "first-sync-fixture")]
        "first-sync-crash" => {
            let selected = fs::read_to_string(root.join("selected-checkpoint"))?;
            let evidence = root.clone();
            okilum_sync_controller::direct::inventory::merge::fixture::install(
                move |name, operation| {
                    if name == selected {
                        let mut receipt =
                            fs::File::create(evidence.join("crash-receipt.json")).unwrap();
                        receipt
                            .write_all(
                                &serde_json::to_vec(
                                    &json!({"checkpoint":name,"operation":operation}),
                                )
                                .unwrap(),
                            )
                            .unwrap();
                        receipt.sync_all().unwrap();
                        fs::File::open(&evidence).unwrap().sync_all().unwrap();
                        unsafe { libc::_exit(86) }
                    }
                },
            );
            let identities: Vec<DaemonIdentity> =
                serde_json::from_slice(&fs::read(root.join("identities.json"))?)?;
            let first = Controller::open(
                root.join("source-controller"),
                vec![identities[0].config_file.clone()],
            )?;
            first.refresh_pairing()?;
            let second = Controller::open(
                root.join("target-controller"),
                vec![identities[1].config_file.clone()],
            )?;
            let peer = second.snapshot()?.peers[0].id;
            let vault = second.catalog(peer)?[0].id.clone();
            let preview = second.preview_remote_vault(peer, &vault, &root.join("here"))?;
            let plan = preview.plan().clone();
            let mut backend = second.first_sync_backend(preview);
            plan.apply(plan.consent(true).unwrap(), backend.as_mut())
                .map_err(|error| anyhow::anyhow!("child merge: {error:?}"))?;
            anyhow::bail!("selected crash checkpoint was not fired")
        }
        _ => anyhow::bail!("unknown fixture mode"),
    }
    Ok(())
}

fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
fn private_json(path: &Path, value: &Value) -> Result<()> {
    fs::write(path, serde_json::to_vec(value)?)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// This fixture delays the PATCH acknowledgement, not admission. The ready
/// marker proves the real FolderController has reached its REST effect while
/// retaining the operation; all subsequent status/readback endpoints also run.
struct DelayedRest {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<Result<usize>>>,
}
impl DelayedRest {
    fn new(root: &Path) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let root = root.to_owned();
        let join = thread::spawn(move || -> Result<usize> {
            let mut paused = true;
            let mut patches = 0;
            while !flag.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(v) => v,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                };
                stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                let mut bytes = Vec::new();
                let header_end = loop {
                    let mut block = [0; 1024];
                    let n = stream.read(&mut block)?;
                    ensure!(n > 0, "empty fixture request");
                    bytes.extend_from_slice(&block[..n]);
                    if let Some(i) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let header = String::from_utf8(bytes[..header_end].to_vec())?;
                let length = header
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(str::trim)
                            .map(str::to_owned)
                    })
                    .map(|s| s.parse::<usize>())
                    .transpose()?
                    .unwrap_or(0);
                while bytes.len() < header_end + length {
                    let mut block = [0; 1024];
                    let n = stream.read(&mut block)?;
                    ensure!(n > 0, "short fixture request");
                    bytes.extend_from_slice(&block[..n]);
                }
                let mut words = header.lines().next().unwrap().split_whitespace();
                let method = words.next().unwrap();
                let path = words.next().unwrap();
                if method == "PATCH" {
                    paused = serde_json::from_slice::<Value>(&bytes[header_end..])?["paused"]
                        .as_bool()
                        .unwrap();
                    patches += 1;
                    fs::write(root.join("patch-ready"), "actual PATCH reached")?;
                    wait("ack release", || {
                        Ok(root.join("allow-ack").exists() || flag.load(Ordering::SeqCst))
                    })?;
                }
                let folder = json!({"id":"fixture","path":root.join("selected"),"paused":paused,"type":"sendreceive"});
                let value = if path.starts_with("/rest/system/status") {
                    json!({"myID":(["AAAAAAA";8].join("-"))})
                } else if path.starts_with("/rest/system/version") {
                    json!({"version":"v2.1.6"})
                } else if path.starts_with("/rest/config/folders/") {
                    folder.clone()
                } else if path == "/rest/config" {
                    json!({"folders":[folder]})
                } else if path.starts_with("/rest/db/ignores") {
                    json!({"ignore":[]})
                } else if path.starts_with("/rest/system/connections") {
                    json!({"connections":{}})
                } else {
                    json!({})
                };
                let body = serde_json::to_vec(&value)?;
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())?;
                stream.write_all(&body)?;
            }
            Ok(patches)
        });
        Ok(Self {
            address,
            stop,
            join: Some(join),
        })
    }
    fn finish(mut self) -> Result<usize> {
        self.stop.store(true, Ordering::SeqCst);
        self.join.take().unwrap().join().unwrap()
    }
}
impl Drop for DelayedRest {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[test]
fn issue2582_configuration_process_drains_and_later_real_effects_are_refused() -> Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path();
    private_dir(&path.join("selected"))?;
    private_dir(&path.join("external"))?;
    private_dir(&path.join("folder-state"))?;
    let server = DelayedRest::new(path)?;
    let certificate = b"synthetic certificate";
    fs::write(path.join("external/cert.pem"), certificate)?;
    let config = path.join("external/config.xml");
    fs::write(&config, format!("<configuration><gui tls=\"false\"><address>{}</address><apikey>fixture-key</apikey></gui></configuration>", server.address))?;
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600))?;
    use sha2::{Digest, Sha256};
    let daemon = DaemonIdentity {
        config_file: config,
        device_id: ["AAAAAAA"; 8].join("-"),
        certificate_hash: format!("{:x}", Sha256::digest(certificate)),
    };
    private_json(
        &path.join("folder-state/folder.json"),
        &json!({
            "daemon":daemon,"owner":Uuid::new_v4(),"registration":Uuid::new_v4(),"vault":Uuid::new_v4(),
            "descriptor":{"folder_id":"fixture","hub_device_id":(["BBBBBBB";8].join("-")),"hub_address":"tcp://127.0.0.1:22000","ignores":[]},
            "path":path.join("selected"),"owns_folder":false,"owns_hub":false,"original_paused":true,
            "changed_pause":false,"desired_paused":true,"remove_requested":false,"phase":"Reused"
        }),
    )?;
    assert!(
        live_generation(&path.join("external")).is_err(),
        "external runtime cannot acquire managed hold ownership"
    );
    assert!(!path.join("managed-launch").exists());
    let authority = Authority::for_root(&path.join("selected"))?;
    let mut child = worker("resume", path)?;
    wait("actual configuration PATCH", || {
        Ok(path.join("patch-ready").exists())
    })?;
    let mut hold = authority.begin_drain("configuration-race")?;
    let token = hold.token().clone();
    assert!(
        !hold.try_hold(&token)?,
        "configuration lease must survive until acknowledgement/readback"
    );
    let controller = FolderController::new(path.join("folder-state"));
    for _ in 0..2 {
        assert!(
            controller.pause(false).is_err(),
            "a later resume must refuse before REST"
        );
    }
    fs::write(path.join("allow-ack"), "finish admitted effect")?;
    assert!(
        child.0.wait()?.success(),
        "already admitted configuration must finish through Draining"
    );
    assert!(hold.try_hold(&token)?);
    let token = hold.token().clone();
    let mut later = worker("resume", path)?;
    assert!(
        !later.0.wait()?.success(),
        "a separate later process must refuse the durable hold"
    );
    assert!(
        controller.remove().is_err(),
        "pause restoration during removal is also admitted"
    );
    hold.release(&token)?;
    controller.pause(false)?;
    assert_eq!(
        server.finish()?,
        2,
        "one admitted operation and one positive post-release resume"
    );
    Ok(())
}

#[derive(Default)]
struct ServiceState {
    command: Option<Vec<String>>,
    child: Option<Child>,
    runtime_pid: Option<u32>,
    registered: bool,
    unit: Option<PathBuf>,
    starts: usize,
    stops: usize,
}
impl ServiceState {
    fn stop(&mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            fixture_process::stop(&mut child)?;
        }
        if let Some(pid) = self.runtime_pid.take() {
            // The fixture test is a subreaper; the helper may have died before
            // waiting its child. Reap only the exact fixture generation we own.
            unsafe {
                libc::waitpid(pid as libc::pid_t, std::ptr::null_mut(), 0);
            }
        }
        Ok(())
    }
}
impl Drop for ServiceState {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
#[derive(Clone, Default)]
struct Services(Arc<Mutex<ServiceState>>);
impl Services {
    fn launch(&self) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        if let Some(child) = &mut state.child {
            if child.try_wait()?.is_none() {
                return Ok(());
            }
        }
        let command = state
            .command
            .clone()
            .context("fixture unit not registered")?;
        let log_path = Path::new(&command[0]).parent().unwrap().join("launch.log");
        let log = fs::File::create(log_path)?;
        let tools = Path::new(&command[0])
            .parent()
            .unwrap()
            .join("fixture-tools");
        private_dir(&tools)?;
        let script = tools.join("systemctl");
        fs::write(&script, format!("#!/bin/sh\ncase \"$5\" in\nFragmentPath) printf '%s\\n' '{}' ;;\nDropInPaths) printf '\\n' ;;\n*) exit 1 ;;\nesac\n", state.unit.as_ref().unwrap().display()))?;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700))?;
        state.child = Some(fixture_process::spawn(
            Command::new(&command[0])
                .args(&command[1..])
                .env("PATH", &tools)
                .stdout(log.try_clone()?)
                .stderr(log),
        )?);
        state.starts += 1;
        Ok(())
    }
    fn remember_pid(&self, home: &Path) -> Result<Uuid> {
        wait("authenticated helper generation ready", || {
            Ok(live_generation(home).is_ok())
        })?;
        let generation = live_generation(home)?;
        self.0.lock().unwrap().runtime_pid = Some(generation.pid);
        Ok(generation.id)
    }
    fn crash_runtime(&self) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        let pid = state.runtime_pid.take().context("no observed runtime")?;
        ensure!(
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) } == 0,
            "fixture kill failed"
        );
        ensure!(
            !state.child.as_mut().unwrap().wait()?.success(),
            "helper must report runtime crash"
        );
        state.child = None;
        Ok(())
    }
    fn crash_helper(&self, home: &Path) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        let pid = state.runtime_pid.take().context("no observed runtime")?;
        let mut helper = OwnedChild(state.child.take().context("no launch helper")?);
        // Kill only the helper, so process-group cleanup cannot make an unfired
        // PDEATHSIG control look successful. OwnedChild cleans up on failure.
        helper.0.kill()?;
        helper.0.wait()?;
        assert!(
            live_generation(home).is_err(),
            "dead launch ownership invalidates its generation"
        );
        wait("PDEATHSIG killed runtime", || {
            Ok(fs::metadata(format!("/proc/{pid}/exe")).is_err())
        })?;
        unsafe {
            libc::waitpid(pid as libc::pid_t, std::ptr::null_mut(), 0);
        }
        Ok(())
    }
}
impl UserServices for Services {
    fn reload(&self) -> Result<()> {
        Ok(())
    }
    fn verify_definition(&self, _: &str, path: &Path) -> Result<()> {
        let content = fs::read_to_string(path)?;
        let line = content
            .lines()
            .find_map(|s| s.strip_prefix("ExecStart="))
            .context("fixture ExecStart")?;
        // Fixture paths contain no expansion characters/spaces; command is read
        // from the actual production unit, never reconstructed independently.
        let command = line
            .split_whitespace()
            .map(|s| s.replace('"', ""))
            .collect::<Vec<_>>();
        let mut state = self.0.lock().unwrap();
        state.command = Some(command);
        state.unit = Some(path.to_owned());
        Ok(())
    }
    fn enable_start(&self, _: &str) -> Result<()> {
        self.0.lock().unwrap().registered = true;
        self.launch()
    }
    fn disable_stop(&self, _: &str) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        state.registered = false;
        state.stops += 1;
        state.stop()
    }
    fn verify_stopped(&self, _: &str) -> Result<()> {
        let state = self.0.lock().unwrap();
        ensure!(
            state.child.is_none() && !state.registered,
            "fixture still running"
        );
        Ok(())
    }
}
fn binary() -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os("OKILUM_SYNC_DIRECT_CLIENT").context("pinned fixture binary required")?,
    )
    .canonicalize()?)
}
fn launcher() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_okilum-sync-launch"))
}
fn subreaper() -> Result<()> {
    ensure!(
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } == 0,
        "fixture subreaper failed"
    );
    Ok(())
}
fn approval(daemon: &DaemonIdentity, source: &Peer, id: &str) -> Snapshot {
    Snapshot {
        request_id: Uuid::new_v4(),
        intent: Intent::Pair,
        removal: None,
        registration: Some(Registration {
            id: Uuid::new_v4(),
            vault: Uuid::new_v4(),
            device_id: daemon.device_id.clone(),
            name: "fixture".into(),
            state: State::HubReady,
            last_error: None,
        }),
        descriptor: Some(Descriptor {
            folder_id: id.into(),
            hub_device_id: source.id.clone(),
            hub_address: format!("tcp://{}", source.listen),
            ignores: vec![],
        }),
    }
}
fn add_source(source: &Peer, target: &DaemonIdentity, id: &str, path: &Path) -> Result<()> {
    source.api.add_paused_folder(&json!({"id":id,"path":path,"type":"sendreceive","paused":true,
        "rescanIntervalS":0,"fsWatcherEnabled":false,"devices":[{"deviceID":source.id},{"deviceID":target.device_id}]}))?;
    source.api.patch_folder(id, &json!({"paused":false}))?;
    Ok(())
}

fn first_sync_runtime(
    root: &Path,
    name: &str,
) -> Result<(Runtime<Services>, DaemonIdentity, Services)> {
    let services = Services::default();
    let rest = TcpListener::bind("127.0.0.1:0")?;
    let listen = TcpListener::bind("127.0.0.1:0")?;
    let request = Preparation {
        executable: binary()?,
        rest_address: rest.local_addr()?,
        listen_address: listen.local_addr()?,
    };
    drop((rest, listen));
    let runtime = Runtime::with_services(
        root.join(name),
        root.join(format!("{name}-units")),
        services.clone(),
        launcher(),
    );
    let identity = runtime
        .enable(Selection::Managed(request), &[])?
        .identity
        .unwrap();
    services.remember_pid(identity.config_file.parent().unwrap())?;
    Ok((runtime, identity, services))
}
fn first_sync_files(root: &Path) -> Result<std::collections::BTreeMap<String, Vec<u8>>> {
    fn walk(
        root: &Path,
        path: &Path,
        out: &mut std::collections::BTreeMap<String, Vec<u8>>,
    ) -> Result<()> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_str().context("fixture name")?;
            if matches!(
                name,
                ".stfolder" | ".stversions" | ".stignore" | ".okilum-index"
            ) || name.starts_with(".okilum-save-")
            {
                continue;
            }
            if entry.file_type()?.is_dir() {
                walk(root, &entry.path(), out)?;
            } else {
                out.insert(
                    entry
                        .path()
                        .strip_prefix(root)?
                        .to_str()
                        .unwrap()
                        .to_owned(),
                    fs::read(entry.path())?,
                );
            }
        }
        Ok(())
    }
    let mut files = std::collections::BTreeMap::new();
    walk(root, root, &mut files)?;
    Ok(files)
}
struct FirstSyncFixture {
    source_runtime: Runtime<Services>,
    target_runtime: Runtime<Services>,
    source: DaemonIdentity,
    target: DaemonIdentity,
    source_services: Services,
    target_services: Services,
    first: Controller,
    second: Controller,
    here: PathBuf,
    there: PathBuf,
    peer: Uuid,
    vault: String,
}
fn first_sync_fixture(root: &Path) -> Result<FirstSyncFixture> {
    let (source_runtime, source, source_services) = first_sync_runtime(root, "source-runtime")?;
    let (target_runtime, target, target_services) = first_sync_runtime(root, "target-runtime")?;
    let there = root.join("there");
    let here = root.join("here");
    fs::create_dir(&there)?;
    fs::create_dir(&here)?;
    fs::write(there.join("same.md"), b"same\r\n")?;
    fs::write(here.join("same.md"), b"same\r\n")?;
    fs::write(there.join("remote.md"), b"only on source")?;
    fs::write(here.join("local.md"), b"only on destination")?;
    fs::write(there.join("different.md"), b"source original\n")?;
    fs::write(here.join("different.md"), b"destination original\r\n")?;
    fs::write(there.join("binary.dat"), vec![0x93; 12_345])?;
    fs::write(here.join("binary.dat"), vec![0xff; 12_345])?;
    let first = Controller::open(
        root.join("source-controller"),
        vec![source.config_file.clone()],
    )?;
    let second = Controller::open(
        root.join("target-controller"),
        vec![target.config_file.clone()],
    )?;
    first.share_vault(&root.join("source-runtime/prepared"), &there)?;
    let url = first
        .begin_pairing_at(
            "Source fixture",
            "127.0.0.1:0".parse()?,
            "127.0.0.1".parse()?,
        )?
        .offer_url
        .unwrap();
    second.redeem(&url, "Destination fixture")?;
    first.approve(first.snapshot()?.pending.unwrap().request_id)?;
    let peer = second.refresh()?.peers[0].id;
    let catalog = second.catalog(peer)?;
    assert_eq!(catalog.len(), 1);
    let vault = catalog[0].id.clone();
    Ok(FirstSyncFixture {
        source_runtime,
        target_runtime,
        source,
        target,
        source_services,
        target_services,
        first,
        second,
        here,
        there,
        peer,
        vault,
    })
}
#[test]
#[ignore = "requires checksum-pinned fixture binary; explicitly selected by gh-check"]
fn issue2043_two_managed_instances_preview_union_preservation_and_bidirectional_handoff(
) -> Result<()> {
    use okilum_sync_controller::merge_preview::Kind;
    subreaper()?;
    let root = tempfile::tempdir()?;
    let FirstSyncFixture {
        source_runtime,
        target_runtime,
        source,
        target,
        source_services,
        target_services,
        first,
        second,
        here,
        there,
        peer,
        vault,
    } = first_sync_fixture(root.path())?;
    let vault = &vault;
    let a = source.connect()?;
    let b = target.connect()?;
    let a_address = a.config()?["options"]["listenAddresses"][0].clone();
    let b_address = b.config()?["options"]["listenAddresses"][0].clone();
    a.add_device(&json!({"deviceID":target.device_id,"addresses":[b_address]}))?;
    b.add_device(&json!({"deviceID":source.device_id,"addresses":[a_address]}))?;
    let before = (first_sync_files(&here)?, first_sync_files(&there)?);
    assert!(
        second.attach(peer, vault, &here).is_err(),
        "ordinary non-empty attach must still require preview consent"
    );
    let preview = second.preview_remote_vault(peer, vault, &here)?;
    let plan = preview.plan().clone();
    assert_eq!(plan.count(Kind::Equal), 1);
    assert_eq!(plan.count(Kind::OnlyHere), 1);
    assert_eq!(plan.count(Kind::OnlyThere), 1);
    assert_eq!(plan.count(Kind::Different), 2);
    let mut backend = second.first_sync_backend(preview);
    assert_eq!(
        (first_sync_files(&here)?, first_sync_files(&there)?),
        before,
        "preview/adapter construction must not publish"
    );
    // Stale local and remote revisions refuse before any merge publication.
    fs::write(here.join("local.md"), b"changed since preview")?;
    assert!(plan
        .apply(plan.consent(true).unwrap(), backend.as_mut())
        .is_err());
    assert_eq!(first_sync_files(&there)?, before.1);
    fs::write(here.join("local.md"), before.0.get("local.md").unwrap())?;
    let preview = second.preview_remote_vault(peer, vault, &here)?;
    let plan = preview.plan().clone();
    let mut backend = second.first_sync_backend(preview);
    fs::write(there.join("remote.md"), b"remote changed since preview")?;
    assert!(plan
        .apply(plan.consent(true).unwrap(), backend.as_mut())
        .is_err());
    assert_eq!(first_sync_files(&here)?, before.0);
    fs::write(there.join("remote.md"), before.1.get("remote.md").unwrap())?;
    let preview = second.preview_remote_vault(peer, vault, &here)?;
    let plan = preview.plan().clone();
    let conflicts = plan
        .entries()
        .iter()
        .filter_map(|entry| entry.conflict_path.clone())
        .collect::<Vec<_>>();
    let mut backend = second.first_sync_backend(preview);
    plan.apply(plan.consent(true).unwrap(), backend.as_mut())
        .map_err(|error| anyhow::anyhow!("first merge: {error:?}"))?;
    let merged = first_sync_files(&here)?;
    assert_eq!(merged, first_sync_files(&there)?);
    assert_eq!(merged.len(), 7);
    for bytes in before.0.values().chain(before.1.values()) {
        let hash = sha2::Sha256::digest(bytes);
        assert!(
            merged
                .values()
                .any(|value| sha2::Sha256::digest(value) == hash),
            "an original byte version was lost"
        );
    }
    assert_eq!(conflicts.len(), 2);
    for conflict in conflicts {
        assert!(here.join(&conflict).is_file() && there.join(&conflict).is_file());
    }
    assert!(second.pending_first_sync()?.is_empty());
    assert_eq!(b.folder(vault)?["type"], "sendreceive");
    fs::write(here.join("later-here.md"), b"later destination edit")?;
    wait("post-merge destination to source transfer", || {
        a.scan(vault)?;
        b.scan(vault)?;
        Ok(fs::read(there.join("later-here.md"))
            .is_ok_and(|bytes| bytes == b"later destination edit"))
    })?;
    fs::write(there.join("later-there.md"), b"later source edit")?;
    wait("post-merge source to destination transfer", || {
        a.scan(vault)?;
        b.scan(vault)?;
        Ok(fs::read(here.join("later-there.md")).is_ok_and(|bytes| bytes == b"later source edit"))
    })?;
    drop(backend);
    drop(first);
    drop(second);
    source_runtime.disable()?;
    target_runtime.disable()?;
    drop((source_services, target_services));
    Ok(())
}

#[test]
#[ignore = "requires checksum-pinned fixture binary; explicitly selected by gh-check"]
fn issue2043_scoped_hold_blocks_real_transfer_and_recovers_across_restart() -> Result<()> {
    use okilum_sync_controller::managed_launch::ScopedHold;
    subreaper()?;
    let root = tempfile::tempdir()?;
    let state = root.path().join("runtime");
    let services = Services::default();
    let rest = TcpListener::bind("127.0.0.1:0")?;
    let listen = TcpListener::bind("127.0.0.1:0")?;
    let request = Preparation {
        executable: binary()?,
        rest_address: rest.local_addr()?,
        listen_address: listen.local_addr()?,
    };
    drop((rest, listen));
    let runtime = Runtime::with_services(
        state,
        root.path().join("units"),
        services.clone(),
        launcher(),
    );
    let identity = runtime
        .enable(Selection::Managed(request), &[])?
        .identity
        .unwrap();
    let home = identity.config_file.parent().unwrap();
    services.remember_pid(home)?;
    let source = Peer::new(root.path(), "source", binary()?)?;
    let selected = root.path().join("selected");
    let second = root.path().join("second");
    let source_second = root.path().join("source-second");
    for path in [&selected, &second, &source_second] {
        fs::create_dir(path)?;
    }
    let api = identity.connect()?;
    let address = api.config()?["options"]["listenAddresses"][0]
        .as_str()
        .unwrap()
        .to_owned();
    source
        .api
        .add_device(&json!({"deviceID":identity.device_id,"addresses":[address]}))?;
    for (id, path, source_path) in [
        ("selected", &selected, &source.vault),
        ("second", &second, &source_second),
    ] {
        FolderController::new(root.path().join(format!("{id}-state"))).enroll(
            &identity,
            std::slice::from_ref(&identity.config_file),
            &approval(&identity, &source, id),
            path,
        )?;
        add_source(&source, &identity, id, source_path)?;
        fs::write(source_path.join("control.md"), format!("{id} before hold"))?;
    }
    wait("positive control transfers before scoped hold", || {
        source.api.scan("selected")?;
        source.api.scan("second")?;
        Ok(selected.join("control.md").exists() && second.join("control.md").exists())
    })?;
    private_json(
        &root.path().join("scoped-daemon.json"),
        &serde_json::to_value(&identity)?,
    )?;
    let mut child = worker("scoped-holder", root.path())?;
    wait("actual transport drain receipt", || {
        Ok(root.path().join("holder-ready").exists())
    })?;
    assert_eq!(api.folder("selected")?["paused"], true);
    assert_eq!(api.folder("second")?["paused"], false);
    let authority = Authority::for_root(&selected)?;
    assert!(authority.operation().is_err());
    fs::write(
        source.vault.join("withheld.md"),
        b"withheld during scoped hold",
    )?;
    fs::write(
        source_second.join("progress.md"),
        b"unrelated folder continues",
    )?;
    wait("unrelated folder transfer under scoped gate", || {
        source.api.scan("selected")?;
        source.api.scan("second")?;
        Ok(second.join("progress.md").exists())
    })?;
    for _ in 0..10 {
        source.api.scan("selected")?;
        assert!(
            !selected.join("withheld.md").exists(),
            "transport published during scoped ownership"
        );
        thread::sleep(Duration::from_millis(100));
    }
    fixture_process::stop(&mut child.0)?;
    assert_eq!(authority.snapshot()?.phase, Phase::RecoveryPending);
    services.crash_runtime()?;
    services.launch()?;
    services.remember_pid(home)?;
    assert!(live_generation(home)?
        .paused_at_start
        .contains(&"selected".into()));
    let mut hold = authority.recover(&authority.snapshot()?)?;
    let token = hold.token().clone();
    wait("recovery drains local writers", || hold.try_hold(&token))?;
    let scope = ScopedHold::acquire(identity.clone(), "selected", hold, true)?;
    scope.validate()?;
    assert!(!selected.join("withheld.md").exists());
    scope.release_and_resume()?;
    wait(
        "same withheld change transfers after authorized release",
        || {
            source.api.scan("selected")?;
            Ok(selected.join("withheld.md").exists())
        },
    )?;
    assert_eq!(
        fs::read(selected.join("withheld.md"))?,
        b"withheld during scoped hold"
    );
    assert!(authority.operation().is_ok());
    runtime.disable()?;
    Ok(())
}

#[test]
#[ignore = "requires checksum-pinned fixture binary; explicitly selected by gh-check"]
fn issue2582_autonomous_restart_fences_dead_holder_and_other_folder_transfers() -> Result<()> {
    subreaper()?;
    let root = tempfile::tempdir()?;
    let state = root.path().join("runtime");
    let units = root.path().join("units");
    let services = Services::default();
    let rest = TcpListener::bind("127.0.0.1:0")?;
    let listen = TcpListener::bind("127.0.0.1:0")?;
    let request = Preparation {
        executable: binary()?,
        rest_address: rest.local_addr()?,
        listen_address: listen.local_addr()?,
    };
    drop((rest, listen));
    let runtime =
        Runtime::with_services(state.clone(), units.clone(), services.clone(), launcher());
    let identity = runtime
        .enable(Selection::Managed(request), &[])?
        .identity
        .unwrap();
    wait("managed REST ready", || Ok(identity.connect().is_ok()))?;
    let home = identity.config_file.parent().unwrap();
    let original_generation = services.remember_pid(home)?;
    let sibling = state.join("prepared/external-sibling");
    fs::create_dir(&sibling)?;
    assert!(
        live_generation(&sibling).is_err(),
        "a sibling external home must not inherit managed generation ownership"
    );
    let generation_file = state.join("prepared/managed-launch/generation.json");
    let original = fs::read(&generation_file)?;
    let mut stale: Value = serde_json::from_slice(&original)?;
    stale["rest_start_time"] = json!("stale REST process");
    private_json(&generation_file, &stale)?;
    assert!(
        live_generation(home).is_err(),
        "REST generation must match the actual child listener"
    );
    fs::write(&generation_file, original)?;

    let source = Peer::new(root.path(), "source", binary()?)?;
    let source_second = root.path().join("source-second");
    let selected = root.path().join("selected");
    let second = root.path().join("second");
    fs::create_dir(&source_second)?;
    fs::create_dir(&selected)?;
    fs::create_dir(&second)?;
    let api = identity.connect()?;
    let listen =
        okilum_sync_controller::daemon::discover(std::slice::from_ref(&identity.config_file));
    assert_eq!(listen.candidates.len(), 1);
    // Explicit route from the source to the managed runtime's configured listener.
    let address = api.config()?["options"]["listenAddresses"][0]
        .as_str()
        .unwrap()
        .to_owned();
    source
        .api
        .add_device(&json!({"deviceID":identity.device_id,"addresses":[address]}))?;
    let folder = FolderController::new(root.path().join("folder-state"));
    let other = FolderController::new(root.path().join("other-folder-state"));
    let grant = approval(&identity, &source, "selected");
    let other_grant = approval(&identity, &source, "second");
    folder.enroll(
        &identity,
        std::slice::from_ref(&identity.config_file),
        &grant,
        &selected,
    )?;
    other.enroll(
        &identity,
        std::slice::from_ref(&identity.config_file),
        &other_grant,
        &second,
    )?;
    add_source(&source, &identity, "selected", &source.vault)?;
    add_source(&source, &identity, "second", &source_second)?;
    fs::write(
        source.vault.join("control.md"),
        "selected transfer before hold",
    )?;
    fs::write(
        source_second.join("control.md"),
        "second transfer before hold",
    )?;
    wait("both positive transfers", || {
        source.api.scan("selected")?;
        source.api.scan("second")?;
        Ok(selected.join("control.md").exists() && second.join("control.md").exists())
    })?;
    let mut holder = worker("holder", root.path())?;
    wait("separate holder admitted", || {
        Ok(root.path().join("holder-ready").exists())
    })?;
    assert!(folder.pause(false).is_err());
    // Kill controller/holder ownership; never release the durable closed state.
    fixture_process::stop(&mut holder.0)?;
    let authority = Authority::for_root(&selected)?;
    assert_eq!(authority.snapshot()?.phase, Phase::RecoveryPending);
    services.crash_runtime()?;
    assert!(
        live_generation(home).is_err(),
        "stale generation must refuse"
    );
    // A damaged/stale XML config must not retain a second paused=false that
    // the runtime could decode after the launch boundary's selected fence.
    let mut xml = xmltree::Element::parse(fs::read(&identity.config_file)?.as_slice())?;
    for folder in xml
        .children
        .iter_mut()
        .filter_map(xmltree::XMLNode::as_mut_element)
        .filter(|e| e.name == "folder" && e.attributes.get("id").is_some_and(|id| id == "selected"))
    {
        let mut paused = xmltree::Element::new("paused");
        paused.children.push(xmltree::XMLNode::Text("false".into()));
        folder.children.push(xmltree::XMLNode::Element(paused));
    }
    let mut bytes = Vec::new();
    xml.write(&mut bytes)?;
    fs::write(&identity.config_file, bytes)?;
    // Exact autonomous ExecStart, with no Runtime/controller call to pause.
    services.launch()?;
    wait("autonomous REST ready", || Ok(identity.connect().is_ok()))?;
    let generation = services.remember_pid(home)?;
    assert_ne!(original_generation, generation);
    assert_eq!(api.folder("selected")?["paused"], true);
    assert_eq!(api.folder("second")?["paused"], false);
    fs::write(
        source.vault.join("held.md"),
        "must stay fenced after restart",
    )?;
    fs::write(
        source_second.join("moving.md"),
        "second progresses after restart",
    )?;
    wait("second folder transfers across restart", || {
        source.api.scan("selected")?;
        source.api.scan("second")?;
        Ok(second.join("moving.md").exists())
    })?;
    for _ in 0..10 {
        source.api.scan("selected")?;
        assert!(!selected.join("held.md").exists());
        assert_eq!(api.folder("selected")?["paused"], true);
        thread::sleep(Duration::from_millis(100));
    }
    // Application reconcile uses the same managed launch command and never
    // clears the hold/fence. A second crash produces a fresh generation again.
    services.crash_helper(home)?;
    let unit = services.0.lock().unwrap().unit.clone().unwrap();
    let owned_definition = fs::read_to_string(&unit)?;
    fs::write(
        &unit,
        owned_definition.replace("RestartSec=5", "RestartSec=9"),
    )?;
    services.launch()?;
    {
        let mut state = services.0.lock().unwrap();
        assert!(
            !state.child.as_mut().unwrap().wait()?.success(),
            "autonomous launch must refuse a replaced definition"
        );
        state.child = None;
    }
    assert!(identity.connect().is_err());
    fs::write(&unit, owned_definition)?;
    runtime.reconcile()?;
    wait("Runtime reconcile REST", || Ok(identity.connect().is_ok()))?;
    assert_ne!(generation, services.remember_pid(home)?);
    assert_eq!(api.folder("selected")?["paused"], true);
    assert!(folder.pause(false).is_err());
    let hold = authority.recover(&authority.snapshot()?)?;
    let token = hold.token().clone();
    hold.release(&token)?;
    folder.pause(false)?;
    wait(
        "same held change transfers after explicit authorized resume",
        || {
            source.api.scan("selected")?;
            Ok(selected.join("held.md").exists())
        },
    )?;
    assert_eq!(
        fs::read_to_string(selected.join("held.md"))?,
        "must stay fenced after restart"
    );
    runtime.disable()?;
    assert!(!services.0.lock().unwrap().registered);
    Ok(())
}

#[test]
#[ignore = "requires checksum-pinned fixture binary; explicitly selected by gh-check"]
fn issue2582_linux_bootstrap_and_replaced_bindings_fail_closed() -> Result<()> {
    subreaper()?;
    let root = tempfile::tempdir()?;
    let state = root.path().join("bootstrap");
    let units = root.path().join("units");
    let services = Services::default();
    let identity = okilum_sync_controller::linux_bootstrap::connect_with(
        true,
        &state,
        &units,
        &[],
        || Ok(()),
        binary,
        services.clone(),
        Some(launcher()),
    )?;
    let home = identity.config_file.parent().unwrap();
    services.remember_pid(home)?;
    let vault = root.path().join("selected");
    fs::create_dir(&vault)?;
    let controller = Controller::open(
        root.path().join("direct"),
        vec![identity.config_file.clone()],
    )?;
    controller.share_vault(&state.join("prepared"), &vault)?;
    let mut holder = worker("holder", root.path())?;
    wait("holder ready", || {
        Ok(root.path().join("holder-ready").exists())
    })?;
    assert!(
        controller
            .share_vault(&state.join("prepared"), &vault)
            .is_err(),
        "source resume capable entrance must refuse"
    );
    let before_policy = identity.connect()?.config()?;
    assert!(
        okilum_sync_controller::network_policy::reconcile(
            &state.join("prepared"),
            &identity,
            Some(okilum_sync_controller::network_policy::Policy::LocalNetworkAndVpn)
        )
        .is_err(),
        "runtime-wide configuration must retain admission for every affected vault"
    );
    assert_eq!(identity.connect()?.config()?, before_policy);
    fixture_process::stop(&mut holder.0)?;
    services.crash_runtime()?;
    // Bootstrap's retry path must use exactly the same fence-aware command.
    okilum_sync_controller::linux_bootstrap::connect_with(
        true,
        &state,
        &units,
        std::slice::from_ref(&identity.config_file),
        || Ok(()),
        binary,
        services.clone(),
        Some(launcher()),
    )?;
    services.remember_pid(home)?;
    let config = identity.connect()?.config()?;
    assert!(config["folders"]
        .as_array()
        .unwrap()
        .iter()
        .all(|f| f["paused"] == true));
    let lifecycle = Lifecycle::new(state.join("service"), units.clone(), services.clone())
        .with_launcher(launcher());
    lifecycle.disable()?;
    // A fixture-only unwrapped runtime on the managed endpoint is external to
    // the helper's generation ownership. Refuse before offline config effects,
    // and never stop/adopt that process when reconciling the helper's service.
    let mut unwrapped = OwnedChild(fixture_process::spawn(
        Command::new(state.join("syncthing"))
            .env_clear()
            .env("HOME", home)
            .env("STMONITORED", "1")
            .args([
                "serve",
                "--no-browser",
                "--no-restart",
                "--no-upgrade",
                "--home",
            ])
            .arg(home)
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )?);
    wait("unwrapped fixture ready", || Ok(identity.connect().is_ok()))?;
    let external_config = identity.connect()?.config()?;
    let instance = okilum_sync_controller::lifecycle::ManagedInstance {
        home: home.to_owned(),
        executable: state.join("syncthing"),
    };
    lifecycle.enable(instance.clone())?;
    {
        let mut service = services.0.lock().unwrap();
        assert!(
            !service.child.as_mut().unwrap().wait()?.success(),
            "occupied external endpoint must refuse autonomous launch"
        );
        service.child = None;
    }
    assert!(
        live_generation(home).is_err(),
        "an external runtime must not obtain managed hold ownership"
    );
    assert_eq!(identity.connect()?.config()?, external_config);
    lifecycle.disable()?;
    assert!(
        unwrapped.0.try_wait()?.is_none(),
        "helper Disable must not stop an unowned fixture process"
    );
    drop(unwrapped);
    let preparation_receipt = state.join("prepared/prepared.json");
    let saved_receipt: Value = serde_json::from_slice(&fs::read(&preparation_receipt)?)?;
    let offline_config = fs::read(&identity.config_file)?;
    fs::remove_file(&preparation_receipt)?;
    assert!(
        okilum_sync_controller::linux_bootstrap::connect_with(
            true,
            &state,
            &units,
            std::slice::from_ref(&identity.config_file),
            || Ok(()),
            binary,
            services.clone(),
            Some(launcher())
        )
        .is_err(),
        "preparation retry must not bypass launch/configuration admission"
    );
    assert_eq!(fs::read(&identity.config_file)?, offline_config);
    private_json(&preparation_receipt, &saved_receipt)?;
    let binding = state.join("prepared/managed-launch/binding.json");
    let original = fs::read(&binding)?;
    let mut changed: Value = serde_json::from_slice(&original)?;
    changed["id"] = json!(Uuid::new_v4());
    private_json(&binding, &changed)?;
    assert!(lifecycle
        .enable(okilum_sync_controller::lifecycle::ManagedInstance {
            home: home.to_owned(),
            executable: state.join("syncthing")
        })
        .is_err());
    fs::write(&binding, &original)?;
    fs::remove_file(&binding)?;
    assert!(
        lifecycle
            .enable(okilum_sync_controller::lifecycle::ManagedInstance {
                home: home.to_owned(),
                executable: state.join("syncthing")
            })
            .is_err(),
        "missing binding must refuse a fresh start"
    );
    assert_eq!(
        services.0.lock().unwrap().stops,
        2,
        "only the two explicit helper Disable operations may stop a service"
    );
    Ok(())
}

/// Every checkpoint is in the production protocol, not in a fake adapter. The
/// application subprocess dies without Drop; both managed runtimes are then
/// restarted from their persisted config/authority before explicit recovery.
#[cfg(feature = "first-sync-fixture")]
#[test]
#[ignore = "requires checksum-pinned fixture binary; explicitly selected by gh-check"]
fn issue2043_crash_recovery_retains_versions_and_rejects_third_version() -> Result<()> {
    subreaper()?;
    for stage in [
        "destination-prepared",
        "source-prepared",
        "source-frozen",
        "source-captured",
        "source-published",
        "destination-published",
        "source-complete",
        "destination-released",
        "third-version",
    ] {
        if std::env::var("OKILUM_FIRST_SYNC_CRASH_CASE").is_ok_and(|selected| selected != stage) {
            continue;
        }
        let root = tempfile::tempdir()?;
        let FirstSyncFixture {
            source_runtime,
            target_runtime,
            source,
            target,
            source_services,
            target_services,
            first,
            second,
            here,
            there,
            peer: _,
            vault,
        } = first_sync_fixture(root.path())?;
        let originals = (first_sync_files(&here)?, first_sync_files(&there)?);
        fs::set_permissions(
            there.join("different.md"),
            fs::Permissions::from_mode(0o640),
        )?;
        private_json(
            &root.path().join("identities.json"),
            &json!([source, target]),
        )?;
        let checkpoint = if stage == "third-version" {
            "source-published"
        } else {
            stage
        };
        fs::write(root.path().join("selected-checkpoint"), checkpoint)?;
        first.shutdown();
        second.shutdown();
        drop((first, second));
        let mut child = worker("first-sync-crash", root.path())?;
        assert_eq!(
            child.0.wait()?.code(),
            Some(86),
            "checkpoint {stage} never fired"
        );
        let receipt: Value =
            serde_json::from_slice(&fs::read(root.path().join("crash-receipt.json"))?)?;
        assert_eq!(receipt["checkpoint"], checkpoint);
        let operation: Uuid = serde_json::from_value(receipt["operation"].clone())?;
        source_services.crash_helper(source.config_file.parent().unwrap())?;
        target_services.crash_helper(target.config_file.parent().unwrap())?;
        source_services.launch()?;
        target_services.launch()?;
        source_services.remember_pid(source.config_file.parent().unwrap())?;
        target_services.remember_pid(target.config_file.parent().unwrap())?;
        let first = Controller::open(
            root.path().join("source-controller"),
            vec![source.config_file.clone()],
        )?;
        first.refresh_pairing()?;
        let second = Controller::open(
            root.path().join("target-controller"),
            vec![target.config_file.clone()],
        )?;
        assert!(second
            .pending_first_sync()?
            .iter()
            .any(|pending| pending.operation == operation));
        if stage == "third-version" {
            fs::write(
                there.join("different.md"),
                b"third version while interrupted",
            )?;
            assert!(
                second.recover_first_sync(operation).is_err(),
                "third version must require review"
            );
            assert_eq!(
                fs::read(there.join("different.md"))?,
                b"third version while interrupted"
            );
            // Restore only this synthetic test's saved intended canonical bytes.
            fs::write(there.join("different.md"), &originals.0["different.md"])?;
        }
        second
            .recover_first_sync(operation)
            .with_context(|| format!("recover checkpoint {stage}"))?;
        let merged = first_sync_files(&here)?;
        assert_eq!(merged, first_sync_files(&there)?, "checkpoint {stage}");
        assert_eq!(
            merged.len(),
            7,
            "recovery created extra conflict copies at {stage}"
        );
        for bytes in originals.0.values().chain(originals.1.values()) {
            assert!(
                merged.values().any(|value| value == bytes),
                "original lost at {stage}"
            );
        }
        assert_eq!(
            fs::metadata(there.join("different.md"))?
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert!(second.pending_first_sync()?.is_empty());
        let a = source.connect()?;
        let b = target.connect()?;
        fs::write(here.join("after-recovery.md"), b"later edit")?;
        wait("post-recovery transfer", || {
            a.scan(&vault)?;
            b.scan(&vault)?;
            Ok(fs::read(there.join("after-recovery.md")).is_ok_and(|bytes| bytes == b"later edit"))
        })?;
        // Completed journals never re-run or resurrect a later deleted file.
        fs::remove_file(here.join("local.md"))?;
        assert!(second.recover_first_sync(operation).is_err());
        assert!(!here.join("local.md").exists());
        first.shutdown();
        second.shutdown();
        drop((first, second));
        source_runtime.disable()?;
        target_runtime.disable()?;
        println!("FIRST_SYNC_CRASH_RECOVERY_PASS checkpoint={stage} operation={operation}");
    }
    Ok(())
}
