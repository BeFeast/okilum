//! The Windows run loop against a real Job Object tree and a real named pipe, driven the
//! way the app will drive it: read the hint, connect (the server is identified from the
//! connected pipe), Status, an authorized Stop. The "runtime" is a tiny rustc-built
//! fixture started with the production argv; nothing here starts Syncthing or touches
//! personal state. This test process plays the supervisor's process (its image, owner and
//! start time are what the client verifies).
#![cfg(windows)]

use anyhow::Result;
use okilum_sync_controller::sidecar::{
    authority::Envelope,
    store::{Selection, WindowsStore},
    supervisor::ipc::{
        exchange,
        windows_discovery::{own_image_path, ImagePolicy},
        windows_transport::WindowsTransport,
        Command, Request, Scope, Status,
    },
    windows::{private::PrivateDirectory, security::current_sid},
    Binding,
};
use okilum_sync_supervisor::{
    serve::{self, Exit, Settings},
    startup::{self, Startup},
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command as Process,
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;

// The root starts one child and waits for it; marker files in the data directory pick
// the variant. The child ends when `release-child` appears.
const FIXTURE: &str = r#"
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("child") {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(120);
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

struct Trust;
impl ImagePolicy for Trust {
    fn verify_image(&self, _: &Path) -> Result<()> {
        Ok(())
    }
}
fn soon() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn alive(pid: u32) -> bool {
    let out = Process::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
}
fn settings() -> Settings {
    Settings {
        exchange: Duration::from_secs(8),
        stop: Duration::from_secs(10),
        tick: Duration::from_millis(400),
        store: Duration::from_secs(5),
    }
}

struct World {
    _root: tempfile::TempDir,
    state: PathBuf,
    own: PathBuf,
    store: WindowsStore,
    binding: Binding,
}
impl World {
    fn new(markers: &[&str]) -> Self {
        Self::with_supervisor(markers, PathBuf::from(own_image_path().unwrap()))
    }
    /// `own` is the executable the journal names as the supervisor.
    fn with_supervisor(markers: &[&str], own: PathBuf) -> Self {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        // The protected owner-only directory, prepared like the product does; the
        // preparation handle must go before the store takes its exclusive lock.
        drop(PrivateDirectory::prepare(state.to_str().unwrap()).unwrap());
        for name in ["config", "data"] {
            fs::create_dir(state.join(name)).unwrap();
        }
        for marker in markers {
            fs::write(state.join("data").join(marker), b"").unwrap();
        }
        let staging = state.join("runtime").join("v1");
        fs::create_dir_all(&staging).unwrap();
        let source = staging.join("fixture.rs");
        let executable = staging.join("syncthing.exe");
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
            owner: current_sid().unwrap(),
            supervisor: own.to_string_lossy().into_owned(),
            state_directory: state.to_string_lossy().into_owned(),
            device_identity: "existing-device".into(),
        };
        let store = WindowsStore::open_existing(state.to_str().unwrap()).unwrap();
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
            .and_then(|text| text.trim().parse().ok())
    }
}

fn start(world: &World) -> (std::thread::JoinHandle<anyhow::Result<Exit>>, Scope) {
    let Startup::Run(prepared) = world.prepare().unwrap() else {
        panic!("expected a runnable start")
    };
    let (state, store) = (world.state.clone(), world.store.clone());
    let handle = std::thread::spawn(move || {
        serve::run(&state, store, *prepared, &settings(), Arc::new(Trust))
    });
    let hint = loop {
        if let Ok(Some(hint)) = world.store.read_hint(soon()) {
            break hint;
        }
        assert!(
            !handle.is_finished(),
            "the supervisor ended before it published a hint"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let scope = Scope {
        installation: world.binding.installation,
        instance: world.binding.instance,
        generation: hint.generation(),
    };
    (handle, scope)
}
/// One connect attempt per iteration: between connections the supervisor recreates its
/// single pipe instance, so a client retries briefly (the app's glue does the same).
fn call(world: &World, scope: &Scope, command: Command) -> Result<Status> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last = None;
    while Instant::now() < deadline {
        let hint = world
            .store
            .read_hint(soon())?
            .ok_or_else(|| anyhow::anyhow!("no live supervisor"))?;
        match WindowsTransport::connect_discovering(
            world.binding.clone(),
            scope.clone(),
            hint,
            Arc::new(Trust),
            Instant::now() + Duration::from_secs(10),
        ) {
            Ok(mut transport) => {
                return exchange(
                    &mut transport,
                    &world.binding,
                    &Request::new(scope.clone(), command),
                )
            }
            Err(error) => last = Some(error),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no attempt")))
}
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
}

#[test]
fn a_bad_stop_costs_only_that_exchange() {
    let world = World::new(&[]);
    let (handle, scope) = start(&world);
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
fn a_tree_that_ends_by_itself_ends_the_supervisor_non_zero_with_the_hint_cleared() {
    let world = World::new(&["exit-parent"]);
    let (handle, _scope) = start(&world);
    wait_until("the runtime's descendant", || world.child_pid().is_some());
    let child = world.child_pid().unwrap();
    // The root already exited; the tree is still running while its descendant lives.
    assert!(alive(child));
    std::thread::sleep(Duration::from_millis(800));
    assert!(
        !handle.is_finished(),
        "a live descendant is still the runtime"
    );
    // Now the descendant ends too. Its exit was never captured (capture happens inside a
    // Stop), and the Job Object accounting refuses to certify such a tree as gone, so the
    // supervisor fails closed instead of reporting a clean exit: non-zero, hint cleared,
    // job closed (which kills anything left), and the OS restart policy takes over.
    fs::write(world.state.join("data/release-child"), b"").unwrap();
    let error = handle.join().unwrap().unwrap_err();
    assert!(format!("{error:#}").contains("uncaptured"), "{error:#}");
    assert_eq!(world.store.read_hint(soon()).unwrap(), None);
    wait_until("the runtime tree to be gone", || !alive(child));
}

#[test]
fn a_selection_changed_after_prepare_never_reaches_the_spawn() {
    let world = World::new(&[]);
    let Startup::Run(prepared) = world.prepare().unwrap() else {
        panic!("expected a runnable start")
    };
    fs::write(world.state.join("runtime/v1/syncthing.exe"), b"MZ tampered").unwrap();
    let error = serve::run(
        &world.state,
        world.store.clone(),
        *prepared,
        &settings(),
        Arc::new(Trust),
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

    let wrong_exe = Path::new(r"C:\Windows\System32\cmd.exe");
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

    let tampered = World::new(&[]);
    fs::write(tampered.state.join("runtime/v1/syncthing.exe"), b"not it").unwrap();
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
    let root = tempfile::tempdir().unwrap();
    let empty = root.path().join("state");
    drop(PrivateDirectory::prepare(empty.to_str().unwrap()).unwrap());
    let store = WindowsStore::open_existing(empty.to_str().unwrap()).unwrap();
    assert!(startup::prepare(&store, &empty, None, &good.own, soon()).is_err());
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

    let refused = run(&["--state", &state]);
    assert_eq!(refused.status.code(), Some(1));
    let message = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(message.contains("no signature policy"), "{message}");
    assert!(
        world.child_pid().is_none(),
        "nothing may start without a policy"
    );
    assert_eq!(world.store.read_hint(soon()).unwrap(), None);

    assert_eq!(run(&["--state", &state, "--evil"]).status.code(), Some(1));
    assert_eq!(run(&["serve"]).status.code(), Some(1));

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

mod glue {
    use super::*;
    use okilum_sync_controller::sidecar::{
        discovery::{discover, discover_detailed, stop, Discovered, WindowsConnector},
        store::Hint,
    };

    struct Distrust;
    impl ImagePolicy for Distrust {
        fn verify_image(&self, _: &Path) -> Result<()> {
            anyhow::bail!("untrusted signer")
        }
    }
    const BUDGET: Duration = Duration::from_secs(10);

    #[test]
    fn a_live_supervisor_is_found_and_stopped_through_the_glue() {
        let world = World::new(&[]);
        let connector = WindowsConnector::new(Arc::new(Trust));
        assert_eq!(
            discover_detailed(&world.store, &world.binding, &connector, BUDGET),
            Discovered::Absent
        );
        let (handle, scope) = start(&world);
        // Discovering twice also proves the supervisor re-creates its pipe in between.
        for _ in 0..2 {
            assert_eq!(
                discover(&world.store, &world.binding, &connector, BUDGET),
                Some(scope.clone())
            );
        }
        let Command::Stop(token) = arm_stop(&world, &scope) else {
            unreachable!()
        };
        stop(&world.store, &world.binding, &connector, &token, BUDGET).unwrap();
        assert_eq!(handle.join().unwrap().unwrap(), Exit::Stopped);
        assert_eq!(
            discover_detailed(&world.store, &world.binding, &connector, BUDGET),
            Discovered::Absent
        );
    }

    #[test]
    fn a_stale_hint_is_unreachable_and_an_untrusted_server_is_never_sent_a_stop() {
        let world = World::new(&[]);
        let trusted = WindowsConnector::new(Arc::new(Trust));
        world
            .store
            .publish_hint(soon(), &Hint::new(Uuid::new_v4(), 7).unwrap())
            .unwrap();
        let found = discover_detailed(
            &world.store,
            &world.binding,
            &trusted,
            Duration::from_secs(3),
        );
        assert!(matches!(found, Discovered::Unreachable(_)), "{found:?}");
        world.store.clear_hint(soon()).unwrap();

        let (handle, scope) = start(&world);
        let distrust = WindowsConnector::new(Arc::new(Distrust));
        let found = discover_detailed(
            &world.store,
            &world.binding,
            &distrust,
            Duration::from_secs(4),
        );
        assert!(matches!(found, Discovered::Unreachable(_)), "{found:?}");
        let Command::Stop(token) = arm_stop(&world, &scope) else {
            unreachable!()
        };
        assert!(stop(
            &world.store,
            &world.binding,
            &distrust,
            &token,
            Duration::from_secs(4)
        )
        .is_err());
        assert!(
            !handle.is_finished(),
            "a refused stop must not stop anything"
        );
        // Positive control: the trusted connector finds it and stops it with that token.
        assert_eq!(
            discover(&world.store, &world.binding, &trusted, BUDGET),
            Some(scope)
        );
        stop(&world.store, &world.binding, &trusted, &token, BUDGET).unwrap();
        assert_eq!(handle.join().unwrap().unwrap(), Exit::Stopped);
    }
}
