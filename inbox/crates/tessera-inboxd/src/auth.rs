//! One-owner WebAuthn authentication. Challenges and sessions are deliberately
//! memory-only: server restart requires login, never replay of a pending ceremony.
use std::collections::{HashMap, VecDeque};

use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};
use tessera_inbox_domain::OwnerId;
use url::Url;
use uuid::Uuid;
use webauthn_rs::prelude::*;

use crate::store::Store;

pub const FLOW_SECONDS: i64 = 300;
pub const SESSION_SECONDS: i64 = 7 * 24 * 3600;
const MAX_FLOWS: usize = 128;
const MAX_SESSIONS: usize = 100;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("authentication required or ceremony expired")]
    Unauthorized,
    #[error("authentication request rejected")]
    Rejected,
    #[error("authentication rate limit reached")]
    Limited,
    #[error("owner is already enrolled")]
    Enrolled,
    #[error("configured HTTPS origin does not match this Inbox")]
    Origin,
    #[error("authentication storage failure")]
    Storage,
}
impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self::Storage
    }
}
impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self::Storage
    }
}

enum Ceremony {
    Register {
        state: PasskeyRegistration,
        bootstrap_hash: String,
    },
    Login(PasskeyAuthentication),
}
struct Flow {
    expires: i64,
    ceremony: Ceremony,
}

pub struct Auth {
    pub store: Store,
    pub origin: String,
    pub owner: OwnerId,
    webauthn: Webauthn,
    flows: HashMap<String, Flow>,
    sessions: HashMap<String, i64>,
    starts: VecDeque<i64>,
}
impl Auth {
    pub fn new(store: Store, origin: &str) -> Result<Self, Error> {
        let url = Url::parse(origin).map_err(|_| Error::Origin)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
            || url
                .host()
                .is_some_and(|h| !matches!(h, url::Host::Domain(_)))
        {
            return Err(Error::Origin);
        }
        let canonical = url.origin().ascii_serialization();
        let webauthn = WebauthnBuilder::new(url.host_str().unwrap(), &url)
            .map_err(|_| Error::Origin)?
            .rp_name("Tessera Inbox")
            .build()
            .map_err(|_| Error::Origin)?;
        let owner = OwnerId(Uuid::new_v4());
        store.connection.execute(
            "INSERT OR IGNORE INTO auth_owner(singleton,owner_id,origin) VALUES(1,?1,?2)",
            params![owner.0.to_string(), canonical],
        )?;
        let (id, saved): (String, String) = store.connection.query_row(
            "SELECT owner_id,origin FROM auth_owner WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if saved != canonical {
            return Err(Error::Origin);
        }
        let owner = OwnerId(Uuid::parse_str(&id).map_err(|_| Error::Storage)?);
        Ok(Self {
            store,
            origin: canonical,
            owner,
            webauthn,
            flows: HashMap::new(),
            sessions: HashMap::new(),
            starts: VecDeque::new(),
        })
    }

    /// Called only by local admin CLI. Rotation invalidates unfinished enrollment.
    /// Only the digest is persisted; the returned secret must not be logged.
    pub fn bootstrap(&mut self, now: i64) -> Result<String, Error> {
        if self.passkey()?.is_some() {
            return Err(Error::Enrolled);
        }
        let token = secret();
        let updated = self.store.connection.execute(
            "UPDATE auth_owner SET bootstrap_hash=?1,bootstrap_expires=?2 WHERE singleton=1 AND passkey IS NULL",
            params![digest(&token), now + 600],
        )?;
        if updated != 1 {
            return Err(Error::Enrolled);
        }
        Ok(token)
    }

    pub fn register_start(
        &mut self,
        token: &str,
        now: i64,
    ) -> Result<(String, CreationChallengeResponse), Error> {
        self.limit(now)?;
        let hashed = digest(token);
        self.check_bootstrap(&hashed, now)?;
        let (options, state) = self
            .webauthn
            .start_passkey_registration(self.owner.0, "owner", "Inbox owner", None)
            .map_err(|_| Error::Rejected)?;
        let flow = self.insert_flow(
            Ceremony::Register {
                state,
                bootstrap_hash: hashed,
            },
            now,
        );
        Ok((flow, options))
    }

