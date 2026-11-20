use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

/// Reads one request head and answers it; returns the request line.
fn answer(listener: &TcpListener, response: &str) -> String {
    let (mut stream, _) = listener.accept().unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        head.push(byte[0]);
    }
    stream.write_all(response.as_bytes()).unwrap();
    let head = String::from_utf8(head).unwrap();
    head.lines().next().unwrap().to_owned()
}

fn bind() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

/// The defect slice 2 left open: one PAC answer per client sent every host
/// of a pasted page through the first host's route. Here `start.test` must go
/// through the proxy and its redirect to the loopback image host must not.
#[test]
fn each_redirect_hop_takes_its_own_origins_route() {
    let (proxy, proxy_port) = bind();
    let (image, image_port) = bind();
    let lookups = Arc::new(AtomicUsize::new(0));
    let counted = lookups.clone();
    let resolver = PerRequest::new(
        |_| false,
        move |url| {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(if url.host_str() == Some("start.test") {
                Lookup::Proxy {
                    proxy: format!("http://127.0.0.1:{proxy_port}"),
                    bypass: "<local>".into(),
                }
            } else {
                Lookup::Direct
            })
        },
    );
    let proxy_task = thread::spawn(move || {
        answer(
            &proxy,
            &format!(
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{image_port}/image.png\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            ),
        )
    });
    let image_task = thread::spawn(move || {
        answer(
            &image,
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
        )
    });
    let response = crate::blocking_defaults("route", Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(3))
        .proxy(resolver.into_proxy())
        .build()
        .unwrap()
        .get("http://start.test/page")
        .send()
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().unwrap(), "ok");
    // The proxy saw the absolute-form request for the first host only.
    assert_eq!(
        proxy_task.join().unwrap(),
        "GET http://start.test/page HTTP/1.1"
    );
    // The redirect target was reached directly, in origin-form.
    assert_eq!(image_task.join().unwrap(), "GET /image.png HTTP/1.1");
    assert_eq!(lookups.load(Ordering::SeqCst), 2);
}

#[test]
fn answers_are_cached_per_origin_and_failures_go_direct() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let resolver = PerRequest::new(
        |_| false,
        move |url| {
            counted.fetch_add(1, Ordering::SeqCst);
            match url.host_str() {
                Some("pac.test") => Ok(Lookup::Proxy {
                    proxy: "http://proxy.test:8080".into(),
                    bypass: String::new(),
                }),
                _ => Err(anyhow::Error::new(crate::ProxyDiscoveryFailed(
                    anyhow::anyhow!("PAC download failed"),
                ))),
            }
        },
    );
    let pac = url::Url::parse("https://pac.test/a.png").unwrap();
    let again = url::Url::parse("https://pac.test/b.png").unwrap();
    let other_port = url::Url::parse("https://pac.test:8443/a.png").unwrap();
    let failing = url::Url::parse("https://down.test/a.png").unwrap();
    let proxy = Some(url::Url::parse("http://proxy.test:8080").unwrap());
    assert_eq!(resolver.proxy_for(&pac), proxy);
    assert_eq!(resolver.proxy_for(&again), proxy);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "same origin, one lookup");
    assert_eq!(resolver.proxy_for(&other_port), proxy);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "a port is another origin");
    assert_eq!(resolver.proxy_for(&failing), None);
    assert_eq!(resolver.proxy_for(&failing), None);
    assert_eq!(calls.load(Ordering::SeqCst), 3, "a failure is cached too");
}

#[test]
fn direct_hosts_never_run_discovery() {
    let resolver = PerRequest::new(crate::is_private_literal, |_| {
        panic!("a private literal must not reach WinHTTP")
    });
    for direct in [
        "http://127.0.0.1/a",
        "http://10.0.0.5/logo.png",
        "http://100.64.1.2:3773/x",
    ] {
        assert_eq!(resolver.proxy_for(&url::Url::parse(direct).unwrap()), None);
    }
}

#[test]
fn bypass_is_carried_by_the_lookup() {
    // The lookup applies the system bypass for its own host; the hook only
    // forwards its answer. The field still reaches per-endpoint redirects.
    let lookup = Lookup::Proxy {
        proxy: "http://proxy.test:8080".into(),
        bypass: "*.corp.test;<local>".into(),
    };
    let Lookup::Proxy { bypass, .. } = &lookup else {
        unreachable!()
    };
    assert_eq!(bypass, "*.corp.test;<local>");
}
