use okilum_brain::todoist::*;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

const OP: &str = "06000000-0000-4000-8000-000000000001";
const GOAL: &str = "02000000-0000-4000-8000-000000000001";
const TOKEN: &str = "fixture-secret-never-real";

struct Step {
    status: u16,
    body: String,
    headers: String,
    drop_reply: bool,
}
fn reply(status: u16, body: Value) -> Step {
    Step {
        status,
        body: body.to_string(),
        headers: String::new(),
        drop_reply: false,
    }
}
struct Server {
    url: String,
    worker: thread::JoinHandle<Vec<String>>,
}
impl Server {
    fn new(steps: Vec<Step>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/api/v1/", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            let mut requests = Vec::new();
            for step in steps {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(e) => panic!("missing expected HTTP request: {e}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut buffer = [0; 2048];
                    let n = stream.read(&mut buffer).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(boundary) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..boundary]);
                        let size = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .and_then(|v| v.parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= boundary + 4 + size {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8(bytes).unwrap());
                if !step.drop_reply {
                    write!(stream, "HTTP/1.1 {} fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}", step.status, step.body.len(), step.headers, step.body).unwrap();
                }
            }
            requests
        });
        Self { url, worker }
    }
    fn client(&self) -> Todoist {
        Todoist::new(TodoistConfig::new(
            "account-fixture".into(),
            self.url.clone(),
            TOKEN.into(),
        ))
        .unwrap()
    }
    fn finish(self) -> Vec<String> {
        self.worker.join().unwrap()
    }
}
fn fields() -> TaskFields {
    TaskFields {
        content: Some("POC task".into()),
        description: Some("Context lives in Markdown".into()),
        ..Default::default()
    }
}
fn create(client: &Todoist) -> PreparedMutation {
    client
        .prepare(
            OP.into(),
            GOAL.into(),
            Mutation::Create {
                fields: fields(),
                project_id: Some("project-A".into()),
                section_id: None,
            },
        )
        .unwrap()
}
fn ack() -> Value {
    json!({"sync_status": {OP: "ok"}, "temp_id_mapping": {OP: "task-A"}})
}
fn task(recurring: bool) -> Value {
    json!({"id": "task-A", "project_id": "project-A", "content": "Remote current content", "description": "Remote description", "checked": false, "is_deleted": false, "labels": ["home"], "priority": 3, "due": {"date": "2026-09-07", "is_recurring": recurring, "string": "every Monday"}, "updated_at": "2026-09-05T12:00:00Z", "completed_at": null})
}
fn accepted(outcome: MutationOutcome) -> MutationReceipt {
    match outcome {
        MutationOutcome::Accepted { receipt } => receipt,
        _ => panic!("expected acceptance, got {outcome:?}"),
    }
}
fn wire(request: &str) -> Value {
    let body = request.split("\r\n\r\n").nth(1).unwrap();
    let url = reqwest::Url::parse(&format!("http://fixture/?{body}")).unwrap();
    let commands = url
        .query_pairs()
        .find(|(key, _)| key == "commands")
        .unwrap()
        .1
        .into_owned();
    serde_json::from_str::<Value>(&commands).unwrap()[0].clone()
}

#[test]
fn create_read_and_serializable_binding_keep_remote_authority() {
    let server = Server::new(vec![reply(200, ack()), reply(200, task(false))]);
    let client = server.client();
    let command = create(&client);
    let receipt = accepted(client.execute(&command));
    let restored: TaskBinding =
        serde_json::from_str(&serde_json::to_string(&receipt.binding).unwrap()).unwrap();
    let observation = client.read(&restored).unwrap();
    assert_eq!(observation.task.content, "Remote current content");
    assert_eq!(observation.binding.goal_id, GOAL);
    assert!(!observation.task.checked);
    assert!(!observation.observed_at.is_empty());
    let requests = server.finish();
    assert!(requests[0].starts_with("POST /api/v1/sync "));
    assert!(requests[0]
        .to_ascii_lowercase()
        .contains(&format!("authorization: bearer {TOKEN}")));
    assert!(requests[1].starts_with("GET /api/v1/tasks/task-A "));
    let command = wire(&requests[0]);
    assert_eq!(command["type"], "item_add");
    assert_eq!(command["uuid"], OP);
    assert_eq!(command["temp_id"], OP);
    assert_eq!(command["args"]["project_id"], "project-A");
}

