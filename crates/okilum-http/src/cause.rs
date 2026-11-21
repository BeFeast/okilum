//! #1944: typed network failure causes for UI text. Recognises reqwest,
//! I/O (including WinHTTP/SChannel codes), tunnel and proxy discovery errors by
//! type. The text fallback never reads a `reqwest::Error`'s own message, which
//! names the request URL: a host such as `ssl-gw.corp.example` must not turn a
//! refused connection into a certificate failure.
use std::error::Error;

use crate::TunnelError;

/// What the user can act on. `Connection` covers DNS, refusal and resets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkCause {
    Certificate,
    Proxy,
    ProxyAuthentication,
    Timeout,
    Connection,
}

impl std::fmt::Display for NetworkCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Certificate => f.write_str("The TLS certificate is not trusted by this computer"),
            Self::Proxy => f.write_str("The system proxy could not complete the request"),
            Self::ProxyAuthentication => f.write_str(&crate::http_reason(407)),
            Self::Timeout => f.write_str("The network request timed out"),
            Self::Connection => f.write_str("The network connection could not be completed"),
        }
    }
}

/// Windows proxy configuration or PAC/WPAD discovery failed for this request.
#[derive(Debug)]
pub struct ProxyDiscoveryFailed(pub(crate) anyhow::Error);

impl std::fmt::Display for ProxyDiscoveryFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Windows proxy discovery failed")
    }
}

impl Error for ProxyDiscoveryFailed {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        let inner: &(dyn Error + 'static) = self.0.as_ref();
        Some(inner)
    }
}

/// The cause of a failed connection, from its error chain.
pub fn classify(error: &(dyn Error + 'static)) -> NetworkCause {
    let mut text = String::new();
    let mut current = Some(error);
    while let Some(cause) = current {
        if let Some(found) = typed(cause) {
            return found;
        }
        if !cause.is::<reqwest::Error>() {
            text.push_str(&cause.to_string().to_ascii_lowercase());
            text.push('\n');
        }
        current = cause.source();
    }
    from_text(&text)
}

fn typed(cause: &(dyn Error + 'static)) -> Option<NetworkCause> {
    if let Some(error) = cause.downcast_ref::<reqwest::Error>() {
        if error.status().is_some_and(|s| s.as_u16() == 407) {
            return Some(NetworkCause::ProxyAuthentication);
        }
        return error.is_timeout().then_some(NetworkCause::Timeout);
    }
    if let Some(error) = cause.downcast_ref::<TunnelError>() {
        return Some(match error {
            TunnelError::ProxyAuthentication => NetworkCause::ProxyAuthentication,
            TunnelError::Io(error) if timed_out(error) => NetworkCause::Timeout,
            _ => NetworkCause::Proxy,
        });
    }
    if cause.is::<ProxyDiscoveryFailed>() {
        return Some(NetworkCause::Proxy);
    }
    if let Some(error) = cause.downcast_ref::<std::io::Error>() {
        if timed_out(error) {
            return Some(NetworkCause::Timeout);
        }
        return error.raw_os_error().and_then(os_code);
    }
    None
}

fn timed_out(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::TimedOut
}

/// WinHTTP, Winsock and SChannel/CryptoAPI codes. HRESULTs arrive as negative
/// `i32`; none of these values is a POSIX errno.
fn os_code(code: i32) -> Option<NetworkCause> {
    match code as u32 {
        // SEC_E_UNTRUSTED_ROOT, SEC_E_WRONG_PRINCIPAL, SEC_E_CERT_EXPIRED,
        // CERT_E_EXPIRED, CERT_E_UNTRUSTEDROOT, CERT_E_CN_NO_MATCH,
        // CRYPT_E_NO_REVOCATION_CHECK, CRYPT_E_REVOCATION_OFFLINE,
        // ERROR_WINHTTP_SECURE_FAILURE.
        0x8009_0325 | 0x8009_0322 | 0x8009_0328 | 0x800B_0101 | 0x800B_0109 | 0x800B_010F
        | 0x8009_2012 | 0x8009_2013 | 12175 => Some(NetworkCause::Certificate),
        // ERROR_WINHTTP_BAD_AUTO_PROXY_SCRIPT, _UNABLE_TO_DOWNLOAD_SCRIPT,
        // _AUTO_PROXY_SERVICE_ERROR, _AUTODETECTION_FAILED.
        12166 | 12167 | 12178 | 12180 => Some(NetworkCause::Proxy),
        // ERROR_WINHTTP_TIMEOUT, WSAETIMEDOUT.
        12002 | 10060 => Some(NetworkCause::Timeout),
        _ => None,
    }
}

/// TLS libraries (SChannel via native-tls, rustls) report certificate
/// failures only as text; hyper reports tunnel refusals as text too.
fn from_text(text: &str) -> NetworkCause {
    let has = |needles: &[&str]| needles.iter().any(|n| text.contains(n));
    if has(&[
        "proxy authentication required",
        "proxy authorization required",
        "http 407",
    ]) {
        NetworkCause::ProxyAuthentication
    } else if has(&[
        "certificate",
        "unknownissuer",
        "untrustedroot",
        "untrusted root",
        "0x80090325",
        "0x800b0109",
        "tls",
        "ssl",
    ]) {
        NetworkCause::Certificate
    } else if has(&["proxy", "autoconfig", "tunnel"]) {
        NetworkCause::Proxy
    } else if has(&["timed out", "timeout"]) {
        NetworkCause::Timeout
    } else {
        NetworkCause::Connection
    }
}

#[cfg(test)]
mod tests;
