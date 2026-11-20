//! #1944: route and HTTP CONNECT tunnel for a raw socket that cannot use
//! reqwest (the T3 WebSocket over tungstenite). The route follows the same
//! Windows decision as [`crate::local_network_builder`]; the caller connects
//! TCP to the proxy, calls [`connect_tunnel`], then runs TLS end to end with
//! the origin over the tunnel, so the proxy never sees the ticket or frames.
use std::io::{Read, Write};

/// Where the socket's TCP connection goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SocketRoute {
    Direct,
    /// An HTTP proxy that must be asked to `CONNECT` to the origin first.
    Proxy {
        host: String,
        port: u16,
    },
}

/// The socket route for a `ws`/`wss`/`http`/`https` endpoint.
///
/// Windows: loopback and private IP literals are direct; otherwise an
/// environment proxy, then the current user's static/per-protocol/PAC/WPAD
/// proxy, with the system bypass list and `NO_PROXY`. Discovery errors fail
/// closed, as for the single-origin HTTP clients. Only `http://` proxies can
/// tunnel; a SOCKS or HTTPS proxy is refused rather than bypassed.
/// macOS/Linux: direct, unchanged.
pub fn local_network_socket_route(endpoint: &url::Url) -> anyhow::Result<SocketRoute> {
    let http = http_equivalent(endpoint)?;
    #[cfg(windows)]
    {
        if crate::is_private_literal(&http) {
            return Ok(SocketRoute::Direct);
        }
        route_from(&http, crate::windows_proxy::lookup(&http)?)
    }
    #[cfg(not(windows))]
    {
        let _ = http;
        Ok(SocketRoute::Direct)
    }
}

/// PAC scripts and proxy settings know only `http`/`https`.
fn http_equivalent(endpoint: &url::Url) -> anyhow::Result<url::Url> {
    let scheme = match endpoint.scheme() {
        "ws" | "http" => "http",
        "wss" | "https" => "https",
        _ => anyhow::bail!("Unsupported socket scheme"),
    };
    let mut http = endpoint.clone();
    http.set_scheme(scheme)
        .map_err(|()| anyhow::anyhow!("Unsupported socket scheme"))?;
    http.set_query(None);
    http.set_fragment(None);
    Ok(http)
}

#[cfg(any(windows, test))]
fn route_from(url: &url::Url, lookup: crate::windows_proxy::Lookup) -> anyhow::Result<SocketRoute> {
    use crate::windows_proxy::{env_no_proxy, env_proxy, no_proxy_matches, Lookup};
    let proxy = match lookup {
        Lookup::Direct => return Ok(SocketRoute::Direct),
        Lookup::Inherit => match env_proxy(url.scheme()) {
            Some(proxy) => proxy,
            None => return Ok(SocketRoute::Direct),
        },
        Lookup::Proxy { proxy, .. } => proxy,
    };
    if no_proxy_matches(url.host_str().unwrap_or_default(), &env_no_proxy()) {
        return Ok(SocketRoute::Direct);
    }
    let proxy = if proxy.contains("://") {
        proxy
    } else {
        format!("http://{proxy}")
    };
    let proxy = url::Url::parse(&proxy).map_err(|_| TunnelError::UnsupportedProxy)?;
    let host = proxy
        .host_str()
        .filter(|_| proxy.scheme() == "http")
        .ok_or(TunnelError::UnsupportedProxy)?;
    Ok(SocketRoute::Proxy {
        host: host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned(),
        port: proxy.port_or_known_default().unwrap_or(80),
    })
}

/// A failed tunnel. Carries no host, URL or proxy response body.
#[derive(Debug)]
pub enum TunnelError {
    /// The configured proxy is not an `http://` proxy.
    UnsupportedProxy,
    /// HTTP 407: integrated NTLM/Kerberos proxy authentication is unsupported.
    ProxyAuthentication,
    /// The proxy refused to connect to the origin.
    Status(u16),
    Malformed,
    Closed,
    Io(std::io::Error),
}

impl std::fmt::Display for TunnelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedProxy => f.write_str("Only HTTP proxies can tunnel this connection"),
            Self::ProxyAuthentication => {
                f.write_str("The proxy requires authentication (HTTP 407)")
            }
            Self::Status(status) => write!(f, "The proxy refused the tunnel (HTTP {status})"),
            Self::Malformed => f.write_str("The proxy sent an invalid tunnel response"),
            Self::Closed => f.write_str("The proxy closed the tunnel"),
            Self::Io(_) => f.write_str("The proxy tunnel failed"),
        }
    }
}

impl std::error::Error for TunnelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

const MAX_HEAD: usize = 16 * 1024;

/// Ask the proxy on `stream` to open a tunnel to `host:port`. Reads the reply
/// one byte at a time so no byte of the following TLS handshake is consumed.
/// The stream's own timeouts bound the exchange. No credentials are sent.
pub fn connect_tunnel<S: Read + Write>(
    stream: &mut S,
    host: &str,
    port: u16,
    user_agent: &str,
) -> Result<(), TunnelError> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() || host.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(TunnelError::Malformed);
    }
    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let request = format!(
        "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: {user_agent}\r\nProxy-Connection: Keep-Alive\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(TunnelError::Io)?;
    let mut head = Vec::with_capacity(256);
    let mut byte = [0];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_HEAD {
            return Err(TunnelError::Malformed);
        }
        match stream.read(&mut byte) {
            Ok(0) => return Err(TunnelError::Closed),
            Ok(_) => head.push(byte[0]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(TunnelError::Io(error)),
        }
    }
    let line = head
        .split(|b| *b == b'\r')
        .next()
        .and_then(|line| std::str::from_utf8(line).ok())
        .ok_or(TunnelError::Malformed)?;
    let mut parts = line.split(' ');
    let status = match (parts.next(), parts.next()) {
        (Some(version), Some(code)) if version.starts_with("HTTP/1.") && code.len() == 3 => {
            code.parse::<u16>().map_err(|_| TunnelError::Malformed)?
        }
        _ => return Err(TunnelError::Malformed),
    };
    match status {
        200..=299 => Ok(()),
        407 => Err(TunnelError::ProxyAuthentication),
        status => Err(TunnelError::Status(status)),
    }
}

#[cfg(test)]
mod tests;