#[test]
fn ambiguous_create_reconciles_exact_saved_uuid_without_automatic_retry() {
    let server = Server::new(vec![
        Step {
            drop_reply: true,
            ..reply(200, ack())
        },
        reply(200, ack()),
    ]);
    let client = server.client();
    let command = create(&client);
    assert!(matches!(
        client.execute(&command),
        MutationOutcome::Indeterminate { .. }
    ));
    // Simulate runner restart: only the persisted envelope survives.
    let saved: PreparedMutation =
        serde_json::from_str(&serde_json::to_string(&command).unwrap()).unwrap();
    let restarted = server.client();
    let receipt = accepted(restarted.reconcile(&saved));
    assert_eq!(receipt.binding.external_id, "task-A");
    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert_eq!(wire(&requests[0]), wire(&requests[1]));
}

#[test]
fn update_omits_unchanged_fields_and_reflects_next_remote_read() {
    let server = Server::new(vec![reply(200, ack()), reply(200, task(false))]);
    let client = server.client();
    let command = client
        .prepare(
            OP.into(),
            GOAL.into(),
            Mutation::Update {
                task_id: "task-A".into(),
                fields: TaskFields {
                    labels: Some(vec![]),
                    ..Default::default()
                },
            },
        )
        .unwrap();
    let receipt = accepted(client.execute(&command));
    assert_eq!(receipt.effect, MutationEffect::Updated);
    assert_eq!(client.read(&receipt.binding).unwrap().task.labels, ["home"]);
    let requests = server.finish();
    assert_eq!(
        wire(&requests[0])["args"],
        json!({"id": "task-A", "labels": []})
    );
}

#[test]
fn close_acknowledges_occurrence_and_recurring_task_remains_open() {
    let server = Server::new(vec![reply(200, ack()), reply(200, task(true))]);
    let client = server.client();
    let command = client
        .prepare(
            OP.into(),
            GOAL.into(),
            Mutation::Close {
                task_id: "task-A".into(),
            },
        )
        .unwrap();
    let receipt = accepted(client.execute(&command));
    assert_eq!(receipt.effect, MutationEffect::OccurrenceClosed);
    let observation = client.read(&receipt.binding).unwrap();
    assert!(!observation.task.checked);
    assert!(observation.task.due.unwrap().is_recurring);
    let requests = server.finish();
    assert_eq!(wire(&requests[0])["type"], "item_close");
    assert_ne!(wire(&requests[0])["type"], "item_complete");
}

#[test]
fn auth_validation_and_rate_limit_errors_are_rejected_without_secret_bodies() {
    for (status, kind) in [
        (400, ErrorKind::Rejected),
        (401, ErrorKind::Unauthorized),
        (403, ErrorKind::Forbidden),
        (429, ErrorKind::RateLimited),
    ] {
        let server = Server::new(vec![Step {
            headers: "Retry-After: 23\r\n".into(),
            ..reply(status, json!({"error": TOKEN}))
        }]);
        let client = server.client();
        let outcome = client.execute(&create(&client));
        let MutationOutcome::Rejected { error } = outcome else {
            panic!("expected rejected")
        };
        assert_eq!(error.kind, kind);
        assert_eq!(error.retry_after_secs, Some(23));
        assert!(!format!("{error:?} {error}").contains(TOKEN));
        assert_eq!(server.finish().len(), 1);
    }
}

#[test]
fn server_uncertainty_missing_mapping_and_malformed_success_stay_indeterminate() {
    for step in [
        reply(500, json!({"error": TOKEN})),
        reply(200, json!({"sync_status": {OP: "ok"}})),
        reply(200, json!({"sync_status": {"other-operation": "ok"}})),
        Step {
            body: "not JSON".into(),
            ..reply(200, Value::Null)
        },
    ] {
        let server = Server::new(vec![step]);
        let client = server.client();
        assert!(matches!(
            client.execute(&create(&client)),
            MutationOutcome::Indeterminate { .. }
        ));
        assert_eq!(server.finish().len(), 1);
    }
}

#[test]
fn sync_command_errors_are_not_hidden_by_http_200() {
    let server = Server::new(vec![reply(
        200,
        json!({"sync_status": {OP: {"error_code": 20, "http_code": 400, "error": TOKEN}}}),
    )]);
    let client = server.client();
    let outcome = client.execute(&create(&client));
    assert!(matches!(outcome, MutationOutcome::Rejected { .. }));
    assert!(!serde_json::to_string(&outcome).unwrap().contains(TOKEN));
    server.finish();
}

