//! Offline OS fakes and a loopback REST fixture, not native service acceptance.
use super::*;
use anyhow::bail;
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    net::{SocketAddr, TcpListener},
    path::Path,
    thread::JoinHandle,
};

fn fixture_identity(root: &Path) -> DaemonIdentity {
    DaemonIdentity {
        config_file: root.join("config.xml"),
        device_id: ["AAAAAAA"; 8].join("-"),
        certificate_hash: "fixture-certificate".into(),
    }
}
fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}
fn millis(milliseconds: u64) -> Duration {
    Duration::from_millis(milliseconds)
}

/// The helper answers a fixed time after the one dispatch, as launchd, the helper's
/// claim and its runtime spawn make it do on a first launch.
struct SlowFirstLaunch {
    running_after: Duration,
    dispatched_at: Option<Instant>,
    register_calls: usize,
    waits: usize,
    deadlines: Vec<Instant>,
}
impl SlowFirstLaunch {
    fn new(running_after: Duration) -> Self {
        Self {
            running_after,
            dispatched_at: None,
            register_calls: 0,
            waits: 0,
            deadlines: Vec::new(),
        }
    }
    fn observe(&self) -> ManagedStatus {
        match self.dispatched_at {
            Some(at) if at.elapsed() >= self.running_after => ManagedStatus::Running,
            _ => ManagedStatus::RecoveryPending,
        }
    }
}
impl ConnectRegistration for SlowFirstLaunch {
    fn register_prepared(&mut self) -> Result<ManagedStatus> {
        if self.dispatched_at.is_none() {
            self.dispatched_at = Some(Instant::now());
            self.register_calls += 1;
        }
        Ok(self.observe())
    }
    fn native_status(&mut self) -> Result<Status> {
        Ok(Status::Enabled)
    }
    fn managed_status(&mut self, deadline: Instant) -> ManagedStatus {
        self.deadlines.push(deadline);
        self.observe()
    }
    fn wait(&mut self) {
        self.waits += 1;
        std::thread::sleep(millis(5));
    }
}

#[test]
fn issue2589_slow_first_launch_completes_in_one_connect() -> Result<()> {
    let root = tempfile::tempdir()?;
    let identity = fixture_identity(root.path());
    // The helper answers only after the whole preparation budget has passed, and
    // its REST a little later. Either used to end a first Connect on a Mac.
    let budget = ConnectBudget::new(millis(40), secs(20))?;
    let running_after = millis(120);
    assert!(running_after > budget.preparation());
    let mut os = SlowFirstLaunch::new(running_after);
    let started = Instant::now();
    let mut reads = 0;
    let outcome = complete_explicit_connect(&mut os, identity.clone(), budget, |bound, _| {
        assert_eq!(bound.device_id, identity.device_id);
        reads += 1;
        Ok(if reads < 3 {
            Readback::Unanswered
        } else {
            Readback::Verified
        })
    })?;
    let ConnectOutcome::Ready {
        config_path,
        device_id,
    } = outcome
    else {
        panic!("a slow first launch must complete in the same Connect")
    };
    assert_eq!(
        (config_path, device_id),
        (identity.config_file.clone(), identity.device_id.clone())
    );
    assert!(started.elapsed() >= running_after);
    assert_eq!(os.register_calls, 1, "one dispatch, never re-armed");
    assert_eq!(reads, 3, "REST that did not answer yet is waited for");
    assert!(os.waits > 1 && os.waits == os.deadlines.len());
    // One startup deadline from registration, never renewed per poll.
    assert!(os.deadlines.windows(2).all(|pair| pair[0] == pair[1]));
    assert!(os.deadlines[0] >= started + budget.startup());

    // Control: a bound shorter than the first launch fails exactly as reported in
    // #2589, still after the single dispatch.
    let mut os = SlowFirstLaunch::new(secs(3600));
    let started = Instant::now();
    let short = ConnectBudget::new(millis(40), millis(40))?;
    let error = complete_explicit_connect(&mut os, identity.clone(), short, |_, _| {
        panic!("an unclaimed generation cannot reach REST")
    })
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("Sync did not become authenticated before the deadline"));
    assert!(started.elapsed() >= short.startup());
    assert_eq!(os.register_calls, 1);
    Ok(())
}

