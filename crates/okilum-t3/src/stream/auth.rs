use std::io::Read;
use std::time::Duration;

use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};

use super::{StreamError, StreamResult};
use crate::Environment;

const READ: &str = "orchestration:read";
const OPERATE: &str = "orchestration:operate";

/// A native-session credential. Do not deserialize an MCP StoredToken into
/// this type: its audience, origin and storage key are deliberately distinct.
#[derive(Clone, Serialize, Deserialize)]
pub struct NativeToken {
    origin: String,
    access_token: String,
    expires_at: u64,
    scope: String,
}

impl std::fmt::Debug for NativeToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeToken")
            .field("origin", &self.origin)
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .field("access_token", &"<redacted>")
            .finish()
    }
}

impl NativeToken {
    /// Explicit native-client pairing only. The caller must tell the user
    /// this is a separate client with read/operate access, not an upgrade of
    /// their MCP runtime-mode ceiling. Narrow the grant on the server if desired.
    /// The one-time credential is never persisted; only the returned token
    /// belongs in OS credential storage under `credential_key`.
    pub fn pair(
        environment: &Environment,
        credential: &str,
        timeout: Duration,
    ) -> StreamResult<Self> {
        if !crate::NetworkAccess::current().allowed() {
            return Err(StreamError::Closed);
        }
        if credential.trim().is_empty() {
            return Err(StreamError::PairingRequired);
        }
        #[derive(Deserialize)]
        struct Issued {
            access_token: String,
            token_type: String,
            expires_in: u64,
            scope: String,
        }
        let started = crate::unix_now();
        let response = http(timeout)?
            .post(environment.endpoint("/oauth/token"))
            .form(&[
                (
                    "grant_type",
                    "urn:ietf:params:oauth:grant-type:token-exchange",
                ),
                ("subject_token", credential.trim()),
                (
                    "subject_token_type",
                    "urn:t3:params:oauth:token-type:environment-bootstrap",
                ),
                (
                    "requested_token_type",
                    "urn:ietf:params:oauth:token-type:access_token",
                ),
                ("scope", "orchestration:read orchestration:operate"),
                ("client_label", "Okilum native Manager"),
                ("client_device_type", "desktop"),
                ("client_os", "linux"),
            ])
            .send()
            .map_err(|_| StreamError::Unreachable)?;
        let issued: Issued = decode(response)?;
        if issued.token_type != "Bearer" || issued.access_token.trim().is_empty() {
            return Err(StreamError::Protocol("invalid bearer session"));
        }
        // The server may narrow a grant, but cannot expand this request.
        // Refuse an unexpected expansion rather than silently accepting it.
        if issued
            .scope
            .split_whitespace()
            .any(|scope| scope != READ && scope != OPERATE)
        {
            return Err(StreamError::Protocol("unexpected native session scope"));
        }
        let token = Self {
            origin: environment.origin(),
            access_token: issued.access_token,
            expires_at: started.saturating_add(issued.expires_in),
            scope: issued.scope,
        };
        token.validate(environment, crate::unix_now())?;
        Ok(token)
    }

    pub fn credential_key(environment: &Environment) -> String {
        environment.endpoint("/ws").to_string()
    }
    /// Device enrollment is consumed only by the explicit Pair action.
    pub fn pairing_key(environment: &Environment) -> String {
        environment.endpoint("/ws/pairing").to_string()
    }

    pub fn can_operate(&self) -> bool {
        self.scope.split_whitespace().any(|scope| scope == OPERATE)
    }

    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }

    pub fn to_secret(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("native token serializes")
    }

    pub fn from_secret(environment: &Environment, secret: &[u8]) -> StreamResult<Self> {
        let token: Self = serde_json::from_slice(secret)
            .map_err(|_| StreamError::Protocol("unreadable native credential"))?;
        token.validate(environment, crate::unix_now())?;
        Ok(token)
    }

    pub(super) fn validate(&self, environment: &Environment, now: u64) -> StreamResult<()> {
        if self.origin != environment.origin() {
            return Err(StreamError::WrongOrigin);
        }
        if self.expires_at <= now {
            return Err(StreamError::Expired);
        }
        if self.access_token.trim().is_empty() {
            return Err(StreamError::PairingRequired);
        }
        if !self.scope.split_whitespace().any(|scope| scope == READ) {
            return Err(StreamError::MissingReadPermission);
        }
        Ok(())
    }
}

pub(super) fn http(timeout: Duration) -> StreamResult<Client> {
    if timeout.is_zero() {
        return Err(StreamError::Protocol("zero network timeout"));
    }
    Client::builder()
        .connect_timeout(timeout)
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("okilum/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| StreamError::Unreachable)
}

fn decode<T: serde::de::DeserializeOwned>(response: Response) -> StreamResult<T> {
    let status = response.status().as_u16();
    if status == 401 {
        return Err(StreamError::PairingRequired);
    }
    if !(200..300).contains(&status) {
        return Err(StreamError::Http(status));
    }
    // Auth replies are tiny. Bound input before parsing and never include
    // a peer's body or serde's source snippets in an error.
    let mut bytes = Vec::new();
    response
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| StreamError::Unreachable)?;
    if bytes.len() > 64 * 1024 {
        return Err(StreamError::Protocol("oversized auth response"));
    }
    serde_json::from_slice(&bytes).map_err(|_| StreamError::Protocol("invalid auth response"))
}

pub(super) fn history(
    http: &Client,
    environment: &Environment,
    token: &NativeToken,
    thread_id: &str,
    cursor: &str,
) -> StreamResult<crate::projection::HistoryPage> {
    let mut endpoint = environment.endpoint("/api/orchestration/threads/");
    endpoint
        .path_segments_mut()
        .map_err(|_| StreamError::Protocol("history URL"))?
        .pop_if_empty()
        .push(thread_id)
        .push("history");
    let response = http
        .get(endpoint)
        .query(&[("cursor", cursor)])
        .header("x-t3-orchestration-protocol", "2")
        .bearer_auth(&token.access_token)
        .send()
        .map_err(|_| StreamError::Unreachable)?;
    let status = response.status().as_u16();
    if status == 401 {
        return Err(StreamError::PairingRequired);
    }
    if !(200..300).contains(&status) {
        return Err(StreamError::Http(status));
    }
    let mut bytes = Vec::new();
    // T3 bounds history pages to 1 MiB. Leave room for envelope metadata.
    response
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| StreamError::Unreachable)?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(StreamError::Protocol("oversized history page"));
    }
    serde_json::from_slice(&bytes).map_err(|_| StreamError::Protocol("invalid history page"))
}

pub(super) fn ticket(
    http: &Client,
    environment: &Environment,
    token: &NativeToken,
) -> StreamResult<String> {
    #[derive(Deserialize)]
    struct Ticket {
        ticket: String,
    }
    let response = http
        .post(environment.endpoint("/api/auth/websocket-ticket"))
        .bearer_auth(&token.access_token)
        .send()
        .map_err(|_| StreamError::Unreachable)?;
    let ticket: Ticket = decode(response)?;
    if ticket.ticket.trim().is_empty() {
        return Err(StreamError::Protocol("empty WebSocket ticket"));
    }
    Ok(ticket.ticket)
}
