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
    next: Status,
    /// Runs inside the owned-tree stop, i.e. while the lease must be held.
    during_stop: Option<Box<dyn FnMut()>>,
}
impl OwnedTree for Tree {
    fn status(&mut self) -> Result<Status> {
        Ok(Status::Running)
    }
    fn stop(&mut self) -> Result<Status> {
        self.stops += 1;
        if let Some(probe) = &mut self.during_stop {
            probe();
        }
        Ok(self.next)
    }
}
fn tree() -> Tree {
    Tree {
        stops: 0,
        next: Status::Stopped,
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
