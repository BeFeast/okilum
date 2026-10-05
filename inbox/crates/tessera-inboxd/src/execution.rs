//! Canonical execution drafts and intents, outside the vault and derived cache.
use crate::store::{Error, Store};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{de::DeserializeOwned, Serialize};
use tessera_inbox_domain::{
    execution::{Brief, Project, SaveBrief, SaveProject},
    OwnerId,
};
use uuid::Uuid;

pub(crate) const SCHEMA: &str = "
CREATE TABLE execution_projects (
 owner_id TEXT NOT NULL, project_id TEXT NOT NULL, revision INTEGER NOT NULL,
 body TEXT NOT NULL, PRIMARY KEY(owner_id,project_id)
);
CREATE TABLE execution_briefs (
 owner_id TEXT NOT NULL, brief_id TEXT NOT NULL, project_id TEXT NOT NULL,
 revision INTEGER NOT NULL, body TEXT NOT NULL,
 PRIMARY KEY(owner_id,brief_id,revision),
 FOREIGN KEY(owner_id,project_id) REFERENCES execution_projects(owner_id,project_id)
);
CREATE TABLE execution_mutations (
 owner_id TEXT NOT NULL, operation_id TEXT NOT NULL, kind TEXT NOT NULL,
 request TEXT NOT NULL, response TEXT NOT NULL,
 PRIMARY KEY(owner_id,operation_id)
);
PRAGMA user_version=6;";

