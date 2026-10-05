use super::*;
use std::{fs, io::Read, sync::mpsc, thread, time::Instant};
use tessera_core::source::WriteBoundary;
use uuid::Uuid;

struct Step {
    status: u16,
    body: String,
    wait: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
}
fn reply(status: u16, body: Value) -> Step {
    Step {
        status,
        body: body.to_string(),
        wait: None,
    }
}
struct Server {
    base: String,
    worker: thread::JoinHandle<Vec<String>>,
}
impl Server {
    fn new(steps: Vec<Step>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}/api/v1/", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            let mut requests = vec![];
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
                        Err(e) => panic!("missing fixture request: {e}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                let mut size = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        size = value.trim().parse::<usize>().unwrap();
                    }
                }
                let mut body = vec![0; size];
                reader.read_exact(&mut body).unwrap();
                requests.push(request.trim().to_owned());
                if let Some((entered, wait)) = step.wait {
                    entered.send(()).unwrap();
                    wait.recv_timeout(Duration::from_secs(5)).unwrap();
                }
                write!(stream, "HTTP/1.1 {} fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nRetry-After: 7\r\nConnection: close\r\n\r\n{}", step.status, step.body.len(), step.body).unwrap();
            }
            requests
        });
        Self { base, worker }
    }
    fn finish(self) -> Vec<String> {
        self.worker.join().unwrap()
    }
}
fn user() -> Value {
    json!({"id":"account-A","inbox_project_id":"inbox-A"})
}
fn task(id: &str, description: &str) -> Value {
    json!({"id":id,"project_id":"inbox-A","content":"Duplicate title","description":description,"checked":false,"is_deleted":false,"labels":["fixture"],"priority":2,"due":{"date":"2026-09-09","is_recurring":false},"updated_at":"2026-09-08T01:00:00Z","completed_at":null})
}
fn page(items: Vec<Value>, cursor: Option<&str>) -> Value {
    json!({"results":items,"next_cursor":cursor})
}
fn fixture(base: &str) -> (tempfile::TempDir, Arc<Mutex<Backend>>, String) {
    let temp = tempfile::tempdir().unwrap();
    let (backend, goal) = fixture_at(temp.path(), base);
    (temp, backend, goal)
}
fn fixture_at(path: &std::path::Path, base: &str) -> (Arc<Mutex<Backend>>, String) {
    let root = path.join("brain");
    let operational = path.join("state");
    fs::create_dir_all(root.join("records")).unwrap();
    fs::create_dir(&operational).unwrap();
    fs::write(path.join("token"), "fixture-not-a-real-token").unwrap();
    let mut runner = Runner::open(RunnerConfig {
        brain_id: Uuid::new_v4().to_string(),
        root,
        operational_dir: operational.clone(),
        records_dir: "records".into(),
        boundary: WriteBoundary::Managed,
    })
    .unwrap();
    let goal = add_goal(&mut runner);
    let config = ApplicationConfig {
        actor: "fixture".into(),
        chat: None,
        t3: None,
        maestro: None,
        todoist: Some(crate::application::TodoistSettings {
            base_url: base.into(),
            instance_id: "fixture-instance".into(),
            token_env: format!("file:{}", path.join("token").display()),
        }),
    };
    let (app, adapters) =
        Application::configure(config.clone(), &operational, &mut runner).unwrap();
    crate::settings::save(&operational, &config, Some("account-A".into())).unwrap();
    let backend = Arc::new(Mutex::new(Backend {
        runner,
        app,
        adapters,
        exports: Default::default(),
        todoist_picker: Default::default(),
        index: None,
        index_error: None,
        context_jobs: crate::context_jobs::Jobs::open(&operational).unwrap(),
    }));
    (backend, goal)
}
fn add_goal(runner: &mut Runner) -> String {
    add_named_goal(runner, "Picker fixture")
}
fn add_named_goal(runner: &mut Runner, title: &str) -> String {
    let id = Uuid::new_v4().to_string();
    runner
        .create_goal(
            Goal {
                id: id.clone(),
                title: title.into(),
                status: "draft".into(),
                criteria: vec![Criterion {
                    id: "C1".into(),
                    description: "Exact task".into(),
                    requires_human: false,
                }],
                stage_ids: vec![],
                task_ref: None,
                extra: BTreeMap::new(),
            },
            "\n# Picker\n".into(),
        )
        .unwrap();
    id
}
fn saved(temp: &tempfile::TempDir) -> Value {
    serde_json::from_slice(&fs::read(temp.path().join("state/state.json")).unwrap()).unwrap()
}
fn sid(value: &Value) -> String {
    value["session_id"].as_str().unwrap().to_owned()
}
fn error_code(error: anyhow::Error) -> String {
    error_value(&error)["code"].as_str().unwrap().to_owned()
}

