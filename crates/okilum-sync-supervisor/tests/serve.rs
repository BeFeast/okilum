//! The run loop against a real process tree and real sockets, driven the way the app
//! will drive it: read the hint, connect, Status, an authorized Stop. The "runtime" is a
//! tiny rustc-built fixture started with the production argv; nothing here starts
//! Syncthing or touches personal state.
#![cfg(unix)]

use okilum_sync_controller::sidecar::{
    authority::Envelope,
    store::{Selection, UnixStore},
    supervisor::ipc::{
        exchange,
        unix_transport::{connect, PeerEnd, SameUser, UnixTransport},
        Command, Request, Scope, Status,
    },
    Binding,
};
use okilum_sync_supervisor::{
    serve::{self, Exit, Settings},
    startup::{self, Startup},
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command as Process,
    time::{Duration, Instant},
};
use uuid::Uuid;

// The root starts one child in its own process group and waits for it; marker files in
// the data directory pick the variant (the root exiting at once).
const FIXTURE: &str = r#"
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("child") {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while std::time::Instant::now() < until {
            if std::path::Path::new(&args[2]).join("release-child").exists() { break; }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        return;
    }
    let data = args.windows(2).find(|a| a[0] == "--data").unwrap()[1].clone();
    let dir = std::path::Path::new(&data);
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("child").arg(&data).spawn().unwrap();
    std::fs::write(dir.join("child.pid"), child.id().to_string()).unwrap();
    if dir.join("exit-parent").exists() { return; }
    let _ = child.wait();
}
"#;

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn alive(pid: u32) -> bool {
    let out = Process::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&out.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}
fn settings() -> Settings {
    Settings {
        exchange: Duration::from_secs(5),
        stop: Duration::from_secs(10),
        tick: Duration::from_millis(50),
        store: Duration::from_secs(5),
    }
}

struct World {
    _root: tempfile::TempDir,
    state: PathBuf,
    own: PathBuf,
    store: UnixStore,
    binding: Binding,
}
impl World {
    fn new(markers: &[&str]) -> Self {
        Self::with_supervisor(markers, std::env::current_exe().unwrap())
    }
    /// `own` is the executable the journal names as the supervisor.
    fn with_supervisor(markers: &[&str], own: PathBuf) -> Self {
        let root = tempfile::tempdir().unwrap();
        let state = fs::canonicalize(root.path()).unwrap().join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        for name in ["config", "data"] {
            fs::create_dir(state.join(name)).unwrap();
        }
        for marker in markers {
            fs::write(state.join("data").join(marker), b"").unwrap();
        }
        // Stage the runtime inside its own versioned directory and select it.
        let staging = state.join("runtime").join("v1");
        fs::create_dir_all(&staging).unwrap();
        let source = staging.join("fixture.rs");
        let executable = staging.join("syncthing");
        fs::write(&source, FIXTURE).unwrap();
        let compiled = Process::new("rustc")
            .arg("--edition=2021")
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        fs::remove_file(&source).unwrap();
        let digest: String = Sha256::digest(fs::read(&executable).unwrap())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let binding = Binding {
            instance: Uuid::from_u128(2),
            installation: Uuid::from_u128(1),
            owner: "501".into(),
            supervisor: own.to_string_lossy().into_owned(),
            state_directory: state.to_string_lossy().into_owned(),
            device_identity: "existing-device".into(),
        };
        let store = UnixStore::open_existing(&state).unwrap();
        store
            .begin(soon())
            .unwrap()
            .commit(Envelope::first(binding.clone()))
            .unwrap();
        store
            .publish_selection(soon(), &Selection::new("v1", &digest, &executable).unwrap())
            .unwrap();
        Self {
            _root: root,
            state,
            own,
            store,
            binding,
        }
    }
    fn prepare(&self) -> anyhow::Result<Startup> {
        startup::prepare(&self.store, &self.state, None, &self.own, soon())
    }
    fn child_pid(&self) -> Option<u32> {
        fs::read_to_string(self.state.join("data/child.pid"))
            .ok()
            .and_then(|text| text.parse().ok())
    }
}
/// Run the supervisor in a thread and return it with the scope it announced.
fn start(world: &World) -> (std::thread::JoinHandle<anyhow::Result<Exit>>, Scope) {
    let Startup::Run(prepared) = world.prepare().unwrap() else {
        panic!("expected a runnable start")
    };
    let (state, store) = (world.state.clone(), world.store.clone());
    let handle =
        std::thread::spawn(move || serve::run(&state, store, *prepared, &settings(), || SameUser));
    let hint = loop {
        if let Ok(Some(hint)) = world.store.read_hint(soon()) {
            break hint;
        }
        assert!(
            !handle.is_finished(),
            "the supervisor ended before it published a hint"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let scope = Scope {
        installation: world.binding.installation,
        instance: world.binding.instance,
        generation: hint.generation(),
    };
    (handle, scope)
}
fn call(world: &World, scope: &Scope, command: Command) -> anyhow::Result<Status> {
    let stream = connect(&world.state, scope)?;
    let mut transport = UnixTransport::new(
        stream,
        PeerEnd::Server,
        world.binding.clone(),
        scope.clone(),
        Instant::now() + Duration::from_secs(5),
        SameUser,
    )?;
    exchange(
        &mut transport,
        &world.binding,
        &Request::new(scope.clone(), command),
    )
}
/// Arm a Disable stop for this generation, as the controller does before it sends Stop.
fn arm_stop(world: &World, scope: &Scope) -> Command {
    let mut transaction = world.store.begin(soon()).unwrap();
    let current = transaction.current().unwrap().unwrap().clone();
    let (next, token) = current.disable(scope.clone()).unwrap();
    transaction.commit(next).unwrap();
    Command::Stop(token)
}

#[test]
fn a_valid_start_serves_status_and_ends_cleanly_on_an_authorized_stop() {
    let world = World::new(&[]);
    let (handle, scope) = start(&world);
    wait_until("the runtime's descendant", || world.child_pid().is_some());
    let child = world.child_pid().unwrap();
    assert!(alive(child), "positive control: the runtime tree is up");
    assert_eq!(
        call(&world, &scope, Command::Status).unwrap(),
        Status::Running
    );

    let stop = arm_stop(&world, &scope);
    assert_eq!(call(&world, &scope, stop).unwrap(), Status::Stopped);
    assert_eq!(handle.join().unwrap().unwrap(), Exit::Stopped);
    wait_until("the runtime tree to be gone", || !alive(child));
    assert_eq!(
        world.store.read_hint(soon()).unwrap(),
        None,
        "hint outlived the supervisor"
    );
    let leftovers: Vec<_> = fs::read_dir(&world.state)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".sock"))
        .collect();
    assert!(leftovers.is_empty(), "socket left behind: {leftovers:?}");
}

