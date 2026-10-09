//! Narrow CLIProxyAPI chat transport: no tools, retries, redirects or raw errors.
use crate::discussion::Message;
use reqwest::{header, Client, Url};
use serde_json::{json, Value};
use std::{path::Path, time::Duration};
use tokio::sync::Semaphore;

pub struct Provider {
    client: Client,
    endpoint: Url,
    pub model: String,
    pub(crate) slots: std::sync::Arc<Semaphore>,
}
impl Provider {
    /// File is supplied by systemd LoadCredential or a private Compose bind mount.
    /// The key is never serialized, persisted to SQLite, or included in errors.
    pub fn from_credential(endpoint: &str, model: &str, path: &Path) -> Result<Self, &'static str> {
        let metadata = std::fs::symlink_metadata(path).map_err(|_| "AI credential unavailable")?;
        if !metadata.is_file() || metadata.len() > 4096 {
            return Err("AI credential must be a small regular file");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err("AI credential must be private");
            }
        }
        let key = std::fs::read_to_string(path).map_err(|_| "AI credential unavailable")?;
        Self::new(endpoint, model, key.trim())
    }
    fn new(endpoint: &str, model: &str, key: &str) -> Result<Self, &'static str> {
        let endpoint = Url::parse(endpoint).map_err(|_| "invalid AI endpoint")?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !endpoint.path().ends_with("/chat/completions")
        {
            return Err("AI endpoint must be an explicit chat completions URL without credentials");
        }
        if model.trim().is_empty() || model.len() > 200 || key.is_empty() {
            return Err("AI model and credential required");
        }
        let mut authorization = header::HeaderValue::from_str(&format!("Bearer {key}"))
            .map_err(|_| "invalid AI credential")?;
        authorization.set_sensitive(true);
        let mut headers = header::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, authorization);
        let client = Client::builder()
            .default_headers(headers)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(90))
            .build()
            .map_err(|_| "AI client unavailable")?;
        Ok(Self {
            client,
            endpoint,
            model: model.into(),
            slots: std::sync::Arc::new(Semaphore::new(4)),
        })
    }
    pub async fn complete(&self, messages: Vec<Message>) -> Result<String, &'static str> {
        let mut response = self
            .client
            .post(self.endpoint.clone())
            .json(&json!({"model":self.model,"messages":messages,"stream":false,"max_tokens":4096}))
            .send()
            .await
            .map_err(|_| "provider_outcome_uncertain")?;
        if !response.status().is_success() {
            return Err("provider_outcome_uncertain");
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "provider_outcome_uncertain")?
        {
            if body.len() + chunk.len() > 1024 * 1024 {
                return Err("provider_outcome_uncertain");
            }
            body.extend_from_slice(&chunk);
        }
        let result: Value =
            serde_json::from_slice(&body).map_err(|_| "provider_outcome_uncertain")?;
        let choice = &result["choices"][0];
        // Truncated output or tool requests are not a completed draft.
        if choice["finish_reason"] != "stop"
            || choice["message"]["tool_calls"]
                .as_array()
                .is_some_and(|v| !v.is_empty())
        {
            return Err("provider_outcome_uncertain");
        }
        let answer = choice["message"]["content"]
            .as_str()
            .filter(|v| !v.trim().is_empty() && v.len() <= 64 * 1024)
            .ok_or("provider_outcome_uncertain")?;
        Ok(answer.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        http::{HeaderMap, StatusCode},
        routing::post,
        Json, Router,
    };
    #[tokio::test]
    async fn sends_only_selected_context_and_rejects_truncated_or_tool_output() {
        let app=Router::new().route("/chat/completions",post(|headers:HeaderMap,Json(body):Json<Value>| async move {
            assert_eq!(headers["authorization"],"Bearer fixture-key");
            assert!(body.get("tools").is_none());
            assert_eq!(body["stream"],false);
            assert_eq!(body["messages"][0]["content"],"selected");
            let model=body["model"].as_str().unwrap();
            let output=match model {
                "good" => json!({"choices":[{"finish_reason":"stop","message":{"content":"useful reply"}}]}),
                "truncated" => json!({"choices":[{"finish_reason":"length","message":{"content":"partial"}}]}),
                "tool" => json!({"choices":[{"finish_reason":"stop","message":{"content":"act","tool_calls":[{}]}}]}),
                _ => return (StatusCode::UNAUTHORIZED,Json(json!({"secret":"never return this body"}))),
            };
            (StatusCode::OK,Json(output))
        }));
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/chat/completions", socket.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(socket, app).await.unwrap() });
        for model in ["good", "truncated", "tool", "denied"] {
            let provider = Provider::new(&endpoint, model, "fixture-key").unwrap();
            let result = provider
                .complete(vec![Message {
                    role: "user",
                    content: "selected".into(),
                }])
                .await;
            if model == "good" {
                assert_eq!(result.unwrap(), "useful reply");
            } else {
                assert_eq!(result.unwrap_err(), "provider_outcome_uncertain");
            }
        }
        task.abort();
    }
}
