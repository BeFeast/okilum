//! Read-only recovery diagnostics. This module never opens a Runner, writes a
//! receipt store, applies settings or rewrites retained history.
use crate::application::{credential, ApplicationConfig, T3Settings};
use anyhow::{ensure, Result};
use serde_json::{json, Value};
use std::path::Path;

/// Reads saved configuration and the durable journal without startup/recovery.
/// The result is an observation, never an authorization to apply live changes.
pub fn preflight(operational: &Path, candidate: &T3Settings) -> Result<Value> {
    preflight_with(operational, candidate, || {
        credential(&candidate.token_env)
            .map_err(|_| anyhow::anyhow!("t3_ticket_http_401"))
            .and_then(|token| crate::t3::discover(&candidate.base_url, &token))
    })
}

fn preflight_with(
    operational: &Path,
    candidate: &T3Settings,
    probe: impl FnOnce() -> Result<Value>,
) -> Result<Value> {
    let settings_path = crate::settings::path(operational);
    let state_path = operational.join("state.json");
    let settings = std::fs::read(&settings_path)?;
    let journal = std::fs::read(&state_path)?;
    let saved = crate::settings::decode_config(&settings)?;
    let original = saved
        .t3
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("t3_not_configured"))?;
    let mut proposed: ApplicationConfig = saved.clone();
    proposed.t3 = Some(candidate.clone());
    let (retained, active, consistent, allowed) =
        crate::runtime::recovery_facts(&journal, &saved, &proposed)?;
    // Validate before resolving credentials or making a request. URL credentials,
    // query strings and fragments are rejected by the existing T3 discovery too.
    let url = reqwest::Url::parse(&candidate.base_url)?;
    ensure!(
        ["http", "https"].contains(&url.scheme())
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "invalid_candidate"
    );
    ensure!(
        !candidate.environment_id.trim().is_empty()
            && !candidate.project_id.trim().is_empty()
            && !candidate.model_instance_id.trim().is_empty()
            && !candidate.model.trim().is_empty()
            && [
                "approval-required",
                "auto-accept-edits",
                "auto",
                "full-access"
            ]
            .contains(&candidate.runtime_mode.as_str())
            && ["default", "plan"].contains(&candidate.interaction_mode.as_str()),
        "invalid_candidate"
    );
    let observation = probe();
    let changed =
        std::fs::read(&settings_path)? != settings || std::fs::read(&state_path)? != journal;
    Ok(report(
        original,
        candidate,
        observation,
        Facts {
            retained,
            active,
            consistent,
            allowed,
            changed,
        },
    ))
}

struct Facts {
    retained: bool,
    active: bool,
    consistent: bool,
    allowed: bool,
    changed: bool,
}

