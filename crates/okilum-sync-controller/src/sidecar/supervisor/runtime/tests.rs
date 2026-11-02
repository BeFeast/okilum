use super::*;
use crate::sidecar::{
    authority::{Envelope, Reason, Stored},
    store::{UnixDir, UnixStore},
    supervisor::ipc::{read_frame, write_frame, Command, Request, Response, Server, Transport},
    Intent, Journal,
};
use std::{
    cell::RefCell,
    fs,
    io::{self, Cursor, Read, Write},
    os::unix::fs::PermissionsExt,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use uuid::Uuid;

type Log = Rc<RefCell<Vec<&'static str>>>;

fn binding() -> Binding {
    Binding {
        instance: Uuid::from_u128(2),
        installation: Uuid::from_u128(1),
        owner: "501".into(),
        supervisor: "/private/supervisor".into(),
        state_directory: "/private/state".into(),
        device_identity: "existing-device".into(),
    }
}
fn far() -> Instant {
    Instant::now() + Duration::from_secs(5)
}
fn soon() -> Instant {
    Instant::now() + Duration::from_millis(30)
}
struct Tree {
    stops: usize,
    status: Status,
    next: Status,
    fail_stop: bool,
    /// Runs inside the owned-tree stop, i.e. while the lease must be held.
    during_stop: Option<Box<dyn FnMut()>>,
}
impl OwnedTree for Tree {
    fn status(&mut self) -> Result<Status> {
        Ok(self.status)
    }
    fn stop(&mut self) -> Result<Status> {
        self.stops += 1;
        if let Some(probe) = &mut self.during_stop {
            probe();
        }
        ensure!(!self.fail_stop, "stop failed after partial effect");
        Ok(self.next)
    }
}
fn tree() -> Tree {
    Tree {
        stops: 0,
        status: Status::Running,
        next: Status::Stopped,
        fail_stop: false,
        during_stop: None,
    }
}
struct Pipe {
    input: Cursor<Vec<u8>>,
    output: Vec<u8>,
    log: Log,
    on_write: Option<Box<dyn FnMut()>>,
}
impl Pipe {
    fn request(request: &Request, log: &Log) -> Self {
        let mut bytes = vec![];
        write_frame(&mut bytes, request).unwrap();
        Self {
            input: Cursor::new(bytes),
            output: vec![],
            log: log.clone(),
            on_write: None,
        }
    }
}
impl Read for Pipe {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.input.read(b)
    }
}
impl Write for Pipe {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if let Some(probe) = &mut self.on_write {
            probe();
        }
        self.log.borrow_mut().push("write");
        self.output.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Transport for Pipe {
    fn verify_peer(&mut self, _: &Binding, _: &Scope) -> Result<()> {
        Ok(())
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    store: UnixStore,
    server: Server<StoreRuntime<UnixDir, Tree>>,
    log: Log,
}
impl Fixture {
    fn new(tree: Tree) -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let store = UnixStore::open_existing(dir.path()).unwrap();
        let server = Server::new(binding(), StoreRuntime::new(store.clone(), tree));
        Self {
            path: dir.path().to_path_buf(),
            _dir: dir,
            store,
            server,
            log: Log::default(),
        }
    }
    fn scope(&self) -> Scope {
        self.server.scope().clone()
    }
    fn open(&self) -> UnixStore {
        UnixStore::open_existing(&self.path).unwrap()
    }
    fn current(&self) -> Envelope {
        match self.store.begin(far()).unwrap().state().unwrap() {
            Stored::Current(e) => e.clone(),
            other => panic!("{other:?}"),
        }
    }
    fn commit(&self, next: Envelope) {
        self.store.begin(far()).unwrap().commit(next).unwrap();
    }
    /// Enabled -> armed Disable for this supervisor generation.
    fn arm_disable(&self) -> StopToken {
        let (next, token) = self.current().disable(self.scope()).unwrap();
        self.commit(next);
        token
    }
    fn serve(&mut self, token: &StopToken, deadline: Option<Instant>) -> Result<Response> {
        let request = Request::new(self.scope(), Command::Stop(token.clone()));
        let mut pipe = Pipe::request(&request, &self.log);
        self.serve_pipe(&mut pipe, deadline)?;
        Ok(read_frame(&mut Cursor::new(pipe.output)).unwrap())
    }
    fn serve_pipe(&mut self, pipe: &mut Pipe, deadline: Option<Instant>) -> Result<()> {
        if let Some(deadline) = deadline {
            self.server.runtime_mut().begin_exchange(deadline);
        }
        self.server.serve_one(pipe)
    }
    fn stops(&mut self) -> usize {
        self.server.runtime_mut().tree().stops
    }
}
fn started(tree: Tree) -> Fixture {
    let fixture = Fixture::new(tree);
    fixture.commit(Envelope::first(binding()));
    fixture
}

#[test]
fn the_lock_is_held_through_the_stop_and_released_before_the_reply() {
    let mut f = started(tree());
    let (during, at_reply) = (Rc::new(RefCell::new(vec![])), Rc::new(RefCell::new(vec![])));
    let (probe, sink) = (f.open(), during.clone());
    *f.server.runtime_mut().tree_mut() = Tree {
        during_stop: Some(Box::new(move || {
            // Enable/Disable from another controller must wait for the effect.
            sink.borrow_mut().push(probe.begin(soon()).is_err());
        })),
        ..tree()
    };
    let token = f.arm_disable();
    let request = Request::new(f.scope(), Command::Stop(token.clone()));
    let mut pipe = Pipe::request(&request, &f.log);
    let (probe, sink) = (f.open(), at_reply.clone());
    pipe.on_write = Some(Box::new(move || {
        sink.borrow_mut().push(probe.begin(far()).is_ok());
    }));
    f.serve_pipe(&mut pipe, Some(far())).unwrap();
    assert_eq!(
        *during.borrow(),
        [true],
        "lock held during the owned-tree stop"
    );
    assert!(at_reply.borrow()[0], "lock released before the reply");
    let response: Response = read_frame(&mut Cursor::new(pipe.output)).unwrap();
    assert_eq!(response.status, Status::Stopped);
    assert_eq!(response.token, Some(token));
    assert_eq!(f.stops(), 1);
}

#[test]
fn aba_stale_stop_is_denied_with_real_files_even_after_the_same_intent_returns() {
    let mut f = started(tree());
    let a = f.arm_disable(); // revision 2, operation A
                             // Enable commits after the lease would have been released, then Disable again.
    f.commit(f.current().enable().unwrap());
    let c = f.arm_disable(); // revision 4, operation C, same Intent and Binding
    assert_eq!(f.current().intent(), Intent::Disabled);
    assert_ne!(a.operation.operation_id, c.operation.operation_id);
    assert!(f.serve(&a, Some(far())).is_err());
    assert_eq!(f.stops(), 0, "the stale token stopped nothing");
    assert!(f.serve(&c, Some(far())).is_ok()); // positive control
    assert_eq!(f.stops(), 1);
    // Completion consumed C: replaying either token is now denied.
    let done = f.current().complete_stop(&c, &f.scope()).unwrap();
    f.commit(done);
    assert!(f.serve(&c, Some(far())).is_err());
    assert!(f.serve(&a, Some(far())).is_err());
    assert_eq!(f.stops(), 1);
}

#[test]
fn missing_foreign_or_mismatched_authority_denies_before_any_stop() {
    // No journal at all.
    let mut f = Fixture::new(tree());
    let bogus = StopToken {
        journal_epoch: Uuid::new_v4(),
        operation: crate::sidecar::authority::StopOperation {
            operation_id: Uuid::new_v4(),
            authorized_revision: 2,
            scope: f.scope(),
            reason: Reason::Disable,
        },
    };
    assert!(f.serve(&bogus, Some(far())).is_err());
    // A legacy journal cannot authorize a v2 Stop.
    fs::write(
        f.path.join("sidecar.json"),
        serde_json::to_vec(&Journal {
            binding: binding(),
            intent: Intent::Disabled,
        })
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(
        f.path.join("sidecar.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(f.serve(&bogus, Some(far())).is_err());
    fs::remove_file(f.path.join("sidecar.json")).unwrap();

    // Journal for another binding.
    let mut f = Fixture::new(tree());
    let mut other = binding();
    other.device_identity = "other-device".into();
    f.commit(Envelope::first(other.clone()));
    let (next, token) = f.current().disable(f.scope()).unwrap();
    f.commit(next);
    assert!(f.serve(&token, Some(far())).is_err());

    // Stored operation for another generation, epoch repaired, and a good control.
    let mut f = started(tree());
    let token = f.arm_disable();
    let mut wrong_operation = token.clone();
    wrong_operation.operation.operation_id = Uuid::new_v4();
    assert!(f.serve(&wrong_operation, Some(far())).is_err());
    let mut wrong_epoch = token.clone();
    wrong_epoch.journal_epoch = Uuid::new_v4();
    assert!(f.serve(&wrong_epoch, Some(far())).is_err());
    assert_eq!(f.stops(), 0);
    assert!(f.serve(&token, Some(far())).is_ok());
    assert_eq!(f.stops(), 1);

    // The stored operation names a dead supervisor generation. The request's token
    // is rewritten to this generation (it must match the request's own scope), but
    // it no longer equals what the journal holds.
    let mut f = started(tree());
    let dead = Scope {
        generation: Uuid::new_v4(),
        ..f.scope()
    };
    let (next, mut stale) = f.current().disable(dead).unwrap();
    f.commit(next);
    stale.operation.scope = f.scope();
    assert!(f.serve(&stale, Some(far())).is_err());
    assert_eq!(f.stops(), 0);
    // Recovery arms a fresh operation for the live generation, which then works.
    let (next, fresh) = f.current().arm_recovery(f.scope()).unwrap();
    f.commit(next);
    assert!(f.serve(&fresh, Some(far())).is_ok());
    assert_eq!(f.stops(), 1);
}

#[test]
fn no_exchange_deadline_or_lock_contention_denies_within_the_budget() {
    let mut f = started(tree());
    let token = f.arm_disable();
    assert!(f.serve(&token, None).is_err(), "no deadline, no wait");
    // Another holder keeps the lock past the exchange deadline.
    let other = f.open();
    let held = other.begin(far()).unwrap();
    let started_at = Instant::now();
    let error = f.serve(&token, Some(soon())).unwrap_err().to_string();
    assert!(error.contains("busy"), "{error}");
    assert!(started_at.elapsed() < Duration::from_secs(2));
    assert_eq!(f.stops(), 0);
    drop(held);
    // The deadline is per exchange: a fresh one is needed even now.
    assert!(f.serve(&token, None).is_err());
    assert!(f.serve(&token, Some(far())).is_ok());
    assert_eq!(f.stops(), 1);
}

#[test]
fn a_timed_out_stop_reports_stopping_releases_the_lock_and_allows_a_retry() {
    let mut f = started(Tree {
        next: Status::Stopping,
        ..tree()
    });
    let token = f.arm_disable();
    let revision = f.current().revision();
    assert_eq!(
        f.serve(&token, Some(far())).unwrap().status,
        Status::Stopping
    );
    assert!(f.open().begin(far()).is_ok(), "lock released");
    assert_eq!(
        f.current().revision(),
        revision,
        "the server never rewrites the journal"
    );
    f.server.runtime_mut().tree_mut().next = Status::Stopped;
    assert_eq!(
        f.serve(&token, Some(far())).unwrap().status,
        Status::Stopped
    );
    assert_eq!(f.stops(), 2);
}

#[derive(Clone)]
struct Clock(Arc<Mutex<Instant>>);
impl Clock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
    fn set(&self, now: Instant) {
        *self.0.lock().unwrap() = now;
    }
}

/// No filesystem, waits or processes. The free lock deliberately succeeds even
/// past its deadline, just like the native store's uncontended lock fast path.
struct MemoryDir {
    journal: Vec<u8>,
    held: AtomicBool,
    clock: Clock,
    after_read: Option<Instant>,
}
struct MemoryLock(Arc<MemoryDir>);
impl Drop for MemoryLock {
    fn drop(&mut self) {
        self.0.held.store(false, Ordering::SeqCst);
    }
}
impl StateDir for MemoryDir {
    type Lock = MemoryLock;
    fn lock(self: &Arc<Self>, _: Instant) -> Result<Self::Lock> {
        ensure!(
            !self.held.swap(true, Ordering::SeqCst),
            "synthetic state is busy"
        );
        Ok(MemoryLock(self.clone()))
    }
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>> {
        ensure!(self.held.load(Ordering::SeqCst), "read without lease");
        ensure!(name == "sidecar.json", "unexpected read");
        if let Some(now) = self.after_read {
            self.clock.set(now);
        }
        Ok(Some(self.journal.clone()))
    }
    fn write(&self, _: &str, _: &[u8]) -> Result<()> {
        anyhow::bail!("supervisor must not write authoritative state")
    }
    fn remove(&self, _: &str) -> Result<()> {
        anyhow::bail!("supervisor must not remove authoritative state")
    }
}

struct Synthetic {
    runtime: StoreRuntime<MemoryDir, Tree>,
    clock: Clock,
    deadline: Instant,
    scope: Scope,
    token: StopToken,
}
impl Synthetic {
    fn new(tree: Tree, expire_on_read: bool) -> Self {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(10);
        let clock = Clock(Arc::new(Mutex::new(now)));
        let scope = Scope {
            installation: binding().installation,
            instance: binding().instance,
            generation: Uuid::from_u128(3),
        };
        let (envelope, token) = Envelope::first(binding()).disable(scope.clone()).unwrap();
        let store = Store::new(MemoryDir {
            journal: envelope.to_vec().unwrap(),
            held: AtomicBool::new(false),
            clock: clock.clone(),
            after_read: expire_on_read.then_some(deadline),
        });
        Self {
            runtime: StoreRuntime::new(store, tree),
            clock,
            deadline,
            scope,
            token,
        }
    }
    fn authorize(&mut self) -> Result<StopLease<MemoryDir>> {
        self.runtime.begin_exchange(self.deadline);
        let clock = self.clock.clone();
        self.runtime
            .authorize_stop_with_clock(&binding(), &self.scope, &self.token, || clock.now())
    }
    fn stop(&mut self, lease: &StopLease<MemoryDir>) -> Result<Status> {
        let clock = self.clock.clone();
        self.runtime.stop_owned_with_clock(lease, || clock.now())
    }
}

#[test]
fn synthetic_healthy_control_and_stop_keep_the_original_deadline() {
    let mut f = Synthetic::new(tree(), false);
    assert_eq!(f.runtime.status().unwrap(), Status::Running);
    let lease = f.authorize().unwrap();
    assert_eq!(lease.deadline, f.deadline);
    f.clock.set(f.deadline - Duration::from_nanos(1));
    assert_eq!(f.stop(&lease).unwrap(), Status::Stopped);
    assert_eq!(f.runtime.tree().stops, 1);
    assert_eq!(f.runtime.status().unwrap(), Status::Stopped);
}

#[test]
fn synthetic_expired_free_lock_and_journal_read_deny_before_stop() {
    for expire_on_read in [false, true] {
        let mut f = Synthetic::new(tree(), expire_on_read);
        if !expire_on_read {
            f.clock.set(f.deadline);
        }
        let error = f.authorize().err().expect("expired authorization");
        assert!(error.to_string().contains("deadline expired"), "{error}");
        assert_eq!(f.runtime.tree().stops, 0);
        assert_eq!(f.runtime.status().unwrap(), Status::Running);
        assert!(f.runtime.store.begin(f.deadline).is_ok(), "lease released");
        let clock = f.clock.clone();
        assert!(
            f.runtime
                .authorize_stop_with_clock(&binding(), &f.scope, &f.token, || clock.now())
                .is_err(),
            "the failed exchange consumed its deadline"
        );
    }
}

#[test]
fn synthetic_expiry_between_authorization_and_effect_cannot_be_refreshed() {
    let mut f = Synthetic::new(tree(), false);
    let lease = f.authorize().unwrap();
    f.clock.set(f.deadline);
    // Preparing the next exchange cannot grant the old lease a fresh budget.
    f.runtime
        .begin_exchange(f.deadline + Duration::from_secs(10));
    assert!(f.stop(&lease).is_err());
    assert_eq!(f.runtime.tree().stops, 0);
    assert_eq!(f.runtime.status().unwrap(), Status::Running);
}

#[test]
fn synthetic_late_stop_result_is_not_exchange_success() {
    let mut f = Synthetic::new(tree(), false);
    let clock = f.clock.clone();
    let deadline = f.deadline;
    f.runtime.tree_mut().during_stop = Some(Box::new(move || clock.set(deadline)));
    let lease = f.authorize().unwrap();
    assert!(f.stop(&lease).is_err());
    assert_eq!(f.runtime.tree().stops, 1);
    assert_eq!(f.runtime.status().unwrap(), Status::Stopped);
    drop(lease);
    // A fresh exchange can confirm the completed effect under current authority.
    f.deadline += Duration::from_secs(10);
    let lease = f.authorize().unwrap();
    assert_eq!(f.stop(&lease).unwrap(), Status::Stopped);
    assert_eq!(f.runtime.tree().stops, 1);
}

#[test]
fn synthetic_partial_or_failed_stop_never_restores_running_status() {
    for fail_stop in [false, true] {
        let mut f = Synthetic::new(
            Tree {
                next: Status::Stopping,
                fail_stop,
                ..tree()
            },
            false,
        );
        assert_eq!(f.runtime.status().unwrap(), Status::Running);
        let lease = f.authorize().unwrap();
        if fail_stop {
            assert!(f.stop(&lease).is_err());
        } else {
            assert_eq!(f.stop(&lease).unwrap(), Status::Stopping);
        }
        // The owned tree still sees a live root after the incomplete stop.
        assert_eq!(f.runtime.tree().status, Status::Running);
        assert_eq!(f.runtime.status().unwrap(), Status::Stopping);
        drop(lease);
        f.runtime.tree_mut().fail_stop = false;
        f.runtime.tree_mut().next = Status::Stopped;
        let lease = f.authorize().unwrap();
        assert_eq!(f.stop(&lease).unwrap(), Status::Stopped);
        assert_eq!(f.runtime.status().unwrap(), Status::Stopped);
        assert_eq!(f.runtime.tree().stops, 2);
    }
}

#[test]
fn synthetic_restart_rejects_old_running_status_and_old_generation_authority() {
    let old = Synthetic::new(tree(), false);
    let mut old_server = Server::with_generation(binding(), old.runtime, old.scope.generation);
    let old_request = Request::new(old.scope.clone(), Command::Status);
    let log = Log::default();
    let mut pipe = Pipe::request(&old_request, &log);
    old_server.serve_one(&mut pipe).unwrap();
    let old_response: Response = read_frame(&mut Cursor::new(pipe.output)).unwrap();
    assert_eq!(
        old_response.validate_for(&old_request).unwrap(),
        Status::Running
    );

    old_server.runtime_mut().begin_exchange(old.deadline);
    let lease = old_server
        .runtime_mut()
        .authorize_stop_with_clock(&binding(), &old.scope, &old.token, || old.clock.now())
        .unwrap();
    assert_eq!(
        old_server
            .runtime_mut()
            .stop_owned_with_clock(&lease, || old.clock.now())
            .unwrap(),
        Status::Stopped
    );
    drop(lease);

    let stopped_request = Request::new(old.scope.clone(), Command::Status);
    let mut pipe = Pipe::request(&stopped_request, &log);
    old_server.serve_one(&mut pipe).unwrap();
    let stopped_response: Response = read_frame(&mut Cursor::new(pipe.output)).unwrap();
    assert_eq!(
        stopped_response.validate_for(&stopped_request).unwrap(),
        Status::Stopped
    );
    assert!(old_response.validate_for(&stopped_request).is_err());

    let mut restarted = Synthetic::new(tree(), false);
    restarted.scope.generation = Uuid::from_u128(4);
    let request = Request::new(restarted.scope.clone(), Command::Status);
    assert!(old_response.validate_for(&request).is_err());
    // Keep the correlation id equal too: generation alone rejects the old reply.
    let same_id = Request {
        id: old_request.id,
        ..request.clone()
    };
    assert!(old_response.validate_for(&same_id).is_err());
    let mut server =
        Server::with_generation(binding(), restarted.runtime, restarted.scope.generation);
    let mut pipe = Pipe::request(&request, &log);
    server.serve_one(&mut pipe).unwrap();
    let response: Response = read_frame(&mut Cursor::new(pipe.output)).unwrap();
    assert_eq!(response.validate_for(&request).unwrap(), Status::Running);
    let stale_stop = Request::new(old.scope, Command::Stop(old.token));
    let mut pipe = Pipe::request(&stale_stop, &log);
    server.runtime_mut().begin_exchange(restarted.deadline);
    assert!(server.serve_one(&mut pipe).is_err());
    assert_eq!(server.runtime_mut().tree().stops, 0);
}
