use super::*;
use std::{io::Write, net::TcpListener, thread};

fn fixture(replies: Vec<(&'static str, &'static str)>) -> (HttpLog, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = url::Url::parse(&format!("http://{}/log", listener.local_addr().unwrap())).unwrap();
    let worker = thread::spawn(move || {
        for (range, reply) in replies {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                assert_eq!(socket.read(&mut byte).unwrap(), 1);
                request.push(byte[0]);
            }
            let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
            assert!(
                request.contains(&format!("\r\nrange: {range}\r\n")),
                "{request}"
            );
            assert!(request.contains("\r\naccept-encoding: identity\r\n"));
            socket.write_all(reply.as_bytes()).unwrap();
        }
    });
    let log = HttpLog {
        client: reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap(),
        url,
        cursor: Cursor::default(),
        retry_at: None,
    };
    (log, worker)
}

#[test]
fn appended_bytes_after_body_disconnect_are_delivered_exactly_once() {
    let (mut log, server) = fixture(vec![
        ("bytes=0-1048575", "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-3/4\r\nContent-Length: 4\r\nConnection: close\r\n\r\none\n"),
        ("bytes=4-1048579", "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 4-7/8\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntw"),
        ("bytes=4-1048579", "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 4-9/10\r\nContent-Length: 6\r\nConnection: close\r\n\r\ntwo\nx\n"),
        ("bytes=10-1048585", "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */10\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
    ]);
    let mut delivered = log.poll().unwrap();
    assert!(log.poll().unwrap().is_empty());
    assert!(matches!(log.status(), Status::Disconnected { .. }));
    assert_eq!(log.offset(), 4);
    assert!(
        log.poll().unwrap().is_empty(),
        "backoff must not send another request"
    );
    log.retry_at = None; // Advance the mock schedule without sleeping.
    delivered.extend(log.poll().unwrap());
    assert_eq!(delivered, b"one\ntwo\nx\n");
    assert_eq!(log.status(), &Status::Connected);
    assert!(log.poll().unwrap().is_empty());
    server.join().unwrap();
}

#[test]
fn server_ignoring_range_is_stopped_before_reading_its_body() {
    let (mut log, server) = fixture(vec![(
        "bytes=0-1048575",
        "HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\nConnection: close\r\n\r\n",
    )]);
    assert_eq!(log.poll(), Err(Error::RangeUnsupported));
    assert_eq!(log.offset(), 0);
    assert_eq!(log.poll(), Err(Error::RangeUnsupported));
    server.join().unwrap();
}

#[test]
fn open_requires_https_and_rejects_embedded_credentials() {
    assert!(HttpLog::open("http://example.com/log").is_err());
    assert!(HttpLog::open("https://user:secret@example.com/log").is_err());
}