fn json(value: &impl Serialize) -> Result<String, Error> {
    serde_json::to_string(value).map_err(|_| Error::InvalidStoredIdentity)
}
fn decode<T: DeserializeOwned>(value: String) -> Result<T, Error> {
    serde_json::from_str(&value).map_err(|_| Error::InvalidStoredIdentity)
}
fn owner(owner: OwnerId) -> Result<String, Error> {
    if owner.0.is_nil() {
        return Err(Error::InvalidOwner);
    }
    Ok(owner.0.to_string())
}
fn replay<T: DeserializeOwned>(
    db: &Connection,
    owner: &str,
    operation: Uuid,
    kind: &str,
    request: &str,
) -> Result<Option<T>, Error> {
    let old: Option<(String,String,String)> = db.query_row(
        "SELECT kind,request,response FROM execution_mutations WHERE owner_id=?1 AND operation_id=?2",
        params![owner,operation.to_string()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    match old {
        Some((k, body, response)) if k == kind && body == request => Ok(Some(decode(response)?)),
        Some(_) => Err(Error::OperationConflict),
        None => Ok(None),
    }
}
fn record(
    db: &Connection,
    owner: &str,
    operation: Uuid,
    kind: &str,
    request: &str,
    response: &str,
) -> Result<(), Error> {
    db.execute("INSERT INTO execution_mutations(owner_id,operation_id,kind,request,response) VALUES(?1,?2,?3,?4,?5)",params![owner,operation.to_string(),kind,request,response])?;
    Ok(())
}
impl Store {
    pub fn latest_execution_brief(&self, who: OwnerId, id: Uuid) -> Result<Option<Brief>, Error> {
        let who = owner(who)?;
        let body: Option<String> = self.connection.query_row("SELECT body FROM execution_briefs WHERE owner_id=?1 AND brief_id=?2 ORDER BY revision DESC LIMIT 1", params![who,id.to_string()], |r| r.get(0)).optional()?;
        body.map(decode).transpose()
    }

    pub fn execution_project(&self, who: OwnerId, id: Uuid) -> Result<Option<Project>, Error> {
        let who = owner(who)?;
        let body: Option<String> = self
            .connection
            .query_row(
                "SELECT body FROM execution_projects WHERE owner_id=?1 AND project_id=?2",
                params![who, id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        body.map(decode).transpose()
    }
    pub fn execution_projects(
        &self,
        who: OwnerId,
        after: &str,
        limit: u32,
    ) -> Result<Vec<Project>, Error> {
        let who = owner(who)?;
        if limit == 0 || limit > 100 || (!after.is_empty() && Uuid::parse_str(after).is_err()) {
            return Err(Error::InvalidPage);
        }
        let mut q=self.connection.prepare("SELECT body FROM execution_projects WHERE owner_id=?1 AND project_id>?2 ORDER BY project_id LIMIT ?3")?;
        let rows = q.query_map(params![who, after, limit], |r| r.get::<_, String>(0))?;
        rows.map(|r| decode(r?)).collect()
    }
    pub fn save_execution_project(
        &mut self,
        who: OwnerId,
        request: &SaveProject,
    ) -> Result<Project, Error> {
        let who = owner(who)?;
        request.validate()?;
        let body = json(request)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(old) = replay(&tx, &who, request.operation_id, "project", &body)? {
            return Ok(old);
        }
        let revision: Option<u64> = tx
            .query_row(
                "SELECT revision FROM execution_projects WHERE owner_id=?1 AND project_id=?2",
                params![who, request.project_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if revision.unwrap_or(0) != request.expected_revision {
            return Err(Error::ExecutionRevisionConflict);
        }
        let project = Project {
            id: request.project_id,
            revision: request.expected_revision + 1,
            draft: request.draft.clone(),
        };
        let response = json(&project)?;
        tx.execute("INSERT INTO execution_projects(owner_id,project_id,revision,body) VALUES(?1,?2,?3,?4) ON CONFLICT(owner_id,project_id) DO UPDATE SET revision=excluded.revision,body=excluded.body",params![who,request.project_id.to_string(),project.revision,response])?;
        record(&tx, &who, request.operation_id, "project", &body, &response)?;
        tx.commit()?;
        Ok(project)
    }
    /// Explicit revision reads let a pending launch pin its original brief.
    pub fn execution_brief(
        &self,
        who: OwnerId,
        id: Uuid,
        revision: u64,
    ) -> Result<Option<Brief>, Error> {
        let who = owner(who)?;
        if revision == 0 || revision > i64::MAX as u64 {
            return Err(Error::ExecutionRevisionConflict);
        }
        let body: Option<String>=self.connection.query_row("SELECT body FROM execution_briefs WHERE owner_id=?1 AND brief_id=?2 AND revision=?3",params![who,id.to_string(),revision],|r|r.get(0)).optional()?;
        body.map(decode).transpose()
    }
    pub fn save_execution_brief(
        &mut self,
        who: OwnerId,
        request: &SaveBrief,
    ) -> Result<Brief, Error> {
        let who = owner(who)?;
        request.validate()?;
        let body = json(request)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(old) = replay(&tx, &who, request.operation_id, "brief", &body)? {
            return Ok(old);
        }
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM execution_projects WHERE owner_id=?1 AND project_id=?2)",
            params![who, request.project_id.to_string()],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::MissingItem);
        }
        let old: Option<(String,u64)>=tx.query_row("SELECT project_id,revision FROM execution_briefs WHERE owner_id=?1 AND brief_id=?2 ORDER BY revision DESC LIMIT 1",params![who,request.brief_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if old
            .as_ref()
            .is_some_and(|(project, _)| project != &request.project_id.to_string())
        {
            return Err(Error::OperationConflict);
        }
        if old.map(|(_, rev)| rev).unwrap_or(0) != request.expected_revision {
            return Err(Error::ExecutionRevisionConflict);
        }
        let brief = Brief {
            id: request.brief_id,
            project_id: request.project_id,
            revision: request.expected_revision + 1,
            title: request.title.clone(),
            text: request.text.clone(),
            target_id: request.target_id.clone(),
        };
        let response = json(&brief)?;
        tx.execute("INSERT INTO execution_briefs(owner_id,brief_id,project_id,revision,body) VALUES(?1,?2,?3,?4,?5)",params![who,request.brief_id.to_string(),request.project_id.to_string(),brief.revision,response])?;
        record(&tx, &who, request.operation_id, "brief", &body, &response)?;
        tx.commit()?;
        Ok(brief)
    }
}