#[test]
fn issue2589_rest_that_never_answers_ends_at_the_startup_deadline() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut os = SlowFirstLaunch::new(Duration::ZERO);
    let budget = ConnectBudget::new(millis(40), millis(60))?;
    let started = Instant::now();
    let mut reads = 0;
    let error = complete_explicit_connect(
        &mut os,
        fixture_identity(root.path()),
        budget,
        |_, deadline| {
            assert!(deadline >= started + budget.startup());
            reads += 1;
            Ok(Readback::Unanswered)
        },
    )
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("managed REST unavailable before the deadline"));
    assert!(started.elapsed() >= budget.startup());
    assert!(reads > 1);
    assert_eq!(os.register_calls, 1);
    Ok(())
}

#[test]
fn turning_sync_off_ends_the_startup_wait() -> Result<()> {
    let root = tempfile::tempdir()?;
    let access = okilum_sync::NetworkAccess::new(true);
    let _scope = access.enter();
    let mut os = SlowFirstLaunch::new(secs(3600));
    access.revoke();
    let error = complete_explicit_connect(
        &mut os,
        fixture_identity(root.path()),
        ConnectBudget::CONNECT,
        |_, _| panic!("an unclaimed generation cannot reach REST"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("Sync is disabled"));
    assert_eq!((os.register_calls, os.waits), (1, 0));
    Ok(())
}

#[test]
fn connect_budget_bounds_preparation_and_startup_separately() -> Result<()> {
    assert_eq!(ConnectBudget::CONNECT.preparation(), secs(60));
    assert_eq!(ConnectBudget::CONNECT.startup(), secs(180));
    assert_eq!(
        ConnectBudget::new(secs(60), secs(180))?,
        ConnectBudget::CONNECT
    );
    for (preparation, startup) in [(0, 1), (61, 1), (1, 0), (1, 181)] {
        assert!(ConnectBudget::new(secs(preparation), secs(startup)).is_err());
    }
    Ok(())
}

#[test]
fn approval_then_explicit_connect_retry_requires_live_peer_and_rest_identity() -> Result<()> {
    struct FakeOs {
        native: Status,
        dispatched: bool,
        register_calls: usize,
        polls: usize,
        waits: usize,
    }
    impl ConnectRegistration for FakeOs {
        fn register_prepared(&mut self) -> Result<ManagedStatus> {
            if !self.dispatched {
                self.dispatched = true;
                self.register_calls += 1;
            }
            Ok(ManagedStatus::RecoveryPending)
        }
        fn native_status(&mut self) -> Result<Status> {
            Ok(self.native)
        }
        fn managed_status(&mut self, _: Instant) -> ManagedStatus {
            self.polls += 1;
            ManagedStatus::Running
        }
        fn wait(&mut self) {
            self.waits += 1;
        }
    }
    let root = tempfile::tempdir()?;
    let identity = fixture_identity(root.path());
    let budget = ConnectBudget::new(secs(3), secs(3))?;
    let mut os = FakeOs {
        native: Status::RequiresApproval,
        dispatched: false,
        register_calls: 0,
        polls: 0,
        waits: 0,
    };
    let approved = complete_explicit_connect(&mut os, identity.clone(), budget, |_, _| {
        panic!("approval is not REST readiness")
    })?;
    assert!(matches!(approved, ConnectOutcome::RequiresApproval { .. }));
    assert_eq!((os.register_calls, os.polls, os.waits), (1, 0, 0));
    // No background continuation: only the next explicit Connect enters again.
    os.native = Status::Enabled;
    let mut reads = 0;
    let ready = complete_explicit_connect(&mut os, identity.clone(), budget, |bound, _| {
        assert_eq!(bound.device_id, identity.device_id);
        assert_eq!(bound.config_file, identity.config_file);
        reads += 1;
        Ok(Readback::Verified)
    })?;
    let ConnectOutcome::Ready {
        config_path,
        device_id,
    } = ready
    else {
        panic!("not ready")
    };
    assert_eq!(config_path, identity.config_file);
    assert_eq!(device_id, identity.device_id);
    assert_eq!((os.register_calls, os.polls, os.waits, reads), (1, 1, 1, 1));
    // Positive failure control: a live peer cannot bypass failed REST identity.
    assert!(
        complete_explicit_connect(&mut os, identity.clone(), budget, |_, _| bail!(
            "wrong REST identity"
        ))
        .is_err()
    );
    for native in [Status::NotFound, Status::NotRegistered] {
        os.native = native;
        assert!(
            complete_explicit_connect(&mut os, identity.clone(), budget, |_, _| panic!(
                "missing registration cannot reach REST"
            ))
            .is_err()
        );
    }
    assert_eq!(os.register_calls, 1);
    Ok(())
}

fn readback_identity(root: &Path, address: SocketAddr) -> Result<DaemonIdentity> {
    let identity = DaemonIdentity {
        config_file: root.join("config.xml"),
        device_id: ["AAAAAAA"; 8].join("-"),
        certificate_hash: format!("{:x}", Sha256::digest(b"fixture-cert")),
    };
    private::write(&root.join("cert.pem"), b"fixture-cert")?;
    private::write(&identity.config_file,format!("<configuration><gui tls=\"false\"><address>{address}</address><apikey>fixture-private-key</apikey></gui></configuration>").as_bytes())?;
    Ok(identity)
}
fn serve_readback(
    listener: TcpListener,
    id: String,
    requests: usize,
    wrong_identity: bool,
    redirect: bool,
) -> JoinHandle<Result<()>> {
    std::thread::spawn(move || -> Result<()> {
        listener.set_nonblocking(true)?;
        let deadline = Instant::now() + secs(3);
        for request in 0..requests {
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        ensure!(Instant::now() < deadline, "fixture request missing");
                        std::thread::sleep(millis(2));
                    }
                    Err(e) => return Err(e.into()),
                }
            };
            socket.set_nonblocking(false)?;
            socket.set_read_timeout(Some(secs(1)))?;
            let mut bytes = [0u8; 8192];
            let count = socket.read(&mut bytes)?;
            let request_text = std::str::from_utf8(&bytes[..count])?.to_ascii_lowercase();
            ensure!(
                request_text.contains("x-api-key: fixture-private-key"),
                "fixture did not observe authenticated request"
            );
            let response = if redirect {
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
            } else {
                let body = if request == 0 {
                    serde_json::json!({"myID":if wrong_identity {"wrong-device"} else {&id}})
                } else {
                    serde_json::json!({"version":CLIENT_VERSION})
                }
                .to_string();
                format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len())
            };
            socket.write_all(response.as_bytes())?;
        }
        Ok(())
    })
}