#[test]
fn paginated_duplicate_titles_freeze_first_seen_and_link_exact_task_without_provider_writes() {
    let a = task("task-A", "First detail");
    let b = task("task-B", "Second detail");
    let server = Server::new(vec![
        reply(200, user()),
        reply(200, page(vec![a.clone(), b.clone()], Some("cursor-1"))),
        reply(200, user()),
        reply(
            200,
            page(
                vec![task("task-A", "changed later"), task("task-C", "Third")],
                None,
            ),
        ),
        reply(200, user()),
        reply(200, b),
    ]);
    let (temp, shared, goal) = fixture(&server.base);
    let before = saved(&temp);
    let first = list(&shared, goal.clone(), None).unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    assert_eq!(first["complete"], false);
    let second = list(&shared, goal.clone(), Some(sid(&first))).unwrap();
    assert_eq!(second["items"].as_array().unwrap().len(), 3);
    assert_eq!(second["items"][0]["description"], "First detail");
    assert_eq!(second["complete"], true);
    assert_eq!(
        saved(&temp),
        before,
        "browsing must not checkpoint task state"
    );
    let linked = link(&shared, goal.clone(), "task-B".into(), Some(sid(&first))).unwrap();
    assert_eq!(linked["task"]["task_id"], "task-B");
    assert_eq!(
        error_code(link(&shared, goal.clone(), "task-B".into(), Some(sid(&first))).unwrap_err()),
        "todoist_selection_stale"
    );
    let current = saved(&temp);
    assert_eq!(current["application"]["task_generation"], 1);
    assert_eq!(current["application"]["task"]["binding"]["goal_id"], goal);
    assert_eq!(
        current["application"]["task"]["binding"]["instance_id"],
        "fixture-instance"
    );
    assert_eq!(
        current["application"]["mutations"],
        before["application"]["mutations"]
    );
    let config = {
        let b = shared.lock().unwrap();
        RunnerConfig {
            brain_id: b.runner.workspace_identity()["brain_id"]
                .as_str()
                .unwrap()
                .into(),
            root: b.runner.root().to_path_buf(),
            operational_dir: b.runner.operational_root().to_path_buf(),
            records_dir: "records".into(),
            boundary: WriteBoundary::Managed,
        }
    };
    drop(shared);
    let reopened = Runner::open(config).unwrap();
    assert_eq!(
        reopened.snapshot().unwrap().goal.unwrap().task_ref.unwrap()["external_id"],
        "task-B"
    );
    assert_eq!(
        saved(&temp)["application"]["task"],
        current["application"]["task"]
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 6);
    assert!(requests.iter().all(|r| r.starts_with("GET ")));
    assert_eq!(
        requests[1],
        "GET /api/v1/tasks?project_id=inbox-A&limit=50 HTTP/1.1"
    );
    assert!(requests[3].contains("cursor=cursor-1"));
    assert_eq!(requests[5], "GET /api/v1/tasks/task-B HTTP/1.1");
}

#[test]
fn provider_method_capture_has_a_post_positive_control() {
    let server = Server::new(vec![reply(200, json!({}))]);
    reqwest::blocking::Client::new()
        .post(format!("{}fixture-probe", server.base))
        .body("positive control")
        .send()
        .unwrap();
    assert_eq!(server.finish(), vec!["POST /api/v1/fixture-probe HTTP/1.1"]);
}

