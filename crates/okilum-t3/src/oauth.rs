//! Connecting an environment: OAuth 2.1 authorization code with PKCE (S256)
//! and dynamic client registration, against T3's `/oauth/mcp/*` endpoints.
//!
//! The user approves in a T3 browser session where they are signed in (or
//! with a pairing code there); the browser comes back to a one-shot loopback
//! listener on this machine (RFC 8252). T3 issues a 30-day bearer token with
//! no refresh grant, so connecting again is the only way to renew it.
//! Every call here talks to the server: nothing runs until the user asks to
//! connect.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, ensure, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use url::Url;

use crate::environment::Environment;
use crate::token::StoredToken;
use crate::unix_now;

/// Read the manager's threads and operate them (send, configure, launch).
pub const SCOPE: &str = "orchestration:read orchestration:operate";
pub const CALLBACK_PATH: &str = "/callback";

mod pending;
pub use pending::{Authorization, Input, Progress, DEFAULT_WAIT};

#[derive(Clone, Debug, Deserialize)]
pub struct AuthServerMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    #[serde(default)]
    pub code_challenge_methods_supported: Vec<String>,
    #[serde(default)]
    pub authorization_response_iss_parameter_supported: bool,
}

/// The whole flow: discover, register this device, open the browser for
/// approval, wait for the redirect, exchange the code. `client_name` names
/// the device in T3's session list ("Okilum on idunn"), which is where the
/// user revokes it.
pub fn connect(
    env: &Environment,
    client_name: &str,
    open_browser: impl FnOnce(&Url) -> Result<()>,
    timeout: Duration,
) -> Result<StoredToken> {
    let http = http_client()?;
    let metadata = discover(&http, env)?;
    let callback = Callback::bind()?;
    let client_id = register(&http, &metadata, client_name, callback.redirect_uri())?;
    let pkce = Pkce::generate()?;
    let state = random_token()?;
    let resource = env.mcp_url();
    let url = authorization_url(
        &metadata,
        &client_id,
        callback.redirect_uri(),
        &state,
        &pkce,
        &resource,
    )?;
    open_browser(&url)?;
    let issuer = metadata
        .authorization_response_iss_parameter_supported
        .then_some(metadata.issuer.as_str());
    let code = callback.wait(&state, issuer, timeout)?;
    exchange_code(
        &http,
        &metadata,
        &client_id,
        &code,
        &pkce,
        callback.redirect_uri(),
        &resource,
    )
}

pub fn http_client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent(concat!("okilum/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

/// Reads the authorization server metadata and checks that it belongs to
/// this environment: the issuer and every endpoint on the same origin, so a
/// code or a token never goes to another host.
pub fn discover(http: &Client, env: &Environment) -> Result<AuthServerMetadata> {
    let url = env.endpoint("/.well-known/oauth-authorization-server");
    let response = http
        .get(url.clone())
        .send()
        .with_context(|| format!("cannot reach {url}"))?;
    ensure!(
        response.status().is_success(),
        "{url} answered {}",
        response.status()
    );
    let metadata: AuthServerMetadata = response
        .json()
        .with_context(|| format!("{url} is not OAuth metadata"))?;
    let origin = env.origin();
    ensure!(
        metadata.issuer.trim_end_matches('/') == origin,
        "the OAuth issuer {} is not {origin}",
        metadata.issuer
    );
    let mut endpoints = vec![&metadata.authorization_endpoint, &metadata.token_endpoint];
    endpoints.extend(metadata.registration_endpoint.as_ref());
    for endpoint in endpoints {
        let parsed = Url::parse(endpoint).with_context(|| format!("bad endpoint {endpoint}"))?;
        ensure!(
            parsed.origin().ascii_serialization() == origin,
            "the OAuth endpoint {endpoint} is not on {origin}"
        );
    }
    ensure!(
        metadata
            .code_challenge_methods_supported
            .iter()
            .any(|method| method == "S256"),
        "the server does not offer PKCE S256"
    );
    Ok(metadata)
}

/// Dynamic client registration (RFC 7591) as a public client. T3 keeps no
/// state for it, so registering again on every connect is harmless.
pub fn register(
    http: &Client,
    metadata: &AuthServerMetadata,
    client_name: &str,
    redirect_uri: &str,
) -> Result<String> {
    #[derive(Deserialize)]
    struct Registered {
        client_id: String,
    }
    let endpoint = metadata
        .registration_endpoint
        .as_deref()
        .ok_or_else(|| anyhow!("the server offers no client registration"))?;
    let response = http
        .post(endpoint)
        .json(&json!({
            "client_name": client_name,
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
            "scope": SCOPE,
        }))
        .send()
        .context("client registration did not reach the server")?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    ensure!(
        status.is_success(),
        "client registration refused: {status} {}",
        oauth_error(&body)
    );
    let registered: Registered =
        serde_json::from_str(&body).context("client registration answered without a client_id")?;
    Ok(registered.client_id)
}

/// A PKCE verifier and its S256 challenge (RFC 7636).
#[derive(Clone, Debug)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn generate() -> Result<Self> {
        Ok(Self::from_verifier(random_token()?))
    }

    pub fn from_verifier(verifier: String) -> Self {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }
}

