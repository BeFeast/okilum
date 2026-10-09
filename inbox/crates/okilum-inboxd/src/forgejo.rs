//! Optional read-only projection from the external Forgejo collector.
use crate::http::{blocking, now, token, ApiError, Shared};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Extension, Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{io::Read, path::PathBuf};
use uuid::Uuid;

#[derive(Clone)]
pub struct Cache(pub PathBuf);
#[derive(Deserialize)]
struct Filter {
    project: Option<Uuid>,
}
pub(crate) fn routes() -> Router<Shared> {
    Router::new().route("/api/v1/forgejo", get(snapshot))
}
impl Cache {
    /// Cache is derived, operator-selected and never contains a source credential.
    pub fn read(&self, owner: Uuid, at: i64) -> Result<Value, &'static str> {
        let meta = std::fs::symlink_metadata(&self.0).map_err(|_| "forgejo_unavailable")?;
        if !meta.is_file() || meta.len() > 32 * 1024 * 1024 {
            return Err("forgejo_unavailable");
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&self.0)
            .map_err(|_| "forgejo_unavailable")?
            .take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "forgejo_unavailable")?;
        if bytes.len() > 32 * 1024 * 1024 {
            return Err("forgejo_unavailable");
        }
        let mut v: Value = serde_json::from_slice(&bytes).map_err(|_| "forgejo_unavailable")?;
        if v["version"] != 1
            || v["owner_id"] != owner.to_string()
            || !v["projects"].is_object()
            || !v["repos"].is_array()
        {
            return Err("forgejo_unavailable");
        }
        if v["repos"]
            .as_array()
            .expect("validated array")
            .iter()
            .any(|r| {
                !r.is_object()
                    || !r["id"].is_u64()
                    || ["issues", "pulls", "releases"]
                        .iter()
                        .any(|k| !r[*k].is_array())
            })
        {
            return Err("forgejo_unavailable");
        }
        let fresh = |stamp: &Value| stamp.as_i64().is_some_and(|t| t <= at && at - t <= 900);
        let stale = !fresh(&v["discovered_at"]) || !v["error"].is_null();
        for r in v["repos"].as_array_mut().ok_or("forgejo_unavailable")? {
            r["stale"] = json!(stale || !fresh(&r["synced_at"]) || !r["error"].is_null());
        }
        v["stale"] = json!(stale);
        Ok(v)
    }
}
async fn snapshot(
    State(state): State<Shared>,
    headers: HeaderMap,
    Query(filter): Query<Filter>,
    Extension(cache): Extension<Option<Cache>>,
) -> Result<Response, ApiError> {
    let session = token(&headers, "__Host-inbox-session")?;
    blocking(state,move|auth| {
        let owner=auth.authenticate(&session,now())?;
        let Some(cache)=cache else {return Ok(Json(json!({"enabled":false})).into_response());};
        let mut data=cache.read(owner.0,now()).map_err(|code|ApiError(StatusCode::SERVICE_UNAVAILABLE,code))?;
        data["enabled"]=json!(true);
        if let Some(project)=filter.project {
            auth.store.execution_project(owner,project)?.ok_or(crate::store::Error::MissingItem)?;
            let refs=data["projects"][project.to_string()].as_array().cloned().unwrap_or_default();
            data["repos"].as_array_mut().expect("validated repos").retain(|r|refs.iter().any(|link|link["repo_id"]==r["id"]));
            // Link only by explicit configured target. Base commit is not a claim
            // about the executor's current HEAD or responsibility for a PR.
            let mut after=0;
            let mut links=Vec::new();
            loop {
                let rows=auth.store.execution_launches(owner,project,after,100)?;
                if rows.is_empty(){break;}
                for (cursor,op) in rows {
                    after=cursor;
                    for link in &refs {
                        if link["launch_target_ids"].as_array().is_some_and(|ids|ids.iter().any(|id|*id==op.target.target.id)) {
                            links.push(json!({"repo_id":link["repo_id"],"operation_id":op.request.operation_id,"thread_id":op.thread_id,"run_id":op.run_id,"state":op.state,"base_commit":op.target.target.base_commit}));
                        }
                    }
                }
                if links.len()>10000{return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE,"execution_list_limit"));}
            }
            data["execution_links"]=json!(links);
        }
        Ok(Json(data).into_response())
    }).await
}