#[test]
fn failed_page_retains_rows_cursor_counters_and_allows_explicit_retry() {
    let server = Server::new(vec![
        reply(200, user()),
        reply(200, page(vec![task("task-A", "A")], Some("c1"))),
        reply(200, user()),
        reply(429, json!({})),
        reply(200, user()),
        reply(200, page(vec![], Some("c2"))),
        reply(200, user()),
        reply(200, page(vec![], None)),
    ]);
    let (_temp, shared, goal) = fixture(&server.base);
    let first = list(&shared, goal.clone(), None).unwrap();
    let error = list(&shared, goal.clone(), Some(sid(&first))).unwrap_err();
    assert_eq!(error_value(&error)["todoist"]["kind"], "rate_limited");
    assert_eq!(error_value(&error)["todoist"]["retry_after_secs"], 7);
    {
        let owner = shared.lock().unwrap();
        let s = owner.todoist_picker.session.as_ref().unwrap();
        assert_eq!(s.pages, 1);
        assert_eq!(s.cursor.as_deref(), Some("c1"));
        assert_eq!(s.view(), first);
        assert!(!s.busy);
    }
    let next = list(&shared, goal.clone(), Some(sid(&first))).unwrap();
    assert_eq!(next["complete"], false);
    assert_eq!(next["items"], first["items"]);
    let last = list(&shared, goal, Some(sid(&first))).unwrap();
    assert_eq!(last["complete"], true);
    let requests = server.finish();
    assert!(requests[3].contains("cursor=c1"));
    assert!(requests[5].contains("cursor=c1"));
    assert!(requests[7].contains("cursor=c2"));
}

#[test]
fn changed_deleted_moved_or_completed_frozen_task_never_commits_a_link() {
    for mode in ["changed", "deleted", "moved", "completed", "missing"] {
        let mut changed = task("task-A", "original");
        match mode {
            "changed" => changed["description"] = json!("changed"),
            "deleted" => changed["is_deleted"] = json!(true),
            "moved" => changed["project_id"] = json!("other-project"),
            "completed" => changed["checked"] = json!(true),
            _ => {}
        }
        let server = Server::new(vec![
            reply(200, user()),
            reply(200, page(vec![task("task-A", "original")], None)),
            reply(200, user()),
            reply(if mode == "missing" { 404 } else { 200 }, changed),
        ]);
        let (temp, shared, goal) = fixture(&server.base);
        let before = saved(&temp);
        let first = list(&shared, goal.clone(), None).unwrap();
        let error = link(&shared, goal, "task-A".into(), Some(sid(&first))).unwrap_err();
        assert_eq!(
            error_code(error),
            if mode == "missing" {
                "todoist_provider_error"
            } else {
                "todoist_selection_stale"
            }
        );
        assert_eq!(saved(&temp), before);
        assert!(
            !shared
                .lock()
                .unwrap()
                .todoist_picker
                .session
                .as_ref()
                .unwrap()
                .busy
        );
        assert!(server.finish().iter().all(|r| r.starts_with("GET ")));
    }
}

#[test]
fn known_id_link_preserves_completed_deleted_and_non_inbox_semantics() {
    for deleted in [false, true] {
        let mut remote = task("known-task", "direct");
        remote["project_id"] = json!("other-project");
        remote["checked"] = json!(true);
        remote["is_deleted"] = json!(deleted);
        let server = Server::new(vec![
            reply(200, json!({"id":"account-A"})),
            reply(200, remote),
        ]);
        let (_temp, shared, goal) = fixture(&server.base);
        let result = link(&shared, goal, "known-task".into(), None).unwrap();
        assert_eq!(
            result["task"]["status"],
            if deleted { "deleted" } else { "completed" }
        );
        assert_eq!(server.finish().len(), 2);
    }
}

#[test]
fn pending_durable_source_projection_consumes_selection_and_recovers_without_duplicate_link() {
    let a = task("task-A", "retained");
    let server = Server::new(vec![
        reply(200, user()),
        reply(200, page(vec![a.clone()], None)),
        reply(200, user()),
        reply(200, a),
    ]);
    let (temp, shared, goal) = fixture(&server.base);
    let first = list(&shared, goal.clone(), None).unwrap();
    shared
        .lock()
        .unwrap()
        .runner
        .interrupt_next_source_projection();
    assert!(
        link(&shared, goal.clone(), "task-A".into(), Some(sid(&first)))
            .unwrap_err()
            .to_string()
            .contains("injected crash")
    );
    let before = saved(&temp);
    assert_eq!(before["application"]["task_generation"], 1);
    assert!(!before["pending_writes"].as_array().unwrap().is_empty());
    assert_eq!(
        error_code(link(&shared, goal, "task-A".into(), Some(sid(&first))).unwrap_err()),
        "todoist_selection_stale"
    );
    assert_eq!(saved(&temp), before);
    let config = {
        let b = shared.lock().unwrap();
        RunnerConfig {
            brain_id: b.runner.workspace_identity()["brain_id"]
                .as_str()
                .unwrap()
                .into(),
            root: b.runner.root().to_path_buf(),
            operational_dir: b.runner.operational_root().to_path_buf(),
            records_dir: "records".into(),
            boundary: WriteBoundary::Managed,
        }
    };
    drop(shared);
    let reopened = Runner::open(config).unwrap();
    assert_eq!(reopened.snapshot().unwrap().pending_writes, 0);
    let after = saved(&temp);
    assert_eq!(after["application"], before["application"]);
    assert_eq!(after["application"]["task_generation"], 1);
    assert_eq!(server.finish().len(), 4);
}

