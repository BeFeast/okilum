use super::*;
use crate::windows_proxy::Lookup;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::Duration,
};

/// A proxy that records the CONNECT head, answers `reply`, then echoes what
/// the client sends through the tunnel (standing in for the origin).
fn proxy(reply: &'static str) -> (u16, thread::JoinHandle<(String, Vec<u8>)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = thread::spawn(move || {
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
        // The reply and the first tunnelled bytes leave in one write: the
        // client must not swallow bytes that belong to the origin's stream.
        stream
            .write_all(format!("{reply}ORIGIN-HELLO").as_bytes())
            .unwrap();
        let mut tail = Vec::new();
        let _ = stream.read_to_end(&mut tail);
        (String::from_utf8(head).unwrap(), tail)
    });
    (port, task)
}

fn client(port: u16) -> TcpStream {
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
}

#[test]
fn tunnel_sends_connect_and_leaves_origin_bytes_unread() {
    let (port, task) = proxy("HTTP/1.1 200 Connection established\r\n\r\n");
    let mut stream = client(port);
    connect_tunnel(&mut stream, "t3.example.test", 443, "okilum/test").unwrap();
    let mut hello = [0; 12];
    stream.read_exact(&mut hello).unwrap();
    assert_eq!(&hello, b"ORIGIN-HELLO");
    stream.write_all(b"client-hello").unwrap();
    drop(stream);
    let (head, tail) = task.join().unwrap();
    assert!(head.starts_with("CONNECT t3.example.test:443 HTTP/1.1\r\n"));
    assert!(head.contains("\r\nHost: t3.example.test:443\r\n"));
    assert!(head.contains("\r\nUser-Agent: okilum/test\r\n"));
    assert!(!head.to_ascii_lowercase().contains("authorization"));
    assert_eq!(tail, b"client-hello");
}

#[test]
fn ipv6_origins_are_bracketed() {
    let (port, task) = proxy("HTTP/1.0 200 OK\r\n\r\n");
    connect_tunnel(&mut client(port), "fd7a::1", 8443, "okilum/test").unwrap();
    assert!(task
        .join()
        .unwrap()
        .0
        .starts_with("CONNECT [fd7a::1]:8443 HTTP/1.1\r\n"));
}

#[test]
fn proxy_refusals_are_typed() {
    let (port, task) = proxy(
        "HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Negotiate\r\n\r\n",
    );
    assert!(matches!(
        connect_tunnel(&mut client(port), "t3.example.test", 443, "ua"),
        Err(TunnelError::ProxyAuthentication)
    ));
    task.join().unwrap();
    let (port, task) = proxy("HTTP/1.1 403 Forbidden\r\n\r\n");
    assert!(matches!(
        connect_tunnel(&mut client(port), "t3.example.test", 443, "ua"),
        Err(TunnelError::Status(403))
    ));
    task.join().unwrap();
    let (port, task) = proxy("SSH-2.0-not-a-proxy\r\n\r\n");
    assert!(matches!(
        connect_tunnel(&mut client(port), "t3.example.test", 443, "ua"),
        Err(TunnelError::Malformed)
    ));
    task.join().unwrap();
}

#[test]
fn header_injection_through_the_host_is_refused() {
    let mut sink = std::io::Cursor::new(Vec::new());
    assert!(matches!(
        connect_tunnel(&mut sink, "a.test\r\nX: y", 443, "ua"),
        Err(TunnelError::Malformed)
    ));
    assert!(sink.get_ref().is_empty());
}

#[test]
fn routes_follow_the_lookup_and_refuse_non_http_proxies() {
    let url = url::Url::parse("https://t3.example.test:3773/ws").unwrap();
    assert_eq!(
        route_from(&url, Lookup::Direct).unwrap(),
        SocketRoute::Direct
    );
    assert_eq!(
        route_from(
            &url,
            Lookup::Proxy {
                proxy: "http://proxy.corp.test:8080".into(),
                bypass: String::new(),
            }
        )
        .unwrap(),
        SocketRoute::Proxy {
            host: "proxy.corp.test".into(),
            port: 8080
        }
    );
    assert_eq!(
        route_from(
            &url,
            Lookup::Proxy {
                proxy: "http://[fd00::8]".into(),
                bypass: String::new(),
            }
        )
        .unwrap(),
        SocketRoute::Proxy {
            host: "fd00::8".into(),
            port: 80
        }
    );
    for unsupported in ["socks5://proxy.corp.test:1080", "https://proxy.corp.test"] {
        let error = route_from(
            &url,
            Lookup::Proxy {
                proxy: unsupported.into(),
                bypass: String::new(),
            },
        )
        .unwrap_err();
        assert!(error.is::<TunnelError>(), "{unsupported}");
    }
    // No environment proxy in the test process: Inherit with nothing set is
    // direct (the hosted runner sets no *_proxy variables).
    if crate::windows_proxy::env_proxy("https").is_none() {
        assert_eq!(
            route_from(&url, Lookup::Inherit).unwrap(),
            SocketRoute::Direct
        );
    }
}

