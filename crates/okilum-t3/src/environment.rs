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
    /// `https://t3.oklabs.uk`. A bearer token never travels over plain HTTP,
    /// so `http` is accepted only for a loopback host (tests, a local server).
    pub fn new(alias: impl Into<String>, origin: &str) -> Result<Self> {
        let alias = alias.into();
        if alias.trim().is_empty() {
            bail!("the environment alias is empty");
        }
        let parsed = Url::parse(origin).with_context(|| format!("invalid T3 origin {origin:?}"))?;
        let loopback = match parsed.host() {
            Some(Host::Domain(name)) => name == "localhost",
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            None => bail!("the T3 origin {origin:?} has no host"),
        };
        match parsed.scheme() {
            "https" => {}
            "http" if loopback => {}
            scheme => bail!("the T3 origin must use https, not {scheme}: {origin}"),
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
    fn plain_http_is_refused_off_loopback() {
        assert!(Environment::new("lan", "http://10.10.0.10:23110").is_err());
        assert!(Environment::new("local", "http://127.0.0.1:23110").is_ok());
        assert!(Environment::new("local", "http://localhost:23110").is_ok());
    }

    #[test]
    fn a_path_or_empty_alias_is_refused() {
        assert!(Environment::new("maestro", "https://t3.oklabs.uk/mcp").is_err());
        assert!(Environment::new(" ", "https://t3.oklabs.uk").is_err());
    }
}
