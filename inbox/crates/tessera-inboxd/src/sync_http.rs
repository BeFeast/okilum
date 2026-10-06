use crate::{
    http::{blocking, guard, now, token, ApiError, Shared, SESSION},
    sync::{self, Config, Exchange, Start},
};
use axum::{
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;
impl From<sync::Error> for ApiError {
    fn from(e: sync::Error) -> Self {
        match e {
            sync::Error::Auth(e) => e.into(),
            sync::Error::Invalid => Self(StatusCode::BAD_REQUEST, "invalid_sync_request"),
            sync::Error::Missing => Self(StatusCode::NOT_FOUND, "sync_request_unavailable"),
            sync::Error::Conflict => Self(StatusCode::CONFLICT, "sync_identity_conflict"),
            sync::Error::Capacity => Self(StatusCode::CONFLICT, "sync_capacity_reached"),
            sync::Error::Limited => Self(StatusCode::TOO_MANY_REQUESTS, "sync_rate_limited"),
            sync::Error::Storage(_) => Self(
                StatusCode::INTERNAL_SERVER_ERROR,
                "sync_storage_unavailable",
            ),
        }
    }
}
pub(crate) fn browser() -> Router<Shared> {
    Router::new()
        .route("/api/v1/sync/vaults", get(vaults))
        .route("/api/v1/sync/requests/{id}", get(pairing))
        .route("/api/v1/sync/approve", post(approve))
        .route("/api/v1/sync/cancel", post(cancel))
        .route("/api/v1/sync/registrations", get(registrations))
        .route("/api/v1/sync/remove", post(remove))
}
pub(crate) fn native(shared: Shared, config: Arc<Config>) -> Router {
    Router::new()
        .route("/api/v1/sync/desktop/start", post(start))
        .route("/api/v1/sync/desktop/exchange", post(exchange))
        .route("/api/v1/sync/desktop/status", post(status))
        .route("/api/v1/sync/desktop/remove", post(desktop_remove))
        .layer(Extension(config))
        .layer(DefaultBodyLimit::max(4096))
        .layer(middleware::from_fn_with_state(None::<String>, guard))
        .with_state(shared)
}
fn enabled(config: Option<Arc<Config>>) -> Result<Arc<Config>, ApiError> {
    config.ok_or(ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "sync_not_configured",
    ))
}
async fn vaults(
    State(s): State<Shared>,
    Extension(c): Extension<Option<Arc<Config>>>,
    h: HeaderMap,
) -> Result<Response, ApiError> {
    let t = token(&h, SESSION)?;
    blocking(s,move|a| {a.authenticate(&t,now())?;Ok(Json(json!({"vaults":c.map(|c|c.vaults.iter().map(|v|json!({"id":v.id,"name":v.name})).collect::<Vec<_>>()).unwrap_or_default()})).into_response())}).await
}
async fn pairing(
    State(s): State<Shared>,
    Extension(c): Extension<Option<Arc<Config>>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    enabled(c)?;
    let t = token(&h, SESSION)?;
    blocking(s, move |a| {
        a.authenticate(&t, now())?;
        Ok(Json(a.store.sync_pairing(a.owner.0, id, now())?).into_response())
    })
    .await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    id: Uuid,
    vault_id: Uuid,
    code: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Id {
    id: Uuid,
}
async fn approve(
    State(s): State<Shared>,
    Extension(c): Extension<Option<Arc<Config>>>,
    h: HeaderMap,
    Json(b): Json<Decision>,
) -> Result<Response, ApiError> {
    let c = enabled(c)?;
    let t = token(&h, SESSION)?;
    blocking(s, move |a| {
        Ok(Json(a.sync_approve(&c, &t, b.id, b.vault_id, &b.code, now())?).into_response())
    })
    .await
}
async fn cancel(
    State(s): State<Shared>,
    Extension(c): Extension<Option<Arc<Config>>>,
    h: HeaderMap,
    Json(b): Json<Id>,
) -> Result<Response, ApiError> {
    enabled(c)?;
    let t = token(&h, SESSION)?;
    blocking(s, move |a| {
        a.sync_cancel(&t, b.id, now())?;
        Ok(Json(json!({"cancelled":true})).into_response())
    })
    .await
}
async fn registrations(
    State(s): State<Shared>,
    Extension(c): Extension<Option<Arc<Config>>>,
    h: HeaderMap,
) -> Result<Response, ApiError> {
    enabled(c)?;
    let t = token(&h, SESSION)?;
    blocking(s, move |a| {
        a.authenticate(&t, now())?;
        Ok(Json(json!({"registrations":a.store.sync_registrations(a.owner.0)?})).into_response())
    })
    .await
}
async fn remove(
    State(s): State<Shared>,
    Extension(c): Extension<Option<Arc<Config>>>,
    h: HeaderMap,
    Json(b): Json<Id>,
) -> Result<Response, ApiError> {
    enabled(c)?;
    let t = token(&h, SESSION)?;
    blocking(s, move |a| {
        a.recent(&t, now())?;
        let state = a.store.sync_remove(a.owner.0, b.id)?;
        Ok(Json(json!({"state":state})).into_response())
    })
    .await
}
async fn start(
    State(s): State<Shared>,
    Extension(c): Extension<Arc<Config>>,
    Json(b): Json<Start>,
) -> Result<Response, ApiError> {
    blocking(s, move |a| {
        if c.owner_id != a.owner.0 {
            return Err(ApiError(StatusCode::FORBIDDEN, "sync_scope_rejected"));
        }
        let p = a.store.sync_start(a.owner.0, &b, now())?;
        Ok(
            Json(json!({"request":p,"approval_url":format!("{}/#sync={}",a.origin,b.id)}))
                .into_response(),
        )
    })
    .await
}
async fn exchange(
    State(s): State<Shared>,
    Extension(c): Extension<Arc<Config>>,
    Json(b): Json<Exchange>,
) -> Result<Response, ApiError> {
    blocking(s, move |a| {
        let p = a.store.sync_pairing(c.owner_id, b.id, now())?;
        if p.vault
            .is_some_and(|id| !c.vaults.iter().any(|v| v.id == id))
        {
            return Err(ApiError(StatusCode::FORBIDDEN, "sync_scope_rejected"));
        }
        let r = a.store.sync_exchange(c.owner_id, &b, now())?;
        Ok(Json(json!({"registration":r})).into_response())
    })
    .await
}
fn bearer(h: &HeaderMap) -> Result<String, ApiError> {
    if h.get_all("authorization").iter().count() != 1 {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "sync_grant_required"));
    }
    h.get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned)
        .ok_or(ApiError(StatusCode::UNAUTHORIZED, "sync_grant_required"))
}
async fn status(
    State(s): State<Shared>,
    Extension(c): Extension<Arc<Config>>,
    h: HeaderMap,
) -> Result<Response, ApiError> {
    let t = bearer(&h)?;
    blocking(s, move |a| {
        Ok(Json(sync::desktop_status(&a.store, &c, &t)?).into_response())
    })
    .await
}
async fn desktop_remove(
    State(s): State<Shared>,
    Extension(c): Extension<Arc<Config>>,
    h: HeaderMap,
) -> Result<Response, ApiError> {
    let t = bearer(&h)?;
    blocking(s, move |a| {
        let r = a.store.sync_grant(c.owner_id, &t)?;
        let state = a.store.sync_remove(c.owner_id, r.id)?;
        Ok(Json(json!({"state":state})).into_response())
    })
    .await
}