/// The socket honours the WinHTTP bypass list like the ticket client does.
#[test]
fn socket_route_uses_the_ticket_clients_exclusions() {
    let lookup = || Lookup::Proxy {
        proxy: "http://proxy.corp.test:8080".into(),
        bypass: "10.20.0.0/16;.t3.corp.test".into(),
    };
    for direct in ["wss://10.20.3.4:3773/ws", "https://mac.t3.corp.test/ws"] {
        let url = http_equivalent(&url::Url::parse(direct).unwrap()).unwrap();
        assert_eq!(
            route_from(&url, lookup()).unwrap(),
            SocketRoute::Direct,
            "{direct}"
        );
    }
    let url = http_equivalent(&url::Url::parse("wss://10.21.0.1/ws").unwrap()).unwrap();
    assert!(matches!(
        route_from(&url, lookup()).unwrap(),
        SocketRoute::Proxy { .. }
    ));
}

#[test]
fn socket_schemes_map_to_http_for_pac() {
    let ws = url::Url::parse("wss://user:pw@t3.example.test:443/ws?wsTicket=secret#f").unwrap();
    let http = http_equivalent(&ws).unwrap();
    assert_eq!(http.as_str(), "https://t3.example.test/");
    assert_eq!(
        http_equivalent(&url::Url::parse("wss://t3.example.test:3773/ws").unwrap())
            .unwrap()
            .as_str(),
        "https://t3.example.test:3773/"
    );
    assert_eq!(
        http_equivalent(&url::Url::parse("ws://10.0.0.5:3773/ws").unwrap())
            .unwrap()
            .scheme(),
        "http"
    );
    assert!(http_equivalent(&url::Url::parse("ftp://t3.example.test/").unwrap()).is_err());
}

/// Review on 127abc002: PAC is asked with the same URL for the ticket and the
/// socket. Under a PAC rule «`/ws` → DIRECT, else PROXY» both must take the
/// proxy; with the socket path still in the URL the socket went direct.
#[test]
fn ticket_and_socket_ask_pac_with_the_same_origin() {
    fn pac(url: &url::Url) -> Lookup {
        if url.path() == "/ws" {
            Lookup::Direct
        } else {
            Lookup::Proxy {
                proxy: "http://proxy.corp.test:8080".into(),
                bypass: String::new(),
            }
        }
    }
    let proxied = SocketRoute::Proxy {
        host: "proxy.corp.test".into(),
        port: 8080,
    };
    // The rule is live: the socket path itself would go direct.
    let raw = url::Url::parse("https://t3.corp.test:3773/ws").unwrap();
    assert_eq!(route_from(&raw, pac(&raw)).unwrap(), SocketRoute::Direct);
    // The ticket client resolves `local_network_builder(environment.origin())`.
    let ticket = crate::parse_endpoint("https://t3.corp.test:3773").unwrap();
    let socket = http_equivalent(
        &url::Url::parse("wss://t3.corp.test:3773/ws?orchestrationProtocol=2&wsTicket=s").unwrap(),
    )
    .unwrap();
    assert_eq!(socket, ticket);
    assert_eq!(route_from(&ticket, pac(&ticket)).unwrap(), proxied);
    assert_eq!(route_from(&socket, pac(&socket)).unwrap(), proxied);
}

#[cfg(not(windows))]
#[test]
fn other_platforms_keep_the_direct_socket() {
    let url = url::Url::parse("wss://t3.example.test/ws").unwrap();
    assert_eq!(
        local_network_socket_route(&url).unwrap(),
        SocketRoute::Direct
    );
}