#[test]
fn stalled_list_and_link_leave_owner_available_and_refuse_changed_targets() {
    for operation in ["list", "direct-link", "picker-link"] {
        for change in ["config", "account", "session", "generation"] {
            let (entered, received) = mpsc::channel();
            let (release, wait) = mpsc::channel();
            let a = task("task-A", "A");
            let mut steps = vec![];
            if operation == "picker-link" {
                steps.extend([reply(200, user()), reply(200, page(vec![a.clone()], None))]);
            }
            steps.push(reply(200, user()));
            steps.push(Step {
                status: 200,
                body: if operation == "list" {
                    page(vec![a.clone()], None)
                } else {
                    a
                }
                .to_string(),
                wait: Some((entered, wait)),
            });
            let server = Server::new(steps);
            let (_temp, shared, goal) = fixture(&server.base);
            let selection = if operation == "picker-link" {
                Some(sid(&list(&shared, goal.clone(), None).unwrap()))
            } else {
                None
            };
            let invoked = shared.clone();
            let target = goal.clone();
            let worker = thread::spawn(move || {
                if operation == "list" {
                    list(&invoked, target, None)
                } else {
                    link(&invoked, target, "task-A".into(), selection)
                }
            });
            received.recv_timeout(Duration::from_secs(3)).unwrap();
            {
                let mut owner = shared
                    .try_lock()
                    .expect("provider GET must release owner lock");
                assert!(owner
                    .handle(Command::Snapshot {
                        goal_id: Some(goal.clone())
                    })
                    .is_ok());
                match change {
                    "config" => owner.app.settings.actor = "changed".into(),
                    "account" => crate::settings::save(
                        owner.runner.operational_root(),
                        &owner.app.settings,
                        Some("account-B".into()),
                    )
                    .unwrap(),
                    "session" => owner.todoist_picker.invalidate(),
                    _ => {
                        let observed = crate::todoist::TaskObservation {
                            binding: crate::todoist::TaskBinding {
                                provider: "todoist".into(),
                                instance_id: "fixture-instance".into(),
                                goal_id: goal.clone(),
                                external_id: "other-task".into(),
                            },
                            observed_at: "2026-09-08T02:00:00Z".into(),
                            task: serde_json::from_value(task(
                                "other-task",
                                "newer explicit selection",
                            ))
                            .unwrap(),
                        };
                        let Backend { runner, app, .. } = &mut *owner;
                        runner
                            .with_goal(&goal, |r| app.task_link_observed(r, goal.clone(), observed))
                            .unwrap();
                    }
                }
            }
            release.send(()).unwrap();
            assert!(worker.join().unwrap().is_err(), "{operation}/{change}");
            let mut owner = shared.lock().unwrap();
            let snapshot = owner
                .handle(Command::Snapshot {
                    goal_id: Some(goal),
                })
                .unwrap();
            assert_eq!(
                snapshot["task"]["task_id"],
                if change == "generation" {
                    json!("other-task")
                } else {
                    Value::Null
                }
            );
            assert!(server.finish().iter().all(|r| r.starts_with("GET ")));
        }
    }
}

#[test]
fn native_unknown_navigation_cannot_redirect_explicit_goal_link() {
    let (entered, received) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let server = Server::new(vec![
        reply(200, user()),
        Step {
            status: 200,
            body: task("task-A", "old goal").to_string(),
            wait: Some((entered, wait)),
        },
    ]);
    let (_temp, shared, goal) = fixture(&server.base);
    let invoked = shared.clone();
    let target = goal.clone();
    let worker = thread::spawn(move || link(&invoked, target, "task-A".into(), None));
    received.recv_timeout(Duration::from_secs(3)).unwrap();
    let other = add_goal(&mut shared.lock().unwrap().runner);
    release.send(()).unwrap();
    assert!(worker.join().unwrap().is_ok());
    let mut owner = shared.lock().unwrap();
    assert_eq!(
        owner
            .handle(Command::Snapshot {
                goal_id: Some(other)
            })
            .unwrap()["task"],
        Value::Null
    );
    assert_eq!(
        owner
            .handle(Command::Snapshot {
                goal_id: Some(goal)
            })
            .unwrap()["task"]["task_id"],
        "task-A"
    );
    server.finish();
}

