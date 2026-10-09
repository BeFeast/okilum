use super::*;
use crate::sidecar::{
    authority::{Reason, StopOperation, StopToken},
    supervisor::ipc::{exchange, Command, OwnedRuntime, Request, Server, Status},
};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use uuid::Uuid;

fn binding() -> Binding {
    Binding {
        instance: Uuid::from_u128(2),
        installation: Uuid::from_u128(1),
        owner: rustix::process::geteuid().as_raw().to_string(),
        supervisor: "/Applications/Tessera.app/Contents/MacOS/supervisor".into(),
        state_directory: "unused".into(),
        device_identity: "existing-device".into(),
    }
}
fn scope() -> Scope {
    Scope {
        installation: Uuid::from_u128(1),
        instance: Uuid::from_u128(2),
        generation: Uuid::new_v4(),
    }
}
fn soon(ms: u64) -> Instant {
    Instant::now() + Duration::from_millis(ms)
}
fn private() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}
struct Runtime;
impl OwnedRuntime for Runtime {
    type Lease = ();
    fn status(&mut self) -> Result<Status> {
        Ok(Status::Running)
    }
    fn authorize_stop(&mut self, _: &Binding, _: &Scope, _: &StopToken) -> Result<()> {
        Ok(())
    }
    fn stop_owned(&mut self, _: &()) -> Result<Status> {
        Ok(Status::Stopped)
    }
}
fn token(scope: &Scope) -> StopToken {
    StopToken {
        journal_epoch: Uuid::new_v4(),
        operation: StopOperation {
            operation_id: Uuid::new_v4(),
            authorized_revision: 2,
            scope: scope.clone(),
            reason: Reason::Disable,
        },
    }
}
struct Deny;
impl PeerCheck for Deny {
    fn verify(&self, _: &UnixStream, _: PeerEnd) -> Result<()> {
        anyhow::bail!("untrusted peer")
    }
}
struct ExpectUid(u32);
impl PeerCheck for ExpectUid {
    fn verify(&self, stream: &UnixStream, _: PeerEnd) -> Result<()> {
        ensure!(peer_uid(stream)? == self.0, "unexpected peer uid");
        Ok(())
    }
}

#[test]
fn the_socket_is_private_and_a_collision_is_never_adopted_or_unlinked() {
    let dir = private();
    let s = scope();
    let endpoint = UnixEndpoint::create(dir.path(), &s).unwrap();
    let meta = std::fs::symlink_metadata(endpoint.path()).unwrap();
    assert!(meta.file_type().is_socket());
    assert_eq!(meta.mode() & 0o777, 0o600);
    assert!(UnixEndpoint::create(dir.path(), &s).is_err(), "collision");
    // The first endpoint is untouched: a client still reaches it.
    let client = connect(dir.path(), &s).unwrap();
    let served = endpoint.accept(soon(1000)).unwrap();
    assert!(peer_uid(&served).is_ok() && peer_uid(&client).is_ok());
    drop((client, served));
    let path = endpoint.path().to_path_buf();
    drop(endpoint);
    assert!(!path.exists(), "dropping removes only what was created");
    UnixEndpoint::create(dir.path(), &s).unwrap(); // positive control after release
}

