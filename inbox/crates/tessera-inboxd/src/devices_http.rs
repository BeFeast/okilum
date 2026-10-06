use crate::{
    auth::FLOW_SECONDS,
    http::{blocking, cookie, now, response, token, ApiError, Shared, FLOW, SESSION},
};
use axum::{
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use webauthn_rs::prelude::RegisterPublicKeyCredential;
pub(crate) fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/v1/passkeys", get(list))
        .route("/api/v1/passkeys/add/start", post(add_start))
        .route("/api/v1/passkeys/add/finish", post(add_finish))
        .route("/api/v1/passkeys/revoke", post(revoke))
        .route("/api/v1/devices/invitations", get(invites).post(invite))
        .route("/api/v1/devices/register/start", post(device_start))
        .route("/api/v1/devices/register/finish", post(device_finish))
        .route("/api/v1/devices/status", post(device_status))
        .route("/api/v1/devices/approve", post(approve))
        .route("/api/v1/devices/cancel", post(cancel))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Name {
    name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Id {
    id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Token {
    token: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    id: String,
    code: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceKey {
    name: String,
    credential: RegisterPublicKeyCredential,
}
async fn list(State(s): State<Shared>, h: HeaderMap) -> Result<Response, ApiError> {
    let t = token(&h, SESSION)?;
    blocking(s, move |a| {
        Ok(Json(json!({"keys":a.passkeys(&t,now())?})).into_response())
    })
    .await
}
async fn add_start(
    State(s): State<Shared>,
    h: HeaderMap,
    Json(b): Json<Name>,
) -> Result<Response, ApiError> {
    let t = token(&h, SESSION)?;
    blocking(s, move |a| {
        let (f, o) = a.add_start(&t, &b.name, now())?;
        Ok(response(json!(o), &[cookie(FLOW, &f, FLOW_SECONDS)]))
    })
    .await
}
async fn add_finish(
    State(s): State<Shared>,
    h: HeaderMap,
    Json(b): Json<RegisterPublicKeyCredential>,
) -> Result<Response, ApiError> {
    let t = token(&h, SESSION)?;
    let f = token(&h, FLOW)?;
    blocking(s, move |a| {
        Ok(response(
            json!({"id":a.add_finish(&t,&f,&b,now())?}),
            &[cookie(FLOW, "", 0)],
        ))
    })
    .await
}
async fn revoke(
    State(s): State<Shared>,
    h: HeaderMap,
    Json(b): Json<Id>,
) -> Result<Response, ApiError> {
    let t = token(&h, SESSION)?;
    blocking(s, move |a| {
        a.revoke(&t, &b.id, now())?;
        let session_revoked = a.authenticate(&t, now()).is_err();
        Ok(response(
            json!({"revoked":true,"session_revoked":session_revoked}),
            &if session_revoked {
                vec![cookie(SESSION, "", 0)]
            } else {
                vec![]
            },
        ))
    })
    .await
}
async fn invites(State(s): State<Shared>, h: HeaderMap) -> Result<Response, ApiError> {
    let t = token(&h, SESSION)?;
    blocking(s, move |a| {
        Ok(Json(json!({"invitations":a.invites(&t,now())?})).into_response())
    })
    .await
}
async fn invite(State(s): State<Shared>, h: HeaderMap) -> Result<Response, ApiError> {
    let t = token(&h, SESSION)?;
    blocking(s, move |a| {
        let (secret, info) = a.invite_start(&t, now())?;
        Ok(Json(json!({"token":secret,"invitation":info})).into_response())
    })
    .await
}
async fn device_start(State(s): State<Shared>, Json(b): Json<Token>) -> Result<Response, ApiError> {
    blocking(s, move |a| {
        let (f, o) = a.device_start(&b.token, now())?;
        Ok(response(json!(o), &[cookie(FLOW, &f, FLOW_SECONDS)]))
    })
    .await
}
async fn device_finish(
    State(s): State<Shared>,
    h: HeaderMap,
    Json(b): Json<DeviceKey>,
) -> Result<Response, ApiError> {
    let f = token(&h, FLOW)?;
    blocking(s, move |a| {
        Ok(response(
            json!(a.device_finish(&f, &b.credential, &b.name, now())?),
            &[cookie(FLOW, "", 0)],
        ))
    })
    .await
}
async fn device_status(
    State(s): State<Shared>,
    Json(b): Json<Token>,
) -> Result<Response, ApiError> {
    blocking(s, move |a| {
        Ok(Json(a.device_status(&b.token, now())?).into_response())
    })
    .await
}
async fn approve(
    State(s): State<Shared>,
    h: HeaderMap,
    Json(b): Json<Decision>,
) -> Result<Response, ApiError> {
    let t = token(&h, SESSION)?;
    blocking(s, move |a| {
        a.approve_device(&t, &b.id, &b.code, now())?;
        Ok(Json(json!({"approved":true})).into_response())
    })
    .await
}
async fn cancel(
    State(s): State<Shared>,
    h: HeaderMap,
    Json(b): Json<Id>,
) -> Result<Response, ApiError> {
    let t = token(&h, SESSION)?;
    blocking(s, move |a| {
        a.cancel_invite(&t, &b.id, now())?;
        Ok(Json(json!({"cancelled":true})).into_response())
    })
    .await
}