    pub fn register_finish(
        &mut self,
        flow: &str,
        credential: &RegisterPublicKeyCredential,
        now: i64,
    ) -> Result<String, Error> {
        let Ceremony::Register {
            state,
            bootstrap_hash,
        } = self.take_flow(flow, now)?
        else {
            return Err(Error::Rejected);
        };
        self.check_bootstrap(&bootstrap_hash, now)?;
        let passkey = self
            .webauthn
            .finish_passkey_registration(credential, &state)
            .map_err(|_| Error::Rejected)?;
        let changed = self.store.connection.execute(
            "UPDATE auth_owner SET passkey=?1,bootstrap_hash=NULL,bootstrap_expires=NULL
             WHERE singleton=1 AND passkey IS NULL AND bootstrap_hash=?2 AND bootstrap_expires>?3",
            params![serde_json::to_string(&passkey)?, bootstrap_hash, now],
        )?;
        if changed != 1 {
            return Err(Error::Unauthorized);
        }
        Ok(self.session(now))
    }

    pub fn login_start(&mut self, now: i64) -> Result<(String, RequestChallengeResponse), Error> {
        self.limit(now)?;
        let key = self.passkey()?.ok_or(Error::Unauthorized)?;
        let (options, state) = self
            .webauthn
            .start_passkey_authentication(&[key])
            .map_err(|_| Error::Rejected)?;
        let flow = self.insert_flow(Ceremony::Login(state), now);
        Ok((flow, options))
    }

    pub fn login_finish(
        &mut self,
        flow: &str,
        credential: &PublicKeyCredential,
        now: i64,
    ) -> Result<String, Error> {
        let Ceremony::Login(state) = self.take_flow(flow, now)? else {
            return Err(Error::Rejected);
        };
        let result = self
            .webauthn
            .finish_passkey_authentication(credential, &state)
            .map_err(|_| Error::Rejected)?;
        let mut key = self.passkey()?.ok_or(Error::Unauthorized)?;
        if key.update_credential(&result).is_none() {
            return Err(Error::Rejected);
        }
        self.store.connection.execute(
            "UPDATE auth_owner SET passkey=?1 WHERE singleton=1",
            [serde_json::to_string(&key)?],
        )?;
        // Other pending login states contain older credential counters. Require
        // a fresh challenge rather than authenticate against stale snapshots.
        self.flows.clear();
        Ok(self.session(now))
    }

    pub fn authenticate(&mut self, token: &str, now: i64) -> Result<OwnerId, Error> {
        self.sessions.retain(|_, expiry| *expiry > now);
        if !self.sessions.contains_key(&digest(token)) {
            return Err(Error::Unauthorized);
        }
        Ok(self.owner)
    }
    pub fn logout(&mut self, token: &str) {
        self.sessions.remove(&digest(token));
    }

    fn passkey(&self) -> Result<Option<Passkey>, Error> {
        let value: Option<String> = self.store.connection.query_row(
            "SELECT passkey FROM auth_owner WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        value
            .map(|s| serde_json::from_str(&s).map_err(Error::from))
            .transpose()
    }
    fn check_bootstrap(&self, hash: &str, now: i64) -> Result<(), Error> {
        let found = self.store.connection.query_row(
            "SELECT 1 FROM auth_owner WHERE singleton=1 AND passkey IS NULL AND bootstrap_hash=?1 AND bootstrap_expires>?2",
            params![hash,now], |_|Ok(()),
        ).optional()?;
        found.ok_or(Error::Unauthorized)
    }
    fn limit(&mut self, now: i64) -> Result<(), Error> {
        self.flows.retain(|_, v| v.expires > now);
        while self.starts.front().is_some_and(|v| *v <= now - 60) {
            self.starts.pop_front();
        }
        if self.starts.len() >= 30 || self.flows.len() >= MAX_FLOWS {
            return Err(Error::Limited);
        }
        self.starts.push_back(now);
        Ok(())
    }
    fn insert_flow(&mut self, ceremony: Ceremony, now: i64) -> String {
        let token = secret();
        self.flows.insert(
            digest(&token),
            Flow {
                expires: now + FLOW_SECONDS,
                ceremony,
            },
        );
        token
    }
    fn take_flow(&mut self, token: &str, now: i64) -> Result<Ceremony, Error> {
        self.flows
            .remove(&digest(token))
            .filter(|v| v.expires > now)
            .map(|v| v.ceremony)
            .ok_or(Error::Unauthorized)
    }
    fn session(&mut self, now: i64) -> String {
        self.sessions.retain(|_, expiry| *expiry > now);
        if self.sessions.len() >= MAX_SESSIONS {
            if let Some(oldest) = self
                .sessions
                .iter()
                .min_by_key(|(_, t)| *t)
                .map(|(k, _)| k.clone())
            {
                self.sessions.remove(&oldest);
            }
        }
        let token = secret();
        self.sessions.insert(digest(&token), now + SESSION_SECONDS);
        token
    }
}
fn secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