#[test]
fn a_bad_stop_costs_only_that_exchange() {
    let world = World::new(&[]);
    let (handle, scope) = start(&world);
    // A token nobody armed: refused, nothing stopped, and the supervisor keeps serving.
    let bogus = Command::Stop(
        Envelope::first(world.binding.clone())
            .disable(scope.clone())
            .unwrap()
            .1,
    );
    assert!(call(&world, &scope, bogus).is_err());
    assert_eq!(
        call(&world, &scope, Command::Status).unwrap(),
        Status::Running
    );
    assert!(!handle.is_finished());
    let stop = arm_stop(&world, &scope);
    assert_eq!(call(&world, &scope, stop).unwrap(), Status::Stopped);
    assert_eq!(handle.join().unwrap().unwrap(), Exit::Stopped);
}

#[test]
fn a_runtime_that_exits_by_itself_is_flushed_reported_and_its_hint_cleared() {
    let world = World::new(&["exit-parent"]);
    let Startup::Run(prepared) = world.prepare().unwrap() else {
        panic!("expected a runnable start")
    };
    let exit = serve::run(
        &world.state,
        world.store.clone(),
        *prepared,
        &settings(),
        || SameUser,
    )
    .unwrap();
    assert_eq!(exit, Exit::RuntimeExited);
    let child = world
        .child_pid()
        .expect("the runtime started its descendant");
    wait_until("the leftover descendant to be flushed", || !alive(child));
    assert_eq!(world.store.read_hint(soon()).unwrap(), None);
}

