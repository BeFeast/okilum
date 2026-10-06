//! Owner-reported publication records, not inferred from executor prose or CI.
use crate::{
    http::{blocking, now, token, ApiError, Shared},
    launch::State as LaunchState,
    store::{Error, Store},
};
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tessera_inbox_domain::OwnerId;
use uuid::Uuid;

pub(crate) const SCHEMA:&str="CREATE TABLE execution_results(sequence INTEGER PRIMARY KEY AUTOINCREMENT,owner_id TEXT NOT NULL,operation_id TEXT NOT NULL,project_id TEXT NOT NULL,body TEXT NOT NULL,UNIQUE(owner_id,operation_id),FOREIGN KEY(owner_id,project_id) REFERENCES execution_projects(owner_id,project_id)); CREATE TABLE execution_outputs(owner_id TEXT NOT NULL,launch_id TEXT NOT NULL,body TEXT NOT NULL,PRIMARY KEY(owner_id,launch_id),FOREIGN KEY(owner_id,launch_id) REFERENCES execution_launches(owner_id,operation_id)); PRAGMA user_version=10;";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub operation_id: Uuid,
    pub project_id: Uuid,
    pub launch_id: Uuid,
    pub run_id: String,
    pub commit: String,
    pub platform: String,
    pub channel: String,
    pub version: String,
    pub publication: Publication,
    pub url: String,
    pub what_to_check: String,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Publication {
    Published,
    Failed,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Record {
    pub report: Report,
    pub recorded_at: i64,
}
impl Report {
    fn validate(&self) -> bool {
        let bounded = |s: &str, n: usize| {
            !s.trim().is_empty() && s.len() <= n && !s.chars().any(|c| c == '\0')
        };
        !self.operation_id.is_nil()
            && !self.project_id.is_nil()
            && !self.launch_id.is_nil()
            && self.commit.len() == 40
            && self.commit.bytes().all(|c| c.is_ascii_hexdigit())
            && [&self.platform, &self.channel, &self.version]
                .iter()
                .all(|s| bounded(s, 128))
            && bounded(&self.run_id, 512)
            && bounded(&self.what_to_check, 8192)
            && self.url.len() <= 2048
            && url::Url::parse(&self.url).is_ok_and(|u| {
                u.scheme() == "https"
                    && u.host_str().is_some()
                    && u.username().is_empty()
                    && u.password().is_none()
            })
    }
}
impl Store {
    pub fn report_execution_result(
        &mut self,
        owner: OwnerId,
        r: &Report,
        at: i64,
    ) -> Result<Record, Error> {
        if !r.validate() || at < 0 {
            return Err(Error::InvalidExecutionTransition);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<String> = tx
            .query_row(
                "SELECT body FROM execution_results WHERE owner_id=?1 AND operation_id=?2",
                params![owner.0.to_string(), r.operation_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(body) = previous {
            let old: Record =
                serde_json::from_str(&body).map_err(|_| Error::InvalidStoredIdentity)?;
            return if old.report == *r {
                Ok(old)
            } else {
                Err(Error::OperationConflict)
            };
        }
        let body: Option<String> = tx
            .query_row(
                "SELECT body FROM execution_launches WHERE owner_id=?1 AND operation_id=?2",
                params![owner.0.to_string(), r.launch_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let op: crate::launch::Operation = serde_json::from_str(&body.ok_or(Error::MissingItem)?)
            .map_err(|_| Error::InvalidStoredIdentity)?;
        if op.brief.project_id != r.project_id
            || op.run_id.as_deref() != Some(&r.run_id)
            || !matches!(op.state, LaunchState::Completed | LaunchState::Failed)
        {
            return Err(Error::InvalidExecutionTransition);
        }
        let record = Record {
            report: r.clone(),
            recorded_at: at,
        };
        tx.execute("INSERT INTO execution_results(owner_id,operation_id,project_id,body) VALUES(?1,?2,?3,?4)",params![owner.0.to_string(),r.operation_id.to_string(),r.project_id.to_string(),serde_json::to_string(&record).map_err(|_|Error::InvalidStoredIdentity)?])?;
        tx.commit()?;
        Ok(record)
    }
    pub fn execution_results(
        &self,
        owner: OwnerId,
        project: Uuid,
        after: u64,
        limit: u32,
    ) -> Result<Vec<(u64, Record)>, Error> {
        if after > i64::MAX as u64 || limit == 0 || limit > 101 {
            return Err(Error::InvalidPage);
        }
        let mut q=self.connection.prepare("SELECT sequence,body FROM execution_results WHERE owner_id=?1 AND project_id=?2 AND sequence>?3 ORDER BY sequence LIMIT ?4")?;
        let rows = q.query_map(
            params![owner.0.to_string(), project.to_string(), after, limit],
            |r| Ok((r.get::<_, u64>(0)?, r.get::<_, String>(1)?)),
        )?;
        rows.map(|row| {
            let (s, b) = row?;
            Ok((
                s,
                serde_json::from_str(&b).map_err(|_| Error::InvalidStoredIdentity)?,
            ))
        })
        .collect()
    }
}
pub(crate) fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/v1/launches/{id}/output", get(output))
        .route("/api/v1/results", post(report))
        .route("/api/v1/projects/{id}/results", get(list))
}
async fn report(
    State(state): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Report>,
) -> Result<Response, ApiError> {
    let session = token(&headers, "__Host-inbox-session")?;
    blocking(state, move |auth| {
        let owner = auth.authenticate(&session, now())?;
        Ok(Json(auth.store.report_execution_result(owner, &body, now())?).into_response())
    })
    .await
}
#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    after: u64,
}
async fn list(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path(project): Path<Uuid>,
    Query(page): Query<Page>,
) -> Result<Response, ApiError> {
    let session = token(&headers, "__Host-inbox-session")?;
    blocking(state,move|auth|{let owner=auth.authenticate(&session,now())?;auth.store.execution_project(owner,project)?.ok_or(Error::MissingItem)?;
        let mut rows=auth.store.execution_results(owner,project,page.after,101)?;let more=rows.len()>100;rows.truncate(100);let cursor=rows.last().map(|r|r.0).unwrap_or(page.after);
        Ok(Json(json!({"results":rows.into_iter().map(|r|r.1).collect::<Vec<_>>(),"has_more":more,"next_cursor":cursor})).into_response())}).await
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Output {
    pub run_id: String,
    pub message_id: String,
    pub text: String,
}
impl Store {
    pub fn execution_output(&self, owner: OwnerId, launch: Uuid) -> Result<Option<Output>, Error> {
        let body: Option<String> = self
            .connection
            .query_row(
                "SELECT body FROM execution_outputs WHERE owner_id=?1 AND launch_id=?2",
                params![owner.0.to_string(), launch.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        body.map(|b| serde_json::from_str(&b).map_err(|_| Error::InvalidStoredIdentity))
            .transpose()
    }
    pub fn record_execution_output(
        &mut self,
        owner: OwnerId,
        launch: Uuid,
        output: &Output,
    ) -> Result<Output, Error> {
        if output.text.is_empty()
            || output.text.len() > 65536
            || output.message_id.is_empty()
            || output.message_id.len() > 512
        {
            return Err(Error::InvalidExecutionTransition);
        }
        let op = self
            .execution_launch(owner, launch)?
            .ok_or(Error::MissingItem)?;
        if op.state != LaunchState::Completed || op.run_id.as_deref() != Some(&output.run_id) {
            return Err(Error::InvalidExecutionTransition);
        }
        let body = serde_json::to_string(output).map_err(|_| Error::InvalidStoredIdentity)?;
        self.connection.execute("INSERT INTO execution_outputs(owner_id,launch_id,body) VALUES(?1,?2,?3) ON CONFLICT(owner_id,launch_id) DO NOTHING",params![owner.0.to_string(),launch.to_string(),body])?;
        let old = self
            .execution_output(owner, launch)?
            .ok_or(Error::MissingItem)?;
        if old != *output {
            return Err(Error::OperationConflict);
        }
        Ok(old)
    }
}
async fn output(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let session = token(&headers, "__Host-inbox-session")?;
    blocking(state, move |auth| {
        let owner = auth.authenticate(&session, now())?;
        auth.store
            .execution_launch(owner, id)?
            .ok_or(Error::MissingItem)?;
        Ok(Json(json!({"output":auth.store.execution_output(owner,id)?})).into_response())
    })
    .await
}