fn report(
    saved: &T3Settings,
    candidate: &T3Settings,
    observation: Result<Value>,
    facts: Facts,
) -> Value {
    let origin_changed = saved.base_url != candidate.base_url;
    let selected_environment_changed = saved.environment_id != candidate.environment_id;
    let selected_project_changed = saved.project_id != candidate.project_id;
    let mut identity = match observation
        .as_ref()
        .err()
        .map(ToString::to_string)
        .as_deref()
    {
        Some("t3_ticket_connection_failed" | "t3_connect_failed" | "t3_dns_failed") => {
            "endpoint_unreachable"
        }
        Some("t3_ticket_http_401" | "t3_ticket_http_403") => "authentication_required",
        Some(_) => "discovery_unavailable",
        None => "invalid_discovery",
    };
    let mut environment_matches = false;
    let mut project_present = false;
    let mut reachable = false;
    if let Ok(ref observation) = observation {
        reachable = true;
        if let (Some(environment), Some(projects)) = (
            observation["environment_id"]
                .as_str()
                .filter(|v| !v.trim().is_empty()),
            observation["projects"].as_array(),
        ) {
            environment_matches = environment == saved.environment_id;
            project_present = projects
                .iter()
                .any(|p| p["id"].as_str() == Some(saved.project_id.as_str()));
            identity = if !environment_matches || selected_environment_changed {
                "environment_mismatch"
            } else if selected_project_changed {
                "project_mismatch"
            } else if !project_present {
                "project_missing"
            } else if origin_changed {
                "transport_relocation"
            } else {
                "same_target"
            };
        } else {
            identity = "invalid_discovery";
        }
    }
    let mut blockers = Vec::new();
    if facts.changed {
        blockers.push("snapshot_changed");
    }
    if !facts.consistent {
        blockers.push("retained_identity_unproven");
    }
    if !["same_target", "transport_relocation"].contains(&identity) {
        blockers.push(identity);
    }
    if !facts.allowed {
        blockers.push("provider_target_change_blocked");
    }
    // Recovery is deliberately stricter than credential reconnect: do not recommend
    // applying anything while retained provider work is active or indeterminate.
    if facts.active {
        blockers.push("active_or_uncertain_work");
    }
    json!({"schema":"tessera-t3-recovery/v1", "read_only":true,
        "identity":identity, "reachable":reachable,
        "observed_environment_matches_saved":environment_matches,
        "saved_project_present":project_present,
        "origin_changed":origin_changed,
        "selected_environment_changed":selected_environment_changed,
        "selected_project_changed":selected_project_changed,
        "retained_history":facts.retained, "active_or_uncertain_work":facts.active,
        "retained_identity_consistent":facts.consistent,
        "existing_target_guard_allows":facts.allowed,
        "supported_recovery_candidate":blockers.is_empty(), "blockers":blockers,
        "live_apply_authorized":false})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target() -> T3Settings {
        serde_json::from_value(json!({"base_url":"http://127.0.0.1:21000", "token_env":"env:FIXTURE_SECRET", "environment_id":"env-a", "project_id":"project-a", "model_instance_id":"provider-a", "model":"model-a", "runtime_mode":"approval-required", "interaction_mode":"default"})).unwrap()
    }
    fn discovery() -> Result<Value> {
        Ok(
            json!({"environment_id":"env-a","projects":[{"id":"project-a","name":"PRIVATE PROJECT TITLE"}]}),
        )
    }
    fn facts() -> Facts {
        Facts {
            retained: true,
            active: false,
            consistent: true,
            allowed: true,
            changed: false,
        }
    }
    fn fixture() -> (tempfile::TempDir, T3Settings, Value) {
        let temp = tempfile::tempdir().unwrap();
        let target = target();
        let config: ApplicationConfig = serde_json::from_value(
            json!({"actor":"fixture","chat":null,"todoist":null,"t3":target}),
        )
        .unwrap();
        let identity = crate::application::Application::provider_identity(&config);
        let empty = json!({"goal_id":"unrelated-goal", "stage_id":null, "dispatch":null,
            "retained_goal":null,"events":[],"attention":[],"human_acceptances":{},"application":null});
        let mut state = empty.clone();
        state["brain_id"] = json!("fixture-brain");
        state["records_dir"] = json!("records");
        state["pending_writes"] = json!([]);
        state["goal_id"] = json!("retained-goal");
        state["other_goals"] = json!({"unrelated-goal":empty});
        state["application"] = json!({"conversations":{},"mutations":{},"provider_identity":identity,
            "task":{"id":"task-operation", "binding":{"provider":"todoist","instance_id":"fixture-instance","goal_id":"retained-goal","external_id":"fixture-task"},"content":"PRIVATE CONTENT","status":"completed","observation":null}});
        std::fs::write(temp.path().join("connector-settings.json"), serde_json::to_vec(&json!({"schema":"tessera-connectors/v1","config":config,"todoist_account_id":null})).unwrap()).unwrap();
        std::fs::write(
            temp.path().join("state.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
        std::fs::write(
            temp.path().join("receipt-sentinel"),
            b"retained receipt; never replace",
        )
        .unwrap();
        (temp, target, state)
    }
    fn files(path: &Path) -> Vec<(std::ffi::OsString, Vec<u8>)> {
        let mut files = std::fs::read_dir(path)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), std::fs::read(entry.path()).unwrap())
            })
            .collect::<Vec<_>>();
        files.sort();
        files
    }
    #[test]
    fn real_multigoal_journal_positive_control_and_no_writes() {
        let (temp, saved, mut state) = fixture();
        let before = files(temp.path());
        let mut probes = 0;
        let value = preflight_with(temp.path(), &saved, || {
            probes += 1;
            discovery()
        })
        .unwrap();
        assert_eq!(
            probes, 1,
            "absence of writes is meaningful only after a real probe"
        );
        assert_eq!(value["retained_history"], true);
        assert_eq!(value["retained_identity_consistent"], true);
        assert_eq!(value["supported_recovery_candidate"], true);
        assert_eq!(files(temp.path()), before);
        let mut moved = saved.clone();
        moved.base_url = "http://127.0.0.1:21001".into();
        let value = preflight_with(temp.path(), &moved, discovery).unwrap();
        assert_eq!(value["identity"], "transport_relocation");
        assert_eq!(value["existing_target_guard_allows"], false);
        assert_eq!(files(temp.path()), before);
        // Supported terminal model changes are not mistaken for a routing mismatch.
        state["application"]["provider_identity"]["t3"]["model"] = json!("historical-model");
        std::fs::write(
            temp.path().join("state.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
        assert_eq!(
            preflight_with(temp.path(), &saved, discovery).unwrap()["supported_recovery_candidate"],
            true
        );
        state["application"]["provider_identity"]["t3"]["environment_id"] = json!("other-env");
        std::fs::write(
            temp.path().join("state.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
        assert_eq!(
            preflight_with(temp.path(), &saved, discovery).unwrap()["retained_identity_consistent"],
            false
        );
    }
    #[test]
    fn retained_t3_envelopes_bindings_and_uncertain_phases_are_checked() {
        let (temp, saved, mut state) = fixture();
        let dispatch = json!({"envelope":{"schema":"ai-brain/v1","operation_id":"01000000-0000-4000-8000-000000000101",
            "goal_id":"01000000-0000-4000-8000-000000000102","stage_id":"01000000-0000-4000-8000-000000000103","context_id":"01000000-0000-4000-8000-000000000104","context_revision":"revision-1",
            "packet":{"id":"01000000-0000-4000-8000-000000000104","goal_id":"01000000-0000-4000-8000-000000000102","stage_id":"01000000-0000-4000-8000-000000000103","goal_revision":"goal-revision","goal":"PRIVATE GOAL", "decisions":[],"constraints":[],"sources":[],"previous_result_id":null,"next_step":"PRIVATE STEP"},
            "target":{"environment_id":"env-a","project_id":"project-a","created_at":"2026-10-03T12:00:00Z"}},
            "phase":"outcome_ready","binding":{"engine":"t3","instance_id":"env-a","thread_id":"thread-1","turn_id":"turn-1","task_id":null},"sequences":{},"cursors":{}});
        state["dispatch"] = dispatch.clone();
        let mut previous = dispatch;
        previous["envelope"]["operation_id"] = json!("01000000-0000-4000-8000-000000000105");
        previous["envelope"]["stage_id"] = json!("01000000-0000-4000-8000-000000000106");
        previous["envelope"]["packet"]["stage_id"] = json!("01000000-0000-4000-8000-000000000106");
        state["previous_stages"] =
            json!({"stage-0":{"dispatch":previous,"retained_goal":null,"human_acceptances":{}}});
        let write = |state: &Value| {
            std::fs::write(
                temp.path().join("state.json"),
                serde_json::to_vec(state).unwrap(),
            )
            .unwrap()
        };
        write(&state);
        let before = files(temp.path());
        assert_eq!(
            preflight_with(temp.path(), &saved, discovery).unwrap()["supported_recovery_candidate"],
            true
        );
        assert_eq!(files(temp.path()), before);
        for phase in [
            "prepared",
            "submitting",
            "running",
            "indeterminate",
            "unknown",
        ] {
            state["dispatch"]["phase"] = json!(phase);
            write(&state);
            assert_eq!(
                preflight_with(temp.path(), &saved, discovery).unwrap()["active_or_uncertain_work"],
                true
            );
        }
        state["dispatch"]["phase"] = json!("outcome_ready");
        state["previous_stages"]["stage-0"]["dispatch"]["binding"]["instance_id"] =
            json!("different-env");
        write(&state);
        assert_eq!(
            preflight_with(temp.path(), &saved, discovery).unwrap()["retained_identity_consistent"],
            false
        );
        state["previous_stages"] = json!({});
        state["dispatch"]["envelope"]["target"]["project_id"] = json!("different-project");
        write(&state);
        assert_eq!(
            preflight_with(temp.path(), &saved, discovery).unwrap()["retained_identity_consistent"],
            false
        );
        state["dispatch"]["envelope"]["target"]["project_id"] = json!("project-a");
        state["application"]["provider_identity"] = Value::Null;
        write(&state);
        assert_eq!(
            preflight_with(temp.path(), &saved, discovery).unwrap()["retained_identity_consistent"],
            false
        );
    }
    #[test]
    fn changed_or_missing_journal_and_invalid_candidate_fail_closed() {
        let (temp, saved, _) = fixture();
        let value = preflight_with(temp.path(), &saved, || {
            let path = temp.path().join("state.json");
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.push(b' ');
            std::fs::write(path, bytes).unwrap();
            discovery()
        })
        .unwrap();
        assert_eq!(value["blockers"], json!(["snapshot_changed"]));
        let mut invalid = saved.clone();
        invalid.model.clear();
        assert!(preflight_with(temp.path(), &invalid, || panic!(
            "invalid input must not probe"
        ))
        .is_err());
        invalid = saved.clone();
        invalid.runtime_mode = "unsupported-mode".into();
        assert!(preflight_with(temp.path(), &invalid, || panic!(
            "invalid mode must not probe"
        ))
        .is_err());
        std::fs::remove_file(temp.path().join("state.json")).unwrap();
        assert!(preflight_with(temp.path(), &saved, || panic!(
            "missing state must not probe"
        ))
        .is_err());
    }
    #[test]
    fn offline_endpoint_is_distinct_from_auth_and_protocol_errors() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let saved = target();
        let offline = crate::t3::discover(&base, "fixture-token");
        assert_eq!(
            report(&saved, &saved, offline, facts())["identity"],
            "endpoint_unreachable"
        );
        for (error, expected) in [
            ("t3_ticket_http_401", "authentication_required"),
            ("t3_ticket_protocol_error", "discovery_unavailable"),
        ] {
            assert_eq!(
                report(&saved, &saved, Err(anyhow::anyhow!(error)), facts())["identity"],
                expected
            );
        }
    }
    #[test]
    fn classifications_are_fail_closed_and_do_not_equate_project_with_environment() {
        let saved = target();
        let good = report(&saved, &saved, discovery(), facts());
        assert_eq!(good["supported_recovery_candidate"], true); // positive control
        let cases = [
            (
                Ok(json!({"environment_id":"env-b","projects":[{"id":"project-a"}]})),
                "environment_mismatch",
            ),
            (
                Ok(json!({"environment_id":"env-a","projects":[]})),
                "project_missing",
            ),
            (
                Ok(json!({"projects":[{"id":"project-a"}]})),
                "invalid_discovery",
            ),
            (
                Err(anyhow::anyhow!("PRIVATE token or URL in upstream error")),
                "discovery_unavailable",
            ),
        ];
        for (observed, expected) in cases {
            let value = report(&saved, &saved, observed, facts());
            assert_eq!(value["identity"], expected);
            assert_eq!(value["supported_recovery_candidate"], false);
            assert!(!value.to_string().contains("PRIVATE"));
        }
        let mut changed = saved.clone();
        changed.base_url = "http://127.0.0.1:21001".into();
        let value = report(
            &saved,
            &changed,
            discovery(),
            Facts {
                allowed: false,
                ..facts()
            },
        );
        assert_eq!(value["identity"], "transport_relocation");
        assert_eq!(value["blockers"], json!(["provider_target_change_blocked"]));
        changed.project_id = "another-project".into();
        assert_eq!(
            report(&saved, &changed, discovery(), facts())["identity"],
            "project_mismatch"
        );
        for (overridden, expected) in [
            (
                Facts {
                    active: true,
                    ..facts()
                },
                "active_or_uncertain_work",
            ),
            (
                Facts {
                    consistent: false,
                    ..facts()
                },
                "retained_identity_unproven",
            ),
            (
                Facts {
                    changed: true,
                    ..facts()
                },
                "snapshot_changed",
            ),
        ] {
            assert_eq!(
                report(&saved, &saved, discovery(), overridden)["blockers"],
                json!([expected])
            );
        }
        assert!(!good.to_string().contains("FIXTURE_SECRET"));
        assert!(!good.to_string().contains("PRIVATE"));
        assert!(!good.to_string().contains("127.0.0.1"));
    }
}