#[test]
fn account_error_and_malformed_pages_never_masquerade_as_empty_inbox() {
    for mode in [
        "401",
        "403",
        "503",
        "account",
        "missing-inbox",
        "wrong-project",
        "completed",
        "deleted",
        "missing-cursor",
        "too-many",
        "oversized",
    ] {
        let mut account = user();
        let mut body = page(vec![task("task-A", "A")], None);
        match mode {
            "account" => account["id"] = json!("account-B"),
            "missing-inbox" => {
                account.as_object_mut().unwrap().remove("inbox_project_id");
            }
            "wrong-project" => body["results"][0]["project_id"] = json!("other-project"),
            "completed" => body["results"][0]["checked"] = json!(true),
            "deleted" => body["results"][0]["is_deleted"] = json!(true),
            "missing-cursor" => {
                body.as_object_mut().unwrap().remove("next_cursor");
            }
            "too-many" => {
                body["results"] = json!((0..51)
                    .map(|n| task(&format!("task-{n}"), "A"))
                    .collect::<Vec<_>>())
            }
            "oversized" => body["results"][0]["description"] = json!("x".repeat(1024 * 1024)),
            _ => {}
        }
        let status = mode.parse::<u16>().unwrap_or(200);
        let mut steps = vec![reply(status, account)];
        if status == 200 && mode != "account" && mode != "missing-inbox" {
            steps.push(reply(200, body));
        }
        let server = Server::new(steps);
        let (temp, shared, goal) = fixture(&server.base);
        // A failed browse retains a real existing binding and saved source selection.
        {
            let mut owner = shared.lock().unwrap();
            let observed = crate::todoist::TaskObservation {
                binding: crate::todoist::TaskBinding {
                    provider: "todoist".into(),
                    instance_id: "fixture-instance".into(),
                    goal_id: goal.clone(),
                    external_id: "existing-task".into(),
                },
                observed_at: "2026-09-08T01:00:00Z".into(),
                task: serde_json::from_value(task("existing-task", "retained")).unwrap(),
            };
            let Backend { runner, app, .. } = &mut *owner;
            app.task_link_observed(runner, goal.clone(), observed)
                .unwrap();
        }
        let before = saved(&temp);
        assert!(list(&shared, goal, None).is_err(), "{mode}");
        assert_eq!(saved(&temp), before);
        assert!(
            !shared
                .lock()
                .unwrap()
                .todoist_picker
                .session
                .as_ref()
                .unwrap()
                .busy
        );
        server.finish();
    }
}

#[test]
fn session_page_row_byte_and_cursor_bounds_are_explicit() {
    let server = Server::new(vec![]);
    let (_temp, shared, goal) = fixture(&server.base);
    let (target, _) = capture(&mut shared.lock().unwrap(), &goal).unwrap();
    let mut s = Session::new(target.clone());
    for page_number in 0..10 {
        let tasks = (0..50)
            .map(|n| {
                serde_json::from_value(task(&format!("task-{}", page_number * 50 + n), "A"))
                    .unwrap()
            })
            .collect();
        s.merge(crate::todoist::InboxPage {
            results: tasks,
            next_cursor: Some(format!("cursor-{page_number}")),
        })
        .unwrap();
    }
    assert_eq!(s.rows.len(), 500);
    assert_eq!(s.pages, 10);
    assert!(s.limited && !s.complete);
    assert_eq!(s.view()["can_load_more"], false);
    let mut repeated = Session::new(target.clone());
    repeated
        .merge(crate::todoist::InboxPage {
            results: vec![],
            next_cursor: Some("same".into()),
        })
        .unwrap();
    let before = repeated.view();
    assert!(repeated
        .merge(crate::todoist::InboxPage {
            results: vec![],
            next_cursor: Some("same".into())
        })
        .is_err());
    assert_eq!(repeated.view(), before);
    assert_eq!(repeated.pages, 1);
    let mut empty = Session::new(target.clone());
    empty
        .merge(crate::todoist::InboxPage {
            results: vec![],
            next_cursor: None,
        })
        .unwrap();
    assert!(empty.complete && !empty.limited);
    let mut bytes = Session::new(target);
    let huge: Task = serde_json::from_value(task("large", &"x".repeat(MAX_BYTES))).unwrap();
    bytes
        .merge(crate::todoist::InboxPage {
            results: vec![huge],
            next_cursor: None,
        })
        .unwrap();
    assert!(bytes.limited && !bytes.complete);
    assert!(bytes.rows.is_empty());
    server.finish();
}

