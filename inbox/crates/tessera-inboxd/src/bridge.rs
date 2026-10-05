//! Optional machine-only question transport. No source calls or launch authority.
use crate::{
    http::{blocking, guard, ApiError, Shared},
    questions::ReplyOperation,
};
use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{path::Path as FilePath, sync::Arc};
use tessera_inbox_domain::{execution::*, OwnerId};
use uuid::Uuid;

/// Operator provisioned; browser project metadata never grants connector access.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub owner_id: Uuid,
    pub project_id: Uuid,
    pub instance_id: String,
    pub source_project_id: String,
    pub ingest: bool,
    pub replies: bool,
}
impl Scope {
    fn permits(&self, question: &Question) -> bool {
        question.project_id == self.project_id
            && question.source.kind == SourceKind::T3
            && question.source.instance_id == self.instance_id
            && question.source.project_id == self.source_project_id
    }
    fn validate(&self) -> bool {
        !self.owner_id.is_nil()
            && !self.project_id.is_nil()
            && (self.ingest || self.replies)
            && [&self.instance_id, &self.source_project_id]
                .into_iter()
                .all(|s| !s.trim().is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
    }
}
// Deliberately no Debug/Serialize: credentials cannot leak through logs or API.
pub struct Bridge {
    scope: Scope,
    token_hash: [u8; 32],
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    scope: Scope,
    token: String,
}
impl Bridge {
    pub fn from_credential(path: &FilePath, owner: OwnerId) -> Result<Self, &'static str> {
        let meta = std::fs::symlink_metadata(path).map_err(|_| "bridge credential unavailable")?;
        if !meta.is_file() || meta.len() > 8192 {
            return Err("bridge credential must be a small regular file");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o077 != 0 {
                return Err("bridge credential must be private");
            }
        }
        let bytes = std::fs::read(path).map_err(|_| "bridge credential unavailable")?;
        let credential: Credential =
            serde_json::from_slice(&bytes).map_err(|_| "invalid bridge configuration")?;
        if credential.scope.owner_id != owner.0
            || !credential.scope.validate()
            || credential.token.len() != 64
            || !credential.token.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err("invalid bridge configuration");
        }
        Ok(Self {
            scope: credential.scope,
            token_hash: Sha256::digest(credential.token.as_bytes()).into(),
        })
    }
    fn authenticate(&self, headers: &HeaderMap, write_reply: bool) -> Result<Scope, ApiError> {
        // Machine transport must never accept ambient browser credentials.
        if headers.contains_key(header::COOKIE) || headers.contains_key(header::ORIGIN) {
            return Err(ApiError(StatusCode::FORBIDDEN, "bridge_browser_rejected"));
        }
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
            .filter(|s| s.len() == 64)
            .ok_or(ApiError(
                StatusCode::UNAUTHORIZED,
                "bridge_authentication_failed",
            ))?;
        let digest = Sha256::digest(token.as_bytes());
        let difference = digest
            .iter()
            .zip(self.token_hash)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b));
        if difference != 0 {
            return Err(ApiError(
                StatusCode::UNAUTHORIZED,
                "bridge_authentication_failed",
            ));
        }
        if if write_reply {
            !self.scope.replies
        } else {
            !self.scope.ingest
        } {
            return Err(ApiError(StatusCode::FORBIDDEN, "bridge_scope_rejected"));
        }
        Ok(self.scope.clone())
    }
}
#[derive(Clone)]
struct BridgeState {
    auth: Shared,
    bridge: Arc<Bridge>,
}
pub(crate) fn router(auth: Shared, bridge: Bridge) -> Router {
    Router::new()
        .route("/api/bridge/v1/questions", post(observe))
        .route("/api/bridge/v1/replies", get(replies))
        .route("/api/bridge/v1/replies/{id}", get(reply).post(advance))
        .layer(DefaultBodyLimit::max(128 * 1024))
        .layer(middleware::from_fn_with_state(None::<String>, guard))
        .with_state(BridgeState {
            auth,
            bridge: Arc::new(bridge),
        })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    question: Question,
    sequence: u64,
}
async fn observe(
    State(state): State<BridgeState>,
    headers: HeaderMap,
    Json(body): Json<Observation>,
) -> Result<Response, ApiError> {
    let scope = state.bridge.authenticate(&headers, false)?;
    if !scope.permits(&body.question) {
        return Err(ApiError(StatusCode::FORBIDDEN, "bridge_scope_rejected"));
    }
    blocking(state.auth, move |auth| {
        auth.store.observe_execution_question(
            OwnerId(scope.owner_id),
            &body.question,
            body.sequence,
        )?;
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    #[serde(default)]
    after: u64,
    #[serde(default = "page_size")]
    limit: u32,
}
fn page_size() -> u32 {
    50
}
#[derive(Serialize)]
struct ReplyPage {
    operations: Vec<ReplyOperation>,
    next_cursor: u64,
    has_more: bool,
}
async fn replies(
    State(state): State<BridgeState>,
    headers: HeaderMap,
    Query(page): Query<Page>,
) -> Result<Response, ApiError> {
    let scope = state.bridge.authenticate(&headers, true)?;
    if page.limit == 0 || page.limit > 100 || page.after > i64::MAX as u64 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_page"));
    }
    blocking(state.auth, move |auth| {
        // Include terminal rows: a restored bridge must reconcile history, not
        // interpret an absent row as permission to resubmit an external action.
        let mut query = auth
            .store
            .connection
            .prepare(
                "SELECT r.sequence,r.operation_id FROM execution_replies r
             JOIN execution_questions q ON r.owner_id=q.owner_id AND r.question_id=q.question_id
             WHERE r.owner_id=?1 AND q.project_id=?2 AND r.sequence>?3
             AND json_extract(q.body,'$.source.kind')='t3'
             AND json_extract(q.body,'$.source.instance_id')=?4
             AND json_extract(q.body,'$.source.project_id')=?5
             ORDER BY r.sequence LIMIT ?6",
            )
            .map_err(crate::store::Error::from)?;
        let rows = query
            .query_map(
                rusqlite::params![
                    scope.owner_id.to_string(),
                    scope.project_id.to_string(),
                    page.after,
                    scope.instance_id,
                    scope.source_project_id,
                    page.limit + 1
                ],
                |r| Ok((r.get::<_, u64>(0)?, r.get::<_, String>(1)?)),
            )
            .map_err(crate::store::Error::from)?;
        let rows: Vec<_> = rows
            .collect::<Result<_, _>>()
            .map_err(crate::store::Error::from)?;
        let has_more = rows.len() > page.limit as usize;
        let mut result = ReplyPage {
            operations: Vec::new(),
            next_cursor: page.after,
            has_more,
        };
        for (seq, id) in rows.into_iter().take(page.limit as usize) {
            let id =
                Uuid::parse_str(&id).map_err(|_| crate::store::Error::InvalidStoredIdentity)?;
            let op = auth
                .store
                .execution_reply(OwnerId(scope.owner_id), id)?
                .ok_or(crate::store::Error::MissingItem)?;
            result.operations.push(op);
            result.next_cursor = seq;
        }
        Ok(Json(result).into_response())
    })
    .await
}
async fn reply(
    State(state): State<BridgeState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let scope = state.bridge.authenticate(&headers, true)?;
    blocking(state.auth, move |auth| {
        let op = scoped_operation(&auth.store, &scope, id)?;
        Ok(Json(op).into_response())
    })
    .await
}
fn scoped_operation(
    store: &crate::store::Store,
    scope: &Scope,
    id: Uuid,
) -> Result<ReplyOperation, ApiError> {
    store
        .execution_reply(OwnerId(scope.owner_id), id)?
        .filter(|op| scope.permits(&op.question))
        .ok_or(ApiError(StatusCode::NOT_FOUND, "not_found"))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Advance {
    expected: DeliveryState,
    next: DeliveryState,
    delivery_id: Option<String>,
    error_code: Option<String>,
}
async fn advance(
    State(state): State<BridgeState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<Advance>,
) -> Result<Response, ApiError> {
    let scope = state.bridge.authenticate(&headers, true)?;
    blocking(state.auth, move |auth| {
        scoped_operation(&auth.store, &scope, id)?;
        let op = auth.store.advance_execution_reply(
            OwnerId(scope.owner_id),
            id,
            body.expected,
            body.next,
            body.delivery_id.as_deref(),
            body.error_code.as_deref(),
        )?;
        Ok(Json(op).into_response())
    })
    .await
}
