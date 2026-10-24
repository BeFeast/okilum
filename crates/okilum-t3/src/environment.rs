use std::net::{Ipv4Addr, Ipv6Addr};

use anyhow::{bail, Context, Result};
use url::{Host, Url};

/// One T3 server Okilum is connected to, named by the alias an area's
/// manifest uses (`t3.environment`, #1417).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Environment {
    pub alias: String,
    origin: Url,
}

impl Environment {
    /// `origin` is the server's scheme and host, for example
    /// `https://t3.oklabs.uk`. A bearer token must not cross the internet over
    /// plain HTTP, so `http` is accepted only for a loopback host (tests, a
    /// local server) or a private-network IP literal (a T3 server on the LAN
    /// or tailnet, reached by address because no certificate covers the IP).
    /// Host names other than `localhost` always need https: DNS can point a
    /// name anywhere.
    pub fn new(alias: impl Into<String>, origin: &str) -> Result<Self> {
        let alias = alias.into();
        if alias.trim().is_empty() {
            bail!("the environment alias is empty");
        }
        let parsed = Url::parse(origin).with_context(|| format!("invalid T3 origin {origin:?}"))?;
        let plain_http_allowed = match parsed.host() {
            Some(Host::Domain(name)) => name == "localhost",
            Some(Host::Ipv4(ip)) => private_ipv4(ip),
            Some(Host::Ipv6(ip)) => private_ipv6(ip),
            None => bail!("the T3 origin {origin:?} has no host"),
        };
        match parsed.scheme() {
            "https" => {}
            "http" if plain_http_allowed => {}
            scheme => bail!(
                "the T3 origin must use https (plain http only on loopback or a private-network IP), not {scheme}: {origin}"
            ),
        }
        if parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() {
            bail!("the T3 origin must be scheme and host only, without a path: {origin}");
        }
        let origin = Url::parse(&parsed.origin().ascii_serialization())?;
        Ok(Self { alias, origin })
    }

    /// The origin without a trailing slash, as T3 reports it as OAuth issuer.
    pub fn origin(&self) -> String {
        self.origin.origin().ascii_serialization()
    }

    /// The host name or IP literal (without brackets), as cookies name it.
    pub fn host(&self) -> String {
        self.origin
            .host_str()
            .unwrap_or_default()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_ascii_lowercase()
    }

    /// The MCP endpoint, which is also the OAuth `resource` indicator.
    pub fn mcp_url(&self) -> Url {
        self.endpoint("/mcp")
    }

    /// Key for the keychain entry that holds this environment's token.
    pub fn credential_url(&self) -> String {
        self.mcp_url().to_string()
    }

    pub(crate) fn endpoint(&self, path: &str) -> Url {
        self.origin.join(path).expect("a path joins onto an origin")
    }
}

/// Loopback, RFC 1918 (10/8, 172.16/12, 192.168/16), link-local
/// (169.254/16) and the CGNAT block Tailscale uses (100.64/10).
fn private_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || (a == 100 && (64..128).contains(&b))
}

/// Loopback, unique-local (fc00::/7), link-local (fe80::/10), or an
/// IPv4-mapped address that is private by the IPv4 rule.
fn private_ipv6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    ip.is_loopback()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
        || ip.to_ipv4_mapped().is_some_and(private_ipv4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_origin_is_normalised() {
        let env = Environment::new("maestro", "https://t3.oklabs.uk").unwrap();
        assert_eq!(env.origin(), "https://t3.oklabs.uk");
        assert_eq!(env.mcp_url().as_str(), "https://t3.oklabs.uk/mcp");
        assert_eq!(env.credential_url(), "https://t3.oklabs.uk/mcp");
    }

    #[test]
    fn plain_http_is_accepted_on_loopback() {
        assert!(Environment::new("local", "http://127.0.0.1:23110").is_ok());
        assert!(Environment::new("local", "http://localhost:23110").is_ok());
        assert!(Environment::new("local", "http://[::1]:23110").is_ok());
    }

    #[test]
    fn plain_http_is_accepted_on_private_ip_literals() {
        for origin in [
            "http://10.10.0.23:23110",
            "http://10.0.0.1",
            "http://172.16.0.1:8080",
            "http://172.31.255.254",
            "http://192.168.1.10:23110",
            "http://100.64.0.1:23110",
            "http://100.127.255.254",
            "http://169.254.10.20",
            "http://[fd7a:115c:a1e0::1]:23110",
            "http://[fc00::1]",
            "http://[fe80::1]",
            "http://[::ffff:10.10.0.23]:23110",
        ] {
            let env = Environment::new("lan", origin)
                .unwrap_or_else(|error| panic!("{origin} should be accepted: {error}"));
            assert!(env.origin().starts_with("http://"), "{origin}");
        }
        let env = Environment::new("lan", "http://10.10.0.23:23110").unwrap();
        assert_eq!(env.origin(), "http://10.10.0.23:23110");
        assert_eq!(env.mcp_url().as_str(), "http://10.10.0.23:23110/mcp");
    }

    #[test]
    fn plain_http_is_refused_on_public_hosts_and_names() {
        for origin in [
            "http://8.8.8.8",
            "http://172.15.255.255",
            "http://172.32.0.1",
            "http://192.169.0.1",
            "http://100.63.255.255",
            "http://100.128.0.1",
            "http://11.0.0.1",
            "http://[2001:db8::1]",
            "http://[fec0::1]",
            "http://[::ffff:8.8.8.8]",
            "http://t3.oklabs.uk",
            "http://maestro.lan:23110",
            "http://10.10.0.23.nip.io",
        ] {
            assert!(
                Environment::new("lan", origin).is_err(),
                "{origin} should be refused"
            );
        }
    }

    #[test]
    fn https_is_accepted_on_any_host() {
        assert!(Environment::new("lan", "https://10.10.0.23:23110").is_ok());
        assert!(Environment::new("public", "https://8.8.8.8").is_ok());
    }

    #[test]
    fn other_schemes_are_refused() {
        assert!(Environment::new("lan", "ws://10.10.0.23:23110").is_err());
        assert!(Environment::new("lan", "ftp://10.10.0.23").is_err());
    }

    #[test]
    fn a_path_or_empty_alias_is_refused() {
        assert!(Environment::new("maestro", "https://t3.oklabs.uk/mcp").is_err());
        assert!(Environment::new(" ", "https://t3.oklabs.uk").is_err());
    }
}
