use super::*;
use std::time::Instant;

fn request() -> Request {
    Request {
        context: Some("existing/team context".into()),
        namespace: "shop".into(),
        pod: "checkout-abc-123".into(),
        container: "api".into(),
        previous: false,
        follow: false,
        tail_lines: 1000,
    }
}

#[test]
fn explicit_target_and_read_only_arguments() {
    let mut request = request();
    assert_eq!(
        request.args().unwrap(),
        [
            "--context=existing/team context",
            "--namespace=shop",
            "logs",
            "pod/checkout-abc-123",
            "--container=api",
            "--tail=1000",
            "--previous=false",
            "--follow=false",
            "--ignore-errors=false",
        ]
    );
    request.context = None;
    request.previous = true;
    request.follow = true;
    let args = request.args().unwrap();
    assert!(!args.iter().any(|arg| arg.starts_with("--context")));
    assert!(args.contains(&"--previous=true".into()));
    assert!(args.contains(&"--follow=true".into()));
}

#[test]
fn rejects_ambiguous_targets_and_option_injection() {
    for bad in [
        "",
        "--all-containers",
        "deployment/api",
        "a b",
        "a;echo",
        "a\n",
        "-api",
        "api.",
    ] {
        for field in 0..3 {
            let mut request = request();
            match field {
                0 => request.namespace = bad.into(),
                1 => request.pod = bad.into(),
                _ => request.container = bad.into(),
            }
            assert!(request.args().is_err(), "accepted {bad:?} in field {field}");
        }
    }
    let mut request = request();
    request.tail_lines = 100_001;
    assert!(request.args().is_err());
    request.tail_lines = 100;
    request.context = Some("\n".into());
    assert!(request.args().is_err());
}

#[cfg(unix)]
fn fixture(script: &str, request: &Request) -> Session {
    // Local synthetic kubectl replacement, never a real cluster process. Feed
    // exactly the production argument vector as distinct argv entries.
    let mut command = Command::new("sh");
    command.args(["-c", script, "fixture-kubectl"]);
    command.args(request.args().unwrap());
    Session::spawn(command).unwrap()
}

#[cfg(unix)]
fn collect(session: &mut Session) -> (Vec<u8>, Vec<u8>, ExitStatus) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let (mut data, mut diagnostic) = (Vec::new(), Vec::new());
    loop {
        assert!(Instant::now() < deadline, "fixture did not finish");
        match session.next(Duration::from_millis(20)).unwrap() {
            Some(Event::Data(bytes)) => data.extend(bytes),
            Some(Event::Diagnostic(bytes)) => diagnostic.extend(bytes),
            Some(Event::Exited(status)) => return (data, diagnostic, status),
            Some(Event::ReadError(error)) => panic!("pipe error: {error}"),
            None => (),
        }
    }
}

#[cfg(unix)]
#[test]
fn streams_exact_bytes_and_argv_without_shell_interpolation() {
    let mut request = request();
    request.context = Some("existing/$(echo injected) context".into());
    request.previous = true;
    let mut session = fixture(
        "printf '%s\\n' \"$@\" >&2; printf 'INFO first\\n'; printf '  continuation\\nlast'; exit 0",
        &request,
    );
    let (data, diagnostic, status) = collect(&mut session);
    assert!(status.success());
    assert_eq!(data, b"INFO first\n  continuation\nlast");
    assert_eq!(
        String::from_utf8(diagnostic).unwrap(),
        request.args().unwrap().join("\n") + "\n"
    );
    assert!(session.next(Duration::ZERO).unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn reports_permission_failure_separately_from_logs() {
    let mut session = fixture("printf 'Forbidden: pods/log\\n' >&2; exit 7", &request());
    let (data, diagnostic, status) = collect(&mut session);
    assert!(data.is_empty());
    assert_eq!(diagnostic, b"Forbidden: pods/log\n");
    assert_eq!(status.code(), Some(7));
}

#[cfg(unix)]
#[test]
fn follow_delivers_before_exit_and_cancel_reaps_under_backpressure() {
    let mut request = request();
    request.follow = true;
    let mut session = fixture("while :; do printf 'INFO followed\\n'; done", &request);
    let deadline = Instant::now() + Duration::from_secs(5);
    // Positive control: prove this process really produced a live stream.
    loop {
        assert!(Instant::now() < deadline);
        if let Some(Event::Data(bytes)) = session.next(Duration::from_millis(20)).unwrap() {
            assert!(!bytes.is_empty());
            break;
        }
    }
    assert!(session
        .child
        .as_mut()
        .unwrap()
        .try_wait()
        .unwrap()
        .is_none());
    // Let the bounded queue fill; cancellation must not wait on pipe senders.
    std::thread::sleep(Duration::from_millis(50));
    let start = Instant::now();
    session.cancel().unwrap();
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(session.child.is_none());
    assert!(session.events.is_none());
    session.cancel().unwrap();
}

#[test]
fn spawn_error_is_visible() {
    assert!(Session::spawn(Command::new("/nonexistent/okilum-fixture-kubectl")).is_err());
}
