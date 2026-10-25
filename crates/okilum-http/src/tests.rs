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

#[test]
fn fallback_is_one_extra_request_after_a_real_connection_failure() {
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let primary = format!("http://{}/feedback", closed.local_addr().unwrap());
    drop(closed);
    let (fallback, received) = server("201 Created");
    let mut attempts = Vec::new();
    let status = primary_then_fallback(&primary, &fallback, |endpoint| {
        attempts.push(endpoint.to_owned());
        let response = client(endpoint, "test", Duration::from_secs(5))?
            .post(endpoint)
            .multipart(reqwest::blocking::multipart::Form::new().text("text", "local mock only"))
            .send()
            .map_err(|error| transport_failure(error.into()))?;
        response
            .error_for_status()
            .map(|response| response.status())
            .map_err(|error| transport_failure(error.into()))
    })
    .unwrap();
    assert_eq!(status, 201);
    assert_eq!(attempts, [primary, fallback]);
    assert!(received.join().unwrap().contains("local mock only"));
}

#[test]
fn http_rejection_never_reaches_the_alternate_address() {
    for status in [
        "400 Bad Request",
        "401 Unauthorized",
        "403 Forbidden",
        "407 Proxy Authentication Required",
        "429 Too Many Requests",
        "503 Unavailable",
    ] {
        let (primary, received) = server(status);
        let mut attempts = Vec::new();
        let result = primary_then_fallback(
            &primary,
            "http://must-not-be-contacted.invalid",
            |endpoint| {
                attempts.push(endpoint.to_owned());
                assert_eq!(endpoint, primary);
                let response = client(endpoint, "test", Duration::from_secs(5))?
                    .get(endpoint)
                    .send()
                    .map_err(|error| transport_failure(error.into()))?;
                response
                    .error_for_status()
                    .map_err(|error| transport_failure(error.into()))
            },
        );
        assert!(result.is_err());
        assert_eq!(attempts, [primary]);
        received.join().unwrap();
    }
}

#[test]
fn two_transport_failures_stop_after_the_alternate() {
    let mut attempts = Vec::new();
    let result: Result<()> = primary_then_fallback("primary", "alternate", |endpoint| {
        attempts.push(endpoint.to_owned());
        Err(transport_failure(
            std::io::Error::from(std::io::ErrorKind::TimedOut).into(),
        ))
    });
    assert!(result.is_err());
    assert_eq!(attempts, ["primary", "alternate"]);
}