#[test]
fn picker_wire_requires_workspace_and_direct_link_remains_compatible() {
    for op in ["todoist_inbox_list", "task_link"] {
        let mut wire = json!({"schema":"ai-brain/v1","id":1,"op":op,"goal_id":"goal","task_id":"task","picker_session_id":"session"});
        assert!(Request::try_from(wire.clone()).is_err());
        wire["schema"] = json!("ai-brain/workspace-v1");
        wire["expected_workspace"] = json!({"brain_id":"fixture"});
        assert!(Request::try_from(wire).is_ok());
    }
    assert!(Request::try_from(
        json!({"schema":"ai-brain/v1","id":1,"op":"task_link","goal_id":"goal","task_id":"task"})
    )
    .is_ok());
    let server = Server::new(vec![]);
    let (_temp, shared, goal) = fixture(&server.base);
    assert!(dispatch(
        &shared,
        Command::TodoistInboxList {
            goal_id: goal,
            session_id: None
        },
        Some(json!({"brain_id":"wrong"}))
    )
    .is_err());
    server.finish();
}

#[test]
fn concurrent_duplicate_link_is_refused_before_any_second_provider_request() {
    let (entered, received) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let a = task("task-A", "A");
    let server = Server::new(vec![
        reply(200, user()),
        reply(200, page(vec![a.clone()], None)),
        reply(200, user()),
        Step {
            status: 200,
            body: a.to_string(),
            wait: Some((entered, wait)),
        },
    ]);
    let (temp, shared, goal) = fixture(&server.base);
    let first = list(&shared, goal.clone(), None).unwrap();
    let invoked = shared.clone();
    let target = goal.clone();
    let selection = sid(&first);
    let worker = thread::spawn(move || link(&invoked, target, "task-A".into(), Some(selection)));
    received.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(
        error_code(link(&shared, goal, "task-A".into(), Some(sid(&first))).unwrap_err()),
        "todoist_picker_busy"
    );
    release.send(()).unwrap();
    assert!(worker.join().unwrap().is_ok());
    assert_eq!(saved(&temp)["application"]["task_generation"], 1);
    assert_eq!(server.finish().len(), 4);
}

#[test]
fn changed_inbox_identity_refuses_picker_link_before_task_read() {
    let mut account = user();
    account["inbox_project_id"] = json!("new-inbox");
    let server = Server::new(vec![
        reply(200, user()),
        reply(200, page(vec![task("task-A", "A")], None)),
        reply(200, account),
    ]);
    let (temp, shared, goal) = fixture(&server.base);
    let first = list(&shared, goal.clone(), None).unwrap();
    let before = saved(&temp);
    assert_eq!(
        error_code(link(&shared, goal, "task-A".into(), Some(sid(&first))).unwrap_err()),
        "todoist_account_changed"
    );
    assert_eq!(saved(&temp), before);
    assert_eq!(server.finish().len(), 3);
}

#[test]
fn initial_saved_config_drift_refuses_before_provider_access_and_preserves_binding() {
    let server = Server::new(vec![]);
    let (temp, shared, goal) = fixture(&server.base);
    {
        let mut owner = shared.lock().unwrap();
        let observed = crate::todoist::TaskObservation {
            binding: crate::todoist::TaskBinding {
                provider: "todoist".into(),
                instance_id: "fixture-instance".into(),
                goal_id: goal.clone(),
                external_id: "existing-task".into(),
            },
            observed_at: "2026-09-08T01:00:00Z".into(),
            task: serde_json::from_value(task("existing-task", "retained")).unwrap(),
        };
        let Backend { runner, app, .. } = &mut *owner;
        app.task_link_observed(runner, goal.clone(), observed)
            .unwrap();
    }
    let before = saved(&temp);
    {
        let owner = shared.lock().unwrap();
        let mut config = owner.app.settings.clone();
        config.actor = "durably changed".into();
        crate::settings::save(
            owner.runner.operational_root(),
            &config,
            Some("account-A".into()),
        )
        .unwrap();
    }
    assert_eq!(
        error_code(list(&shared, goal.clone(), None).unwrap_err()),
        "todoist_configuration_changed"
    );
    assert_eq!(
        error_code(link(&shared, goal, "task-A".into(), None).unwrap_err()),
        "todoist_configuration_changed"
    );
    assert_eq!(saved(&temp), before);
    assert!(server.finish().is_empty());
}

