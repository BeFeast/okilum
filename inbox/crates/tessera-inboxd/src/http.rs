//! Same-origin API behind the LAN TLS proxy. Never trusts proxy headers as auth.
use crate::{
    auth::{self, Auth},
    store,
};
use axum::{
    extract::{DefaultBodyLimit, MatchedPath, Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tessera_inbox_domain::Capture;
use uuid::Uuid;
use webauthn_rs::prelude::{PublicKeyCredential, RegisterPublicKeyCredential};

type Shared = Arc<Mutex<Auth>>;
const SESSION: &str = "__Host-inbox-session";
const FLOW: &str = "__Host-inbox-flow";

pub fn router(auth: Auth) -> Router {
    router_with_ai(auth, None)
}

pub fn router_with_ai(auth: Auth, provider: Option<Arc<crate::provider::Provider>>) -> Router {
    router_with_services(auth, provider, None)
}

pub fn router_with_services(
    auth: Auth,
    provider: Option<Arc<crate::provider::Provider>>,
    vault: Option<Arc<crate::vault::Vault>>,
) -> Router {
    let origin = auth.origin.clone();
    Router::new()
        .merge(crate::web::routes())
        .route("/health", get(|| async { "ok" }))
        .route("/api/v1/auth/register/start", post(register_start))
        .route("/api/v1/auth/register/finish", post(register_finish))
        .route("/api/v1/auth/login/start", post(login_start))
        .route("/api/v1/auth/login/finish", post(login_finish))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/session", get(session))
        .route(
            "/api/v1/projects",
            get(execution_projects).post(save_execution_project),
        )
        .route("/api/v1/projects/{id}", get(execution_project))
        .route("/api/v1/briefs", post(save_execution_brief))
        .route(
            "/api/v1/briefs/{id}/revisions/{revision}",
            get(execution_brief),
        )
        .route("/api/v1/items", get(items).post(capture))
        .route("/api/v1/items/{id}", get(item))
        .route(
            "/api/v1/items/{id}/discussion",
            get(discussion).post(discuss),
        )
        .route("/api/v1/destinations", get(destinations))
        .route(
            "/api/v1/items/{id}/publications",
            get(publications).post(publish),
        )
        .layer(Extension(vault))
        .layer(Extension(provider))
        .layer(DefaultBodyLimit::max(128 * 1024))
        .layer(middleware::from_fn_with_state(origin, guard))
        .with_state(Arc::new(Mutex::new(auth)))
}

#[derive(Clone)]
struct SafeError(&'static str);
fn request_log(
    request_id: Uuid,
    method: &str,
    route: &str,
    status: u16,
    duration_ms: u128,
    error: Option<&str>,
) -> Value {
    json!({"event":"http_request", "request_id":request_id, "method":method, "route":route, "status":status, "duration_ms":duration_ms, "error":error})
}
async fn guard(State(origin): State<String>, request: Request, next: Next) -> Response {
    let start = std::time::Instant::now();
    let request_id = Uuid::new_v4();
    let method = match request.method().as_str() {
        "GET" => "GET",
        "HEAD" => "HEAD",
        "POST" => "POST",
        _ => "OTHER",
    };
    // MatchedPath is the static route template, never a URI, query or item ID.
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|v| v.as_str().to_owned())
        .unwrap_or_else(|| "<unmatched>".into());
    let rejected = !matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    ) && request
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        != Some(origin.as_str());
    let mut response = if rejected {
        ApiError(StatusCode::FORBIDDEN, "origin_rejected").into_response()
    } else {
        next.run(request).await
    };
    let error = response
        .extensions()
        .get::<SafeError>()
        .map(|e| e.0)
        .or_else(|| {
            response
                .status()
                .is_client_error()
                .then_some("request_rejected")
        })
        .or_else(|| {
            response
                .status()
                .is_server_error()
                .then_some("request_failed")
        });
    eprintln!(
        "{}",
        request_log(
            request_id,
            method,
            &route,
            response.status().as_u16(),
            start.elapsed().as_millis(),
            error
        )
    );
    response
        .headers_mut()
        .insert("x-request-id", request_id.to_string().parse().unwrap());
    let headers = response.headers_mut();
    for (key, value) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        (
            "content-security-policy",
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; worker-src 'self'; manifest-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
        ),
    ] {
        headers.insert(key, value.parse().unwrap());
    }
    response
}

