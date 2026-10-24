use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};
fn server(status: &str) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/feedback", listener.local_addr().unwrap());
    let status = status.to_string();
    let task = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            let mut b = [0];
            stream.read_exact(&mut b).unwrap();
            bytes.push(b[0]);
        }
        let headers = String::from_utf8(bytes.clone()).unwrap();
        let length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        bytes.extend(body);
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        String::from_utf8(bytes).unwrap()
    });
    (url, task)
}
#[test]
fn feedback_policy_preserves_multipart_and_explicit_user_agent() {
    let (url, server) = server("201 Created");
    let response = client(&url, "0.1.test", Duration::from_secs(5))
        .unwrap()
        .post(&url)
        .multipart(reqwest::blocking::multipart::Form::new().text("text", "test feedback"))
        .send()
        .unwrap();
    assert_eq!(response.status(), 201);
    let request = server.join().unwrap().to_ascii_lowercase();
    assert!(request.starts_with("post /feedback "));
    assert!(request.contains("user-agent: okilum/0.1.test ("));
    assert!(request.contains("content-type: multipart/form-data; boundary="));
    assert!(request.contains("test feedback"));
}
#[test]
fn proxy_auth_failure_retains_status_and_explains_the_limitation() {
    let (url, server) = server("407 Proxy Authentication Required");
    let error: anyhow::Error = client(&url, "test", Duration::from_secs(5))
        .unwrap()
        .get(url)
        .send()
        .unwrap()
        .error_for_status()
        .unwrap_err()
        .into();
    assert!(format!("{error:#}").contains("407"));
    assert!(reason(&error).contains("proxy requires authentication"));
    server.join().unwrap();
}
#[test]
fn certificate_reason_uses_the_complete_cause_chain() {
    let error = anyhow::anyhow!("certificate chain was issued by an untrusted root")
        .context("request failed");
    assert!(reason(&error).contains("TLS certificate"));
    assert!(format!("{error:#}").contains("untrusted root"));
}
