use anyhow::{bail, ensure, Context, Result};
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
    /// `https://t3.oklabs.uk`. Plain HTTP is an explicit exception for private
    /// IPv4, tailnet IPv4 and localhost; public addresses require HTTPS.
    pub fn new(alias: impl Into<String>, origin: &str) -> Result<Self> {
        let alias = alias.into();
        if alias.trim().is_empty() {
            bail!("the environment alias is empty");
        }
        let parsed = Url::parse(origin).with_context(|| format!("invalid T3 origin {origin:?}"))?;
        let private = match parsed.host() {
            Some(Host::Domain(name)) => name == "localhost",
            Some(Host::Ipv4(ip)) => {
                let octets = ip.octets();
                ip.is_loopback()
                    || ip.is_private()
                    || (octets[0] == 100 && (64..=127).contains(&octets[1]))
            }
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            None => bail!("the T3 origin {origin:?} has no host"),
        };
        match parsed.scheme() {
            "https" => {}
            "http" if private => {}
            _ => bail!("Use https for public T3 addresses. Plain http is allowed only for private IPs, tailnet IPs or localhost"),
        }
        if parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() {
            bail!("the T3 origin must be scheme and host only, without a path: {origin}");
        }
        ensure!(
            parsed.username().is_empty() && parsed.password().is_none(),
            "Enter a T3 address without a username or password"
        );
        let origin = Url::parse(&parsed.origin().ascii_serialization())?;
        Ok(Self { alias, origin })
    }

    /// Human-entered address: names default to HTTPS; IP:port defaults to HTTP.
    /// Browser paths are discarded only here, not by the strict stored-origin API.
    pub fn from_input(alias: impl Into<String>, input: &str) -> Result<Self> {
        let input = input.trim();
        const EXPECTED: &str = "Enter a T3 address like https://t3.oklabs.uk or 10.10.0.23:23110";
        ensure!(
            !input.is_empty() && !input.chars().any(char::is_whitespace) && !input.contains('\\'),
            EXPECTED
        );
        let explicit = input.contains("://");
        let mut parsed = Url::parse(&if explicit {
            input.to_owned()
        } else {
            format!("https://{input}")
        })
        .context(EXPECTED)?;
        ensure!(
            parsed.has_host() && parsed.username().is_empty() && parsed.password().is_none(),
            EXPECTED
        );
        let entered_port = input
            .split(['/', '?', '#'])
            .next()
            .and_then(|authority| authority.rsplit_once(':'))
            .and_then(|(_, port)| port.parse::<u16>().ok());
        if !explicit
            && entered_port.is_some()
            && matches!(
                parsed.host(),
                Some(Host::Ipv4(_) | Host::Ipv6(_)) | Some(Host::Domain("localhost"))
            )
        {
            parsed
                .set_scheme("http")
                .map_err(|_| anyhow::anyhow!(EXPECTED))?;
            parsed
                .set_port(entered_port)
                .map_err(|_| anyhow::anyhow!(EXPECTED))?;
        }
        ensure!(matches!(parsed.scheme(), "https" | "http"), EXPECTED);
        parsed.set_path("/");
        parsed.set_query(None);
        parsed.set_fragment(None);
        Self::new(alias, parsed.as_str()).context(EXPECTED)
    }

    pub fn transport_rule(&self) -> &'static str {
        if self.origin.scheme() == "http" {
            "HTTP allowed for private, tailnet or localhost addresses"
        } else {
            "HTTPS"
        }
    }

    /// The origin without a trailing slash, as T3 reports it as OAuth issuer.
    pub fn origin(&self) -> String {
        self.origin.origin().ascii_serialization()
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
    fn plain_http_is_refused_for_public_addresses() {
        assert!(Environment::new("public", "http://example.com:23110").is_err());
        assert!(Environment::new("lan", "http://10.10.0.10:23110").is_ok());
        assert!(Environment::new("local", "http://127.0.0.1:23110").is_ok());
        assert!(Environment::new("local", "http://localhost:23110").is_ok());
    }

    #[test]
    fn a_path_or_empty_alias_is_refused() {
        assert!(Environment::new("maestro", "https://t3.oklabs.uk/mcp").is_err());
        assert!(Environment::new(" ", "https://t3.oklabs.uk").is_err());
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;

    #[test]
    fn human_addresses_normalise_to_the_expected_origin() {
        for (input, expected) in [
            (" t3.oklabs.uk ", "https://t3.oklabs.uk"),
            ("t3.oklabs.uk:23110", "https://t3.oklabs.uk:23110"),
            ("10.10.0.23:23110", "http://10.10.0.23:23110"),
            ("10.10.0.23:443", "http://10.10.0.23:443"),
            ("localhost:23110", "http://localhost:23110"),
            (
                "https://T3.OKLABS.UK/path/to/thread/?x=1#end",
                "https://t3.oklabs.uk",
            ),
            ("http://100.64.0.1:23110/mcp/", "http://100.64.0.1:23110"),
        ] {
            assert_eq!(
                Environment::from_input("test", input).unwrap().origin(),
                expected
            );
        }
        for bad in [
            "",
            "not a host",
            "ftp://t3.oklabs.uk",
            "https://user:secret@t3.oklabs.uk",
            "http://public.example",
            "8.8.8.8:80",
        ] {
            assert!(Environment::from_input("test", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn private_and_tailnet_http_ranges_have_strict_boundaries() {
        for host in [
            "10.0.0.1",
            "10.255.255.254",
            "172.16.0.1",
            "172.31.255.254",
            "192.168.0.1",
            "100.64.0.1",
            "100.127.255.254",
            "127.0.0.1",
            "localhost",
            "[::1]",
        ] {
            let env = Environment::new("test", &format!("http://{host}:23110")).unwrap();
            assert!(env.transport_rule().contains("private"));
        }
        for host in [
            "9.255.255.255",
            "11.0.0.1",
            "172.15.255.255",
            "172.32.0.1",
            "192.167.255.255",
            "192.169.0.1",
            "100.63.255.255",
            "100.128.0.1",
            "8.8.8.8",
            "private.example",
            "localhost.example",
            "[2001:4860:4860::8888]",
        ] {
            assert!(
                Environment::new("test", &format!("http://{host}:23110")).is_err(),
                "{host}"
            );
            assert!(
                Environment::new("test", &format!("https://{host}:23110")).is_ok(),
                "HTTPS positive control: {host}"
            );
        }
    }
}