#[derive(Debug)]
pub struct ApiError(StatusCode, &'static str);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (self.0, Json(json!({"error":self.1}))).into_response();
        response.extensions_mut().insert(SafeError(self.1));
        response
    }
}
impl From<auth::Error> for ApiError {
    fn from(error: auth::Error) -> Self {
        match error {
            auth::Error::Unauthorized | auth::Error::Rejected => {
                Self(StatusCode::UNAUTHORIZED, "authentication_failed")
            }
            auth::Error::Limited => {
                Self(StatusCode::TOO_MANY_REQUESTS, "authentication_rate_limited")
            }
            auth::Error::Enrolled => Self(StatusCode::CONFLICT, "already_enrolled"),
            _ => Self(
                StatusCode::INTERNAL_SERVER_ERROR,
                "authentication_unavailable",
            ),
        }
    }
}
impl From<store::Error> for ApiError {
    fn from(error: store::Error) -> Self {
        match error {
            store::Error::OperationConflict | store::Error::ItemConflict => {
                Self(StatusCode::CONFLICT, "identity_conflict")
            }
            store::Error::ExecutionRevisionConflict => Self(StatusCode::CONFLICT, "stale_revision"),
            store::Error::InvalidExecution(_) => Self(StatusCode::BAD_REQUEST, "invalid_execution"),
            store::Error::PublicationConflict => Self(StatusCode::CONFLICT, "publication_conflict"),
            store::Error::InvalidPublication => {
                Self(StatusCode::BAD_REQUEST, "invalid_publication")
            }
            store::Error::VaultUnavailable => {
                Self(StatusCode::SERVICE_UNAVAILABLE, "vault_unavailable")
            }
            store::Error::MissingItem => Self(StatusCode::NOT_FOUND, "not_found"),
            store::Error::DiscussionBusy => Self(StatusCode::CONFLICT, "discussion_running"),
            store::Error::DiscussionLimit => {
                Self(StatusCode::UNPROCESSABLE_ENTITY, "discussion_limit")
            }
            store::Error::InvalidDiscussion
            | store::Error::InvalidCapture(_)
            | store::Error::InvalidPage => Self(StatusCode::BAD_REQUEST, "invalid_request"),
            _ => Self(StatusCode::INTERNAL_SERVER_ERROR, "storage_unavailable"),
        }
    }
}
async fn blocking<F>(state: Shared, operation: F) -> Result<Response, ApiError>
where
    F: FnOnce(&mut Auth) -> Result<Response, ApiError> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut auth = state
            .lock()
            .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "backend_unavailable"))?;
        operation(&mut auth)
    })
    .await
    .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "backend_unavailable"))?
}
fn token(headers: &HeaderMap, name: &str) -> Result<String, ApiError> {
    let mut found = None;
    for h in headers.get_all(header::COOKIE) {
        for part in h.to_str().unwrap_or("").split(';') {
            if let Some((k, v)) = part.trim().split_once('=') {
                if k == name {
                    if found.is_some() || v.len() != 64 || !v.bytes().all(|b| b.is_ascii_hexdigit())
                    {
                        return Err(ApiError(
                            StatusCode::UNAUTHORIZED,
                            "authentication_required",
                        ));
                    }
                    found = Some(v.to_owned());
                }
            }
        }
    }
    found.ok_or(ApiError(
        StatusCode::UNAUTHORIZED,
        "authentication_required",
    ))
}
fn cookie(name: &str, token: &str, seconds: i64) -> String {
    format!("{name}={token}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={seconds}")
}
fn response(body: Value, cookies: &[String]) -> Response {
    let mut response = Json(body).into_response();
    for cookie in cookies {
        response
            .headers_mut()
            .append(header::SET_COOKIE, cookie.parse().unwrap());
    }
    response
}
fn signed_in(token: &str, owner: Uuid) -> Response {
    response(
        json!({"owner_id":owner}),
        &[
            cookie(SESSION, token, auth::SESSION_SECONDS),
            cookie(FLOW, "", 0),
        ],
    )
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bootstrap {
    token: String,
}
async fn register_start(
    State(state): State<Shared>,
    Json(body): Json<Bootstrap>,
) -> Result<Response, ApiError> {
    blocking(state, move |auth| {
        let (token, options) = auth.register_start(&body.token, now())?;
        Ok(response(
            json!(options),
            &[cookie(FLOW, &token, auth::FLOW_SECONDS)],
        ))
    })
    .await
}
async fn register_finish(
    State(state): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<RegisterPublicKeyCredential>,
) -> Result<Response, ApiError> {
    let flow = token(&headers, FLOW)?;
    blocking(state, move |auth| {
        let session = auth.register_finish(&flow, &body, now())?;
        Ok(signed_in(&session, auth.owner.0))
    })
    .await
}
async fn login_start(State(state): State<Shared>) -> Result<Response, ApiError> {
    blocking(state, move |auth| {
        let (token, options) = auth.login_start(now())?;
        Ok(response(
            json!(options),
            &[cookie(FLOW, &token, auth::FLOW_SECONDS)],
        ))
    })
    .await
}
async fn login_finish(
    State(state): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<PublicKeyCredential>,
) -> Result<Response, ApiError> {
    let flow = token(&headers, FLOW)?;
    blocking(state, move |auth| {
        let session = auth.login_finish(&flow, &body, now())?;
        Ok(signed_in(&session, auth.owner.0))
    })
    .await
}
async fn logout(State(state): State<Shared>, headers: HeaderMap) -> Result<Response, ApiError> {
    let session = token(&headers, SESSION)?;
    blocking(state, move |auth| {
        auth.logout(&session);
        Ok(response(
            json!({"signed_out":true}),
            &[cookie(SESSION, "", 0)],
        ))
    })
    .await
}
async fn session(State(state): State<Shared>, headers: HeaderMap) -> Result<Response, ApiError> {
    let session = token(&headers, SESSION)?;
    blocking(state, move |auth| {
        let owner = auth.authenticate(&session, now())?;
        Ok(Json(json!({"owner_id":owner.0})).into_response())
    })
    .await
}
async fn capture(
    State(state): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Capture>,
) -> Result<Response, ApiError> {
    let session = token(&headers, SESSION)?;
    blocking(state, move |auth| {
        let owner = auth.authenticate(&session, now())?;
        let result = auth.store.capture(owner, &body, now() * 1000)?;
        Ok(Json(result).into_response())
    })
    .await
}
async fn item(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let session = token(&headers, SESSION)?;
    blocking(state, move |auth| {
        let owner = auth.authenticate(&session, now())?;
        let item = auth
            .store
            .item(owner, id)?
            .ok_or(ApiError(StatusCode::NOT_FOUND, "not_found"))?;
        Ok(Json(item).into_response())
    })
    .await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    after: Option<u64>,
    through: Option<u64>,
    limit: Option<u32>,
}
async fn items(
    State(state): State<Shared>,
    headers: HeaderMap,
    Query(page): Query<Page>,
) -> Result<Response, ApiError> {
    let session = token(&headers, SESSION)?;
    blocking(state, move |auth| {
        let owner = auth.authenticate(&session, now())?;
        Ok(Json(auth.store.captures(
            owner,
            page.after.unwrap_or(0),
            page.through,
            page.limit.unwrap_or(50),
        )?)
        .into_response())
    })
    .await
}

async fn discussion(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let session = token(&headers, SESSION)?;
    blocking(state, move |auth| {
        let owner = auth.authenticate(&session, now())?;
        Ok(Json(auth.store.discussion(owner, id)?).into_response())
    })
    .await
}
async fn discuss(
    State(state): State<Shared>,
    Extension(provider): Extension<Option<Arc<crate::provider::Provider>>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<crate::discussion::Discuss>,
) -> Result<Response, ApiError> {
    let session = token(&headers, SESSION)?;
    let worker_state = state.clone();
    blocking(state, move |auth| {
        let owner = auth.authenticate(&session, now())?;
        let provider = provider.ok_or(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "ai_not_configured",
        ))?;
        let permit = provider
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError(StatusCode::TOO_MANY_REQUESTS, "ai_busy"))?;
        let (turn, messages) =
            auth.store
                .begin_discussion(owner, id, &body, &provider.model, now() * 1000)?;
        if let Some(messages) = messages {
            tokio::spawn(async move {
                let _permit = permit;
                let answer = provider.complete(messages).await.ok();
                // Independent of the HTTP client lifetime. A write failure leaves
                // running durable; restart marks it uncertain instead of resending.
                let _ = tokio::task::spawn_blocking(move || {
                    if let Ok(mut auth) = worker_state.lock() {
                        if auth.store.finish_discussion(owner, body.operation_id, answer.as_deref()).is_err() {
                            eprintln!("AI completion could not be persisted; inspect storage and restart to reconcile interrupted work");
                        }
                    }
                })
                .await;
            });
        }
        Ok((StatusCode::ACCEPTED, Json(turn)).into_response())
    })
    .await
}

