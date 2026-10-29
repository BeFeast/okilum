//! The same read-only shell snapshot the Manager web app consumes.
//! No conversation bodies, commands, pairing, or invented run identities.
use super::*;

#[derive(Deserialize)]
struct Snapshot {
    projects: Vec<ProjectId>,
    threads: Vec<Thread>,
}
#[derive(Deserialize)]
struct ProjectId {
    id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Thread {
    id: String,
    project_id: String,
    title: String,
    model_selection: ModelSelection,
    runtime_mode: String,
    latest_turn: Option<Turn>,
    session: Option<Session>,
    updated_at: String,
    archived_at: Option<String>,
    settled_override: Option<String>,
    settled_at: Option<String>,
    linked_pull_request: Option<Value>,
    #[serde(default)]
    has_pending_approvals: bool,
    #[serde(default)]
    has_pending_user_input: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Turn {
    turn_id: String,
    state: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    status: String,
    active_turn_id: Option<String>,
}

impl Thread {
    fn summary(self) -> ThreadSummary {
        let active_run_id = self.session.as_ref().and_then(|s| s.active_turn_id.clone());
        let status = if active_run_id.is_some()
            && (self.has_pending_approvals || self.has_pending_user_input)
        {
            "waiting"
        } else {
            match self.session.as_ref().map(|s| s.status.as_str()) {
                Some("starting") => "starting",
                Some("running") if active_run_id.is_some() => "running",
                Some("error") => "failed",
                _ => match self.latest_turn.as_ref().map(|t| t.state.as_str()) {
                    Some("completed") => "completed",
                    Some("interrupted") => "interrupted",
                    Some("error") => "failed",
                    Some("running") => "unavailable", // No authoritative active turn.
                    None => "idle",
                    _ => "unavailable",
                },
            }
        };
        let settled = match self.settled_override.as_deref() {
            Some("active") => false,
            Some("settled") => true,
            _ => self.settled_at.is_some(),
        };
        ThreadSummary {
            thread_id: self.id,
            title: self.title,
            status: status.into(),
            latest_run_id: self.latest_turn.map(|turn| turn.turn_id),
            active_run_id,
            provider_instance_id: self.model_selection.instance_id,
            model: self.model_selection.model,
            runtime_mode: self.runtime_mode,
            linked_pull_request: self.linked_pull_request,
            pull_requests: None,
            settled,
            parent_thread_id: None,
            item_count: 0,
            updated_at: self.updated_at,
        }
    }
}

pub(super) fn read(value: Value, project_id: &str) -> GatewayResult<Vec<ThreadSummary>> {
    let snapshot: Snapshot = serde_json::from_value(value).map_err(|_| {
        GatewayError::Protocol("/api/orchestration/shell: invalid thread snapshot".into())
    })?;
    if !snapshot
        .projects
        .iter()
        .any(|project| project.id == project_id)
    {
        return Err(GatewayError::Tool {
            code: "project_not_found".into(),
            message: format!("The Manager session does not list T3 project {project_id}"),
        });
    }
    Ok(snapshot
        .threads
        .into_iter()
        .filter(|thread| thread.project_id == project_id && thread.archived_at.is_none())
        .map(Thread::summary)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(id: &str, project: &str) -> Value {
        json!({
            "id":id,"projectId":project,"title":"Editing",
            "modelSelection":{"instanceId":"codex","model":"model"},
            "runtimeMode":"full-access","updatedAt":"2026-10-29T20:00:00Z",
            "session":{"status":"running","activeTurnId":"current"},
            "latestTurn":{"turnId":"current","state":"running"}
        })
    }
    #[test]
    fn shell_summary_keeps_project_scope_and_authoritative_active_turn() {
        let running = thread("mcp:running", "p");
        let mut historical = thread("mcp:done", "p");
        historical["session"] = json!({"status":"ready","activeTurnId":null});
        historical["latestTurn"]["state"] = json!("completed");
        let mut archived = thread("mcp:archived", "p");
        archived["archivedAt"] = json!("2026-10-29T20:00:00Z");
        let rows = read(json!({"projects":[{"id":"p"}],"threads":[running,historical,archived,thread("mcp:other","other")]}), "p").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].status, "running");
        assert_eq!(rows[0].active_run_id.as_deref(), Some("current"));
        assert_eq!(rows[1].status, "completed");
        assert!(
            rows[1].active_run_id.is_none(),
            "latest completed turn cannot manufacture Working"
        );
    }
    #[test]
    fn waiting_and_settled_override_never_infer_a_different_run() {
        let mut value = thread("mcp:waiting", "p");
        value["hasPendingUserInput"] = json!(true);
        value["settledAt"] = json!("2026-10-29T19:00:00Z");
        value["settledOverride"] = json!("active");
        let rows = read(json!({"projects":[{"id":"p"}],"threads":[value]}), "p").unwrap();
        assert_eq!(rows[0].status, "waiting");
        assert!(!rows[0].settled);
        assert_eq!(rows[0].active_run_id.as_deref(), Some("current"));
    }
    #[test]
    fn unavailable_project_and_malformed_snapshot_are_precise_errors() {
        assert!(
            matches!(read(json!({"projects":[],"threads":[]}), "missing"), Err(GatewayError::Tool{code,..}) if code=="project_not_found")
        );
        assert!(matches!(
            read(json!({"threads":[]}), "p"),
            Err(GatewayError::Protocol(_))
        ));
    }
}