#[test]
fn known_id_without_saved_configuration_needs_no_new_account_auth_flow() {
    let server = Server::new(vec![reply(200, task("task-A", "direct"))]);
    let (temp, shared, goal) = fixture(&server.base);
    fs::remove_file(temp.path().join("state/connector-settings.json")).unwrap();
    let linked = link(&shared, goal, "task-A".into(), None).unwrap();
    assert_eq!(linked["task"]["task_id"], "task-A");
    assert_eq!(server.finish(), vec!["GET /api/v1/tasks/task-A HTTP/1.1"]);
}

#[test]
#[ignore = "explicit native acceptance exporter; requires a fresh exact final root"]
fn export_todoist_picker_native_fixture() {
    let root = std::path::PathBuf::from(
        std::env::var("TESSERA_TODOIST_FIXTURE_ROOT").expect("fixture final root required"),
    );
    assert!(
        root.is_absolute() && !root.exists(),
        "final fixture root must be absolute and new"
    );
    fs::create_dir_all(&root).unwrap();
    let base = std::env::var("TESSERA_TODOIST_FIXTURE_BASE")
        .unwrap_or_else(|_| "http://127.0.0.1:24190/api/v1/".into());
    let (backend, goal) = fixture_at(&root, &base);
    let mut owner = backend.lock().unwrap();
    let other = add_named_goal(&mut owner.runner, "Other fixture goal");
    let workspace = owner.runner.workspace_identity();
    let data = json!({"schema":"tessera-todoist-picker-native-fixture/v1","workspace":workspace,"goal_id":goal,"other_goal_id":other,"provider_base":base,"account_id":"account-A","instance_id":"fixture-instance","fixture_only":true});
    fs::write(
        root.join("fixture.json"),
        serde_json::to_vec_pretty(&data).unwrap(),
    )
    .unwrap();
    let config = RunnerConfig {
        brain_id: workspace["brain_id"].as_str().unwrap().into(),
        root: owner.runner.root().to_path_buf(),
        operational_dir: owner.runner.operational_root().to_path_buf(),
        records_dir: "records".into(),
        boundary: WriteBoundary::Managed,
    };
    drop(owner);
    drop(backend);
    let reopened = Runner::open(config).expect("fixture must reopen at its final bound root");
    assert_eq!(reopened.goal_ids().len(), 2);
    println!("{}", root.join("fixture.json").display());
}

#[test]
fn large_valid_identity_wire_budget_preserves_positive_rows_below_rpc_limit() {
    let server = Server::new(vec![]);
    let (_temp, shared, goal) = fixture(&server.base);
    let (mut target, _) = capture(&mut shared.lock().unwrap(), &goal).unwrap();
    target.account = Some("a".repeat(1_000_000));
    let mut session = Session::new(target);
    for n in 0..5 {
        let id = format!("{}-{n}", "x".repeat(920_000));
        let raw = task(&id, "near response bound");
        assert!(
            serde_json::to_vec(&page(vec![raw.clone()], Some("next")))
                .unwrap()
                .len()
                < 1024 * 1024
        );
        session
            .merge(crate::todoist::InboxPage {
                results: vec![serde_json::from_value(raw).unwrap()],
                next_cursor: Some(format!("cursor-{n}")),
            })
            .unwrap();
    }
    assert_eq!(
        session.rows.len(),
        4,
        "a near-bound positive page must remain usable"
    );
    assert!(session.limited && !session.complete);
    let response = serde_json::to_vec(&session.view()).unwrap();
    assert!(
        response.len() > 7 * 1024 * 1024,
        "positive control exercises a large real wire response"
    );
    assert!(
        response.len() < 16 * 1024 * 1024,
        "ordinary RPC cap must not be raised"
    );
    server.finish();
}