#[test]
fn ready_readback_requires_exact_identity_and_rejects_redirects() -> Result<()> {
    for (wrong_identity, redirect, expected_requests) in
        [(false, false, 2), (true, false, 1), (false, true, 1)]
    {
        let root = tempfile::tempdir()?;
        let root = root.path().canonicalize()?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let identity = readback_identity(&root, listener.local_addr()?)?;
        let server = serve_readback(
            listener,
            identity.device_id.clone(),
            expected_requests,
            wrong_identity,
            redirect,
        );
        let result = verify_rest(&identity, Instant::now() + secs(2));
        if wrong_identity || redirect {
            assert!(result.is_err(), "any answer must be the exact identity");
        } else {
            assert_eq!(result?, Readback::Verified);
        }
        server.join().unwrap()?;
    }
    Ok(())
}

#[test]
fn issue2589_rest_not_listening_yet_is_unanswered_not_a_failure() -> Result<()> {
    let root = tempfile::tempdir()?;
    let root = root.path().canonicalize()?;
    // The endpoint is reserved and released, as preparation does: nothing listens
    // there until the supervisor's runtime opens it.
    let address = TcpListener::bind("127.0.0.1:0")?.local_addr()?;
    let identity = readback_identity(&root, address)?;
    assert_eq!(
        verify_rest(&identity, Instant::now() + secs(2))?,
        Readback::Unanswered
    );
    let listener = TcpListener::bind(address)?;
    let server = serve_readback(listener, identity.device_id.clone(), 2, false, false);
    assert_eq!(
        verify_rest(&identity, Instant::now() + secs(2))?,
        Readback::Verified
    );
    server.join().unwrap()?;
    Ok(())
}
