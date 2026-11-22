use super::*;
use std::io::{Error as Io, ErrorKind};

fn of(error: anyhow::Error) -> NetworkCause {
    classify(error.as_ref())
}

#[test]
fn os_codes_are_typed() {
    assert_eq!(
        of(Io::from_raw_os_error(0x8009_0325_u32 as i32).into()),
        NetworkCause::Certificate
    );
    assert_eq!(of(Io::from_raw_os_error(12167).into()), NetworkCause::Proxy);
    assert_eq!(
        of(Io::from(ErrorKind::TimedOut).into()),
        NetworkCause::Timeout
    );
    assert_eq!(
        of(Io::from(ErrorKind::ConnectionRefused).into()),
        NetworkCause::Connection
    );
}

#[test]
fn tunnel_and_discovery_errors_are_typed() {
    assert_eq!(
        of(TunnelError::ProxyAuthentication.into()),
        NetworkCause::ProxyAuthentication
    );
    assert_eq!(of(TunnelError::Status(502).into()), NetworkCause::Proxy);
    assert_eq!(
        of(TunnelError::Io(Io::from(ErrorKind::TimedOut)).into()),
        NetworkCause::Timeout
    );
    let discovery = anyhow::Error::new(ProxyDiscoveryFailed(anyhow::anyhow!("no PAC")))
        .context("Build HTTP client");
    assert_eq!(of(discovery), NetworkCause::Proxy);
}

/// The pre-#1944 text match read reqwest's Display, which carries the URL:
/// a refused connection to `ssl-gw...` was reported as a certificate error.
#[test]
fn a_host_name_in_the_url_never_decides_the_cause() {
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .resolve(
            "ssl-gw.corp.example",
            std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        )
        .build()
        .unwrap();
    let error = client
        .get(format!("http://ssl-gw.corp.example:{port}/tls-proxy"))
        .send()
        .unwrap_err();
    // Control: the URL really is in the message the old match read.
    assert!(format!("{error:#}").contains("ssl-gw"));
    assert_eq!(classify(&error), NetworkCause::Connection);
    assert_eq!(
        crate::reason(&error.into()),
        "The network connection could not be completed"
    );
}

#[test]
fn text_from_tls_libraries_still_classifies() {
    assert_eq!(
        of(anyhow::anyhow!(
            "certificate chain was issued by an untrusted root"
        )),
        NetworkCause::Certificate
    );
    assert_eq!(
        of(anyhow::anyhow!("unsuccessful tunnel")),
        NetworkCause::Proxy
    );
}
