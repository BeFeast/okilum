//! Owner-authorized passkeys and short-lived device invitations.
//! Invitations/ceremonies expire on restart; they never grant a session themselves.
use crate::auth::{digest, secret, Auth, Ceremony, Error, FLOW_SECONDS};
use rusqlite::params;
use serde::Serialize;
use uuid::Uuid;
use webauthn_rs::prelude::*;

pub(crate) const SCHEMA: &str = "CREATE TABLE auth_passkeys(id TEXT PRIMARY KEY,name TEXT NOT NULL,passkey TEXT NOT NULL,created_at INTEGER NOT NULL,last_used INTEGER); INSERT INTO auth_passkeys(id,name,passkey,created_at) SELECT lower(hex(randomblob(16))),'Original passkey',passkey,0 FROM auth_owner WHERE passkey IS NOT NULL; UPDATE auth_owner SET passkey=NULL; PRAGMA user_version=11;";
const MAX_KEYS: usize = 20;
#[derive(Serialize)]
pub struct KeyInfo {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub last_used: Option<i64>,
    pub current: bool,
}
pub(crate) struct Invitation {
    pub id: String,
    pub creator_key: String,
    pub expires: i64,
    pub pending: Option<(String, Passkey)>,
    pub approved: bool,
}
#[derive(Serialize)]
pub struct InviteInfo {
    pub id: String,
    pub expires: i64,
    pub state: &'static str,
    pub name: Option<String>,
    pub code: Option<String>,
}
fn name(value: &str) -> Result<String, Error> {
    let v = value.trim();
    if v.is_empty() || v.len() > 100 || v.chars().any(char::is_control) {
        return Err(Error::InvalidName);
    }
    Ok(v.into())
}
impl Auth {
    pub(crate) fn recent(&mut self, token: &str, now: i64) -> Result<String, Error> {
        self.authenticate(token, now)?;
        let session = self
            .sessions
            .get(&digest(token))
            .ok_or(Error::Unauthorized)?;
        if now < session.verified || now - session.verified > FLOW_SECONDS {
            return Err(Error::RecentRequired);
        }
        Ok(session.key.clone())
    }
    pub fn passkeys(&mut self, token: &str, now: i64) -> Result<Vec<KeyInfo>, Error> {
        self.authenticate(token, now)?;
        let current = &self
            .sessions
            .get(&digest(token))
            .ok_or(Error::Unauthorized)?
            .key;
        let mut query = self.store.connection.prepare(
            "SELECT id,name,created_at,last_used FROM auth_passkeys ORDER BY created_at,id",
        )?;
        let rows = query.query_map([], |r| {
            let id: String = r.get(0)?;
            Ok(KeyInfo {
                current: &id == current,
                id,
                name: r.get(1)?,
                created_at: r.get(2)?,
                last_used: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
    fn new_key_options(&self) -> Result<(CreationChallengeResponse, PasskeyRegistration), Error> {
        let keys = self.keys()?;
        if keys.len() >= MAX_KEYS {
            return Err(Error::Limited);
        }
        self.webauthn
            .start_passkey_registration(
                self.owner.0,
                "owner",
                "Inbox owner",
                Some(keys.into_iter().map(|(_, k)| k.cred_id().clone()).collect()),
            )
            .map_err(|_| Error::Rejected)
    }
    fn insert_key(&mut self, label: &str, key: &Passkey, now: i64) -> Result<String, Error> {
        let keys = self.keys()?;
        if keys.len() >= MAX_KEYS {
            return Err(Error::Limited);
        }
        if keys.iter().any(|(_, k)| k.cred_id() == key.cred_id()) {
            return Err(Error::Rejected);
        }
        let id = Uuid::new_v4().to_string();
        self.store.connection.execute(
            "INSERT INTO auth_passkeys(id,name,passkey,created_at) VALUES(?1,?2,?3,?4)",
            params![id, label, serde_json::to_string(key)?, now],
        )?;
        Ok(id)
    }
    pub fn add_start(
        &mut self,
        token: &str,
        label: &str,
        now: i64,
    ) -> Result<(String, CreationChallengeResponse), Error> {
        self.recent(token, now)?;
        self.limit(now)?;
        let label = name(label)?;
        let (options, state) = self.new_key_options()?;
        let flow = self.insert_flow(
            Ceremony::Add {
                state,
                session_hash: digest(token),
                name: label,
            },
            now,
        );
        Ok((flow, options))
    }
    pub fn add_finish(
        &mut self,
        token: &str,
        flow: &str,
        credential: &RegisterPublicKeyCredential,
        now: i64,
    ) -> Result<String, Error> {
        self.recent(token, now)?;
        let Ceremony::Add {
            state,
            session_hash,
            name,
        } = self.take_flow(flow, now)?
        else {
            return Err(Error::Rejected);
        };
        if session_hash != digest(token) {
            return Err(Error::Unauthorized);
        }
        let key = self
            .webauthn
            .finish_passkey_registration(credential, &state)
            .map_err(|_| Error::Rejected)?;
        self.insert_key(&name, &key, now)
    }
    pub fn revoke(&mut self, token: &str, id: &str, now: i64) -> Result<(), Error> {
        self.recent(token, now)?;
        let tx = self.store.connection.transaction()?;
        let count: i64 = tx.query_row("SELECT count(*) FROM auth_passkeys", [], |r| r.get(0))?;
        if count <= 1 {
            return Err(Error::LastKey);
        }
        if tx.execute("DELETE FROM auth_passkeys WHERE id=?1", [id])? != 1 {
            return Err(Error::Rejected);
        }
        tx.commit()?;
        self.sessions.retain(|_, s| s.key != id);
        self.flows.clear();
        self.invitations.clear();
        Ok(())
    }
    pub fn invite_start(&mut self, token: &str, now: i64) -> Result<(String, InviteInfo), Error> {
        let key = self.recent(token, now)?;
        self.limit(now)?;
        self.invitations.retain(|_, i| i.expires > now);
        if self.invitations.len() >= 10 || self.keys()?.len() >= MAX_KEYS {
            return Err(Error::Limited);
        }
        let secret = secret();
        let invite = Invitation {
            id: Uuid::new_v4().to_string(),
            creator_key: key,
            expires: now + FLOW_SECONDS,
            pending: None,
            approved: false,
        };
        let info = invite.info();
        self.invitations.insert(digest(&secret), invite);
        Ok((secret, info))
    }
    pub fn invites(&mut self, token: &str, now: i64) -> Result<Vec<InviteInfo>, Error> {
        self.authenticate(token, now)?;
        self.invitations.retain(|_, i| i.expires > now);
        Ok(self.invitations.values().map(Invitation::info).collect())
    }
    pub fn device_start(
        &mut self,
        token: &str,
        now: i64,
    ) -> Result<(String, CreationChallengeResponse), Error> {
        self.limit(now)?;
        let hashed = digest(token);
        let i = self
            .invitations
            .get(&hashed)
            .filter(|i| i.expires > now && !i.approved && i.pending.is_none())
            .ok_or(Error::Unauthorized)?;
        if !self.keys()?.iter().any(|(id, _)| id == &i.creator_key) {
            return Err(Error::Unauthorized);
        }
        let (options, state) = self.new_key_options()?;
        Ok((
            self.insert_flow(
                Ceremony::Device {
                    state,
                    invite_hash: hashed,
                },
                now,
            ),
            options,
        ))
    }
    pub fn device_finish(
        &mut self,
        flow: &str,
        credential: &RegisterPublicKeyCredential,
        label: &str,
        now: i64,
    ) -> Result<InviteInfo, Error> {
        let label = name(label)?;
        let Ceremony::Device { state, invite_hash } = self.take_flow(flow, now)? else {
            return Err(Error::Rejected);
        };
        let key = self
            .webauthn
            .finish_passkey_registration(credential, &state)
            .map_err(|_| Error::Rejected)?;
        if self
            .keys()?
            .iter()
            .any(|(_, k)| k.cred_id() == key.cred_id())
        {
            return Err(Error::Rejected);
        }
        let i = self
            .invitations
            .get_mut(&invite_hash)
            .filter(|i| i.expires > now && !i.approved && i.pending.is_none())
            .ok_or(Error::Unauthorized)?;
        i.pending = Some((label, key));
        Ok(i.info())
    }
    pub fn device_status(&mut self, token: &str, now: i64) -> Result<InviteInfo, Error> {
        self.limit(now)?;
        Ok(self
            .invitations
            .get(&digest(token))
            .filter(|i| i.expires > now)
            .ok_or(Error::Unauthorized)?
            .info())
    }
    pub fn approve_device(
        &mut self,
        token: &str,
        id: &str,
        code: &str,
        now: i64,
    ) -> Result<(), Error> {
        self.recent(token, now)?;
        let hashed = self
            .invitations
            .iter()
            .find(|(_, i)| i.id == id && i.expires > now && !i.approved)
            .map(|(hash, _)| hash.clone())
            .ok_or(Error::Unauthorized)?;
        let i = self.invitations.get(&hashed).ok_or(Error::Unauthorized)?;
        if i.info().code.as_deref() != Some(code) {
            return Err(Error::Rejected);
        }
        let (label, key) = i.pending.clone().ok_or(Error::Rejected)?;
        self.insert_key(&label, &key, now)?;
        self.invitations
            .get_mut(&hashed)
            .ok_or(Error::Unauthorized)?
            .approved = true;
        Ok(())
    }
    pub fn cancel_invite(&mut self, token: &str, id: &str, now: i64) -> Result<(), Error> {
        self.authenticate(token, now)?;
        self.invitations.retain(|_, i| i.id != id);
        Ok(())
    }
}
impl Invitation {
    fn info(&self) -> InviteInfo {
        InviteInfo {
            id: self.id.clone(),
            expires: self.expires,
            state: if self.approved {
                "approved"
            } else if self.pending.is_some() {
                "confirm"
            } else {
                "waiting"
            },
            name: self.pending.as_ref().map(|(name, _)| name.clone()),
            code: self
                .pending
                .as_ref()
                .map(|(_, key)| digest(&format!("{:?}", key.cred_id()))[..12].to_uppercase()),
        }
    }
}