async fn destinations(
    State(state): State<Shared>,
    Extension(vault): Extension<Option<Arc<crate::vault::Vault>>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let session = token(&headers, SESSION)?;
    blocking(state, move |auth| {
        auth.authenticate(&session, now())?;
        Ok(
            Json(json!({"folders":vault.as_ref().map(|v|v.folders()).unwrap_or(&[])}))
                .into_response(),
        )
    })
    .await
}
async fn publications(
    State(state): State<Shared>,
    Extension(vault): Extension<Option<Arc<crate::vault::Vault>>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let session = token(&headers, SESSION)?;
    blocking(state, move |auth| {
        let owner = auth.authenticate(&session, now())?;
        let mut rows = auth.store.publications(owner, id)?;
        if let Some(vault) = vault {
            for row in &mut rows {
                vault.inspect(row);
            }
        }
        Ok(Json(rows).into_response())
    })
    .await
}
async fn publish(
    State(state): State<Shared>,
    Extension(vault): Extension<Option<Arc<crate::vault::Vault>>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<crate::publication::Publish>,
) -> Result<Response, ApiError> {
    let session = token(&headers, SESSION)?;
    // Filesystem publication stays on a blocking worker and continues even if
    // the HTTP client disconnects. The shared mutex serializes local deliveries.
    blocking(state, move |auth| {
        let owner = auth.authenticate(&session, now())?;
        let vault = vault.ok_or(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "vault_unavailable",
        ))?;
        Ok(Json(vault.publish(&mut auth.store, owner, id, &body)?).into_response())
    })
    .await
}

