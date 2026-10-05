//! Maestro observation and explicit, exact guarded approval decisions.
use anyhow::{ensure, Context, Result};
use reqwest::{blocking::Client as Http, redirect::Policy, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    time::Duration,
};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

pub const MAX_RESPONSE: usize = 16 * 1024 * 1024;
pub const POLL_SECONDS: u64 = 15;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub base_url: String,
    pub instance_id: String,
    #[serde(default)]
    pub token_env: Option<String>,
    #[serde(default)]
    pub ui_origin: Option<String>,
}
impl Settings {
    pub fn identity(&self) -> Value {
        serde_json::json!({"base_url":self.base_url,"instance_id":self.instance_id})
    }
    pub fn validate(&self) -> Result<()> {
        let url = Url::parse(&self.base_url)?;
        ensure!(
            ["http", "https"].contains(&url.scheme())
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/",
            "Maestro requires an explicit HTTP(S) origin without a path or credentials"
        );
        if let Some(origin) = &self.ui_origin {
            let url = Url::parse(origin)?;
            ensure!(
                ["http", "https"].contains(&url.scheme()) && url.host_str().is_some()
                    && url.username().is_empty() && url.password().is_none()
                    && url.query().is_none() && url.fragment().is_none() && url.path() == "/",
                "Maestro ui_origin requires an explicit HTTP(S) origin without a path or credentials"
            );
        }
        ensure!(
            uuid::Uuid::parse_str(&self.instance_id).is_ok(),
            "Maestro instance_id must be a UUID"
        );
        ensure!(
            self.token_env.as_ref().is_none_or(|s| !s.trim().is_empty()),
            "empty Maestro credential reference"
        );
        Ok(())
    }
}
#[derive(Clone)]
pub struct Client {
    config: Settings,
    http: Http,
    token: Option<String>,
}
impl Client {
    pub fn new(config: Settings) -> Result<Self> {
        config.validate()?;
        let token = config
            .token_env
            .as_deref()
            .map(crate::application::credential)
            .transpose()?;
        Ok(Self {
            config,
            token,
            http: Http::builder()
                .timeout(Duration::from_secs(5))
                .connect_timeout(Duration::from_secs(3))
                .redirect(Policy::none())
                .retry(reqwest::retry::never())
                .build()?,
        })
    }
    /// Make exactly one explicit attempt. A refusal describes only this attempt;
    /// callers must retain any uncertainty from an earlier attempt.
    pub fn guarded_decide(
        &self,
        request: &crate::maestro_control::Request,
    ) -> Result<crate::maestro_control::Response> {
        use crate::maestro_control::{
            NotSent, Refusal, Response, Uncertain, MAX_DECISION_RESPONSE,
        };
        let prepared = (|| -> Result<_> {
            request.validate()?;
            ensure!(
                request.instance == self.config.identity(),
                "Guarded provider identity changed"
            );
            let mut url = Url::parse(&self.config.base_url)?;
            url.path_segments_mut()
                .expect("validated origin")
                .clear()
                .extend([
                    "api",
                    "v1",
                    "fleet",
                    "approvals",
                    &request.review.expected.approval_id,
                    request.decision.route(),
                ]);
            url.query_pairs_mut()
                .append_pair("project", &request.review.expected.project_name);
            let mut call = self.http.post(url).json(&request.provider_body());
            if let Some(token) = &self.token {
                call = call.bearer_auth(token);
            }
            Ok(call.build()?)
        })()
        .map_err(|_| NotSent {
            code: "invalid_local_request".into(),
        })?;
        let response = self.http.execute(prepared).map_err(|_| Uncertain {
            code: "transport_interrupted".into(),
            http_status: None,
        })?;
        let status = response.status().as_u16();
        let unknown = |code: &str| Uncertain {
            code: code.into(),
            http_status: Some(status),
        };
        let mut bytes = Vec::new();
        response
            .take(MAX_DECISION_RESPONSE as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| unknown("response_interrupted"))?;
        if bytes.len() > MAX_DECISION_RESPONSE {
            return Err(unknown("response_too_large").into());
        }
        if status != 200 {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Failure {
                ok: bool,
                error: Detail,
            }
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Detail {
                code: String,
                message: String,
            }
            if let Ok(error) = serde_json::from_slice::<Failure>(&bytes) {
                let code = error.error.code.as_str();
                let definitive_attempt_refusal = !error.ok
                    && error.error.message.len() <= 32768
                    && match status {
                        400 => code == "invalid_expected",
                        409 => matches!(
                            code,
                            "identity_conflict" | "revision_conflict" | "decision_conflict"
                        ),
                        422 => code == "unsupported",
                        _ => false,
                    };
                if definitive_attempt_refusal {
                    return Err(Refusal {
                        code: code.into(),
                        message: format!("Guarded attempt refused: {code}"),
                    }
                    .into());
                }
            }
            let code = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|v| v["error"]["code"].as_str().map(str::to_owned))
                .filter(|c| matches!(c.as_str(), "unresolved" | "persistence_failed"))
                .unwrap_or_else(|| "provider_outcome_unresolved".into());
            return Err(unknown(&code).into());
        }
        let result: Response =
            serde_json::from_slice(&bytes).map_err(|_| unknown("invalid_receipt"))?;
        if !result.ok
            || !result.receipt.matches(request)
            || result.execution_status.is_empty()
            || result.execution_status.len() > 128
            || result.execution_status.contains('\0')
        {
            return Err(unknown("receipt_mismatch").into());
        }
        Ok(result)
    }
    pub fn settings(&self) -> &Settings {
        &self.config
    }
    pub fn discover(&self) -> Result<Discovery> {
        project(&self.fleet_raw()?, &self.config, OffsetDateTime::now_utc())
    }
    pub(crate) fn fleet_raw(&self) -> Result<Value> {
        let url = Url::parse(&self.config.base_url)?.join("api/v1/fleet")?;
        let mut request = self.http.get(url);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .map_err(|_| anyhow::anyhow!("Maestro observation unavailable"))?;
        ensure!(
            response.status().is_success(),
            "Maestro HTTP {} (redirects are not followed)",
            response.status().as_u16()
        );
        let mut bytes = Vec::new();
        response
            .take(MAX_RESPONSE as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| anyhow::anyhow!("Maestro response interrupted"))?;
        ensure!(
            bytes.len() <= MAX_RESPONSE,
            "Maestro response exceeds limit"
        );
        serde_json::from_slice(&bytes).context("Malformed Maestro snapshot")
    }
}
pub use tessera_core::maestro_observation::{Approval, Attempt, Issue};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Project {
    pub project_id: String,
    pub name: String,
    pub repo: String,
    pub paused: bool,
    pub dashboard_url: Option<String>,
    pub stale: bool,
    pub issues: Vec<Issue>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Discovery {
    pub instance: Value,
    pub observed_at: String,
    pub refreshed_at: String,
    pub projects: Vec<Project>,
    pub unsupported_projects: usize,
    pub controls_enabled: bool,
}
pub fn digest(value: &impl Serialize) -> String {
    format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("serializable observation"))
    )
}
pub fn selection_guard(instance: &Value, project: &Project, issue: &Issue) -> String {
    digest(
        &serde_json::json!({"instance":instance,"project_id":project.project_id,"name":project.name,"repo":project.repo,"paused":project.paused,"issue":issue}),
    )
}
fn text(value: &Value, key: &str, max: usize) -> Result<String> {
    let s = value.get(key).and_then(Value::as_str).unwrap_or("");
    ensure!(
        s.len() <= max && !s.chars().any(|c| c == '\0'),
        "Invalid Maestro field {key}"
    );
    Ok(s.to_owned())
}
fn link(value: &Value, key: &str) -> Result<Option<String>> {
    let s = text(value, key, 4096)?;
    if s.is_empty() {
        return Ok(None);
    }
    let url = Url::parse(&s).context("Invalid Maestro source link")?;
    ensure!(
        ["http", "https"].contains(&url.scheme())
            && url.username().is_empty()
            && url.password().is_none(),
        "Invalid Maestro source link"
    );
    Ok(Some(s))
}
// Maestro source binds project routes to /project/<name> and approval routes to
// /approvals?id=<ID>. Only the provider's matching route may be projected, and
// only against an explicitly configured desktop UI origin.
fn dashboard_url(
    value: &Value,
    config: &Settings,
    project: Option<&str>,
    approval: Option<&str>,
) -> Result<Option<String>> {
    let raw = text(value, "dashboard_url", 4096)?;
    let Some(origin) = &config.ui_origin else {
        return Ok(None);
    };
    if raw.is_empty() || raw.starts_with("//") || raw.contains('\\') {
        return Ok(None);
    }
    let base = Url::parse(origin)?;
    let Ok(actual) = base.join(&raw) else {
        return Ok(None);
    };
    let mut expected = base.clone();
    if let Some(name) = project {
        expected
            .path_segments_mut()
            .expect("validated HTTP origin")
            .clear()
            .push("project")
            .push(name);
    } else if let Some(id) = approval {
        expected.set_path("/approvals");
        expected.query_pairs_mut().append_pair("id", id);
    }
    if actual != expected || !actual.username().is_empty() || actual.password().is_some() {
        return Ok(None);
    }
    Ok(Some(actual.into()))
}
fn array<'a>(v: &'a Value, key: &str, max: usize) -> Result<&'a [Value]> {
    let values = v
        .get(key)
        .and_then(Value::as_array)
        .context(format!("Missing Maestro {key}"))?;
    ensure!(values.len() <= max, "Maestro {key} exceeds limit");
    Ok(values)
}
fn timestamp(s: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).context("Invalid Maestro observation time")
}
fn approval_issue(a: &Value) -> Result<Option<u64>> {
    let positive = |value: &Value| {
        value
            .as_u64()
            .filter(|n| *n > 0)
            .context("Invalid Maestro approval issue target")
    };
    let number = a.get("issue_number").map(positive).transpose()?;
    let target = a
        .get("target")
        .and_then(|t| t.get("issue"))
        .map(positive)
        .transpose()?;
    ensure!(
        number.is_none() || target.is_none() || number == target,
        "Conflicting Maestro approval issue targets"
    );
    Ok(number.or(target))
}
fn project(raw: &Value, config: &Settings, now: OffsetDateTime) -> Result<Discovery> {
    let refreshed = text(raw, "refreshed_at", 64)?;
    let remote = timestamp(&refreshed)?;
    ensure!(
        remote <= now + time::Duration::minutes(2),
        "Maestro snapshot time is in the future"
    );
    let workers = array(raw, "workers", 8192)?;
    let approvals = raw
        .get("approvals")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    ensure!(approvals.len() <= 8192, "Maestro approvals exceeds limit");
    let mut projects = Vec::new();
    let mut unsupported = 0;
    let mut identities = BTreeMap::new();
    for p in array(raw, "projects", 256)? {
        let id = text(p, "project_id", 64)?;
        if uuid::Uuid::parse_str(&id).is_err() {
            unsupported += 1;
            continue;
        }
        let name = text(p, "name", 256)?;
        let repo = text(p, "repo", 512)?;
        ensure!(
            !name.is_empty() && !repo.is_empty(),
            "Missing Maestro project identity"
        );
        ensure!(
            identities.insert(id.clone(), name.clone()).is_none(),
            "Duplicate Maestro project identity"
        );
        ensure!(
            !projects.iter().any(|p: &Project| p.name == name),
            "Duplicate Maestro project row"
        );
        let mut issues: BTreeMap<u64, Issue> = BTreeMap::new();
        for w in workers
            .iter()
            .filter(|w| w["project_name"].as_str() == Some(&name))
        {
            let Some(number) = w["issue_number"].as_u64().filter(|n| *n > 0) else {
                continue;
            };
            ensure!(
                w["project_repo"].as_str() == Some(&repo),
                "Maestro worker repository identity is missing or mismatched"
            );
            let issue = issues.entry(number).or_insert(Issue {
                number,
                title: text(w, "issue_title", 4096)?,
                url: link(w, "issue_url")?,
                attempts: vec![],
                approvals: vec![],
            });
            ensure!(
                issue.attempts.len() < 256,
                "Too many Maestro attempts for one issue"
            );
            let started = text(w, "started_at", 64)?;
            let generation = w["worker_generation"].as_u64().filter(|n| *n > 0);
            let started_present = !started.is_empty();
            let started_at = if started.is_empty() {
                None
            } else {
                timestamp(&started)?;
                Some(started)
            };
            let slot = text(w, "slot", 256)?;
            ensure!(!slot.is_empty(), "Missing Maestro slot");
            ensure!(
                !issue.attempts.iter().any(|a| a.slot == slot),
                "Ambiguous Maestro slot"
            );
            issue.attempts.push(Attempt {
                slot,
                generation,
                started_at,
                status: text(w, "status", 128)?,
                live: w["live"].as_bool().unwrap_or(false)
                    && generation.is_some()
                    && started_present
                    && w["project_repo"].as_str() == Some(&repo),
                needs_attention: w["needs_attention"].as_bool().unwrap_or(false),
                reason: text(w, "status_reason", 8192)?,
                pr_number: w["pr_number"].as_u64().filter(|n| *n > 0),
                pr_url: link(w, "pr_url")?,
            });
        }
        let mut approval_ids = BTreeSet::new();
        let mut metadata: BTreeMap<u64, (BTreeSet<String>, BTreeSet<String>)> = BTreeMap::new();
        for a in approvals
            .iter()
            .filter(|a| a["project_name"].as_str() == Some(&name))
        {
            // Project-global approvals have no issue association and are not
            // choices in the linked-issue interface.
            let Some(number) = approval_issue(a)? else {
                continue;
            };
            ensure!(
                a["project_repo"].as_str() == Some(&repo)
                    && a.get("project_id")
                        .is_none_or(|value| value.as_str() == Some(&id)),
                "Maestro approval project identity is missing or mismatched"
            );
            let approval_id = text(a, "id", 256)?;
            ensure!(
                !approval_id.is_empty() && approval_ids.insert(approval_id.clone()),
                "Missing or ambiguous Maestro approval identity"
            );
            if let Some(review) = a.get("guarded_review").filter(|v| !v.is_null()) {
                let expected = &review["expected"];
                ensure!(
                    expected["project_id"].as_str() == Some(&id)
                        && expected["project_name"].as_str() == Some(&name)
                        && expected["project_repo"].as_str() == Some(&repo)
                        && expected["approval_id"].as_str() == Some(&approval_id)
                        && review["target"]["issue"].as_u64() == Some(number),
                    "Conflicting Maestro guarded approval identity"
                );
            }
            let issue = issues.entry(number).or_insert_with(|| Issue {
                number,
                title: format!("Issue #{number}"),
                url: None,
                attempts: vec![],
                approvals: vec![],
            });
            if issue.attempts.is_empty() {
                let (titles, urls) = metadata.entry(number).or_default();
                let title = text(a, "issue_title", 4096)?;
                if !title.is_empty() {
                    titles.insert(title);
                }
                if let Some(url) = link(a, "issue_url")? {
                    urls.insert(url);
                }
            }
            issue.approvals.push(Approval {
                dashboard_url: dashboard_url(a, config, None, Some(&approval_id))?,
                id: approval_id,
                action: text(a, "action", 128)?,
                status: text(a, "status", 128)?,
                summary: text(a, "summary", 8192)?,
            });
        }
        // Missing metadata is harmless. Conflicting metadata has no arbitrary
        // winner, and worker-backed issue metadata remains unchanged.
        for (number, (titles, urls)) in metadata {
            let issue = issues.get_mut(&number).expect("approval issue exists");
            if titles.len() == 1 {
                issue.title = titles.into_iter().next().unwrap();
            }
            if urls.len() == 1 {
                issue.url = urls.into_iter().next();
            }
        }
        let age = p["freshness"]["snapshot_age_seconds"].as_u64();
        let stale_after = p["freshness"]["stale_after_seconds"]
            .as_u64()
            .unwrap_or(900)
            .min(900);
        let stale = age.is_none_or(|a| a > stale_after)
            || now - remote > time::Duration::minutes(15)
            || p.get("error").is_some_and(|e| !e.is_null());
        for i in issues.values_mut() {
            ensure!(
                serde_json::to_vec(i)?.len() <= 256 * 1024,
                "Selected Maestro issue exceeds observation limit"
            );
            i.attempts.sort_by(|a, b| a.slot.cmp(&b.slot));
            i.approvals.sort_by(|a, b| a.id.cmp(&b.id));
        }
        let dashboard_url = dashboard_url(p, config, Some(&name), None)?;
        projects.push(Project {
            project_id: id,
            name,
            dashboard_url,
            repo,
            paused: p["paused"]
                .as_bool()
                .context("Missing Maestro pause state")?,
            stale,
            issues: issues.into_values().collect(),
        });
    }
    projects.sort_by(|a, b| a.project_id.cmp(&b.project_id));
    Ok(Discovery {
        instance: config.identity(),
        observed_at: now.format(&Rfc3339)?,
        refreshed_at: refreshed,
        projects,
        unsupported_projects: unsupported,
        controls_enabled: false,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        sync::mpsc,
        thread,
    };
    pub fn settings(base: &str) -> Settings {
        Settings {
            base_url: base.into(),
            instance_id: "01000000-0000-4000-8000-000000000151".into(),
            token_env: None,
            ui_origin: None,
        }
    }
    pub fn raw() -> Value {
        serde_json::json!({"refreshed_at":"2026-09-07T01:00:00Z","projects":[{"project_id":"01000000-0000-4000-8000-000000000152","name":"fixture","repo":"fixture/test","paused":true,"freshness":{"snapshot_age_seconds":1,"stale_after_seconds":900}}],"workers":[{"project_name":"fixture","project_repo":"fixture/test","slot":"test-1","worker_generation":1,"started_at":"2026-09-07T00:59:00Z","issue_number":42,"issue_title":"Observed fixture","issue_url":"https://example.test/fixture/test/issues/42","status":"running","live":true}],"approvals":[]})
    }
    pub fn discovery() -> Discovery {
        project(
            &raw(),
            &settings("http://127.0.0.1:8786"),
            timestamp("2026-09-07T01:00:01Z").unwrap(),
        )
        .unwrap()
    }
    pub fn approval_only_raw() -> Value {
        let mut value = raw();
        value["workers"] = serde_json::json!([]);
        value["approvals"] = serde_json::json!([{
            "project_name": "fixture", "project_repo": "fixture/test", "issue_number": 42,
            "target": {"issue": 42}, "id": "approval-1", "action": "merge_pr",
            "status": "pending", "summary": "Approval prose is not an issue title"
        }]);
        value
    }
    pub fn approval_only_discovery() -> Discovery {
        project(
            &approval_only_raw(),
            &settings("http://127.0.0.1:8786"),
            timestamp("2026-09-07T01:00:01Z").unwrap(),
        )
        .unwrap()
    }
    #[test]
    fn approval_only_and_mixed_issues_preserve_actual_evidence() {
        let config = settings("http://127.0.0.1:8786");
        let now = timestamp("2026-09-07T01:00:01Z").unwrap();
        let mut raw = approval_only_raw();
        let d = project(&raw, &config, now).unwrap();
        let i = &d.projects[0].issues[0];
        assert_eq!(i.number, 42);
        assert_eq!(i.title, "Issue #42");
        assert!(i.url.is_none() && i.attempts.is_empty());
        assert_eq!(i.approvals.len(), 1);
        assert!(!d.controls_enabled);
        let guard = selection_guard(&d.instance, &d.projects[0], i);
        let mut another = raw["approvals"][0].clone();
        another["id"] = serde_json::json!("approval-2");
        raw["approvals"].as_array_mut().unwrap().push(another);
        let d2 = project(&raw, &config, now).unwrap();
        assert_eq!(d2.projects[0].issues.len(), 1);
        assert_eq!(d2.projects[0].issues[0].approvals.len(), 2);
        assert_ne!(
            guard,
            selection_guard(&d2.instance, &d2.projects[0], &d2.projects[0].issues[0])
        );
        raw["workers"] = self::raw()["workers"].clone();
        let mixed = project(&raw, &config, now).unwrap();
        assert_eq!(mixed.projects[0].issues.len(), 1);
        let i = &mixed.projects[0].issues[0];
        let baseline = discovery();
        assert_eq!(i.title, baseline.projects[0].issues[0].title);
        assert_eq!(i.url, baseline.projects[0].issues[0].url);
        assert_eq!(i.attempts, baseline.projects[0].issues[0].attempts);
        assert_eq!(i.approvals.len(), 2);
    }
    #[test]
    fn approval_only_metadata_is_explicit_and_order_independent() {
        let config = settings("http://127.0.0.1:8786");
        let now = timestamp("2026-09-07T01:00:01Z").unwrap();
        let mut raw = approval_only_raw();
        raw["approvals"][0]["issue_title"] = serde_json::json!("Original issue");
        raw["approvals"][0]["issue_url"] = serde_json::json!("https://example.test/issues/42");
        let d = project(&raw, &config, now).unwrap();
        assert_eq!(d.projects[0].issues[0].title, "Original issue");
        assert_eq!(
            d.projects[0].issues[0].url.as_deref(),
            Some("https://example.test/issues/42")
        );
        let mut other = raw["approvals"][0].clone();
        other["id"] = serde_json::json!("approval-2");
        other["issue_title"] = serde_json::json!("Conflicting title");
        other["issue_url"] = serde_json::json!("https://other.test/issues/42");
        raw["approvals"].as_array_mut().unwrap().push(other);
        let first = project(&raw, &config, now).unwrap();
        raw["approvals"].as_array_mut().unwrap().reverse();
        let second = project(&raw, &config, now).unwrap();
        assert_eq!(first.projects, second.projects);
        assert_eq!(first.projects[0].issues[0].title, "Issue #42");
        assert!(first.projects[0].issues[0].url.is_none());
    }
    #[test]
    fn approval_only_identity_targets_and_duplicate_ids_fail_closed() {
        let config = settings("http://127.0.0.1:8786");
        let now = timestamp("2026-09-07T01:00:01Z").unwrap();
        for (key, bad) in [
            ("project_repo", Value::Null),
            ("project_repo", serde_json::json!("other/repo")),
            (
                "project_id",
                serde_json::json!("01000000-0000-4000-8000-000000000999"),
            ),
            ("id", serde_json::json!("")),
            ("issue_number", serde_json::json!(0)),
            ("issue_number", Value::Null),
            ("issue_number", serde_json::json!("42")),
            ("target", serde_json::json!({"issue": 43})),
            ("issue_url", serde_json::json!("javascript:alert(1)")),
        ] {
            let mut raw = approval_only_raw();
            raw["approvals"][0][key] = bad;
            assert!(project(&raw, &config, now).is_err(), "{key}");
        }
        let mut duplicate = approval_only_raw();
        let mut other = duplicate["approvals"][0].clone();
        other["issue_number"] = serde_json::json!(43);
        other["target"]["issue"] = serde_json::json!(43);
        duplicate["approvals"].as_array_mut().unwrap().push(other);
        assert!(project(&duplicate, &config, now).is_err());
        let mut unrelated = approval_only_raw();
        unrelated["approvals"][0]["project_name"] = serde_json::json!("unrelated");
        assert!(project(&unrelated, &config, now).unwrap().projects[0]
            .issues
            .is_empty());
        let mut global = approval_only_raw();
        global["approvals"][0]
            .as_object_mut()
            .unwrap()
            .remove("issue_number");
        global["approvals"][0]["target"] = serde_json::json!({"config": "pause"});
        assert!(project(&global, &config, now).unwrap().projects[0]
            .issues
            .is_empty());
    }
    #[test]
    fn approval_only_guarded_identity_cannot_override_project_or_issue() {
        let config = settings("http://127.0.0.1:8786");
        let now = timestamp("2026-09-07T01:00:01Z").unwrap();
        let mut raw = approval_only_raw();
        raw["approvals"][0]["guarded_review"] = serde_json::json!({
            "expected": {"project_id": raw["projects"][0]["project_id"],
                "project_name": "fixture", "project_repo": "fixture/test", "approval_id": "approval-1"},
            "target": {"issue": 42}
        });
        assert!(project(&raw, &config, now).is_ok());
        for key in ["project_id", "project_name", "project_repo", "approval_id"] {
            let mut bad = raw.clone();
            bad["approvals"][0]["guarded_review"]["expected"][key] = serde_json::json!("different");
            assert!(project(&bad, &config, now).is_err(), "{key}");
        }
        raw["approvals"][0]["guarded_review"]["target"]["issue"] = serde_json::json!(43);
        assert!(project(&raw, &config, now).is_err());
    }
    #[test]
    fn normalized_choices_reject_ambiguous_identity_and_scope_guard_to_instance() {
        let d = discovery();
        let p = &d.projects[0];
        let i = &p.issues[0];
        assert_ne!(
            selection_guard(&d.instance, p, i),
            selection_guard(&serde_json::json!({"instance_id":"other"}), p, i)
        );
        let mut v = raw();
        let duplicate = v["projects"][0].clone();
        v["projects"].as_array_mut().unwrap().push(duplicate);
        assert!(project(
            &v,
            &settings("http://127.0.0.1:8786"),
            timestamp("2026-09-07T01:00:01Z").unwrap()
        )
        .is_err());
        let mut v = raw();
        v["projects"][0]["project_id"] = Value::Null;
        let d = project(
            &v,
            &settings("http://127.0.0.1:8786"),
            timestamp("2026-09-07T01:00:01Z").unwrap(),
        )
        .unwrap();
        assert_eq!(d.unsupported_projects, 1);
        assert!(d.projects.is_empty());
    }
    #[test]
    fn live_requires_attempt_identity_and_old_snapshots_are_stale() {
        let mut v = raw();
        v["workers"][0]["worker_generation"] = Value::Null;
        let d = project(
            &v,
            &settings("http://127.0.0.1:8786"),
            timestamp("2026-09-07T01:20:01Z").unwrap(),
        )
        .unwrap();
        assert!(!d.projects[0].issues[0].attempts[0].live);
        assert!(d.projects[0].stale);
        v["refreshed_at"] = serde_json::json!("2026-09-08T00:00:00Z");
        assert!(project(
            &v,
            &settings("http://127.0.0.1:8786"),
            timestamp("2026-09-07T01:20:01Z").unwrap()
        )
        .is_err());
    }
    #[test]
    fn historical_attempts_require_matching_repository_and_pause_is_guarded() {
        let d = discovery();
        let mut p = d.projects[0].clone();
        let guard = selection_guard(&d.instance, &p, &p.issues[0]);
        p.paused = !p.paused;
        assert_ne!(selection_guard(&d.instance, &p, &p.issues[0]), guard);
        for repo in [Value::Null, serde_json::json!("other/repository")] {
            let mut v = raw();
            v["workers"][0]["project_repo"] = repo;
            v["workers"][0]["status"] = serde_json::json!("code_landed");
            v["workers"][0]["live"] = serde_json::json!(false);
            assert!(project(
                &v,
                &settings("http://127.0.0.1:8786"),
                timestamp("2026-09-07T01:00:01Z").unwrap(),
            )
            .is_err());
        }
    }
    #[test]
    fn approval_evidence_requires_matching_repository_and_unique_nonempty_identity() {
        let mut v = raw();
        v["approvals"] = serde_json::json!([{
            "project_name": "fixture", "project_repo": "fixture/test", "issue_number": 42,
            "id": "approval-1", "action": "edit_issue_body", "status": "pending", "summary": "Proposal",
        }]);
        let config = settings("http://127.0.0.1:8786");
        let now = timestamp("2026-09-07T01:00:01Z").unwrap();
        assert_eq!(
            project(&v, &config, now).unwrap().projects[0].issues[0]
                .approvals
                .len(),
            1
        );
        let mut duplicate = v.clone();
        duplicate["approvals"]
            .as_array_mut()
            .unwrap()
            .push(v["approvals"][0].clone());
        assert!(project(&duplicate, &config, now).is_err());
        for (key, value) in [
            ("project_repo", Value::Null),
            ("project_repo", serde_json::json!("other/repo")),
            ("id", serde_json::json!("")),
        ] {
            let mut bad = v.clone();
            bad["approvals"][0][key] = value;
            assert!(project(&bad, &config, now).is_err());
        }
    }
    #[test]
    fn desktop_urls_require_explicit_origin_and_match_original_project_or_approval_route() {
        let mut config = settings("http://127.0.0.1:8786");
        let raw = serde_json::json!({"dashboard_url":"/project/fixture"});
        assert!(dashboard_url(&raw, &config, Some("fixture"), None)
            .unwrap()
            .is_none());
        config.ui_origin = Some("https://maestro.example.test".into());
        config.validate().unwrap();
        assert_eq!(
            dashboard_url(&raw, &config, Some("fixture"), None)
                .unwrap()
                .as_deref(),
            Some("https://maestro.example.test/project/fixture")
        );
        let approval = serde_json::json!({"dashboard_url":"/approvals?id=approval-1"});
        assert_eq!(
            dashboard_url(&approval, &config, None, Some("approval-1"))
                .unwrap()
                .as_deref(),
            Some("https://maestro.example.test/approvals?id=approval-1")
        );
        assert!(
            dashboard_url(&approval, &config, None, Some("other-approval"))
                .unwrap()
                .is_none()
        );
        for url in [
            "/project/other",
            "https://other.example.test/project/fixture",
            "//other.example.test/project/fixture",
            "https://user@maestro.example.test/project/fixture",
            "javascript:alert(1)",
        ] {
            assert!(dashboard_url(
                &serde_json::json!({"dashboard_url":url}),
                &config,
                Some("fixture"),
                None
            )
            .unwrap()
            .is_none());
        }
        let identity = config.identity();
        config.ui_origin = Some("https://another-ui.example.test".into());
        assert_eq!(config.identity(), identity);
        config.ui_origin = Some("https://maestro.example.test/not-an-origin".into());
        assert!(config.validate().is_err());
    }
    #[test]
    fn http_is_get_only_and_redirects_never_reach_another_origin() {
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap();
        let redirect = target.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        let task = thread::spawn(move || {
            let (mut stream, _) = server.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            tx.send(first).unwrap();
            write!(stream,"HTTP/1.1 302 Found\r\nLocation: http://{redirect}/elsewhere\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let client = Client::new(settings(&format!("http://{address}"))).unwrap();
        assert!(client.discover().unwrap_err().to_string().contains("302"));
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            "GET /api/v1/fleet HTTP/1.1\r\n"
        );
        task.join().unwrap();
        assert_eq!(
            target.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
    #[test]
    fn origin_configuration_refuses_path_userinfo_and_non_http() {
        for base in [
            "file:///etc/passwd",
            "https://user:pass@example.test",
            "https://example.test/api",
            "https://example.test/?token=x",
        ] {
            assert!(settings(base).validate().is_err());
        }
    }
    #[test]
    fn oversized_http_body_is_refused_before_json_projection() {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let task = thread::spawn(move || {
            let (mut stream, _) = server.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.starts_with("GET "));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_RESPONSE + 1
            )
            .unwrap();
            stream.write_all(&vec![b'x'; MAX_RESPONSE + 1]).unwrap();
        });
        let error = Client::new(settings(&format!("http://{addr}")))
            .unwrap()
            .discover()
            .unwrap_err();
        assert!(error.to_string().contains("exceeds limit"));
        task.join().unwrap();
    }
}
