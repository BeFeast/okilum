//! Public Feedback HTTP policy with Windows OS trust and system proxy support.
//! Windows uses SChannel's OS trust policy, never a verification bypass.
use anyhow::{Context, Result};
use reqwest::blocking::{Client, ClientBuilder};
use std::time::Duration;

#[cfg(any(windows, test))]
mod windows_proxy;

pub fn client(endpoint: &str, version: &str, timeout: Duration) -> Result<Client> {
    builder(endpoint, version, timeout)?
        .build()
        .context("Build HTTP client")
}

pub fn builder(endpoint: &str, version: &str, timeout: Duration) -> Result<ClientBuilder> {
    let endpoint = url::Url::parse(endpoint).context("Invalid HTTP endpoint")?;
    anyhow::ensure!(
        matches!(endpoint.scheme(), "https" | "http"),
        "Unsupported HTTP scheme"
    );
    let builder = Client::builder()
        .user_agent(format!("Okilum/{version} ({})", std::env::consts::OS))
        .connect_timeout(Duration::from_secs(5))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none());
    #[cfg(windows)]
    let builder = {
        let builder = builder.use_native_tls();
        match windows_proxy::resolve(&endpoint)? {
            windows_proxy::Policy::Inherit => builder,
            windows_proxy::Policy::Direct => builder.no_proxy(),
            windows_proxy::Policy::Proxy(proxy) => builder.proxy(proxy),
        }
    };
    Ok(builder)
}

/// Async uploads preserve an early HTTP rejection while the body is streaming.
pub fn async_client(endpoint: &str, version: &str, timeout: Duration) -> Result<reqwest::Client> {
    let endpoint = url::Url::parse(endpoint).context("Invalid HTTP endpoint")?;
    anyhow::ensure!(
        matches!(endpoint.scheme(), "https" | "http"),
        "Unsupported HTTP scheme"
    );
    let builder = reqwest::Client::builder()
        .user_agent(format!("Okilum/{version} ({})", std::env::consts::OS))
        .connect_timeout(Duration::from_secs(5))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none());
    #[cfg(windows)]
    let builder = {
        let builder = builder.use_native_tls();
        match windows_proxy::resolve(&endpoint)? {
            windows_proxy::Policy::Inherit => builder,
            windows_proxy::Policy::Direct => builder.no_proxy(),
            windows_proxy::Policy::Proxy(proxy) => builder.proxy(proxy),
        }
    };
    builder.build().context("Build HTTP client")
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
pub fn reason(error: &anyhow::Error) -> String {
    for cause in error.chain() {
        if let Some(error) = cause.downcast_ref::<reqwest::Error>() {
            if let Some(status) = error.status() {
                return http_reason(status.as_u16());
            }
            if error.is_timeout() {
                return "The network request timed out".into();
            }
        }
    }
    let detail = format!("{error:#}").to_ascii_lowercase();
    if detail.contains("proxy authentication required") || detail.contains("http 407") {
        return http_reason(407);
    }
    if detail.contains("certificate")
        || detail.contains("unknownissuer")
        || detail.contains("untrustedroot")
        || detail.contains("0x80090325")
        || detail.contains("0x800b0109")
        || detail.contains("tls")
        || detail.contains("ssl")
    {
        "The TLS certificate is not trusted by this computer".into()
    } else if detail.contains("proxy") || detail.contains("autoconfig") {
        "The system proxy could not complete the request".into()
    } else if detail.contains("timed out") || detail.contains("timeout") {
        "The network request timed out".into()
    } else {
        "The network connection could not be completed".into()
    }
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
