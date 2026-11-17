//! #1944: every T3 and Forgejo HTTP client this crate builds takes the shared
//! `okilum_http` policy for its one origin. On Windows that is SChannel with the
//! OS trust store and the current user's static/per-protocol/PAC/WPAD proxy; a
//! loopback or private IP literal (LAN, Tailscale) stays direct, because WinHTTP's
//! `<local>` bypass covers only dotless names. Redirects stay refused. PAC discovery runs once
//! per client: recreate the gateway/reader to refresh it after a network change.
//! macOS/Linux keep reqwest's bundled roots and environment proxies.
use std::time::Duration;

/// Version-only User-Agent T3 has always seen from Okilum.
pub(crate) const USER_AGENT: &str = concat!("okilum/", env!("CARGO_PKG_VERSION"));

/// The shared policy for `origin`, with `timeout` as the whole-request budget.
/// Callers may still tighten connect/request timeouts or set their User-Agent.
pub(crate) fn builder(
    origin: &str,
    timeout: Duration,
) -> anyhow::Result<reqwest::blocking::ClientBuilder> {
    Ok(
        okilum_http::local_network_builder(origin, env!("CARGO_PKG_VERSION"), timeout)?
            .user_agent(USER_AGENT),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_refuses_non_http_origins_before_any_request() {
        assert!(builder("ftp://t3.example.test", Duration::from_secs(1)).is_err());
        assert!(builder("not a url", Duration::from_secs(1)).is_err());
    }

    #[test]
    fn policy_keeps_the_t3_user_agent_and_refuses_redirects() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            write!(
                stream,
                "HTTP/1.1 302 Found\r\nLocation: http://elsewhere.invalid/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            String::from_utf8(request).unwrap().to_ascii_lowercase()
        });
        let response = builder(&origin, Duration::from_secs(5))
            .unwrap()
            .build()
            .unwrap()
            .get(format!("{origin}/probe"))
            .send()
            .unwrap();
        // The redirect is returned, never followed to the other host.
        assert_eq!(response.status(), 302);
        let request = server.join().unwrap();
        assert!(request.contains(&format!(
            "user-agent: {}\r\n",
            USER_AGENT.to_ascii_lowercase()
        )));
    }
}