/// 32 random bytes, base64url: a PKCE verifier or a `state`.
pub fn random_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| anyhow!("no randomness: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub fn authorization_url(
    metadata: &AuthServerMetadata,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    pkce: &Pkce,
    resource: &Url,
) -> Result<Url> {
    let mut url = Url::parse(&metadata.authorization_endpoint)?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", SCOPE)
        .append_pair("state", state)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("resource", resource.as_str());
    Ok(url)
}

/// Trades the one-use code for the token. T3 expires the code after 60
/// seconds, so this runs right after the redirect.
pub fn exchange_code(
    http: &Client,
    metadata: &AuthServerMetadata,
    client_id: &str,
    code: &str,
    pkce: &Pkce,
    redirect_uri: &str,
    resource: &Url,
) -> Result<StoredToken> {
    #[derive(Deserialize)]
    struct Issued {
        access_token: String,
        token_type: String,
        expires_in: u64,
        scope: Option<String>,
    }
    let response = http
        .post(&metadata.token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", pkce.verifier.as_str()),
            ("resource", resource.as_str()),
        ])
        .send()
        .context("the token request did not reach the server")?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    ensure!(
        status.is_success(),
        "the server refused the code: {status} {}",
        oauth_error(&body)
    );
    let issued: Issued = serde_json::from_str(&body).context("the token answer is not readable")?;
    ensure!(
        issued.token_type.eq_ignore_ascii_case("bearer"),
        "unexpected token type {}",
        issued.token_type
    );
    Ok(StoredToken {
        access_token: issued.access_token,
        expires_at: unix_now() + issued.expires_in,
        scope: issued.scope.unwrap_or_else(|| SCOPE.into()),
        client_id: client_id.into(),
    })
}

/// `error: error_description` from an OAuth error body, never the body itself.
fn oauth_error(body: &str) -> String {
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let field = |name: &str| {
        parsed
            .get(name)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned()
    };
    match (field("error"), field("error_description")) {
        (error, description) if !description.is_empty() => format!("{error}: {description}"),
        (error, _) => error,
    }
}

/// The loopback listener the browser returns to after approval.
pub struct Callback {
    listener: TcpListener,
    redirect_uri: String,
}

impl Callback {
    pub fn bind() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").context("cannot open a loopback port")?;
        let port = listener.local_addr()?.port();
        Ok(Self {
            listener,
            redirect_uri: format!("http://127.0.0.1:{port}{CALLBACK_PATH}"),
        })
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Waits for the redirect and returns the code once `state` (and `iss`,
    /// when the server sends it) match. Other paths, such as a favicon
    /// request, are answered 404 and the wait goes on.
    pub fn wait(&self, state: &str, issuer: Option<&str>, timeout: Duration) -> Result<String> {
        self.listener.set_nonblocking(true)?;
        let deadline = Instant::now() + timeout;
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(code) = answer(stream, state, issuer)? {
                        return Ok(code);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        bail!("no answer from the browser within {} s", timeout.as_secs());
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) => return Err(error).context("the loopback listener failed"),
            }
        }
    }
}

/// Handles one request on the loopback port: `Ok(None)` for a request that
/// is not the callback.
fn answer(stream: TcpStream, state: &str, issuer: Option<&str>) -> Result<Option<String>> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let target = line.split_whitespace().nth(1).unwrap_or_default();
    let url = Url::parse(&format!("http://127.0.0.1{target}"))?;
    if url.path() != CALLBACK_PATH {
        respond(&stream, 404, "Not found.");
        return Ok(None);
    }
    let param = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    let failure = |reason: String| {
        respond(
            &stream,
            400,
            "Okilum could not connect to T3. Return to Okilum for details.",
        );
        Err(anyhow!(reason))
    };
    if let Some(error) = param("error") {
        let description = param("error_description").unwrap_or_default();
        return failure(format!(
            "T3 did not approve the connection: {error} {description}"
        ));
    }
    if param("state").as_deref() != Some(state) {
        return failure(
            "the browser came back with a different state; try connecting again".into(),
        );
    }
    if let Some(issuer) = issuer {
        let returned = param("iss");
        if returned.as_deref().map(|iss| iss.trim_end_matches('/'))
            != Some(issuer.trim_end_matches('/'))
        {
            return failure(format!(
                "the answer came from issuer {returned:?}, not {issuer}"
            ));
        }
    }
    let Some(code) = param("code") else {
        return failure("the browser came back without a code".into());
    };
    respond(
        &stream,
        200,
        "Okilum is connected to T3. You can close this tab.",
    );
    Ok(Some(code))
}

fn respond(mut stream: &TcpStream, status: u16, message: &str) {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>Okilum</title>\
         <p style=\"font:16px system-ui;margin:3em\">{message}</p>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\ncontent-type: text/html; charset=utf-8\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
}

#[cfg(test)]
mod tests;
