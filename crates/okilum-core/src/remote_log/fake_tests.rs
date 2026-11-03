use super::*;

#[test]
fn single_and_split_write_posts_and_chunked_bodies_reach_search() {
    let server = FakeKibana::start("127.0.0.1:0", 3, Scenario::default()).unwrap();
    let body = serde_json::to_vec(&json!({"params":{
        "index":"remote-*:events", "size":2,
        "sort":[{"@timestamp":"asc"},{"_doc":"asc"}],
        "query":{"match_all":{}}
    }}))
    .unwrap();
    for chunked in [false, true] {
        for split in [false, true] {
            let mut stream =
                TcpStream::connect(server.endpoint.strip_prefix("http://").unwrap()).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let framing = if chunked {
                "Transfer-Encoding: chunked".into()
            } else {
                format!("Content-Length: {}", body.len())
            };
            let headers = format!("POST /internal/search/es HTTP/1.1\r\nHost: localhost\r\nAuthorization: ApiKey test-secret\r\nkbn-xsrf: true\r\nelastic-api-version: 1\r\nx-elastic-internal-origin: Kibana\r\nContent-Type: application/json\r\n{framing}\r\nConnection: close\r\n\r\n");
            let payload = if chunked {
                let mut bytes = Vec::new();
                for chunk in body.chunks(31) {
                    write!(bytes, "{:X};synthetic=yes\r\n", chunk.len()).unwrap();
                    bytes.extend(chunk);
                    bytes.extend(b"\r\n");
                }
                bytes.extend(b"0\r\nX-Synthetic: trailer\r\n\r\n");
                bytes
            } else {
                body.clone()
            };
            if split {
                stream.write_all(headers.as_bytes()).unwrap();
                thread::sleep(Duration::from_millis(80));
                // Split inside chunk-size lines, data and CRLF as well.
                for piece in payload.chunks(17) {
                    stream.write_all(piece).unwrap();
                    thread::sleep(Duration::from_millis(2));
                }
            } else {
                stream
                    .write_all(&[headers.as_bytes(), &payload].concat())
                    .unwrap();
            }
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            assert!(
                response.starts_with("HTTP/1.1 200"),
                "chunked={chunked}, split={split}: {response}"
            );
            let parsed: Value =
                serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(
                parsed["rawResponse"]["hits"]["hits"]
                    .as_array()
                    .unwrap()
                    .len(),
                2,
                "positive control: valid JSON reached search and returned actual rows"
            );
        }
    }
    let state = server.state.lock().unwrap();
    assert_eq!(state.searches, 4);
    assert!(state
        .calls
        .iter()
        .all(|call| call.body["params"]["size"] == 2));
}

#[test]
fn body_reader_rejects_truncated_oversized_and_ambiguous_framing() {
    for (framing, bytes) in [
        ("content-length", "4"),
        ("content-length", "1048577"),
        ("content-length", "invalid"),
        ("transfer-encoding", "gzip"),
        ("transfer-encoding", "chunked"),
    ] {
        let headers = BTreeMap::from([(framing.into(), bytes.into())]);
        assert!(read_body(&mut &b"ab"[..], &headers).is_err());
    }
    let headers = BTreeMap::from([("transfer-encoding".into(), "chunked".into())]);
    for bytes in [b"100001\r\n".as_slice(), b"1\r\naXX0\r\n\r\n", b"0\r\n"] {
        assert!(read_body(&mut &bytes[..], &headers).is_err());
    }
    let headers = BTreeMap::from([
        ("transfer-encoding".into(), "chunked".into()),
        ("content-length".into(), "2".into()),
    ]);
    assert!(read_body(&mut &b"0\r\n\r\n"[..], &headers).is_err());
}

#[test]
fn split_post_on_inherited_nonblocking_socket_waits_for_body() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let (accepted, _) = listener.accept().unwrap();
    // Model macOS accept inheritance explicitly so Linux also detects the bug.
    accepted.set_nonblocking(true).unwrap();
    client.write_all(b"POST /internal/search/es HTTP/1.1\r\nAuthorization: ApiKey test-secret\r\nkbn-xsrf: true\r\nelastic-api-version: 1\r\nx-elastic-internal-origin: Kibana\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n").unwrap();
    let state = Arc::new(Mutex::new(State::default()));
    let task = thread::spawn(move || respond(accepted, state));
    thread::sleep(Duration::from_millis(80));
    client.write_all(b"{}").unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    task.join().unwrap().unwrap();
    assert!(
        response.starts_with("HTTP/1.1 400"),
        "complete body reaches route validation instead of EOF: {response}"
    );
}
