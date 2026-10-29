use super::*;
use crate::sidecar::{
    authority::Envelope,
    macos::{
        bundle::{SignatureVerifier, HELPER},
        bundled_plist, PLIST,
    },
    store::{Hint, UnixDir, UnixStore},
    supervisor::ipc::{
        self, code_requirement::CodeRequirement, Command, Request, Response, Scope, Status,
        Transport,
    },
};
use std::{
    cell::Cell,
    fs,
    io::{self, Cursor, Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    rc::Rc,
};
use uuid::Uuid;

struct Signature;
impl SignatureVerifier for Signature {
    fn verify(&self, _: &Path, _: &CodeRequirement) -> Result<()> {
        Ok(())
    }
}
#[derive(Clone)]
struct Script {
    calls: Rc<Cell<usize>>,
    status: Rc<Cell<Status>>,
    refuse: Rc<Cell<bool>>,
}
impl Default for Script {
    fn default() -> Self {
        Self {
            calls: Rc::new(Cell::new(0)),
            status: Rc::new(Cell::new(Status::Running)),
            refuse: Rc::new(Cell::new(false)),
        }
    }
}
struct Wire {
    script: Script,
    input: Vec<u8>,
    output: Option<Cursor<Vec<u8>>>,
}
impl Connector for Script {
    type Transport = Wire;
    fn connect(&self, _: &Binding, _: &Hint, _: &Scope, _: Instant) -> Result<Wire> {
        self.calls.set(self.calls.get() + 1);
        Ok(Wire {
            script: self.clone(),
            input: vec![],
            output: None,
        })
    }
}
impl Transport for Wire {
    fn verify_peer(&mut self, _: &Binding, _: &Scope) -> Result<()> {
        ensure!(
            !self.script.refuse.get(),
            "injected peer authentication failure"
        );
        Ok(())
    }
}
impl Write for Wire {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.input.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Read for Wire {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self.output.is_none() {
            let request: Request =
                ipc::read_frame(&mut Cursor::new(&self.input)).map_err(io::Error::other)?;
            let token = match request.command {
                Command::Stop(token) => Some(token),
                Command::Status => None,
            };
            let response = Response {
                version: ipc::VERSION,
                scope: request.scope,
                id: request.id,
                status: self.script.status.get(),
                token,
            };
            let mut encoded = vec![];
            ipc::write_frame(&mut encoded, &response).map_err(io::Error::other)?;
            self.output = Some(Cursor::new(encoded));
        }
        self.output.as_mut().unwrap().read(bytes)
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    app: PathBuf,
    binding: Binding,
    store: UnixStore,
}
impl Fixture {
    fn new() -> Result<Self> {
        let root = tempfile::tempdir_in("/tmp")?;
        let base = root.path().canonicalize()?;
        let app = base.join("Fixture.app");
        for relative in ["Contents/MacOS", "Contents/Library/LaunchAgents"] {
            fs::create_dir_all(app.join(relative))?;
        }
        for relative in ["Contents/MacOS/reader", HELPER] {
            fs::write(app.join(relative), b"not executable test code")?;
            fs::set_permissions(app.join(relative), fs::Permissions::from_mode(0o755))?;
        }
        fs::write(
            app.join("Contents/Library/LaunchAgents").join(PLIST),
            bundled_plist(HELPER)?,
        )?;
        let state = base.join("state");
        fs::create_dir(&state)?;
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
        let binding = Binding {
            installation: Uuid::new_v4(),
            instance: Uuid::new_v4(),
            owner: rustix::process::geteuid().as_raw().to_string(),
            supervisor: app.join(HELPER).to_str().unwrap().to_owned(),
            state_directory: state.to_str().unwrap().to_owned(),
            device_identity: "existing-fixture-device".into(),
        };
        let store = UnixStore::open_existing(&state)?;
        Ok(Self {
            _root: root,
            app,
            binding,
            store,
        })
    }
    fn prepare(&self) -> Result<()> {
        self.store
            .begin(soon())?
            .commit(Envelope::first(self.binding.clone()))
    }
    fn connection(&self, script: Script) -> Result<Connection<Signature, UnixDir, Script>> {
        let requirement =
            CodeRequirement::team_and_identifier("ABCDE12345", "org.example.fixture")?;
        let bundle = BundleGuard::new(
            &self.app,
            "reader",
            self.binding.clone(),
            requirement.clone(),
            requirement,
            Signature,
        )?;
        Connection::new(
            self.binding.clone(),
            bundle,
            self.store.clone(),
            script,
            Duration::from_secs(1),
        )
    }
    fn hint(&self) -> Result<Scope> {
        let scope = Scope {
            installation: self.binding.installation,
            instance: self.binding.instance,
            generation: Uuid::new_v4(),
        };
        self.store
            .publish_hint(soon(), &Hint::new(scope.generation, 1)?)?;
        Ok(scope)
    }
}
fn soon() -> Instant {
    Instant::now() + Duration::from_secs(1)
}

#[test]
fn only_prepared_matching_state_opens_and_missing_hint_is_distinct_from_unreachable() -> Result<()>
{
    let fixture = Fixture::new()?;
    let script = Script::default();
    assert!(fixture.connection(script.clone()).is_err());
    assert_eq!(script.calls.get(), 0);
    fixture.prepare()?;
    let connection = fixture.connection(script.clone())?;
    assert_eq!(connection.inspect()?, Discovered::Absent);
    assert_eq!(script.calls.get(), 0);
    let scope = fixture.hint()?;
    // Status inspection must work while the lifecycle holds its transaction.
    let transaction = fixture.store.begin(soon())?;
    assert_eq!(connection.inspect()?, Discovered::Live(scope));
    drop(transaction);
    script.refuse.set(true);
    assert!(matches!(connection.inspect()?, Discovered::Unreachable(_)));
    script.refuse.set(false);
    script.status.set(Status::Stopping);
    assert!(matches!(connection.inspect()?, Discovered::Unreachable(_)));
    assert_eq!(script.calls.get(), 3);
    Ok(())
}

#[test]
fn bundle_changes_are_refused_before_any_ipc() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.prepare()?;
    let script = Script::default();
    let connection = fixture.connection(script.clone())?;
    fixture.hint()?;
    let plist = fixture
        .app
        .join("Contents/Library/LaunchAgents")
        .join(PLIST);
    fs::write(&plist, b"changed definition")?;
    assert!(connection.inspect().is_err());
    assert_eq!(script.calls.get(), 0);
    fs::write(plist, bundled_plist(HELPER)?)?;
    assert!(matches!(connection.inspect()?, Discovered::Live(_)));
    Ok(())
}

#[test]
fn stop_needs_current_durable_authority_and_exact_generation_and_complete_reply() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.prepare()?;
    let script = Script::default();
    let connection = fixture.connection(script.clone())?;
    let scope = fixture.hint()?;
    assert!(matches!(
        connection
            .link
            .detailed_until(&fixture.binding, Instant::now()),
        Discovered::Unreachable(_)
    ));
    assert_eq!(
        script.calls.get(),
        0,
        "expired deadline reached the connector"
    );
    let token = {
        let mut tx = fixture.store.begin(soon())?;
        let (next, token) = tx.current()?.unwrap().disable(scope)?;
        tx.commit(next)?;
        token
    };
    script.status.set(Status::Stopping);
    assert!(connection.request_stop(&token).is_err());
    script.status.set(Status::Stopped);
    connection.request_stop(&token)?;
    assert_eq!(script.calls.get(), 2);
    fixture.hint()?; // Another supervisor generation cannot receive the old token.
    assert!(connection.request_stop(&token).is_err());
    assert_eq!(script.calls.get(), 2);
    {
        let mut tx = fixture.store.begin(soon())?;
        let next = tx.current()?.unwrap().enable()?;
        tx.commit(next)?;
    }
    assert!(connection.request_stop(&token).is_err());
    assert_eq!(script.calls.get(), 2, "stale authority reached IPC");
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn native_open_checks_real_signatures_and_existing_state_without_starting_a_helper() -> Result<()> {
    use super::NativeConnection;
    let fixture = Fixture::new()?;
    let (app, helper) = sign_fixture(&fixture)?;
    fixture.prepare()?;
    let connection = NativeConnection::open(
        &fixture.app,
        "reader",
        fixture.binding.clone(),
        app.clone(),
        helper.clone(),
        Duration::from_secs(1),
    )?;
    assert_eq!(connection.inspect()?, Discovered::Absent);
    let supervisor = super::NativeSupervisor::open(
        &fixture.app,
        "reader",
        fixture.binding.clone(),
        app.clone(),
        helper.clone(),
        Duration::from_secs(1),
    )?;
    assert_eq!(supervisor.inspect()?, Discovered::Absent);
    assert!(fixture.store.peek_hint()?.is_none());
    let wrong = CodeRequirement::team_and_identifier("ABCDE12345", "org.example.fixture.sync")?;
    assert!(NativeConnection::open(
        &fixture.app,
        "reader",
        fixture.binding.clone(),
        app,
        wrong,
        Duration::from_secs(1)
    )
    .is_err());
    // No registration API or process launcher is reachable from this connection.
    Ok(())
}

#[test]
fn stop_completion_requires_both_receipt_and_exit_and_rechecks_authority_after_wait() -> Result<()>
{
    let fixture = Fixture::new()?;
    fixture.prepare()?;
    let script = Script::default();
    let connection = fixture.connection(script.clone())?;
    let scope = fixture.hint()?;
    let token = {
        let mut tx = fixture.store.begin(soon())?;
        let (next, token) = tx.current()?.unwrap().disable(scope)?;
        tx.commit(next)?;
        token
    };
    let confirmed = Cell::new(0);
    script.status.set(Status::Stopping);
    assert!(connection
        .stop_and_confirm(&token, soon(), |_| {
            confirmed.set(confirmed.get() + 1);
            Ok(true)
        })
        .is_err());
    assert_eq!(
        confirmed.get(),
        0,
        "incomplete Stop reached exit completion"
    );
    script.status.set(Status::Stopped);
    assert!(connection
        .stop_and_confirm(&token, soon(), |_| Ok(false))
        .is_err());
    let deadline = soon();
    connection.stop_and_confirm(&token, deadline, |received| {
        assert_eq!(received, deadline, "exit phase renewed the deadline");
        // No journal lock may be held while awaiting process exit.
        let transaction = fixture.store.begin(deadline)?;
        assert!(transaction.current()?.is_some());
        Ok(true)
    })?;
    assert!(
        connection
            .stop_and_confirm(&token, soon(), |_| {
                let mut tx = fixture.store.begin(soon())?;
                let next = tx.current()?.unwrap().enable()?;
                tx.commit(next)?;
                Ok(true)
            })
            .is_err(),
        "concurrent Enable was overwritten by old stop/exit evidence"
    );
    let calls = script.calls.get();
    assert!(connection
        .stop_and_confirm(&token, Instant::now(), |_| {
            panic!("expired stop reached exit observation")
        })
        .is_err());
    assert_eq!(script.calls.get(), calls);
    Ok(())
}

#[cfg(target_os = "macos")]
fn sign_fixture(fixture: &Fixture) -> Result<(CodeRequirement, CodeRequirement)> {
    use std::process::Command;
    for relative in ["Contents/MacOS/reader", HELPER] {
        fs::copy(std::env::current_exe()?, fixture.app.join(relative))?;
    }
    fs::write(
        fixture.app.join("Contents/Info.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>reader</string><key>CFBundleIdentifier</key><string>org.example.fixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>"#,
    )?;
    for (path, identifier) in [
        (fixture.app.join(HELPER), "org.example.fixture.sync"),
        (fixture.app.clone(), "org.example.fixture"),
    ] {
        let output = Command::new("codesign")
            .args(["--force", "--sign", "-", "--identifier", identifier])
            .arg(path)
            .output()?;
        ensure!(output.status.success(), "fixture signing failed");
    }
    let requirement = |path: &Path| -> Result<CodeRequirement> {
        let output = Command::new("codesign").arg("-dvvv").arg(path).output()?;
        ensure!(
            output.status.success(),
            "fixture signature inspection failed"
        );
        let text = String::from_utf8(output.stderr)?;
        let hash = text
            .lines()
            .find_map(|line| line.strip_prefix("CDHash="))
            .context("fixture has no code directory hash")?;
        CodeRequirement::code_directory_hash(hash.trim())
    };
    Ok((
        requirement(&fixture.app)?,
        requirement(&fixture.app.join(HELPER))?,
    ))
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "owned subprocess fixture for the native Stop/exit composition test"]
fn native_stop_peer_fixture() -> Result<()> {
    use crate::sidecar::supervisor::{
        ipc::{
            unix_transport::{PeerEnd, SameUser, UnixEndpoint, UnixTransport},
            Server,
        },
        runtime::{OwnedTree, StoreRuntime},
    };
    // Only tree-stop acknowledgement is injected here. The client authenticates
    // a real signed helper process, sends the durable token and observes its exit.
    struct Tree;
    impl OwnedTree for Tree {
        fn status(&mut self) -> Result<Status> {
            Ok(Status::Running)
        }
        fn stop(&mut self) -> Result<Status> {
            Ok(Status::Stopped)
        }
    }
    let state =
        PathBuf::from(std::env::var_os("OKILUM_STOP_FIXTURE").context("fixture state missing")?);
    let generation = std::env::var("OKILUM_STOP_GENERATION")?.parse()?;
    let store = UnixStore::open_existing(&state)?;
    let binding = store
        .begin(soon())?
        .current()?
        .context("fixture journal missing")?
        .binding()
        .clone();
    let scope = Scope {
        installation: binding.installation,
        instance: binding.instance,
        generation,
    };
    let endpoint = UnixEndpoint::create(&state, &scope)?;
    store.publish_hint(
        soon(),
        &Hint::new(
            generation,
            crate::sidecar::supervisor::process_group::process_start_time(std::process::id())?,
        )?,
    )?;
    let mut server = Server::with_generation(
        binding.clone(),
        StoreRuntime::new(store.clone(), Tree),
        generation,
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !server.has_stopped() {
        let stream = endpoint
            .try_accept(deadline)?
            .context("fixture accept deadline elapsed")?;
        let mut transport = UnixTransport::new(
            stream,
            PeerEnd::Client,
            binding.clone(),
            scope.clone(),
            deadline,
            SameUser,
        )?;
        server.runtime_mut().begin_exchange(deadline);
        server.serve_one(&mut transport)?;
    }
    drop(endpoint);
    store.clear_hint(soon())?;
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn native_stop_authenticates_one_socket_and_confirms_the_same_helper_exited() -> Result<()> {
    use std::process::{Child, Command, Stdio};
    struct ChildGuard(Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let fixture = Fixture::new()?;
    let (app, helper) = sign_fixture(&fixture)?;
    fixture.prepare()?;
    let generation = Uuid::new_v4();
    let mut child = ChildGuard(
        Command::new(fixture.app.join(HELPER))
            .args([
                "--ignored",
                "--exact",
                "sidecar::macos::connection::tests::native_stop_peer_fixture",
                "--test-threads=1",
            ])
            .env_clear()
            .env("OKILUM_STOP_FIXTURE", &fixture.binding.state_directory)
            .env("OKILUM_STOP_GENERATION", generation.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let ready = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(hint) = fixture.store.peek_hint()? {
            ensure!(
                hint.generation() == generation,
                "unexpected fixture generation"
            );
            break;
        }
        ensure!(
            child.0.try_wait()?.is_none(),
            "fixture exited before publishing its endpoint"
        );
        ensure!(Instant::now() < ready, "fixture setup deadline elapsed");
        std::thread::sleep(Duration::from_millis(5));
    }
    let supervisor = super::NativeSupervisor::open(
        &fixture.app,
        "reader",
        fixture.binding.clone(),
        app,
        helper,
        Duration::from_secs(2),
    )?;
    let scope = Scope {
        installation: fixture.binding.installation,
        instance: fixture.binding.instance,
        generation,
    };
    assert_eq!(supervisor.inspect()?, Discovered::Live(scope.clone()));
    assert!(
        child.0.try_wait()?.is_none(),
        "live positive control was already gone"
    );
    let token = {
        let mut tx = fixture.store.begin(soon())?;
        let (next, token) = tx.current()?.unwrap().disable(scope)?;
        tx.commit(next)?;
        token
    };
    supervisor.stop_and_wait(&token)?;
    assert!(child.0.wait()?.success()); // ExitWatch already confirmed this process exited.
    assert!(fixture.store.peek_hint()?.is_none());
    fixture
        .store
        .begin(soon())?
        .current()?
        .unwrap()
        .authorize(&token, &token.operation.scope)?;
    Ok(())
}

#[test]
fn managed_status_requires_durable_claim_and_authenticated_answer_and_survives_damage() -> Result<()>
{
    use crate::sidecar::macos::recovery::ManagedStatus;
    let fixture = Fixture::new()?;
    fixture
        .store
        .begin(soon())?
        .commit(Envelope::first_with_launch(fixture.binding.clone()))?;
    let script = Script::default();
    let connection = fixture.connection(script.clone())?;
    assert_eq!(connection.managed_status(), ManagedStatus::RecoveryPending);
    fixture
        .store
        .dispatch_launch(&fixture.binding, soon())?
        .execute(|_| Ok(()))?;
    assert_eq!(connection.managed_status(), ManagedStatus::RecoveryPending);
    let generation = fixture.store.claim_launch(&fixture.binding, soon())?;
    // A registration/claim alone is still not a live helper.
    assert_eq!(connection.managed_status(), ManagedStatus::RecoveryPending);
    fixture
        .store
        .publish_hint(soon(), &Hint::new(generation, 1)?)?;
    assert_eq!(connection.managed_status(), ManagedStatus::Running); // positive control
    script.refuse.set(true);
    assert_eq!(connection.managed_status(), ManagedStatus::RecoveryPending);
    script.refuse.set(false);
    script.status.set(Status::Stopped);
    assert_eq!(connection.managed_status(), ManagedStatus::RecoveryPending);
    script.status.set(Status::Running);
    fixture.hint()?; // genuine answer, but for a different generation
    assert_eq!(connection.managed_status(), ManagedStatus::RecoveryPending);
    fixture.store.clear_hint(soon())?;
    assert_eq!(connection.managed_status(), ManagedStatus::RecoveryPending);
    let journal = Path::new(&fixture.binding.state_directory).join("sidecar.json");
    fs::write(&journal, b"{")?;
    assert_eq!(connection.managed_status(), ManagedStatus::RecoveryPending);
    fs::remove_file(journal)?;
    assert_eq!(connection.managed_status(), ManagedStatus::RecoveryPending);
    Ok(())
}
