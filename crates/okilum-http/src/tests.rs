use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    thread,
};
use velopack::{bundle::Manifest, VelopackAsset};

fn server(status: &str, body: &[u8]) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let status = status.to_string();
    let body = body.to_vec();
    let task = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
        String::from_utf8(request).unwrap()
    });
    (url, task)
}

#[test]
fn feed_keeps_velopack_query_and_explicit_user_agent() {
    let (url, server) = server("200 OK", br#"{"Assets":[]}"#);
    let source = updates::Source::new(&url, "0.1.test").unwrap();
    let manifest = Manifest {
        id: "BeFeast.Okilum".into(),
        ..Default::default()
    };
    assert!(source
        .feed("beta", &manifest, "fixture")
        .unwrap()
        .Assets
        .is_empty());
    let request = server.join().unwrap().to_ascii_lowercase();
    assert!(request.starts_with(
        "get /releases.beta.json?localversion=0.0.0&id=befeast.okilum&stagingid=fixture "
    ));
    assert!(request.contains("user-agent: okilum/0.1.test ("));
}

#[test]
fn package_download_reports_progress_and_rejects_truncation() {
    let (url, server) = server("200 OK", b"package");
    let source = updates::Source::new(&url, "test").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let asset = VelopackAsset {
        FileName: "test.nupkg".into(),
        Size: 7,
        ..Default::default()
    };
    let (send, receive) = mpsc::channel();
    source
        .download(&asset, &dir.path().join("test.partial"), Some(send))
        .unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("test.partial")).unwrap(),
        b"package"
    );
    assert_eq!(receive.iter().collect::<Vec<_>>(), [0, 100]);
    server.join().unwrap();
    let (url, server) = self::server("200 OK", b"short");
    let error = updates::Source::new(&url, "test")
        .unwrap()
        .download(&asset, &dir.path().join("short.partial"), None)
        .unwrap_err();
    assert!(error.to_string().contains("Incomplete package"));
    server.join().unwrap();
}

#[test]
fn feed_cannot_turn_a_package_name_into_another_origin() {
    let source = updates::Source::new("https://updates.example.test/beta/", "test").unwrap();
    for name in [
        "https://other.test/pkg",
        "../pkg",
        "..\\pkg",
        "//other.test/pkg",
    ] {
        let asset = VelopackAsset {
            FileName: name.into(),
            ..Default::default()
        };
        assert!(source
            .download(&asset, std::path::Path::new("must-not-be-created"), None)
            .unwrap_err()
            .to_string()
            .contains("filename"));
    }
}
#[test]
fn http_failure_keeps_the_cause_and_explains_proxy_authentication() {
    let (url, server) = server("407 Proxy Authentication Required", b"");
    let error = updates::Source::new(&url, "test")
        .unwrap()
        .feed("beta", &Manifest::default(), "fixture")
        .unwrap_err();
    assert!(format!("{error:#}").contains("407"));
    assert!(reason(&error).contains("proxy requires authentication"));
    server.join().unwrap();
    let tls = anyhow::anyhow!("certificate chain was issued by an untrusted root")
        .context("request failed");
    assert!(reason(&tls).contains("TLS certificate"));
    assert!(format!("{tls:#}").contains("untrusted root"));
}
