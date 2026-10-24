//! Shared public HTTP policy for Feedback and Windows update downloads.
//! Windows uses SChannel's OS trust policy, never a verification bypass.
use anyhow::{Context, Result};
use reqwest::blocking::{Client, ClientBuilder};
use std::time::Duration;

pub mod updates;
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
    let builder = windows_proxy::configure(builder.use_native_tls(), &endpoint)?;
    Ok(builder)
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
    if detail.contains("certificate")
        || detail.contains("unknownissuer")
        || detail.contains("untrustedroot")
        || detail.contains("0x80090325")
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
