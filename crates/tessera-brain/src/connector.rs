//! Scoped Telegram connector configuration and authenticated request authority.
//! Separate from the unauthenticated native API; disabled unless explicitly bound.
use crate::inbox::SourceIdentity;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};

pub const SCHEMA: &str = "ai-brain/connector-v1";
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    schema: String,
    listen: SocketAddr,
    connector_id: String,
    token_file: PathBuf,
    workspace: Value,
    instance_id: String,
    account_id: String,
    actor_id: String,
    sender_id: String,
    routes: Vec<Route>,
}
#[derive(Clone, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
struct Route {
    chat_id: String,
    #[serde(deserialize_with = "required_nullable")]
    topic_id: Option<String>,
}
fn required_nullable<'de, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d)
}
/// A configured listener and secret remain operational process state only.
pub struct Listener {
    pub(crate) socket: TcpListener,
    pub(crate) config: TrustedConfig,
}
pub(crate) struct TrustedConfig {
    config: Config,
    token_digest: [u8; 32],
    policy_fingerprint: String,
}
impl Config {
    fn policy_fingerprint(&self) -> Result<String> {
        // Canonical across languages without JSON escaping or map-order rules.
        // A nullable field is n;, otherwise s<UTF-8 byte length>:<bytes>;.
        let mut digest = Sha256::new();
        let mut field = |value: Option<&str>| {
            if let Some(value) = value {
                digest.update(format!("s{}:", value.len()).as_bytes());
                digest.update(value.as_bytes());
                digest.update(b";");
            } else {
                digest.update(b"n;");
            }
        };
        field(Some("ai-brain/connector-policy/v1"));
        field(Some(&self.connector_id));
        for key in ["brain_id", "root", "records_dir"] {
            field(Some(
                self.workspace[key]
                    .as_str()
                    .context("invalid policy workspace identity")?,
            ));
        }
        field(Some(if self.workspace["managed"] == true {
            "true"
        } else {
            "false"
        }));
        for value in [
            &self.instance_id,
            &self.account_id,
            &self.actor_id,
            &self.sender_id,
        ] {
            field(Some(value));
        }
        let mut routes = self.routes.clone();
        routes.sort();
        field(Some(&routes.len().to_string()));
        for route in routes {
            field(Some(&route.chat_id));
            field(route.topic_id.as_deref());
        }
        Ok(format!("{:x}", digest.finalize()))
    }
}
impl Listener {
    pub fn bind(path: &Path, workspace: &Value) -> Result<Self> {
        let config: Config =
            serde_json::from_slice(&fs::read(path)?).context("invalid connector configuration")?;
        ensure!(
            config.schema == "ai-brain/connector-config-v1",
            "unsupported connector configuration schema"
        );
        ensure!(
            config.listen.ip().is_loopback(),
            "connector listener must be loopback-only"
        );
        ensure!(
            config.workspace == *workspace && workspace["managed"] == true,
            "connector requires its exact configured managed workspace"
        );
        for id in [
            &config.connector_id,
            &config.instance_id,
            &config.account_id,
            &config.actor_id,
        ] {
            identifier(id)?;
        }
        positive_id(&config.sender_id)?;
        ensure!(
            !config.routes.is_empty() && config.routes.len() <= 100,
            "connector requires 1–100 configured routes"
        );
        for route in &config.routes {
            route.validate()?;
        }
        let mut routes = config.routes.clone();
        routes.sort();
        ensure!(
            !routes.windows(2).any(|pair| pair[0] == pair[1]),
            "duplicate connector route"
        );
        let policy_fingerprint = config.policy_fingerprint()?;
        let root = Path::new(
            workspace["root"]
                .as_str()
                .context("workspace root missing")?,
        )
        .canonicalize()?;
        let token_path = config.token_file.canonicalize()?;
        ensure!(
            !token_path.starts_with(&root) && !path.canonicalize()?.starts_with(&root),
            "connector configuration and credentials must remain outside canonical brain"
        );
        let token =
            fs::read_to_string(token_path).context("connector token file cannot be read")?;
        let token = token.trim_end_matches(['\r', '\n']);
        ensure!(
            !token.is_empty() && token.len() <= 4096 && !token.chars().any(char::is_control),
            "connector credential is invalid"
        );
        let token_digest = Sha256::digest(token.as_bytes()).into();
        let socket = TcpListener::bind(config.listen)?;
        Ok(Self {
            socket,
            config: TrustedConfig {
                config,
                token_digest,
                policy_fingerprint,
            },
        })
    }
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }
}
fn identifier(value: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control),
        "invalid connector identifier"
    );
    Ok(())
}
fn positive_id(value: &str) -> Result<()> {
    ensure!(
        value
            .parse::<u64>()
            .is_ok_and(|n| n > 0 && n.to_string() == value),
        "invalid positive Telegram identifier"
    );
    Ok(())
}
impl Route {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.chat_id
                .parse::<i64>()
                .is_ok_and(|n| n != 0 && n.to_string() == self.chat_id),
            "invalid Telegram chat identifier"
        );
        if let Some(topic) = &self.topic_id {
            positive_id(topic)?;
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub(crate) schema: String,
    pub(crate) id: String,
    expected_workspace: Value,
    connector: Credential,
    telegram: Telegram,
    pub(crate) command: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    id: String,
    token: String,
    policy_fingerprint: String,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Telegram {
    sender_id: String,
    chat_id: String,
    #[serde(deserialize_with = "required_nullable")]
    topic_id: Option<String>,
    message_id: Option<String>,
    update_id: Option<String>,
}
/// Only successful authentication can create this value; callers cannot select
/// actor/channel/source instance through tool arguments or the native listener.
pub(crate) struct Authorized {
    actor: String,
    instance: String,
    account: String,
    telegram: Telegram,
}
impl TrustedConfig {
    pub(crate) fn authorize(&self, request: &Request, workspace: &Value) -> Result<Authorized> {
        let digest: [u8; 32] = Sha256::digest(request.connector.token.as_bytes()).into();
        let mismatch = digest
            .iter()
            .zip(self.token_digest)
            .fold(0u8, |diff, (a, b)| diff | (a ^ b));
        if mismatch != 0
            || request.connector.id != self.config.connector_id
            || request.telegram.sender_id != self.config.sender_id
        {
            return Err(error(
                "connector_unauthorized",
                "Connector credentials or sender do not match configured authority",
            ));
        }
        if request.connector.policy_fingerprint != self.policy_fingerprint {
            return Err(error(
                "connector_policy_mismatch",
                "Connector source policy changed; retain pending requests for review",
            ));
        }
        if request.expected_workspace != self.config.workspace
            || *workspace != self.config.workspace
        {
            return Err(error(
                "connector_workspace_mismatch",
                "Connector workspace differs from its configured brain",
            ));
        }
        let route = Route {
            chat_id: request.telegram.chat_id.clone(),
            topic_id: request.telegram.topic_id.clone(),
        };
        if !self.config.routes.contains(&route) {
            return Err(error(
                "connector_route_forbidden",
                "Telegram route is not configured",
            ));
        }
        Ok(Authorized {
            actor: self.config.actor_id.clone(),
            instance: self.config.instance_id.clone(),
            account: self.config.account_id.clone(),
            telegram: request.telegram.clone(),
        })
    }
    pub(crate) fn policy_fingerprint(&self) -> &str {
        &self.policy_fingerprint
    }
    pub(crate) fn id(&self) -> &str {
        &self.config.connector_id
    }
}
impl Authorized {
    pub(crate) fn actor(&self) -> &str {
        &self.actor
    }
    pub(crate) fn source(&self) -> Result<SourceIdentity> {
        let message = self.telegram.message_id.as_ref().ok_or_else(|| {
            error(
                "connector_invalid_request",
                "Mutation requires Telegram message identity",
            )
        })?;
        let update = self.telegram.update_id.as_ref().ok_or_else(|| {
            error(
                "connector_invalid_request",
                "Mutation requires Telegram update identity",
            )
        })?;
        // The update key is stable Telegram identity, optionally with a durable
        // client-assigned invocation suffix. It is never a source/actor selector.
        positive_id(message).map_err(|_| {
            error(
                "connector_invalid_request",
                "Invalid Telegram message identity",
            )
        })?;
        identifier(update).map_err(|_| {
            error(
                "connector_invalid_request",
                "Invalid Telegram update identity",
            )
        })?;
        Ok(SourceIdentity {
            channel: "telegram".into(),
            instance_id: self.instance.clone(),
            account_id: self.account.clone(),
            actor_id: self.actor.clone(),
            chat_id: Some(self.telegram.chat_id.clone()),
            topic_id: self.telegram.topic_id.clone(),
            message_id: message.clone(),
            update_id: update.clone(),
            uri: None,
        })
    }
    pub(crate) fn validate_source(&self, source: &SourceIdentity) -> Result<()> {
        ensure!(
            *source == self.source()?,
            "source does not match authenticated connector authority"
        );
        Ok(())
    }
}
#[derive(Debug)]
pub(crate) struct Error {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for Error {}
pub(crate) fn error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    Error {
        code,
        message: message.into(),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_policy_digest_matches_shared_cross_language_vectors() {
        let corpus: Value = serde_json::from_str(include_str!(
            "../../../docs/fixtures/connector-policy-v1.json"
        ))
        .unwrap();
        let mut values = Vec::new();
        for fixture in corpus["fixtures"].as_array().unwrap() {
            let config: Config = serde_json::from_value(fixture["config"].clone()).unwrap();
            let digest = config.policy_fingerprint().unwrap();
            assert_eq!(
                digest,
                fixture["policy_fingerprint"].as_str().unwrap(),
                "{}",
                fixture["name"]
            );
            values.push(digest);
        }
        assert_eq!(values[0], values[1], "route ordering is not authority");
        assert_eq!(
            values[0], values[2],
            "transport endpoint and token path are not authority"
        );
        assert_ne!(values[0], values[4], "account changes authority");
    }
}
