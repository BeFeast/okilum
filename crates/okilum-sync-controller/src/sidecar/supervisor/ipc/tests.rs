use super::*;
use crate::sidecar::authority::{Reason, StopOperation};
use std::{
    io::{self, Cursor},
    sync::{Arc, Mutex},
};

type Events = Arc<Mutex<Vec<&'static str>>>;

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
fn stop(scope: &Scope) -> Request {
    Request::new(scope.clone(), Command::Stop(token(scope)))
}

fn binding() -> Binding {
    Binding {
        instance: Uuid::new_v4(),
        installation: Uuid::new_v4(),
        owner: "test-owner".into(),
        supervisor: "verified-supervisor".into(),
        state_directory: "private-state".into(),
        device_identity: "existing-device".into(),
    }
}
struct Runtime {
    probes: usize,
    stops: usize,
    authorizations: usize,
    allowed: bool,
    /// When set, only this exact token is authorized (the durable stored one).
    stored: Option<StopToken>,
    next: Status,
    events: Events,
}
impl Default for Runtime {
    fn default() -> Self {
        Self {
            probes: 0,
            stops: 0,
            authorizations: 0,
            allowed: true,
            stored: None,
            next: Status::Stopped,
            events: Events::default(),
        }
    }
}
/// The held instance lock: dropping it is the release.
struct Lease(Events);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.lock().unwrap().push("release");
    }
}
impl OwnedRuntime for Runtime {
    type Lease = Lease;
    fn status(&mut self) -> Result<Status> {
        self.probes += 1;
        Ok(Status::Running)
    }
    fn authorize_stop(&mut self, _: &Binding, _: &Scope, token: &StopToken) -> Result<Lease> {
        self.authorizations += 1;
        ensure!(self.allowed, "durable intent does not allow stop");
        if let Some(stored) = &self.stored {
            ensure!(stored == token, "stale or mismatched stop operation");
        }
        self.events.lock().unwrap().push("authorize");
        Ok(Lease(self.events.clone()))
    }
    fn stop_owned(&mut self, _: &Lease) -> Result<Status> {
        self.stops += 1;
        self.events.lock().unwrap().push("stop");
        Ok(self.next)
    }
}
struct Connection {
    input: Cursor<Vec<u8>>,
    output: Vec<u8>,
    authenticated: bool,
    broken_reply: bool,
    events: Events,
}
impl Connection {
    fn request(request: &Request) -> Self {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, request).unwrap();
        Self {
            input: Cursor::new(bytes),
            output: vec![],
            authenticated: true,
            broken_reply: false,
            events: Events::default(),
        }
    }
    fn reply(&self, request: &Request) -> Status {
        read_frame::<Response>(&mut Cursor::new(&self.output))
            .unwrap()
            .validate_for(request)
            .unwrap()
    }
}
impl Read for Connection {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let n = bytes.len().min(1);
        self.input.read(&mut bytes[..n])
    }
}
impl Write for Connection {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.broken_reply {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let n = bytes.len().min(2);
        self.events.lock().unwrap().push("write");
        self.output.extend_from_slice(&bytes[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Transport for Connection {
    fn verify_peer(&mut self, _: &Binding, _: &Scope) -> Result<()> {
        ensure!(self.authenticated, "foreign peer");
        Ok(())
    }
}

#[test]
fn status_is_inert_and_fragmented_io_preserves_correlation() {
    let binding = binding();
    let mut server = Server::new(binding.clone(), Runtime::default());
    assert_eq!(server.runtime.probes + server.runtime.stops, 0);
    assert_ne!(
        server.scope.generation,
        Server::new(binding, Runtime::default()).scope.generation
    );
    let request = Request::new(server.scope().clone(), Command::Status);
    let mut connection = Connection::request(&request);
    server.serve_one(&mut connection).unwrap();
    assert_eq!(connection.reply(&request), Status::Running);
    assert_eq!(server.runtime.stops, 0);
    assert_eq!(server.runtime.probes, 1); // positive control: request reached the runtime
}

#[test]
fn authentication_scope_version_and_durable_intent_fail_before_effects() {
    let mut server = Server::new(binding(), Runtime::default());
    let valid = stop(server.scope());
    let mut foreign = Connection::request(&valid);
    foreign.authenticated = false;
    assert!(server.serve_one(&mut foreign).is_err());
    assert_eq!(
        foreign.input.position(),
        0,
        "authenticate before reading attacker data"
    );
    for field in 0..8 {
        let mut request = valid.clone();
        let Command::Stop(token) = &mut request.command else {
            unreachable!()
        };
        match field {
            0 => request.version += 1,
            1 => request.scope.instance = Uuid::new_v4(),
            2 => request.scope.installation = Uuid::new_v4(),
            3 => request.scope.generation = Uuid::new_v4(),
            4 => token.operation.scope.generation = Uuid::new_v4(), // token for another generation
            5 => token.journal_epoch = Uuid::nil(),
            6 => token.operation.operation_id = Uuid::nil(),
            _ => token.operation.authorized_revision = 0,
        }
        let mut connection = Connection::request(&request);
        assert!(server.serve_one(&mut connection).is_err());
        assert!(connection.output.is_empty());
    }
    server.runtime.allowed = false;
    assert!(server.serve_one(&mut Connection::request(&valid)).is_err());
    assert_eq!(server.runtime.stops + server.runtime.probes, 0);
    server.runtime.allowed = true;
    let mut connection = Connection::request(&valid);
    server.serve_one(&mut connection).unwrap();
    assert_eq!(connection.reply(&valid), Status::Stopped);
    assert_eq!(server.runtime.stops, 1);
}

#[test]
fn timeout_and_lost_reply_do_not_claim_exit_or_repeat_completed_stop() {
    let mut server = Server::new(
        binding(),
        Runtime {
            next: Status::Stopping,
            ..Runtime::default()
        },
    );
    let request = stop(server.scope());
    let mut connection = Connection::request(&request);
    server.serve_one(&mut connection).unwrap();
    assert_eq!(connection.reply(&request), Status::Stopping);
    assert!(server.completed.is_none());
    server.runtime.next = Status::Stopped;
    let mut lost = Connection::request(&request);
    lost.broken_reply = true;
    assert!(server.serve_one(&mut lost).is_err());
    assert!(server.completed.is_some());
    let mut retry = Connection::request(&request);
    server.serve_one(&mut retry).unwrap();
    assert_eq!(retry.reply(&request), Status::Stopped);
    assert_eq!(server.runtime.stops, 2, "same-token retry is cached");
    assert_eq!(
        server.runtime.authorizations, 3,
        "but authorized again each time"
    );
    let probe = Request::new(server.scope().clone(), Command::Status);
    let mut connection = Connection::request(&probe);
    server.serve_one(&mut connection).unwrap();
    assert_eq!(connection.reply(&probe), Status::Stopped);
    assert_eq!(server.runtime.probes, 0);
}

#[test]
fn malformed_truncated_oversized_and_unknown_messages_never_stop() {
    let mut server = Server::new(binding(), Runtime::default());
    let request = stop(server.scope());
    for length in [0, MAX_FRAME as u32 + 1, u32::MAX] {
        let mut cursor = Cursor::new(length.to_be_bytes());
        assert!(read_frame::<Request>(&mut cursor).is_err());
        assert_eq!(cursor.position(), 4);
    }
    let valid = Connection::request(&request).input.into_inner();
    for length in [0, 1, 3, 4, valid.len() - 1] {
        assert!(read_frame::<Request>(&mut Cursor::new(&valid[..length])).is_err());
    }
    let mut unknown = serde_json::to_value(&request).unwrap();
    unknown["executable"] = "foreign.exe".into();
    let mut start = serde_json::to_value(&request).unwrap();
    start["command"] = "Start".into();
    let mut nested = serde_json::to_value(&request).unwrap();
    nested["scope"]["owner"] = "claimed-owner".into();
    for value in [unknown, start, nested] {
        let mut connection = Connection::request(&request);
        let mut bytes = vec![];
        write_frame(&mut bytes, &value).unwrap();
        connection.input = Cursor::new(bytes);
        assert!(server.serve_one(&mut connection).is_err());
        assert!(connection.output.is_empty());
    }
    assert_eq!(server.runtime.stops, 0);
    let mut connection = Connection::request(&request);
    server.serve_one(&mut connection).unwrap();
    assert_eq!(connection.reply(&request), Status::Stopped);
}

#[test]
fn replies_from_another_request_or_supervisor_are_not_accepted() {
    let mut server = Server::new(binding(), Runtime::default());
    let request = Request::new(server.scope().clone(), Command::Status);
    let mut connection = Connection::request(&request);
    server.serve_one(&mut connection).unwrap();
    let response: Response = read_frame(&mut Cursor::new(connection.output)).unwrap();
    let mut other = request.clone();
    other.id = Uuid::new_v4();
    assert!(response.validate_for(&other).is_err());
    other = request.clone();
    other.scope.generation = Uuid::new_v4();
    assert!(response.validate_for(&other).is_err());
    other = request;
    other.version += 1;
    assert!(response.validate_for(&other).is_err());
}

#[cfg(unix)]
#[test]
fn framing_roundtrips_over_a_real_local_stream_with_deadlines() {
    use std::{os::unix::net::UnixStream, time::Duration};
    // Transport framing fixture only: this injected peer gate does NOT establish
    // native peer-credential or signed-supervisor acceptance.
    struct Stream(UnixStream);
    impl Read for Stream {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            self.0.read(b)
        }
    }
    impl Write for Stream {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.write(b)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }
    impl Transport for Stream {
        fn verify_peer(&mut self, _: &Binding, _: &Scope) -> Result<()> {
            Ok(())
        }
    }
    let (mut client, peer) = UnixStream::pair().unwrap();
    for stream in [&client, &peer] {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
    }
    let mut server = Server::new(binding(), Runtime::default());
    let request = stop(server.scope());
    let worker = std::thread::spawn(move || {
        server.serve_one(&mut Stream(peer)).unwrap();
        server.runtime.stops
    });
    write_frame(&mut client, &request).unwrap();
    let response: Response = read_frame(&mut client).unwrap();
    assert_eq!(response.validate_for(&request).unwrap(), Status::Stopped);
    assert_eq!(worker.join().unwrap(), 1);
}

fn response_connection(request: &Request, status: Status) -> Connection {
    let response = Response {
        version: VERSION,
        scope: request.scope.clone(),
        id: request.id,
        status,
        token: match &request.command {
            Command::Stop(token) => Some(token.clone()),
            Command::Status => None,
        },
    };
    let mut connection = Connection::request(request);
    let mut bytes = vec![];
    write_frame(&mut bytes, &response).unwrap();
    connection.input = Cursor::new(bytes);
    connection
}

#[test]
fn client_authenticates_before_wire_io_and_rejects_wrong_local_binding() {
    let binding = binding();
    let server = Server::new(binding.clone(), Runtime::default());
    let request = Request::new(server.scope().clone(), Command::Status);
    let mut foreign = response_connection(&request, Status::Running);
    foreign.authenticated = false;
    assert!(exchange(&mut foreign, &binding, &request).is_err());
    assert_eq!(foreign.input.position(), 0);
    assert!(foreign.output.is_empty());
    for field in 0..3 {
        let mut invalid = request.clone();
        match field {
            0 => invalid.scope.installation = Uuid::new_v4(),
            1 => invalid.scope.instance = Uuid::new_v4(),
            _ => invalid.version += 1,
        }
        let mut connection = response_connection(&invalid, Status::Running);
        assert!(exchange(&mut connection, &binding, &invalid).is_err());
        assert!(connection.output.is_empty());
        assert_eq!(connection.input.position(), 0);
    }
    let mut valid = response_connection(&request, Status::Running);
    assert_eq!(
        exchange(&mut valid, &binding, &request).unwrap(),
        Status::Running
    );
    let sent: Request = read_frame(&mut Cursor::new(valid.output)).unwrap();
    assert_eq!(sent.id, request.id);
    assert_eq!(sent.command, Command::Status);
}

#[test]
fn stop_requires_the_durable_token_and_repeats_are_authorized_again() {
    let mut server = Server::new(binding(), Runtime::default());
    let request = stop(server.scope());
    let Command::Stop(stored) = &request.command else {
        unreachable!()
    };
    server.runtime.stored = Some(stored.clone());
    // Stale token (e.g. from before an Enable/Disable cycle): refused, no effect.
    let stale = stop(server.scope());
    let mut connection = Connection::request(&stale);
    assert!(server.serve_one(&mut connection).is_err());
    assert!(connection.output.is_empty());
    assert_eq!(server.runtime.stops, 0);
    // Positive control: the stored token stops, then repeats re-authorize but do
    // not stop again.
    for expected_stops in [1, 1] {
        let mut connection = Connection::request(&request);
        server.serve_one(&mut connection).unwrap();
        assert_eq!(connection.reply(&request), Status::Stopped);
        assert_eq!(server.runtime.stops, expected_stops);
    }
    // After the durable operation is superseded the same bytes are refused, even
    // though this generation already completed it.
    server.runtime.stored = Some(token(server.scope()));
    let mut connection = Connection::request(&request);
    assert!(server.serve_one(&mut connection).is_err());
    assert!(connection.output.is_empty());
    // A different valid token is not served from the first token's cache.
    let next = stop(server.scope());
    let Command::Stop(next_token) = &next.command else {
        unreachable!()
    };
    server.runtime.stored = Some(next_token.clone());
    let mut connection = Connection::request(&next);
    server.serve_one(&mut connection).unwrap();
    assert_eq!(server.runtime.stops, 2);
}

#[test]
fn lease_is_released_before_the_reply_is_written() {
    let events = Events::default();
    let mut server = Server::new(
        binding(),
        Runtime {
            events: events.clone(),
            ..Runtime::default()
        },
    );
    let request = stop(server.scope());
    let mut connection = Connection::request(&request);
    connection.events = events.clone();
    server.serve_one(&mut connection).unwrap();
    let log = events.lock().unwrap().clone();
    assert_eq!(&log[..3], ["authorize", "stop", "release"]);
    assert!(log[3..].iter().all(|e| *e == "write"), "{log:?}");
    assert!(log.len() > 3, "reply was written");
    // The cached repeat path releases too.
    events.lock().unwrap().clear();
    let mut again = Connection::request(&request);
    again.events = events.clone();
    server.serve_one(&mut again).unwrap();
    assert_eq!(&events.lock().unwrap()[..2], ["authorize", "release"]);
}

#[test]
fn version_one_frames_and_token_shape_violations_are_refused_before_effects() {
    let mut server = Server::new(binding(), Runtime::default());
    let good = stop(server.scope());
    let value = serde_json::to_value(&good).unwrap();
    let mut v1_stop = value.clone();
    v1_stop["version"] = 1.into();
    v1_stop["command"] = "Stop".into(); // v1 unit variant, no token
    let mut v1_version = value.clone();
    v1_version["version"] = 1.into();
    let mut status_with_token = value.clone();
    status_with_token["command"] = serde_json::json!({"Status": value["command"]["Stop"]});
    let mut two_tokens = value.clone();
    two_tokens["command"]["Stop"]["extra"] = value["command"]["Stop"].clone();
    let mut unknown_token_field = value.clone();
    unknown_token_field["command"]["Stop"]["operation"]["pid"] = 42.into();
    let mut nil_request = value.clone();
    nil_request["id"] = Uuid::nil().to_string().into();
    for frame in [
        v1_stop,
        v1_version,
        status_with_token,
        two_tokens,
        unknown_token_field,
        nil_request,
    ] {
        let mut connection = Connection::request(&good);
        let mut bytes = vec![];
        write_frame(&mut bytes, &frame).unwrap();
        connection.input = Cursor::new(bytes);
        assert!(server.serve_one(&mut connection).is_err(), "{frame}");
        assert!(connection.output.is_empty());
    }
    assert_eq!(server.runtime.stops + server.runtime.authorizations, 0);
    // Positive control: the unmodified frame passes the same gates.
    let mut connection = Connection::request(&good);
    server.serve_one(&mut connection).unwrap();
    assert_eq!(server.runtime.stops, 1);
}

#[test]
fn stop_token_fits_a_frame_and_client_refuses_v1_or_unechoed_replies() {
    let binding = binding();
    let server = Server::new(binding.clone(), Runtime::default());
    let request = stop(server.scope());
    let mut buffer = vec![];
    write_frame(&mut buffer, &request).unwrap();
    assert!(buffer.len() < MAX_FRAME / 2, "token leaves room to grow");
    let Command::Stop(sent) = &request.command else {
        unreachable!()
    };
    let reply = |version: u16, token: Option<StopToken>| {
        let response = Response {
            version,
            scope: request.scope.clone(),
            id: request.id,
            status: Status::Stopped,
            token,
        };
        let mut connection = Connection::request(&request);
        let mut bytes = vec![];
        write_frame(&mut bytes, &response).unwrap();
        connection.input = Cursor::new(bytes);
        connection
    };
    let mut other = sent.clone();
    other.operation.operation_id = Uuid::new_v4();
    for (version, token) in [
        (1, Some(sent.clone())),
        (VERSION, None),
        (VERSION, Some(other)),
    ] {
        assert!(exchange(&mut reply(version, token), &binding, &request).is_err());
    }
    // A Status reply that smuggles a token is refused too.
    let probe = Request::new(request.scope.clone(), Command::Status);
    let mut smuggled = response_connection(&probe, Status::Running);
    let mut bytes = vec![];
    write_frame(
        &mut bytes,
        &Response {
            version: VERSION,
            scope: probe.scope.clone(),
            id: probe.id,
            status: Status::Running,
            token: Some(sent.clone()),
        },
    )
    .unwrap();
    smuggled.input = Cursor::new(bytes);
    assert!(exchange(&mut smuggled, &binding, &probe).is_err());
    // Positive control: the faithful echo is accepted.
    assert_eq!(
        exchange(&mut reply(VERSION, Some(sent.clone())), &binding, &request).unwrap(),
        Status::Stopped
    );
}

#[test]
fn client_never_treats_lost_wrong_or_running_stop_reply_as_exit() {
    let binding = binding();
    let server = Server::new(binding.clone(), Runtime::default());
    let request = stop(server.scope());
    for field in 0..6 {
        let mut other = request.clone();
        match field {
            0 => other.id = Uuid::new_v4(),
            1 => other.scope.generation = Uuid::new_v4(),
            2 => other.scope.instance = Uuid::new_v4(),
            3 => other.scope.installation = Uuid::new_v4(),
            _ => (),
        }
        let status = if field == 4 {
            Status::Running
        } else {
            Status::Stopped
        };
        let mut connection = response_connection(&other, status);
        if field == 5 {
            connection.input = Cursor::new(vec![]); // request sent, reply lost
        }
        assert!(exchange(&mut connection, &binding, &request).is_err());
        let mut sent = Cursor::new(connection.output);
        assert_eq!(read_frame::<Request>(&mut sent).unwrap().id, request.id);
        assert_eq!(sent.position() as usize, sent.get_ref().len(), "no retry");
    }
    for status in [Status::Stopping, Status::Stopped] {
        let mut connection = response_connection(&request, status);
        assert_eq!(
            exchange(&mut connection, &binding, &request).unwrap(),
            status
        );
    }
}

#[test]
fn malformed_json_and_trailing_values_fail_before_runtime_effects() {
    let mut server = Server::new(binding(), Runtime::default());
    let request = stop(server.scope());
    let mut trailing = serde_json::to_vec(&request).unwrap();
    trailing.extend_from_slice(b" {}");
    for body in [vec![0xff], b"{".to_vec(), trailing] {
        let mut bytes = (body.len() as u32).to_be_bytes().to_vec();
        bytes.extend(body);
        let mut connection = Connection::request(&request);
        connection.input = Cursor::new(bytes);
        assert!(server.serve_one(&mut connection).is_err());
        assert!(connection.output.is_empty());
    }
    assert_eq!(server.runtime.stops + server.runtime.probes, 0);
    let mut valid = Connection::request(&request);
    server.serve_one(&mut valid).unwrap();
    assert_eq!(server.runtime.stops, 1);
    assert_eq!(valid.reply(&request), Status::Stopped);
}
