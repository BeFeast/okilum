//! Durable desktop pairing, separate from browser credentials and local daemon lifecycle.
use crate::{
    auth::{self, digest, Auth},
    store::Store,
};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

pub(crate) const SCHEMA:&str="
CREATE TABLE sync_scopes(owner TEXT NOT NULL,vault TEXT NOT NULL,binding TEXT NOT NULL,PRIMARY KEY(owner,vault));
CREATE TABLE sync_requests(id TEXT PRIMARY KEY,owner TEXT NOT NULL,device TEXT NOT NULL,name TEXT NOT NULL,verifier_hash TEXT NOT NULL,grant_hash TEXT NOT NULL,created INTEGER NOT NULL,expires INTEGER NOT NULL,state TEXT NOT NULL,vault TEXT,approved_by TEXT);
CREATE TABLE sync_grants(id TEXT PRIMARY KEY,owner TEXT NOT NULL,vault TEXT NOT NULL,device TEXT NOT NULL,name TEXT NOT NULL,grant_hash TEXT NOT NULL UNIQUE,state TEXT NOT NULL,last_error TEXT);
CREATE UNIQUE INDEX sync_live_device ON sync_grants(owner,vault,device) WHERE state<>'revoked';
CREATE INDEX sync_requests_expiry ON sync_requests(expires);
CREATE INDEX sync_requests_created ON sync_requests(created);
PRAGMA user_version=12;";
const TTL: i64 = 600;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sync request rejected")]
    Invalid,
    #[error("sync request expired or unavailable")]
    Missing,
    #[error("sync request conflicts with durable state")]
    Conflict,
    #[error("too many pending sync requests")]
    Limited,
    #[error("sync history capacity reached")]
    Capacity,
    #[error("sync storage unavailable")]
    Storage(#[from] rusqlite::Error),
    #[error(transparent)]
    Auth(#[from] auth::Error),
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vault {
    pub id: Uuid,
    pub name: String,
    pub folder_id: String,
    pub hub_device_id: String,
    pub hub_address: String,
    pub ignores: Vec<String>,
    pub adapter_socket: PathBuf,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub owner_id: Uuid,
    pub vaults: Vec<Vault>,
}
impl Config {
    pub fn bind(&self, store: &mut Store) -> Result<(), Error> {
        let rows = store.sync_registrations(self.owner_id)?;
        for row in rows {
            self.vault(self.owner_id, row.vault)?;
        }
        let tx = store.connection.transaction()?;
        for v in &self.vaults {
            let binding = serde_json::json!([
                v.folder_id,
                v.hub_device_id,
                v.hub_address,
                v.ignores,
                v.adapter_socket
            ])
            .to_string();
            let saved: Option<String> = tx
                .query_row(
                    "SELECT binding FROM sync_scopes WHERE owner=?1 AND vault=?2",
                    params![self.owner_id.to_string(), v.id.to_string()],
                    |r| r.get(0),
                )
                .optional()?;
            if saved.as_ref().is_some_and(|s| s != &binding) {
                return Err(Error::Conflict);
            }
            tx.execute(
                "INSERT OR IGNORE INTO sync_scopes VALUES(?1,?2,?3)",
                params![self.owner_id.to_string(), v.id.to_string(), binding],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn load(path: &Path, owner: Uuid) -> anyhow::Result<Self> {
        #[cfg(unix)]
        crate::sync_hub::private_file(path)?;
        let config: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        anyhow::ensure!(
            config.owner_id == owner && !owner.is_nil() && !config.vaults.is_empty(),
            "sync owner/scope mismatch"
        );
        let mut ids = std::collections::HashSet::new();
        for v in &config.vaults {
            anyhow::ensure!(
                !v.id.is_nil()
                    && ids.insert(v.id)
                    && !v.name.is_empty()
                    && v.name.len() <= 100
                    && valid_device(&v.hub_device_id)
                    && !v.folder_id.is_empty()
                    && v.adapter_socket.is_absolute(),
                "invalid sync vault"
            );
            let address = url::Url::parse(&v.hub_address)?;
            anyhow::ensure!(
                address.scheme() == "tcp"
                    && address.host_str().is_some()
                    && address.port().is_some()
                    && address.username().is_empty()
                    && address.password().is_none()
                    && address.query().is_none()
                    && address.fragment().is_none(),
                "invalid hub transport address"
            );
        }
        Ok(config)
    }
    pub(crate) fn vault(&self, owner: Uuid, id: Uuid) -> Result<&Vault, Error> {
        if self.owner_id != owner {
            return Err(Error::Invalid);
        }
        self.vaults
            .iter()
            .find(|v| v.id == id)
            .ok_or(Error::Invalid)
    }
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Start {
    pub id: Uuid,
    pub device_id: String,
    pub name: String,
    pub verifier_challenge: String,
    pub grant_challenge: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exchange {
    pub id: Uuid,
    pub verifier: String,
    pub grant_secret: String,
}
#[derive(Clone, Serialize)]
pub struct Pairing {
    pub id: Uuid,
    pub device_id: String,
    pub name: String,
    pub code: String,
    pub state: String,
    pub expires: i64,
    pub vault: Option<Uuid>,
}
#[derive(Clone, Serialize)]
pub struct Registration {
    pub id: Uuid,
    pub vault: Uuid,
    pub device_id: String,
    pub name: String,
    pub state: String,
    pub last_error: Option<String>,
}
#[derive(Serialize)]
pub struct DesktopStatus {
    pub registration: Registration,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hub_device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hub_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignores: Option<Vec<String>>,
}
fn secret_shape(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
pub fn valid_device(s: &str) -> bool {
    s.len() == 63
        && s.split('-').count() == 8
        && s.split('-').all(|g| {
            g.len() == 7
                && g.bytes()
                    .all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b))
        })
}
fn pairing(row: &rusqlite::Row<'_>) -> rusqlite::Result<Pairing> {
    let id: String = row.get(0)?;
    let device: String = row.get(1)?;
    let challenge: String = row.get(6)?;
    let code = digest(&format!("{id}:{device}:{challenge}"))[..8].to_uppercase();
    Ok(Pairing {
        id: Uuid::parse_str(&id).map_err(|_| rusqlite::Error::InvalidQuery)?,
        device_id: device,
        name: row.get(2)?,
        state: row.get(3)?,
        expires: row.get(4)?,
        vault: row
            .get::<_, Option<String>>(5)?
            .map(|v| Uuid::parse_str(&v).map_err(|_| rusqlite::Error::InvalidQuery))
            .transpose()?,
        code,
    })
}
fn registration(row: &rusqlite::Row<'_>) -> rusqlite::Result<Registration> {
    Ok(Registration {
        id: Uuid::parse_str(&row.get::<_, String>(0)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        vault: Uuid::parse_str(&row.get::<_, String>(1)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        device_id: row.get(2)?,
        name: row.get(3)?,
        state: row.get(4)?,
        last_error: row.get(5)?,
    })
}
impl Store {
    pub fn sync_start(&mut self, owner: Uuid, request: &Start, now: i64) -> Result<Pairing, Error> {
        if owner.is_nil()
            || request.id.is_nil()
            || !valid_device(&request.device_id)
            || request.name.trim() != request.name
            || request.name.is_empty()
            || request.name.len() > 100
            || request.name.chars().any(char::is_control)
            || !secret_shape(&request.verifier_challenge)
            || !secret_shape(&request.grant_challenge)
            || request.verifier_challenge == request.grant_challenge
            || now < 0
        {
            return Err(Error::Invalid);
        }
        let existing: Option<(String, String, String, String, String)> = self
            .connection
            .query_row(
                "SELECT owner,device,name,verifier_hash,grant_hash FROM sync_requests WHERE id=?1",
                [request.id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        if let Some(saved) = existing {
            if saved
                != (
                    owner.to_string(),
                    request.device_id.clone(),
                    request.name.clone(),
                    request.verifier_challenge.clone(),
                    request.grant_challenge.clone(),
                )
            {
                return Err(Error::Conflict);
            }
            return self.sync_pairing(owner, request.id, now);
        }
        let total: i64 =
            self.connection
                .query_row("SELECT count(*) FROM sync_requests", [], |r| r.get(0))?;
        if total >= 10_000 {
            return Err(Error::Capacity);
        }
        let count:i64=self.connection.query_row("SELECT count(*) FROM sync_requests WHERE expires>?1 AND state IN ('requested','approved')",[now],|r|r.get(0))?;
        let recent: i64 = self.connection.query_row(
            "SELECT count(*) FROM sync_requests WHERE created>?1",
            [now - 60],
            |r| r.get(0),
        )?;
        if count >= 128 || recent >= 30 {
            return Err(Error::Limited);
        }
        self.connection.execute(
            "INSERT INTO sync_requests VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'requested',NULL,NULL)",
            params![
                request.id.to_string(),
                owner.to_string(),
                request.device_id,
                request.name,
                request.verifier_challenge,
                request.grant_challenge,
                now,
                now + TTL
            ],
        )?;
        self.sync_pairing(owner, request.id, now)
    }
    pub fn sync_pairing(&self, owner: Uuid, id: Uuid, now: i64) -> Result<Pairing, Error> {
        self.connection.query_row("SELECT id,device,name,state,expires,vault,verifier_hash FROM sync_requests WHERE id=?1 AND owner=?2 AND expires>?3",params![id.to_string(),owner.to_string(),now],pairing).optional()?.ok_or(Error::Missing)
    }
    pub fn sync_exchange(
        &mut self,
        owner: Uuid,
        b: &Exchange,
        now: i64,
    ) -> Result<Option<Registration>, Error> {
        if !secret_shape(&b.verifier) || !secret_shape(&b.grant_secret) {
            return Err(Error::Invalid);
        }
        let p = self.sync_pairing(owner, b.id, now)?;
        let valid: bool = self.connection.query_row(
            "SELECT verifier_hash=?2 AND grant_hash=?3 FROM sync_requests WHERE id=?1",
            params![
                b.id.to_string(),
                digest(&b.verifier),
                digest(&b.grant_secret)
            ],
            |r| r.get(0),
        )?;
        if !valid {
            return Err(Error::Missing);
        }
        match p.state.as_str() {
            "requested" => return Ok(None),
            "approved" | "exchanged" => {}
            _ => return Err(Error::Missing),
        }
        let tx = self.connection.transaction()?;
        if p.state == "approved" {
            let total: i64 = tx.query_row(
                "SELECT count(*) FROM sync_grants WHERE owner=?1",
                [owner.to_string()],
                |r| r.get(0),
            )?;
            if total >= 100 {
                return Err(Error::Capacity);
            }
            let duplicate:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sync_grants WHERE owner=?1 AND vault=?2 AND device=?3 AND state<>'revoked')",params![owner.to_string(),p.vault.ok_or(Error::Invalid)?.to_string(),p.device_id],|r|r.get(0))?;
            if duplicate {
                return Err(Error::Conflict);
            }
            tx.execute(
                "INSERT INTO sync_grants VALUES(?1,?2,?3,?4,?5,?6,'provisioning',NULL)",
                params![
                    b.id.to_string(),
                    owner.to_string(),
                    p.vault.ok_or(Error::Invalid)?.to_string(),
                    p.device_id,
                    p.name,
                    digest(&b.grant_secret)
                ],
            )?;
            tx.execute(
                "UPDATE sync_requests SET state='exchanged' WHERE id=?1",
                [b.id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(Some(self.sync_grant(owner, &b.grant_secret)?))
    }
    pub fn sync_grant(&self, owner: Uuid, secret: &str) -> Result<Registration, Error> {
        if !secret_shape(secret) {
            return Err(Error::Missing);
        }
        self.connection.query_row("SELECT id,vault,device,name,state,last_error FROM sync_grants WHERE owner=?1 AND grant_hash=?2",params![owner.to_string(),digest(secret)],registration).optional()?.ok_or(Error::Missing)
    }
    pub fn sync_pending(&self, owner: Uuid, now: i64) -> Result<Vec<Pairing>, Error> {
        let mut q = self.connection.prepare("SELECT id,device,name,state,expires,vault,verifier_hash FROM sync_requests WHERE owner=?1 AND expires>?2 AND state IN ('requested','approved') ORDER BY created DESC")?;
        let rows = q
            .query_map(params![owner.to_string(), now], pairing)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
    pub fn sync_registrations(&self, owner: Uuid) -> Result<Vec<Registration>, Error> {
        let mut q=self.connection.prepare("SELECT id,vault,device,name,state,last_error FROM sync_grants WHERE owner=?1 ORDER BY rowid")?;
        let rows = q
            .query_map([owner.to_string()], registration)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
    pub fn sync_remove(&mut self, owner: Uuid, id: Uuid) -> Result<String, Error> {
        let n=self.connection.execute("UPDATE sync_grants SET state=CASE WHEN state='revoked' THEN state ELSE 'removal_pending' END,last_error=NULL WHERE owner=?1 AND id=?2",params![owner.to_string(),id.to_string()])?;
        if n != 1 {
            return Err(Error::Missing);
        }
        Ok(self.connection.query_row(
            "SELECT state FROM sync_grants WHERE owner=?1 AND id=?2",
            params![owner.to_string(), id.to_string()],
            |r| r.get(0),
        )?)
    }
    pub fn sync_observed(
        &mut self,
        owner: Uuid,
        id: Uuid,
        desired: &str,
        observed: Option<&str>,
    ) -> Result<(), Error> {
        let state = match (desired, observed) {
            ("provisioning" | "hub_ready", Some("hub_ready")) => "hub_ready",
            ("removal_pending" | "revoked", Some("revoked")) => "revoked",
            ("hub_ready", None) => "provisioning",
            ("hub_ready", Some("provisioning")) => "provisioning",
            ("revoked", Some("removal_pending")) => "removal_pending",
            _ => desired,
        };
        self.connection.execute(
            "UPDATE sync_grants SET state=?1,last_error=?2 WHERE id=?3 AND owner=?4 AND state=?5 AND (state<>'revoked' OR NOT EXISTS (SELECT 1 FROM sync_grants newer WHERE newer.owner=sync_grants.owner AND newer.vault=sync_grants.vault AND newer.device=sync_grants.device AND newer.state<>'revoked'))",
            params![
                state,
                if observed.is_none() {
                    Some("hub_unavailable_or_conflict")
                } else {
                    None
                },
                id.to_string(),
                owner.to_string(),
                desired
            ],
        )?;
        Ok(())
    }
}
impl Auth {
    pub fn sync_approve(
        &mut self,
        config: &Config,
        token: &str,
        id: Uuid,
        vault: Uuid,
        code: &str,
        now: i64,
    ) -> Result<Pairing, Error> {
        let key = self.recent(token, now)?;
        config.vault(self.owner.0, vault)?;
        let p = self.store.sync_pairing(self.owner.0, id, now)?;
        if p.code != code {
            return Err(Error::Invalid);
        }
        if p.state == "approved" && p.vault == Some(vault) {
            return Ok(p);
        }
        if p.state != "requested" {
            return Err(Error::Conflict);
        }
        if config.vault(self.owner.0, vault)?.hub_device_id == p.device_id {
            return Err(Error::Invalid);
        }
        self.store.connection.execute("UPDATE sync_requests SET state='approved',vault=?1,approved_by=?2 WHERE owner=?3 AND id=?4 AND state='requested'",params![vault.to_string(),key,self.owner.0.to_string(),id.to_string()])?;
        self.store.sync_pairing(self.owner.0, id, now)
    }
    pub fn sync_cancel(&mut self, token: &str, id: Uuid, now: i64) -> Result<(), Error> {
        self.recent(token, now)?;
        let p = self.store.sync_pairing(self.owner.0, id, now)?;
        if p.state == "exchanged" {
            self.store.sync_remove(self.owner.0, id)?;
        }
        self.store.connection.execute(
            "UPDATE sync_requests SET state='cancelled' WHERE owner=?1 AND id=?2",
            params![self.owner.0.to_string(), id.to_string()],
        )?;
        Ok(())
    }
}
pub fn desktop_status(
    store: &Store,
    config: &Config,
    secret: &str,
) -> Result<DesktopStatus, Error> {
    let registration = store.sync_grant(config.owner_id, secret)?;
    let v = config.vault(config.owner_id, registration.vault)?;
    let ready = registration.state == "hub_ready";
    Ok(DesktopStatus {
        registration,
        folder_id: ready.then(|| v.folder_id.clone()),
        hub_device_id: ready.then(|| v.hub_device_id.clone()),
        hub_address: ready.then(|| v.hub_address.clone()),
        ignores: ready.then(|| v.ignores.clone()),
    })
}
pub fn start_worker(shared: Arc<Mutex<Auth>>, config: Arc<Config>) {
    tokio::spawn(async move {
        loop {
            let shared = shared.clone();
            let config = config.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let rows = match shared.lock() {
                    Ok(a) => match a.store.sync_registrations(config.owner_id) {
                        Ok(rows) => rows,
                        Err(_) => {
                            eprintln!("sync_reconciliation_storage_unavailable");
                            return;
                        }
                    },
                    Err(_) => return,
                };
                for row in rows {
                    let Ok(vault) = config.vault(config.owner_id, row.vault) else {
                        continue;
                    };
                    #[cfg(unix)]
                    let observed = {
                        use crate::sync_hub::{Action, Request};
                        let action = match row.state.as_str() {
                            "removal_pending" => Action::Remove,
                            "hub_ready" | "revoked" => Action::Status,
                            _ => Action::Add,
                        };
                        crate::sync_hub::call(
                            &vault.adapter_socket,
                            &Request {
                                owner_id: config.owner_id,
                                vault_id: row.vault,
                                registration_id: row.id,
                                device_id: row.device_id,
                                action,
                            },
                        )
                        .ok()
                        .map(|r| r.state)
                    };
                    #[cfg(not(unix))]
                    let observed = {
                        let _ = vault;
                        None::<String>
                    };
                    if let Ok(mut a) = shared.lock() {
                        let _ = a.store.sync_observed(
                            config.owner_id,
                            row.id,
                            &row.state,
                            observed.as_deref(),
                        );
                    }
                }
            })
            .await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}