#[test]
fn unsafe_directories_overlong_paths_and_nil_scopes_are_refused() {
    let s = scope();
    let dir = private();
    UnixEndpoint::create(dir.path(), &s).unwrap(); // control
    let shared = tempfile::tempdir().unwrap();
    std::fs::set_permissions(shared.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(UnixEndpoint::create(shared.path(), &scope()).is_err());
    assert!(connect(shared.path(), &scope()).is_err());
    let link = tempfile::tempdir().unwrap();
    let alias = link.path().join("alias");
    std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
    assert!(
        UnixEndpoint::create(&alias, &scope()).is_err(),
        "symlinked directory"
    );
    let mut deep = dir.path().to_path_buf();
    while deep.as_os_str().len() < 120 {
        deep.push("nested-directory");
        std::fs::create_dir(&deep).unwrap();
        std::fs::set_permissions(&deep, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let error = UnixEndpoint::create(&deep, &scope())
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("too long"), "{error}");
    let mut nil = scope();
    nil.generation = Uuid::nil();
    assert!(UnixEndpoint::create(dir.path(), &nil).is_err());
    assert!(connect(dir.path(), &nil).is_err());
}

#[test]
fn connect_never_finds_another_generation_or_a_missing_endpoint() {
    let dir = private();
    let s = scope();
    let _endpoint = UnixEndpoint::create(dir.path(), &s).unwrap();
    assert!(connect(dir.path(), &scope()).is_err(), "other generation");
    assert!(connect(dir.path(), &s).is_ok(), "positive control");
    let empty = private();
    assert!(connect(empty.path(), &s).is_err(), "missing endpoint");
}

#[test]
fn status_and_stop_work_end_to_end_over_the_verified_transport() {
    let dir = private();
    let b = binding();
    let mut server = Server::new(b.clone(), Runtime);
    let s = server.scope().clone();
    let endpoint = UnixEndpoint::create(dir.path(), &s).unwrap();
    let deadline = soon(5000);
    let (sb, ss) = (b.clone(), s.clone());
    let worker = std::thread::spawn(move || -> Result<()> {
        for _ in 0..2 {
            let stream = endpoint.accept(deadline)?;
            let mut transport = UnixTransport::new(
                stream,
                PeerEnd::Client,
                sb.clone(),
                ss.clone(),
                deadline,
                SameUser,
            )?;
            server.serve_one(&mut transport)?;
        }
        Ok(())
    });
    for command in [Command::Status, Command::Stop(token(&s))] {
        let stream = connect(dir.path(), &s).unwrap();
        let mut transport = UnixTransport::new(
            stream,
            PeerEnd::Server,
            b.clone(),
            s.clone(),
            deadline,
            SameUser,
        )
        .unwrap();
        let expected = match command {
            Command::Status => Status::Running,
            Command::Stop(_) => Status::Stopped,
        };
        let request = Request::new(s.clone(), command);
        assert_eq!(exchange(&mut transport, &b, &request).unwrap(), expected);
    }
    worker.join().unwrap().unwrap();
}

#[test]
fn nothing_is_exchanged_before_the_peer_passes_and_a_failed_check_poisons() {
    let dir = private();
    let s = scope();
    let endpoint = UnixEndpoint::create(dir.path(), &s).unwrap();
    let (b, deadline) = (binding(), soon(2000));
    let mut raw = connect(dir.path(), &s).unwrap();
    let served = endpoint.accept(deadline).unwrap();
    let new = |stream, check: Box<dyn PeerCheck>| {
        UnixTransport::new(
            stream,
            PeerEnd::Server,
            b.clone(),
            s.clone(),
            deadline,
            check,
        )
        .unwrap()
    };

    // Unverified: even a write that would succeed on the raw socket is refused, and
    // the peer sees no byte.
    let mut unverified = new(served.try_clone().unwrap(), Box::new(SameUser));
    assert_eq!(
        unverified.write(b"x").unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    raw.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let mut byte = [0u8; 1];
    assert!(raw.read(&mut byte).is_err(), "nothing was sent");

    // A failing check leaves the transport unusable, even for a later good check.
    let mut denied = new(served.try_clone().unwrap(), Box::new(Deny));
    assert!(denied.verify_peer(&b, &s).is_err());
    assert!(denied.write(b"x").is_err());
    assert!(denied.verify_peer(&b, &s).is_err(), "poisoned for good");

    // Wrong uid expectation fails; the same stream passes SameUser (positive control).
    let mut other = new(
        served.try_clone().unwrap(),
        Box::new(ExpectUid(rustix::process::geteuid().as_raw() + 1)),
    );
    assert!(other.verify_peer(&b, &s).is_err());
    let mut good = new(served, Box::new(SameUser));
    good.verify_peer(&b, &s).unwrap();
    good.write_all(b"y").unwrap();
    raw.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    assert_eq!(raw.read(&mut byte).unwrap(), 1);
    assert_eq!(byte[0], b'y');

    // A changed binding or scope is refused before the check runs.
    let mut changed = binding();
    changed.device_identity = "replacement".into();
    assert!(good.verify_peer(&changed, &s).is_err());
}

#[test]
fn a_silent_peer_cannot_outlive_the_absolute_deadline() {
    let dir = private();
    let b = binding();
    let s = scope();
    let endpoint = UnixEndpoint::create(dir.path(), &s).unwrap();
    let stream = connect(dir.path(), &s).unwrap();
    let _served = endpoint.accept(soon(1000)).unwrap(); // accepted, never answers
    let mut transport = UnixTransport::new(
        stream,
        PeerEnd::Server,
        b.clone(),
        s.clone(),
        soon(250),
        SameUser,
    )
    .unwrap();
    let started = Instant::now();
    let request = Request::new(s.clone(), Command::Status);
    assert!(exchange(&mut transport, &b, &request).is_err());
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "deadline not enforced"
    );
    assert!(transport.write(b"x").is_err(), "poisoned after the timeout");
}