#[test]
fn missing_active_task_is_not_reported_completed() {
    let server = Server::new(vec![reply(404, json!({"error": "not found"}))]);
    let client = server.client();
    assert_eq!(
        client
            .associate(GOAL.into(), "task-A".into())
            .unwrap_err()
            .kind,
        ErrorKind::NotFound
    );
    server.finish();
}

#[test]
fn redirects_are_not_followed_with_credentials() {
    let server = Server::new(vec![Step {
        headers: "Location: https://elsewhere.invalid/steal\r\n".into(),
        ..reply(307, json!({}))
    }]);
    let client = server.client();
    let MutationOutcome::Indeterminate { error } = client.execute(&create(&client)) else {
        panic!("redirect must not count as success")
    };
    assert_eq!(error.http_status, Some(307));
    server.finish();
}

#[test]
fn invalid_config_commands_and_cross_account_binding_never_send() {
    for base in [
        "http://public.example/api/v1/",
        "https://name:password@example.com/",
        "https://example.com/?token=hidden",
    ] {
        assert!(matches!(
            Todoist::new(TodoistConfig::new(
                "account".into(),
                base.into(),
                TOKEN.into()
            )),
            Err(TodoistError {
                kind: ErrorKind::InvalidConfiguration,
                ..
            })
        ));
    }
    let server = Server::new(vec![]);
    let client = server.client();
    assert!(client
        .prepare(
            "not-uuid".into(),
            GOAL.into(),
            Mutation::Close {
                task_id: "task-A".into()
            }
        )
        .is_err());
    assert!(client.associate(GOAL.into(), "..".into()).is_err());
    let mut command = create(&client);
    command.instance_id = "another-account".into();
    assert!(matches!(
        client.execute(&command),
        MutationOutcome::Rejected { .. }
    ));
    assert!(server.finish().is_empty());
}

#[test]
fn response_for_different_task_cannot_replace_requested_binding() {
    let server = Server::new(vec![reply(200, task(false))]);
    let client = server.client();
    assert_eq!(
        client
            .associate(GOAL.into(), "task-B".into())
            .unwrap_err()
            .kind,
        ErrorKind::Malformed
    );
    server.finish();
}

#[test]
fn application_recovers_ambiguous_task_intent_and_rejects_changed_uuid_payload() {
    use okilum_brain::{application::*, Criterion, Goal, Runner, RunnerConfig};
    use okilum_core::source::WriteBoundary;
    use std::collections::BTreeMap;
    let server = Server::new(vec![
        Step {
            drop_reply: true,
            ..reply(200, ack())
        },
        reply(200, ack()),
        reply(200, task(false)),
        reply(404, json!({"error":"not found"})),
    ]);
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("brain/records")).unwrap();
    std::fs::create_dir(temp.path().join("runtime")).unwrap();
    let runner_config = || RunnerConfig {
        brain_id: "01000000-0000-4000-8000-000000000009".into(),
        root: temp.path().join("brain"),
        operational_dir: temp.path().join("runtime"),
        records_dir: "records".into(),
        boundary: WriteBoundary::Managed,
    };
    let env_name = "OKILUM_APP_AMBIGUOUS_FIXTURE_TOKEN";
    std::env::set_var(env_name, TOKEN);
    let config = || ApplicationConfig {
        maestro: None,
        actor: "fixture operator".into(),
        chat: None,
        todoist: Some(TodoistSettings {
            base_url: server.url.clone(),
            instance_id: "fixture-account".into(),
            token_env: env_name.into(),
        }),
        t3: None,
    };
    let mut runner = Runner::open(runner_config()).unwrap();
    runner
        .create_goal(
            Goal {
                id: GOAL.into(),
                title: "Recover task".into(),
                status: "draft".into(),
                criteria: vec![Criterion {
                    id: "C1".into(),
                    description: "Actual outcome".into(),
                    requires_human: true,
                }],
                stage_ids: vec![],
                task_ref: None,
                extra: BTreeMap::new(),
            },
            "\n# Recover task\n".into(),
        )
        .unwrap();
    let (app, _) =
        Application::configure(config(), &temp.path().join("runtime"), &mut runner).unwrap();
    let first = app
        .task_create(&mut runner, GOAL.into(), OP.into(), "Original task".into())
        .unwrap();
    assert_eq!(first["status"], "indeterminate");
    assert_eq!(
        app.snapshot(&runner).unwrap()["pending_task_operation_id"],
        OP
    );
    drop(app);
    drop(runner);
    let mut runner = Runner::open(runner_config()).unwrap();
    let (app, _) =
        Application::configure(config(), &temp.path().join("runtime"), &mut runner).unwrap();
    assert!(app
        .task_create(&mut runner, GOAL.into(), OP.into(), "Changed task".into())
        .is_err());
    let recovered = app.task_reconcile(&mut runner, OP.into()).unwrap();
    assert_eq!(recovered["status"], "accepted");
    assert_eq!(recovered["task"]["content"], "Remote current content");
    assert!(app.snapshot(&runner).unwrap()["pending_task_operation_id"].is_null());
    let unavailable = app.task_refresh(&mut runner, GOAL.into()).unwrap();
    assert_eq!(unavailable["status"], "indeterminate");
    assert_eq!(unavailable["task"]["status"], "unknown");
    assert_ne!(runner.snapshot().unwrap().goal.unwrap().status, "completed");
    let journal = std::fs::read_to_string(temp.path().join("runtime/state.json")).unwrap();
    assert!(!journal.contains(TOKEN));
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_eq!(wire(&requests[0]), wire(&requests[1]));
}

