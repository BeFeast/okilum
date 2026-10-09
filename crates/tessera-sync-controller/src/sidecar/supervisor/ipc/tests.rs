use super::*;
use std::io::{self, Cursor};

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
    allowed: bool,
    next: Status,
}
impl Default for Runtime {
    fn default() -> Self {
        Self {
            probes: 0,
            stops: 0,
            allowed: true,
            next: Status::Stopped,
        }
    }
}
impl OwnedRuntime for Runtime {
    fn status(&mut self) -> Result<Status> {
        self.probes += 1;
        Ok(Status::Running)
    }
    fn verify_stop_intent(&mut self, _: &Binding) -> Result<()> {
        ensure!(self.allowed, "durable intent does not allow stop");
        Ok(())
    }
    fn stop_owned(&mut self) -> Result<Status> {
        self.stops += 1;
        Ok(self.next)
    }
}
struct Connection {
    input: Cursor<Vec<u8>>,
    output: Vec<u8>,
    authenticated: bool,
    broken_reply: bool,
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
    let valid = Request::new(server.scope().clone(), Command::Stop);
    let mut foreign = Connection::request(&valid);
    foreign.authenticated = false;
    assert!(server.serve_one(&mut foreign).is_err());
    assert_eq!(
        foreign.input.position(),
        0,
        "authenticate before reading attacker data"
    );
    for field in 0..4 {
        let mut request = valid.clone();
        match field {
            0 => request.version += 1,
            1 => request.scope.instance = Uuid::new_v4(),
            2 => request.scope.installation = Uuid::new_v4(),
            _ => request.scope.generation = Uuid::new_v4(),
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
    let request = Request::new(server.scope().clone(), Command::Stop);
    let mut connection = Connection::request(&request);
    server.serve_one(&mut connection).unwrap();
    assert_eq!(connection.reply(&request), Status::Stopping);
    assert!(!server.stopped);
    server.runtime.next = Status::Stopped;
    let mut lost = Connection::request(&request);
    lost.broken_reply = true;
    assert!(server.serve_one(&mut lost).is_err());
    assert!(server.stopped);
    let mut retry = Connection::request(&request);
    server.serve_one(&mut retry).unwrap();
    assert_eq!(retry.reply(&request), Status::Stopped);
    assert_eq!(server.runtime.stops, 2);
    let probe = Request::new(server.scope().clone(), Command::Status);
    let mut connection = Connection::request(&probe);
    server.serve_one(&mut connection).unwrap();
    assert_eq!(connection.reply(&probe), Status::Stopped);
    assert_eq!(server.runtime.probes, 0);
}

#[test]
fn malformed_truncated_oversized_and_unknown_messages_never_stop() {
    let mut server = Server::new(binding(), Runtime::default());
    let request = Request::new(server.scope().clone(), Command::Stop);
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
    let request = Request::new(server.scope().clone(), Command::Stop);
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
fn client_never_treats_lost_wrong_or_running_stop_reply_as_exit() {
    let binding = binding();
    let server = Server::new(binding.clone(), Runtime::default());
    let request = Request::new(server.scope().clone(), Command::Stop);
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
    let request = Request::new(server.scope().clone(), Command::Stop);
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