#[cfg(test)]
mod log_tests {
    use super::*;
    use tower::ServiceExt;

    #[tokio::test]
    async fn rejected_origin_keeps_security_headers_and_a_server_request_id() {
        let dir = tempfile::tempdir().unwrap();
        let auth = Auth::new(
            crate::store::Store::open(&dir.path().join("db")).unwrap(),
            "https://inbox.example.test",
        )
        .unwrap();
        let response = router(auth)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/items?private-query=not-for-logs")
                    .header(header::ORIGIN, "https://untrusted.example.test")
                    .header(header::COOKIE, "private-cookie=not-for-logs")
                    .body(axum::body::Body::from("private-body-not-for-logs"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let request_id =
            Uuid::parse_str(response.headers()["x-request-id"].to_str().unwrap()).unwrap();
        assert_eq!(response.headers()["cache-control"], "no-store");
        let event = request_log(
            request_id,
            "POST",
            "/api/v1/items",
            403,
            1,
            Some("origin_rejected"),
        );
        assert_eq!(event.as_object().unwrap().len(), 7);
        assert_eq!(event["error"], "origin_rejected");
        assert!(!event.to_string().contains("not-for-logs"));
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectPage {
    #[serde(default)]
    after: String,
    #[serde(default = "project_page_size")]
    limit: u32,
}
fn project_page_size() -> u32 {
    50
}
async fn execution_projects(
    State(state): State<Shared>,
    headers: HeaderMap,
    Query(page): Query<ProjectPage>,
) -> Result<Response, ApiError> {
    let session = token(&headers, SESSION)?;
    blocking(state, move |auth| {
        let owner = auth.authenticate(&session, now())?;
        let projects = auth
            .store
            .execution_projects(owner, &page.after, page.limit)?;
        let next_after = projects.last().map(|p| p.id.to_string());
        Ok(Json(json!({"projects": projects, "next_after": next_after})).into_response())
    })
    .await
}
async fn execution_project(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let session = token(&headers, SESSION)?;
    blocking(state, move |auth| {
        let owner = auth.authenticate(&session, now())?;
        let project = auth
            .store
            .execution_project(owner, id)?
            .ok_or(store::Error::MissingItem)?;
        Ok(Json(project).into_response())
    })
    .await
}
async fn save_execution_project(
    State(state): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<tessera_inbox_domain::execution::SaveProject>,
) -> Result<Response, ApiError> {
    let session = token(&headers, SESSION)?;
    blocking(state, move |auth| {
        let owner = auth.authenticate(&session, now())?;
        match auth.store.save_execution_project(owner, &body) {
            Ok(project) => Ok(Json(project).into_response()),
            Err(store::Error::ExecutionRevisionConflict) => {
                let current = auth.store.execution_project(owner, body.project_id)?;
                let mut response = (
                    StatusCode::CONFLICT,
                    Json(json!({"error":"stale_revision", "current":current})),
                )
                    .into_response();
                response
                    .extensions_mut()
                    .insert(SafeError("stale_revision"));
                Ok(response)
            }
            Err(error) => Err(error.into()),
        }
    })
    .await
}
async fn execution_brief(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path((id, revision)): Path<(Uuid, u64)>,
) -> Result<Response, ApiError> {
    let session = token(&headers, SESSION)?;
    blocking(state, move |auth| {
        let owner = auth.authenticate(&session, now())?;
        let brief = auth
            .store
            .execution_brief(owner, id, revision)?
            .ok_or(store::Error::MissingItem)?;
        Ok(Json(brief).into_response())
    })
    .await
}
async fn save_execution_brief(
    State(state): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<tessera_inbox_domain::execution::SaveBrief>,
) -> Result<Response, ApiError> {
    let session = token(&headers, SESSION)?;
    blocking(state, move |auth| {
        let owner = auth.authenticate(&session, now())?;
        match auth.store.save_execution_brief(owner, &body) {
            Ok(brief) => Ok(Json(brief).into_response()),
            Err(store::Error::ExecutionRevisionConflict) => {
                let current = auth.store.latest_execution_brief(owner, body.brief_id)?;
                let mut response = (
                    StatusCode::CONFLICT,
                    Json(json!({"error":"stale_revision", "current":current})),
                )
                    .into_response();
                response
                    .extensions_mut()
                    .insert(SafeError("stale_revision"));
                Ok(response)
            }
            Err(error) => Err(error.into()),
        }
    })
    .await
}