#[test]
fn obsolete_create_receipt_cannot_replace_a_newer_explicit_task_link() {
    use okilum_brain::{application::*, Criterion, Goal, Runner, RunnerConfig};
    use okilum_core::source::WriteBoundary;
    use std::collections::BTreeMap;
    for initially_accepted in [true, false] {
        let mut other = task(false);
        other["id"] = json!("task-B");
        other["content"] = json!("Explicit new selection");
        let mut steps = if initially_accepted {
            vec![reply(200, ack()), reply(200, task(false))]
        } else {
            vec![Step {
                drop_reply: true,
                ..reply(200, ack())
            }]
        };
        steps.push(reply(200, other));
        if !initially_accepted {
            steps.push(reply(200, ack()));
        }
        let expected_requests = steps.len();
        let server = Server::new(steps);
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("brain/records")).unwrap();
        std::fs::create_dir(temp.path().join("runtime")).unwrap();
        let cfg = || RunnerConfig {
            brain_id: "01000000-0000-4000-8000-000000000019".into(),
            root: temp.path().join("brain"),
            operational_dir: temp.path().join("runtime"),
            records_dir: "records".into(),
            boundary: WriteBoundary::Managed,
        };
        let env_name = "OKILUM_APP_SUPERSEDED_FIXTURE_TOKEN";
        std::env::set_var(env_name, TOKEN);
        let provider = || ApplicationConfig {
            maestro: None,
            actor: "fixture operator".into(),
            chat: None,
            todoist: Some(TodoistSettings {
                base_url: server.url.clone(),
                instance_id: "fixture-account".into(),
                token_env: env_name.into(),
            }),
            t3: None,
        };
        let mut runner = Runner::open(cfg()).unwrap();
        runner
            .create_goal(
                Goal {
                    id: GOAL.into(),
                    title: "Keep selected task".into(),
                    status: "draft".into(),
                    criteria: vec![Criterion {
                        id: "C1".into(),
                        description: "Actual outcome".into(),
                        requires_human: true,
                    }],
                    stage_ids: vec![],
                    task_ref: None,
                    extra: BTreeMap::new(),
                },
                "\n# Keep selected task\n".into(),
            )
            .unwrap();
        let (app, _) =
            Application::configure(provider(), &temp.path().join("runtime"), &mut runner).unwrap();
        app.task_create(&mut runner, GOAL.into(), OP.into(), "Original task".into())
            .unwrap();
        let linked = app
            .task_link(&mut runner, GOAL.into(), "task-B".into())
            .unwrap();
        assert_eq!(linked["task"]["task_id"], "task-B");
        drop(app);
        drop(runner);
        let mut runner = Runner::open(cfg()).unwrap();
        let (app, _) =
            Application::configure(provider(), &temp.path().join("runtime"), &mut runner).unwrap();
        let recovered = app.task_reconcile(&mut runner, OP.into()).unwrap();
        assert_eq!(recovered["task"]["task_id"], "task-B");
        assert!(recovered["error"].as_str().unwrap().contains("superseded"));
        assert_eq!(
            runner.snapshot().unwrap().goal.unwrap().task_ref.unwrap()["external_id"],
            "task-B"
        );
        assert_eq!(server.finish().len(), expected_requests);
    }
}
