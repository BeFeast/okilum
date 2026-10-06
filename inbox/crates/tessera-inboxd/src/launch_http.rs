use crate::{
    bridge::{Bridge, BridgeState},
    http::{blocking, now, token, ApiError, Shared},
    launch::{Launch, Operation, Progress},
    store,
};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tessera_inbox_domain::OwnerId;
use uuid::Uuid;
const SESSION: &str = "__Host-inbox-session";
pub(crate) fn browser() -> Router<Shared> {
    Router::new()
        .route("/api/v1/projects/{project}/launch-targets", get(targets))
        .route("/api/v1/projects/{project}/launches", get(browser_list))
        .route("/api/v1/launches", post(prepare))
        .route("/api/v1/launches/{id}", get(browser_get))
}
pub(crate) fn machine() -> Router<BridgeState> {
    Router::new()
        .route("/api/bridge/v1/launches", get(machine_list))
        .route(
            "/api/bridge/v1/launches/{id}",
            get(machine_get).post(advance),
        )
}
async fn targets(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path(project): Path<Uuid>,
    Extension(bridge): Extension<Option<Arc<Bridge>>>,
) -> Result<Response, ApiError> {
    let session = token(&headers, SESSION)?;
    blocking(state, move |auth| {
        let owner = auth.authenticate(&session, now())?;
        let targets = bridge
            .filter(|b| {
                b.scope.owner_id == owner.0 && b.scope.project_id == project && b.scope.launches
            })
            .map(|b| {
                b.targets
                    .iter()
                    .map(|t| json!({"snapshot":t,"revision":t.revision()}))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok(Json(json!({"targets":targets})).into_response())
    })
    .await
}
async fn prepare(
    State(state): State<Shared>,
    headers: HeaderMap,
    Extension(bridge): Extension<Option<Arc<Bridge>>>,
    Json(body): Json<Launch>,
) -> Result<Response, ApiError> {
    let session = token(&headers, SESSION)?;
    blocking(state, move |auth| {
        let owner = auth.authenticate(&session, now())?;
        // Exact old consent remains readable after operator target revocation.
        if let Some(op) = auth.store.execution_launch(owner, body.operation_id)? {
            return if op.request == body {
                Ok(Json(op).into_response())
            } else {
                Err(store::Error::OperationConflict.into())
            };
        }
        let bridge = bridge
            .filter(|b| b.scope.owner_id == owner.0 && b.scope.launches)
            .ok_or(ApiError(StatusCode::FORBIDDEN, "launch_not_enabled"))?;
        let target = bridge
            .targets
            .iter()
            .find(|t| t.revision() == body.target_revision)
            .ok_or(ApiError(StatusCode::CONFLICT, "launch_target_changed"))?;
        Ok(Json(auth.store.prepare_execution_launch(owner, &body, target)?).into_response())
    })
    .await
}
async fn browser_get(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let session = token(&headers, SESSION)?;
    blocking(state, move |auth| {
        let owner = auth.authenticate(&session, now())?;
        Ok(Json(
            auth.store
                .execution_launch(owner, id)?
                .ok_or(store::Error::MissingItem)?,
        )
        .into_response())
    })
    .await
}
#[derive(Deserialize)]
pub(crate) struct Page {
    #[serde(default)]
    after: u64,
    #[serde(default = "limit")]
    limit: u32,
}
fn limit() -> u32 {
    50
}
fn page(
    store: &store::Store,
    owner: OwnerId,
    project: Uuid,
    p: Page,
) -> Result<serde_json::Value, ApiError> {
    if p.limit == 0 || p.limit > 100 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_page"));
    }
    let mut rows = store.execution_launches(owner, project, p.after, p.limit + 1)?;
    let more = rows.len() > p.limit as usize;
    rows.truncate(p.limit as usize);
    let cursor = rows.last().map(|r| r.0).unwrap_or(p.after);
    Ok(
        json!({"operations":rows.into_iter().map(|r|r.1).collect::<Vec<_>>(),"next_cursor":cursor,"has_more":more}),
    )
}
async fn browser_list(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path(project): Path<Uuid>,
    Query(p): Query<Page>,
) -> Result<Response, ApiError> {
    let session = token(&headers, SESSION)?;
    blocking(state, move |auth| {
        let owner = auth.authenticate(&session, now())?;
        Ok(Json(page(&auth.store, owner, project, p)?).into_response())
    })
    .await
}
fn scoped(op: Option<Operation>, bridge: &Bridge) -> Result<Operation, ApiError> {
    op.filter(|o| {
        o.target.project_id == bridge.scope.project_id
            && o.target.instance_id == bridge.scope.instance_id
            && o.target.source_project_id == bridge.scope.source_project_id
    })
    .ok_or(ApiError(StatusCode::NOT_FOUND, "not_found"))
}
async fn machine_get(
    State(state): State<BridgeState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let scope = state.bridge.authenticate(&headers, "launches")?;
    blocking(state.auth, move |auth| {
        Ok(Json(scoped(
            auth.store.execution_launch(OwnerId(scope.owner_id), id)?,
            &state.bridge,
        )?)
        .into_response())
    })
    .await
}
async fn machine_list(
    State(state): State<BridgeState>,
    headers: HeaderMap,
    Query(p): Query<Page>,
) -> Result<Response, ApiError> {
    let scope = state.bridge.authenticate(&headers, "launches")?;
    blocking(state.auth, move |auth| {
        let data = page(&auth.store, OwnerId(scope.owner_id), scope.project_id, p)?;
        // Scope config replacement cannot expose a previous source's launch.
        let mut data = data;
        data["operations"]
            .as_array_mut()
            .expect("page array")
            .retain(|op| {
                op["target"]["instance_id"] == scope.instance_id
                    && op["target"]["source_project_id"] == scope.source_project_id
            });
        Ok(Json(data).into_response())
    })
    .await
}
async fn advance(
    State(state): State<BridgeState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(p): Json<Progress>,
) -> Result<Response, ApiError> {
    let scope = state.bridge.authenticate(&headers, "launches")?;
    blocking(state.auth, move |auth| {
        scoped(
            auth.store.execution_launch(OwnerId(scope.owner_id), id)?,
            &state.bridge,
        )?;
        Ok(Json(
            auth.store
                .advance_execution_launch(OwnerId(scope.owner_id), id, &p)?,
        )
        .into_response())
    })
    .await
}
