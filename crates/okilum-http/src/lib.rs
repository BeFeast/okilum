//! Explicit per-endpoint HTTP policy with Windows OS trust and system proxy support.
//! Every outbound single-origin client in the Windows binary builds here; the
//! inventory test in `tests/client_inventory.rs` names each deliberate exception.
//! Windows uses SChannel's OS trust policy, never a verification bypass.
use anyhow::{Context, Result};
use reqwest::blocking::{Client, ClientBuilder};
use std::time::Duration;

#[cfg(any(windows, test))]
mod windows_proxy;

mod cause;
#[cfg(any(windows, test))]
mod route;
mod tunnel;
pub use cause::{classify, NetworkCause, ProxyDiscoveryFailed};
pub use tunnel::{connect_tunnel, local_network_socket_route, SocketRoute, TunnelError};

pub fn client(endpoint: &str, version: &str, timeout: Duration) -> Result<Client> {
    builder(endpoint, version, timeout)?
        .build()
        .context("Build HTTP client")
}

pub fn builder(endpoint: &str, version: &str, timeout: Duration) -> Result<ClientBuilder> {
    blocking_builder(endpoint, version, timeout, is_loopback)
}

/// For a peer the user points at by address on their own network (T3, Board).
/// As [`builder`], but a private IP literal is also direct on Windows: WinHTTP's
/// `<local>` bypass covers only dotless names, so `10.0.0.5` or a Tailscale
/// `100.64/10` address would otherwise reach a corporate proxy that cannot route
/// to it. Host names keep the system/PAC decision.
pub fn local_network_builder(
    endpoint: &str,
    version: &str,
    timeout: Duration,
) -> Result<ClientBuilder> {
    blocking_builder(endpoint, version, timeout, is_private_literal)
}

fn blocking_builder(
    endpoint: &str,
    version: &str,
    timeout: Duration,
    direct: fn(&url::Url) -> bool,
) -> Result<ClientBuilder> {
    let endpoint = parse_endpoint(endpoint)?;
    let builder = blocking_defaults(version, timeout);
    #[cfg(windows)]
    let builder = {
        let builder = builder.use_native_tls();
        match windows_policy(&endpoint, direct)? {
            windows_proxy::Policy::Inherit => builder,
            windows_proxy::Policy::Direct => builder.no_proxy(),
            windows_proxy::Policy::Proxy(proxy) => builder.proxy(proxy),
        }
    };
    #[cfg(not(windows))]
    let _ = (endpoint, direct);
    Ok(builder)
}

fn blocking_defaults(version: &str, timeout: Duration) -> ClientBuilder {
    Client::builder()
        .user_agent(format!("Okilum/{version} ({})", std::env::consts::OS))
        .connect_timeout(Duration::from_secs(5))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
}

/// For a client whose hosts are not known when it is built: pasted links and
/// images, project logos, delivery feeds. It may follow redirects to another
/// host, so on Windows the proxy is decided for each request's own origin
/// instead of once per client: the environment proxy for that scheme with
/// `NO_PROXY`, else WinHTTP static/per-protocol/PAC/WPAD (cached per origin
/// for five minutes). Loopback stays direct.
///
/// A discovery failure sends that request direct, as browsers do when a PAC
/// script cannot be fetched: these clients are best-effort previews, and a
/// per-request hook cannot return an error. Single-origin clients keep the
/// fail-closed [`builder`]. macOS/Linux keep bundled roots and env proxies.
pub fn per_request_builder(version: &str, timeout: Duration) -> ClientBuilder {
    per_request(version, timeout, is_loopback)
}

/// As [`per_request_builder`], with private IP literals also direct; see
/// [`local_network_builder`].
pub fn local_network_per_request_builder(version: &str, timeout: Duration) -> ClientBuilder {
    per_request(version, timeout, is_private_literal)
}

fn per_request(version: &str, timeout: Duration, direct: fn(&url::Url) -> bool) -> ClientBuilder {
    let builder = blocking_defaults(version, timeout);
    #[cfg(windows)]
    // Always the hook, even with an environment proxy: reqwest would apply
    // `HTTP_PROXY` to http only and never read PAC for https. The hook takes
    // the request scheme's environment proxy (with `NO_PROXY`) and asks
    // WinHTTP for a scheme without one.
    let builder = builder
        .use_native_tls()
        .proxy(route::PerRequest::new(direct, windows_proxy::lookup).into_proxy());
    #[cfg(not(windows))]
    let _ = direct;
    builder
}

fn parse_endpoint(endpoint: &str) -> Result<url::Url> {
    let endpoint = url::Url::parse(endpoint).context("Invalid HTTP endpoint")?;
    anyhow::ensure!(
        matches!(endpoint.scheme(), "https" | "http"),
        "Unsupported HTTP scheme"
    );
    Ok(endpoint)
}

