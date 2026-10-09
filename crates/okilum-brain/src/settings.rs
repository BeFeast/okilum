//! Saved non-secret connector references and explicit connection recovery.
use crate::{
    application::{credential, Application, ApplicationConfig},
    service::Adapters,
    Runner,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Serialize, Deserialize)]
struct Saved {
    schema: String,
    config: ApplicationConfig,
    todoist_account_id: Option<String>,
}
pub fn state(status: &str, message: &str) -> Value {
    json!({"status":status,"message":message})
}
pub(crate) fn configuration_state<T>(result: &Result<T>) -> Value {
    match result {
        Ok(_) => state(
            "configured",
            "Configured; authentication has not been tested.",
        ),
        Err(error) => state(
            if error.to_string().contains("credential") {
                "authentication_required"
            } else {
                "invalid_configuration"
            },
            &format!(
                "{}; correct the credential reference or settings, then Reconnect.",
                error
            ),
        ),
    }
}
pub fn path(operational: &Path) -> PathBuf {
    operational.join("connector-settings.json")
}
pub(crate) fn decode_config(bytes: &[u8]) -> Result<ApplicationConfig> {
    let saved: Saved = serde_json::from_slice(bytes)?;
    ensure!(
        saved.schema == "okilum-connectors/v1",
        "unsupported saved connector settings"
    );
    Ok(saved.config)
}
pub fn load(operational: &Path) -> Result<Option<(ApplicationConfig, Option<String>)>> {
    match std::fs::read(path(operational)) {
        Ok(bytes) => {
            let saved: Saved =
                serde_json::from_slice(&bytes).context("invalid saved connector settings")?;
            ensure!(
                saved.schema == "okilum-connectors/v1",
                "unsupported saved connector settings"
            );
            Ok(Some((saved.config, saved.todoist_account_id)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
pub fn save(
    operational: &Path,
    config: &ApplicationConfig,
    account_id: Option<String>,
) -> Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(operational)?;
    serde_json::to_writer_pretty(
        &mut temporary,
        &Saved {
            schema: "okilum-connectors/v1".into(),
            config: config.clone(),
            todoist_account_id: account_id,
        },
    )?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary.persist(path(operational))?;
    std::fs::File::open(operational)?.sync_all()?;
    Ok(())
}
pub(crate) fn validate_references(config: &ApplicationConfig, runner: &Runner) -> Result<()> {
    let identity = runner.workspace_identity();
    let root = Path::new(identity["root"].as_str().context("brain root missing")?);
    let references = config
        .chat
        .iter()
        .map(|c| c.api_key_env.as_str())
        .chain(config.todoist.iter().map(|c| c.token_env.as_str()))
        .chain(config.t3.iter().map(|c| c.token_env.as_str()))
        .chain(config.maestro.iter().filter_map(|c| c.token_env.as_deref()));
    for reference in references {
        if let Some(file) = reference.strip_prefix("file:") {
            let file = Path::new(file);
            ensure!(
                file.is_absolute()
                    && !file
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir)),
                "credential file must use an absolute path without parent traversal"
            );
            let resolved = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
            ensure!(
                !resolved.starts_with(root),
                "credential files must be outside the canonical brain"
            );
        }
    }
    Ok(())
}
/// Resolve credentials once into runtime adapters. Todoist's identity probe uses
/// that exact adapter token, never a second read of a possibly changing file.
pub fn apply(
    config: ApplicationConfig,
    operational: &Path,
    runner: &mut Runner,
    account: Option<String>,
) -> Result<(Application, Adapters, Option<String>)> {
    validate_references(&config, runner)?;
    let (mut app, adapters) = Application::configure_runtime(config, operational, runner, false)?;
    let pinned = authenticate(&mut app, account.clone());
    // An offline first setup may have no account pin. Persist a newly authenticated
    // identity before the startup adapter can perform any task operation.
    if pinned != account {
        save(operational, &app.settings, pinned.clone())?;
    }
    app.bind_target(runner)?;
    Ok((app, adapters, pinned))
}
pub(crate) fn authenticate(app: &mut Application, account: Option<String>) -> Option<String> {
    let mut pinned = account;
    if app.settings.todoist.is_some() {
        match app.todoist_account() {
            Ok(id) if pinned.as_ref().is_none_or(|old|old==&id)=>{
                pinned=Some(id);app.connection_states.insert("todoist".into(),state("reachable","Authenticated Todoist account matches the saved identity."));
            },
            Ok(_)=>app.disable_todoist(state("identity_mismatch","This token belongs to another Todoist account. Restore the previous account credential; existing tasks remain bound to it.")),
            Err(_)=>app.disable_todoist(state("authentication_required","Cannot verify the Todoist account. Correct or renew its credential reference, then Reconnect. Task operations are disabled; saved history remains available.")),
        }
    }
    pinned
}
fn get_json(base: &str, suffix: &str, token: &str) -> Result<Value> {
    let url = reqwest::Url::parse(&format!("{}/{}", base.trim_end_matches('/'), suffix))?;
    ensure!(
        ["http", "https"].contains(&url.scheme())
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "invalid provider address"
    );
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()?
        .get(url)
        .bearer_auth(token)
        .send()
        .map_err(|_| anyhow::anyhow!("disconnected"))?;
    ensure!(
        response.status().is_success(),
        "HTTP {}",
        response.status().as_u16()
    );
    use std::io::Read;
    let mut bytes = Vec::new();
    response.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 4 * 1024 * 1024,
        "discovery reply exceeds limit"
    );
    serde_json::from_slice(&bytes).context("invalid provider discovery response")
}
/// Explicit read-only discovery; no chat, task or thread is created.
pub fn check(config: &ApplicationConfig) -> Value {
    let mut statuses = BTreeMap::new();
    let mut choices = json!({});
    if let Some(c) = &config.chat {
        let result =
            credential(&c.api_key_env).and_then(|token| get_json(&c.base_url, "models", &token));
        match result {
            Ok(v) => {
                choices["chat_models"] = json!(v["data"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|m| m["id"].as_str())
                    .collect::<Vec<_>>());
                statuses.insert(
                    "chat",
                    state(
                        "reachable",
                        "Model catalog fetched; select a model and save.",
                    ),
                );
            }
            Err(e) => {
                statuses.insert("chat", probe_error(&e));
            }
        }
    }
    if let Some(c) = &config.todoist {
        let result =
            credential(&c.token_env).and_then(|token| get_json(&c.base_url, "user", &token));
        match result {
            Ok(v) if v["id"].is_string() => {
                statuses.insert(
                    "todoist",
                    state(
                        "reachable",
                        "Authenticated account available; Save and connect pins its identity.",
                    ),
                );
            }
            Ok(_) => {
                statuses.insert("todoist",state("authentication_required","Provider did not return an account identity; task operations remain disabled."));
            }
            Err(e) => {
                statuses.insert("todoist", probe_error(&e));
            }
        }
    }
    if let Some(c) = &config.t3 {
        let result =
            credential(&c.token_env).and_then(|token| crate::t3::discover(&c.base_url, &token));
        match result {
            Ok(v) => {
                choices["t3"] = v;
                statuses.insert("t3",state("reachable","Authenticated T3 configuration and projects fetched; select a project and model."));
            }
            Err(e) => {
                statuses.insert("t3", probe_error(&e));
            }
        }
    }
    if let Some(c) = &config.maestro {
        match crate::maestro::Client::new(c.clone()).and_then(|client| client.discover()) {
            Ok(d) => {
                choices["maestro"] = crate::maestro_links::choices(&d);
                statuses.insert(
                    "maestro",
                    state(
                        "reachable",
                        "Maestro observation is available. Linking does not start work.",
                    ),
                );
            }
            Err(e) => {
                statuses.insert("maestro", probe_error(&e));
            }
        }
    }
    json!({"states":statuses,"choices":choices})
}
fn probe_error(error: &anyhow::Error) -> Value {
    let reason = error.to_string();
    if reason.contains("401") || reason.contains("403") || reason.contains("credential") {
        state(
            "authentication_required",
            "Authentication unavailable or expired. Renew the credential reference and Reconnect.",
        )
    } else {
        state("disconnected","Provider connection or discovery failed. Check its address and service, then check again.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Goal, RunnerConfig};
    fn fixture() -> (tempfile::TempDir, Runner) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("brain");
        let operational = temp.path().join("state");
        std::fs::create_dir_all(root.join("records")).unwrap();
        std::fs::create_dir(&operational).unwrap();
        let runner = Runner::open(RunnerConfig {
            brain_id: "01000000-0000-4000-8000-000000000083".into(),
            root,
            operational_dir: operational,
            records_dir: "records".into(),
            boundary: okilum_core::source::WriteBoundary::Managed,
        })
        .unwrap();
        (temp, runner)
    }
    #[test]
    fn missing_credentials_do_not_remove_source_access_and_file_references_are_reloadable() {
        let (temp, mut runner) = fixture();
        let token = temp.path().join("token");
        let config:ApplicationConfig=serde_json::from_value(json!({"actor":"fixture","chat":{"base_url":"http://127.0.0.1:1/v1","model":"fixture","api_key_env":format!("file:{}",token.display())},"todoist":null,"t3":null})).unwrap();
        let (app, _) =
            Application::configure(config.clone(), &temp.path().join("state"), &mut runner)
                .unwrap();
        assert!(!app.capabilities(&runner)["chat"].as_bool().unwrap());
        assert_eq!(app.capabilities(&runner)["source_write"], true);
        std::fs::write(&token, "renewed-token\n").unwrap();
        let (app, _) =
            Application::configure_runtime(config, &temp.path().join("state"), &mut runner, false)
                .unwrap();
        assert_eq!(app.capabilities(&runner)["chat"], true);
    }
    #[test]
    fn different_model_cannot_reroute_a_nonprimary_goal_chat() {
        let (temp, mut runner) = fixture();
        let token = temp.path().join("token");
        std::fs::write(&token, "fixture-token").unwrap();
        let config:ApplicationConfig=serde_json::from_value(json!({"actor":"fixture","chat":{"base_url":"http://127.0.0.1:1/v1","model":"original","api_key_env":format!("file:{}",token.display())},"todoist":null,"t3":null})).unwrap();
        let (app, _) =
            Application::configure(config.clone(), &temp.path().join("state"), &mut runner)
                .unwrap();
        for id in [
            "02000000-0000-4000-8000-000000000081",
            "02000000-0000-4000-8000-000000000082",
        ] {
            let goal:Goal=serde_json::from_value(json!({"id":id,"title":"fixture","status":"draft","criteria":[{"id":"C1","description":"outcome","requires_human":false}],"stage_ids":[],"task_ref":null})).unwrap();
            runner.create_goal(goal, "# Fixture".into()).unwrap();
        }
        runner
            .with_goal("02000000-0000-4000-8000-000000000082", |r| {
                app.chat_start(
                    r,
                    "02000000-0000-4000-8000-000000000082".into(),
                    "A pending thought".into(),
                    vec![],
                    None,
                )
            })
            .unwrap();
        let mut changed = config.clone();
        changed.chat.as_mut().unwrap().model = "different".into();
        assert!(Application::configure_runtime(
            changed.clone(),
            &temp.path().join("state"),
            &mut runner,
            false
        )
        .is_err());
        let (same, _) = Application::configure_runtime(
            config.clone(),
            &temp.path().join("state"),
            &mut runner,
            false,
        )
        .unwrap();
        runner
            .with_goal("02000000-0000-4000-8000-000000000082", |r| {
                assert_eq!(same.snapshot(r)?["conversations"][0]["status"], "running");
                same.recover(r)?;
                Ok(())
            })
            .unwrap();
        let (updated, _) = Application::configure_runtime(
            changed.clone(),
            &temp.path().join("state"),
            &mut runner,
            false,
        )
        .unwrap();
        assert_eq!(updated.settings.chat.unwrap().model, "different");
        changed.chat.as_mut().unwrap().base_url = "http://127.0.0.1:2/v1".into();
        assert!(
            Application::configure_runtime(changed, &temp.path().join("state"), &mut runner, false)
                .is_err(),
            "terminal history still pins the provider origin"
        );
    }
}