#[test]
fn a_selection_changed_after_prepare_never_reaches_the_spawn() {
    let world = World::new(&[]);
    let Startup::Run(prepared) = world.prepare().unwrap() else {
        panic!("expected a runnable start")
    };
    let executable = world.state.join("runtime/v1/syncthing");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(&executable, b"#!/bin/sh\nexit 0\n").unwrap();
    let error = serve::run(
        &world.state,
        world.store.clone(),
        *prepared,
        &settings(),
        || SameUser,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("does not match"), "{error:#}");
    assert!(
        world.child_pid().is_none(),
        "the tampered runtime was started"
    );
    assert_eq!(world.store.read_hint(soon()).unwrap(), None);
}

#[test]
fn startup_refuses_everything_that_is_not_exactly_as_recorded() {
    let good = World::new(&[]);
    assert!(matches!(good.prepare().unwrap(), Startup::Run(_))); // control

    // Another executable than the journal names, another instance, another state dir.
    let wrong_exe = Path::new("/bin/sh");
    assert!(startup::prepare(&good.store, &good.state, None, wrong_exe, soon()).is_err());
    let other_instance = Some(Uuid::from_u128(77));
    assert!(startup::prepare(&good.store, &good.state, other_instance, &good.own, soon()).is_err());
    let elsewhere = tempfile::tempdir().unwrap();
    assert!(startup::prepare(&good.store, elsewhere.path(), None, &good.own, soon()).is_err());
    assert!(startup::prepare(
        &good.store,
        &good.state,
        Some(good.binding.instance),
        &good.own,
        soon()
    )
    .is_ok());

    // A tampered runtime, a missing selection, a missing data directory.
    let tampered = World::new(&[]);
    fs::write(
        tampered.state.join("runtime/v1/syncthing"),
        b"not the runtime",
    )
    .unwrap();
    let error = tampered.prepare().unwrap_err().to_string();
    assert!(error.contains("does not match"), "{error}");
    let unselected = World::new(&[]);
    fs::remove_file(unselected.state.join("runtime.json")).unwrap();
    assert!(unselected
        .prepare()
        .unwrap_err()
        .to_string()
        .contains("no Syncthing runtime"));
    let no_data = World::new(&[]);
    fs::remove_dir(no_data.state.join("data")).unwrap();
    assert!(no_data.prepare().is_err());

    // No journal at all.
    let empty = tempfile::tempdir().unwrap();
    fs::set_permissions(empty.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let store = UnixStore::open_existing(empty.path()).unwrap();
    assert!(startup::prepare(&store, empty.path(), None, &good.own, soon()).is_err());
}

#[test]
fn a_disabled_instance_is_idle_not_an_error() {
    let world = World::new(&[]);
    let scope = Scope {
        installation: world.binding.installation,
        instance: world.binding.instance,
        generation: Uuid::new_v4(),
    };
    let mut transaction = world.store.begin(soon()).unwrap();
    let current = transaction.current().unwrap().unwrap().clone();
    transaction
        .commit(current.disable(scope).unwrap().0)
        .unwrap();
    drop(transaction);
    assert!(matches!(world.prepare().unwrap(), Startup::Idle));
}

#[test]
fn the_binary_exits_zero_when_idle_and_refuses_to_serve_without_a_signature_policy() {
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_okilum-sync-supervisor"));
    let world = World::with_supervisor(&[], binary.clone());
    let state = world.state.to_str().unwrap().to_string();
    let run = |args: &[&str]| Process::new(&binary).args(args).output().unwrap();

    // Enabled and fully valid, but this build has no signature policy: refused before
    // anything is launched, and for that reason (not an unrelated check).
    let refused = run(&["--state", &state]);
    assert_eq!(refused.status.code(), Some(1));
    let message = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(message.contains("no signature policy"), "{message}");
    assert!(
        world.child_pid().is_none(),
        "nothing may start without a policy"
    );
    assert_eq!(world.store.read_hint(soon()).unwrap(), None);

    // Unknown arguments are refused too.
    assert_eq!(run(&["--state", &state, "--evil"]).status.code(), Some(1));
    assert_eq!(run(&["serve"]).status.code(), Some(1));

    // Not Enabled: nothing to supervise, success, and no policy is needed.
    let scope = Scope {
        installation: world.binding.installation,
        instance: world.binding.instance,
        generation: Uuid::new_v4(),
    };
    let mut transaction = world.store.begin(soon()).unwrap();
    let current = transaction.current().unwrap().unwrap().clone();
    transaction
        .commit(current.disable(scope).unwrap().0)
        .unwrap();
    drop(transaction);
    assert_eq!(run(&["--state", &state]).status.code(), Some(0));
}