/// A loopback endpoint never reaches a system or PAC proxy: a corporate proxy
/// cannot route back to this machine, and WinHTTP discovery would only add the
/// PAC failure modes to a purely local connection.
pub fn is_loopback(endpoint: &url::Url) -> bool {
    match endpoint.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

/// Loopback, RFC 1918, link-local, the CGNAT block Tailscale uses (100.64/10),
/// IPv6 unique-local/link-local and IPv4-mapped forms of those; the same set
/// T3 accepts over plain HTTP. Only literals: a name's DNS answer is not known.
pub fn is_private_literal(endpoint: &url::Url) -> bool {
    fn v4(ip: std::net::Ipv4Addr) -> bool {
        let [a, b, ..] = ip.octets();
        ip.is_loopback()
            || ip.is_private()
            || ip.is_link_local()
            || (a == 100 && (64..128).contains(&b))
    }
    match endpoint.host() {
        Some(url::Host::Ipv4(ip)) => v4(ip),
        Some(url::Host::Ipv6(ip)) => {
            let first = ip.segments()[0];
            ip.is_loopback()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
                || ip.to_ipv4_mapped().is_some_and(v4)
        }
        Some(url::Host::Domain(_)) => is_loopback(endpoint),
        None => false,
    }
}

#[cfg(windows)]
fn windows_policy(
    endpoint: &url::Url,
    direct: fn(&url::Url) -> bool,
) -> Result<windows_proxy::Policy> {
    if direct(endpoint) {
        return Ok(windows_proxy::Policy::Direct);
    }
    windows_proxy::resolve(endpoint)
}

/// Async uploads preserve an early HTTP rejection while the body is streaming.
pub fn async_client(endpoint: &str, version: &str, timeout: Duration) -> Result<reqwest::Client> {
    async_builder(endpoint, version, timeout)?
        .build()
        .context("Build HTTP client")
}

/// Per-source async policy. Callers may tighten timeouts without changing TLS,
/// proxy discovery or redirect guards. Rebuild to refresh Windows PAC settings.
pub fn async_builder(
    endpoint: &str,
    version: &str,
    timeout: Duration,
) -> Result<reqwest::ClientBuilder> {
    let endpoint = parse_endpoint(endpoint)?;
    let builder = reqwest::Client::builder()
        .user_agent(format!("Okilum/{version} ({})", std::env::consts::OS))
        .connect_timeout(Duration::from_secs(5))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none());
    #[cfg(windows)]
    let builder = {
        let builder = builder.use_native_tls();
        match windows_policy(&endpoint, is_loopback)? {
            windows_proxy::Policy::Inherit => builder,
            windows_proxy::Policy::Direct => builder.no_proxy(),
            windows_proxy::Policy::Proxy(proxy) => builder.proxy(proxy),
        }
    };
    #[cfg(not(windows))]
    let _ = endpoint;
    Ok(builder)
}

/// Classifies actual client/stream errors, never HTTP/application rejection.
/// Mark only errors from network IO, not local payload/receipt validation.
pub fn transport_failure(error: anyhow::Error) -> anyhow::Error {
    let auth = format!("{error:#}")
        .to_ascii_lowercase()
        .contains("proxy authentication required");
    let network = !auth
        && error.chain().any(|cause| {
            if let Some(error) = cause.downcast_ref::<reqwest::Error>() {
                return error.status().is_none()
                    && (error.is_connect()
                        || error.is_timeout()
                        || error.is_request()
                        || error.is_body()
                        || error.is_decode());
            }
            if let Some(error) = cause.downcast_ref::<std::io::Error>() {
                return matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::NotConnected
                        | std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::UnexpectedEof
                ) || matches!(
                    error.raw_os_error(),
                    Some(12002 | 12007 | 12029 | 12030 | 12175)
                );
            }
            false
        });
    if network {
        error.context(RetryableTransport)
    } else {
        error
    }
}

#[derive(Debug)]
struct RetryableTransport;
impl std::fmt::Display for RetryableTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Network transport failed")
    }
}

/// Exactly one alternate attempt, only after a marked transport failure.
pub fn primary_then_fallback<T>(
    primary: &str,
    fallback: &str,
    mut attempt: impl FnMut(&str) -> Result<T>,
) -> Result<T> {
    match attempt(primary) {
        Err(error) if error.downcast_ref::<RetryableTransport>().is_some() => attempt(fallback),
        result => result,
    }
}

/// Short actionable reason for UI; the caller retains/logs the entire chain.
/// Typed: an HTTP status wins, then [`classify`], which never reads a URL.
pub fn reason(error: &anyhow::Error) -> String {
    for cause in error.chain() {
        if let Some(status) = cause
            .downcast_ref::<reqwest::Error>()
            .and_then(reqwest::Error::status)
        {
            return http_reason(status.as_u16());
        }
    }
    let error: &(dyn std::error::Error + 'static) = error.as_ref();
    classify(error).to_string()
}

pub fn http_reason(status: u16) -> String {
    if status == 407 {
        "The proxy requires authentication (HTTP 407). Integrated proxy authentication is not supported".into()
    } else {
        format!("The server returned HTTP {status}")
    }
}

#[cfg(test)]
mod tests;

#[cfg(feature = "updates")]
pub mod updates;
