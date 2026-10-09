//! Native pairing credentials are independent of browser cookies/passkeys.
use anyhow::{ensure, Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};
use url::Url;
use uuid::Uuid;

/// Persist before the first request, privately outside disposable indexes.
/// Deliberately no Debug implementation: this record contains two credentials.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    id: Uuid,
    verifier: String,
    grant_secret: String,
    device_id: String,
    name: String,
}
impl Session {
    pub fn new(device_id: String, name: String) -> Result<Self> {
        ensure!(valid_device(&device_id), "invalid Syncthing identity");
        ensure!(
            !name.is_empty()
                && name.len() <= 100
                && name.trim() == name
                && !name.chars().any(char::is_control),
            "invalid computer name"
        );
        Ok(Self {
            id: Uuid::new_v4(),
            verifier: secret()?,
            grant_secret: secret()?,
            device_id,
            name,
        })
    }
    pub fn device_id(&self) -> &str {
        &self.device_id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn id(&self) -> Uuid {
        self.id
    }
}
#[derive(Clone, Debug, Deserialize)]
pub struct Approval {
    pub approval_url: String,
    pub request: Request,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Request {
    pub id: Uuid,
    pub device_id: String,
    pub name: String,
    pub code: String,
    pub state: String,
    pub expires: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Provisioning,
    HubReady,
    RemovalPending,
    Revoked,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Registration {
    pub id: Uuid,
    pub vault: Uuid,
    pub device_id: String,
    pub name: String,
    pub state: State,
    pub last_error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Descriptor {
    pub folder_id: String,
    pub hub_device_id: String,
    pub hub_address: String,
    pub ignores: Vec<String>,
}
#[derive(Deserialize)]
struct StatusWire {
    registration: Registration,
    folder_id: Option<String>,
    hub_device_id: Option<String>,
    hub_address: Option<String>,
    ignores: Option<Vec<String>>,
}
pub struct Status {
    pub registration: Registration,
    pub descriptor: Option<Descriptor>,
}
pub struct Service {
    origin: String,
    client: Client,
}
impl Service {
    pub fn origin(&self) -> &str {
        &self.origin
    }
    /// A custom root certificate is an explicit test/operator trust anchor, never
    /// an accept-invalid-certificates switch or a change to system trust.
    pub fn new(origin: &str, root_certificate: Option<&[u8]>) -> Result<Self> {
        let u = Url::parse(origin)?;
        ensure!(
            u.scheme() == "https"
                && u.host_str().is_some()
                && u.username().is_empty()
                && u.password().is_none()
                && u.query().is_none()
                && u.fragment().is_none()
                && u.path() == "/",
            "pairing service must be an HTTPS origin"
        );
        let mut builder = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10));
        if let Some(pem) = root_certificate {
            builder = builder.add_root_certificate(reqwest::Certificate::from_pem(pem)?);
        }
        // RFC 6761 localhost names stay on loopback even without an OS wildcard.
        let host = u.host_str().unwrap();
        if host == "localhost" || host.ends_with(".localhost") {
            builder = builder.resolve(
                host,
                SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    u.port_or_known_default().unwrap(),
                ),
            );
        }
        Ok(Self {
            origin: u.origin().ascii_serialization(),
            client: builder.build()?,
        })
    }
    fn post(&self, route: &str, body: Value, grant: Option<&str>) -> Result<Value> {
        let mut request = self
            .client
            .post(format!("{}/api/v1/sync/desktop/{route}", self.origin))
            .json(&body);
        if let Some(grant) = grant {
            request = request.bearer_auth(grant);
        }
        let response = request.send().context("pairing service unavailable")?;
        ensure!(
            response.status().is_success(),
            "pairing service rejected the operation ({})",
            response.status().as_u16()
        );
        Ok(response.json()?)
    }
    pub fn start(&self, s: &Session) -> Result<Approval> {
        let value = self.post(
            "start",
            json!({"id":s.id,"device_id":s.device_id,"name":s.name,
            "verifier_challenge":hash(&s.verifier),"grant_challenge":hash(&s.grant_secret)}),
            None,
        )?;
        let approval: Approval = serde_json::from_value(value)?;
        ensure!(
            approval.request.id == s.id
                && approval.request.device_id == s.device_id
                && approval.request.name == s.name,
            "pairing request identity differs"
        );
        let url = Url::parse(&approval.approval_url)?;
        ensure!(
            url.origin().ascii_serialization() == self.origin
                && url.path() == "/"
                && url.query().is_none()
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment() == Some(format!("sync={}", s.id).as_str()),
            "unexpected approval URL"
        );
        ensure!(
            approval.request.code.len() == 8
                && approval.request.code.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid comparison code"
        );
        Ok(approval)
    }
    pub fn exchange(&self, s: &Session) -> Result<Option<Registration>> {
        let value = self.post(
            "exchange",
            json!({"id":s.id,"verifier":s.verifier,"grant_secret":s.grant_secret}),
            None,
        )?;
        let registration: Option<Registration> = serde_json::from_value(
            value
                .get("registration")
                .context("missing exchange result")?
                .clone(),
        )?;
        if let Some(r) = &registration {
            check_registration(s, r)?;
        }
        Ok(registration)
    }
    pub fn status(&self, s: &Session) -> Result<Status> {
        let wire: StatusWire =
            serde_json::from_value(self.post("status", json!({}), Some(&s.grant_secret))?)?;
        check_registration(s, &wire.registration)?;
        let descriptor = if wire.registration.state == State::HubReady {
            let d = Descriptor {
                folder_id: wire.folder_id.context("folder missing")?,
                hub_device_id: wire.hub_device_id.context("hub identity missing")?,
                hub_address: wire.hub_address.context("hub address missing")?,
                ignores: wire.ignores.context("ignore policy missing")?,
            };
            d.validate()?;
            Some(d)
        } else {
            ensure!(
                wire.folder_id.is_none()
                    && wire.hub_device_id.is_none()
                    && wire.hub_address.is_none()
                    && wire.ignores.is_none(),
                "unexpected connection authority before hub readiness"
            );
            None
        };
        Ok(Status {
            registration: wire.registration,
            descriptor,
        })
    }
    pub fn readiness(
        &self,
        s: &Session,
        registration: &Registration,
        descriptor: &Descriptor,
    ) -> Result<crate::readiness::Receipt> {
        check_registration(s, registration)?;
        descriptor.validate()?;
        let value = self.post("readiness", json!({}), Some(&s.grant_secret))?;
        ensure!(
            value["state"] == "observed",
            "readiness observation unavailable"
        );
        let observation = serde_json::from_value(
            value
                .get("observation")
                .context("missing readiness observation")?
                .clone(),
        )?;
        crate::readiness::Receipt::new(observation, registration, descriptor)
    }
    pub fn remove(&self, s: &Session) -> Result<State> {
        let value = self.post("remove", json!({}), Some(&s.grant_secret))?;
        let state: State =
            serde_json::from_value(value.get("state").context("missing removal state")?.clone())?;
        ensure!(
            matches!(state, State::RemovalPending | State::Revoked),
            "invalid removal receipt"
        );
        Ok(state)
    }
}
impl Descriptor {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.folder_id.is_empty()
                && self.folder_id.len() <= 128
                && self.folder_id != "."
                && self.folder_id != ".."
                && self
                    .folder_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
            "invalid folder identity"
        );
        ensure!(valid_device(&self.hub_device_id), "invalid hub identity");
        let u = Url::parse(&self.hub_address)?;
        ensure!(
            u.scheme() == "tcp"
                && u.host_str().is_some()
                && u.port().is_some()
                && u.username().is_empty()
                && u.password().is_none()
                && u.query().is_none()
                && u.fragment().is_none()
                && u.path().is_empty(),
            "invalid hub transport"
        );
        ensure!(
            self.ignores.len() <= 1024
                && self.ignores.iter().all(|line| line.len() <= 4096
                    && !line.chars().any(char::is_control)
                    && !line.trim_start().starts_with("#include")),
            "ignore policy requires explicit review"
        );
        Ok(())
    }
}
fn check_registration(s: &Session, r: &Registration) -> Result<()> {
    ensure!(
        r.id == s.id && r.device_id == s.device_id && !r.vault.is_nil(),
        "grant registration scope differs"
    );
    Ok(())
}
fn valid_device(id: &str) -> bool {
    id.len() == 63
        && id.split('-').count() == 8
        && id.split('-').all(|part| {
            part.len() == 7
                && part
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b))
        })
}
fn hash(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}
fn secret() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| anyhow::anyhow!("secure randomness unavailable"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn id() -> String {
        ["AAAAAAA"; 8].join("-")
    }
    #[test]
    fn service_urls_and_descriptor_authority_fail_closed() {
        for u in [
            "http://localhost:1234",
            "https://user:pass@example.test",
            "https://example.test/a",
            "https://example.test/?token=x",
        ] {
            assert!(Service::new(u, None).is_err());
        }
        let mut d = Descriptor {
            folder_id: "fixture".into(),
            hub_device_id: id(),
            hub_address: "tcp://127.0.0.1:22440".into(),
            ignores: vec!["/.okilum-index".into()],
        };
        assert!(d.validate().is_ok());
        d.ignores.push("#include ../private".into());
        assert!(d.validate().is_err());
    }
    #[test]
    fn credentials_survive_private_serialization_and_bind_registration() -> Result<()> {
        let s = Session::new(id(), "Laptop".into())?;
        let restored: Session = serde_json::from_slice(&serde_json::to_vec(&s)?)?;
        assert_eq!(restored.id, s.id);
        assert_eq!(restored.grant_secret, s.grant_secret);
        assert_eq!(s.verifier.len(), 64);
        assert_ne!(s.verifier, s.grant_secret);
        let mut r = Registration {
            id: s.id,
            vault: Uuid::new_v4(),
            device_id: id(),
            name: "Laptop".into(),
            state: State::Provisioning,
            last_error: None,
        };
        assert!(check_registration(&s, &r).is_ok());
        r.id = Uuid::new_v4();
        assert!(check_registration(&s, &r).is_err());
        Ok(())
    }
}
