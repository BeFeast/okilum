use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::*;
use crate::projects::icon::{self, IconColor, ProjectIcon, Saved};
use crate::testing::{FakeServer, Request, Response};

const GOOD: &str = "t3_session_abc=live";

/// The web view's cookie store: what it holds now, and how often it was read.
#[derive(Default)]
struct Store {
    cookies: Mutex<Vec<SessionCookie>>,
    reads: AtomicUsize,
    hosts: Mutex<Vec<String>>,
}

impl Store {
    fn holding(cookies: &[(&str, &str)]) -> Arc<Self> {
        let store = Arc::new(Self::default());
        store.set(cookies);
        store
    }

    fn set(&self, cookies: &[(&str, &str)]) {
        *self.cookies.lock().unwrap() = cookies
            .iter()
            .map(|(name, value)| SessionCookie {
                name: (*name).into(),
                value: (*value).into(),
            })
            .collect();
    }
}

impl SessionSource for Store {
    fn cookies(&self, host: &str, timeout: Duration) -> anyhow::Result<Vec<SessionCookie>> {
        assert!(timeout <= Duration::from_secs(5), "connecting is bounded");
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.hosts.lock().unwrap().push(host.into());
        Ok(self.cookies.lock().unwrap().clone())
    }
}

struct Broken;

impl SessionSource for Broken {
    fn cookies(&self, _: &str, _: Duration) -> anyhow::Result<Vec<SessionCookie>> {
        anyhow::bail!("the web view's cookie store did not answer within 5 s")
    }
}

fn t3_project(id: &str, icon: Value, deleted: bool) -> Value {
    json!({
        "id": id, "title": "Okilum", "workspaceRoot": "/srv/t3-host/okilum",
        "repositoryIdentity": null,
        "faviconPath": "crates/okilum-shell/assets/brand/app-icon-light.svg",
        "projectIcon": icon, "defaultModelSelection": null, "defaultThreadEnvMode": null,
        "autoPull": false, "scripts": [], "createdAt": "2026-10-03T10:08:44.917Z",
        "updatedAt": "2026-10-18T17:46:52.181Z",
        "deletedAt": if deleted { json!("2026-10-20T00:00:00.000Z") } else { Value::Null }
    })
}

/// T3's HTTP API as the web app uses it: the session cookie `GOOD` is
/// signed in; `/api/projects` lists, `/api/projects/mutate` updates.
fn fake_t3(stored: Arc<Mutex<Value>>) -> FakeServer {
    FakeServer::start(move |request| {
        let signed_in = request
            .header("cookie")
            .is_some_and(|cookie| cookie.split("; ").any(|part| part == GOOD));
        match (request.method.as_str(), request.path()) {
            ("GET", "/api/auth/session") => Response::json(
                200,
                json!({ "authenticated": signed_in, "auth": { "policy": "remote" } }),
            ),
            _ if !signed_in => Response::json(
                401,
                json!({ "_tag": "EnvironmentAuthInvalidError", "code": "auth_invalid",
                        "reason": "invalid_credential", "traceId": "t" }),
            ),
            ("GET", "/api/projects") => Response::json(
                200,
                json!({ "projects": [
                    t3_project("okilum", stored.lock().unwrap().clone(), false),
                    t3_project("gone", Value::Null, true),
                ], "updatedAt": "2026-10-24T09:00:00.000Z" }),
            ),
            ("GET", "/api/orchestration/shell")
                if request.header("x-t3-orchestration-protocol") != Some("2") =>
            {
                Response::empty(400)
            }
            ("GET", "/api/orchestration/shell") => Response::json(
                200,
                json!({
                    "projects": [{"id":"okilum"}],
                    "threads": [{
                        "id":"mcp:launch:executor", "projectId":"okilum", "title":"Editing",
                        "modelSelection":{"instanceId":"codex","model":"model"},
                        "runtimeMode":"full-access", "updatedAt":"2026-10-29T20:00:00Z",
                        "session":{"status":"running","activeTurnId":"run-current"},
                        "latestTurn":{"turnId":"run-current","state":"running"}
                    }]
                }),
            ),
            ("POST", "/api/projects/mutate") => {
                let command = request.json();
                if command["projectId"] != "okilum" {
                    return Response::json(
                        400,
                        json!({ "_tag": "EnvironmentRequestInvalidError", "code": "invalid_request",
                                "reason": "invalid_command", "traceId": "t" }),
                    );
                }
                *stored.lock().unwrap() = command["projectIcon"].clone();
                Response::json(
                    200,
                    t3_project("okilum", command["projectIcon"].clone(), false),
                )
            }
            other => panic!("unexpected request {other:?}"),
        }
    })
}

fn gateway(server: &FakeServer, source: Arc<dyn SessionSource>) -> SessionGateway {
    SessionGateway::new(Environment::new("t3", &server.origin).unwrap(), source).unwrap()
}

fn mutations(requests: &[Request]) -> Vec<&Request> {
    requests
        .iter()
        .filter(|request| request.path() == "/api/projects/mutate")
        .collect()
}

fn calculator() -> ProjectIcon {
    ProjectIcon::Lucide {
        name: "calculator".into(),
        color: IconColor::Green,
    }
}

#[test]
fn the_first_icon_save_connects_with_the_panels_session_cookie() {
    let stored = Arc::new(Mutex::new(Value::Null));
    let server = fake_t3(stored.clone());
    // A stale legacy cookie and another server's instance cookie share the host.
    let store = Store::holding(&[
        ("t3_session", "old"),
        ("t3_session_other", "elsewhere"),
        ("t3_session_abc", "live"),
        ("unrelated", "x"),
    ]);
    let t3 = gateway(&server, store.clone());
    assert_eq!(t3.status(), GatewayStatus::NotConnected);
    assert_eq!(
        store.reads.load(Ordering::SeqCst),
        0,
        "nothing runs before a call"
    );

    let icon = calculator();
    let project = t3.set_project_icon("okilum", Some(&icon)).unwrap();
    assert_eq!(project.project_icon.as_ref(), Some(&icon));
    assert_eq!(
        *stored.lock().unwrap(),
        json!({"kind": "lucide", "name": "calculator", "color": "green"})
    );
    assert_eq!(store.reads.load(Ordering::SeqCst), 1);
    assert_eq!(store.hosts.lock().unwrap().as_slice(), ["127.0.0.1"]);
    assert!(matches!(t3.status(), GatewayStatus::Connected { .. }));

    let requests = server.requests();
    let [mutate] = mutations(&requests)[..] else {
        panic!("one project.update: {requests:?}")
    };
    assert_eq!(mutate.header("cookie"), Some(GOOD));
    assert_eq!(mutate.header("authorization"), None);
    let command = mutate.json();
    assert_eq!(command["type"], "project.update");
    assert_eq!(command["projectId"], "okilum");
    assert!(command["commandId"]
        .as_str()
        .is_some_and(|id| id.len() == 36));
    assert_eq!(
        command.as_object().unwrap().len(),
        4,
        "only projectIcon changes: {command}"
    );

    // Connected now: the store is not read again, a reset is `null`.
    let reset = t3.set_project_icon("okilum", None).unwrap();
    assert_eq!(reset.project_icon, None);
    assert_eq!(store.reads.load(Ordering::SeqCst), 1);
    assert_eq!(
        mutations(&server.requests()).last().unwrap().json()["projectIcon"],
        Value::Null
    );
}

#[test]
fn projects_come_from_t3s_project_snapshot_without_deleted_ones() {
    let server = fake_t3(Arc::new(Mutex::new(
        json!({"kind": "emoji", "emoji": "🧠"}),
    )));
    let t3 = gateway(&server, Store::holding(&[("t3_session_abc", "live")]));
    let projects = t3.list_projects().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].id, "okilum");
    assert!(
        matches!(&projects[0].project_icon, Some(ProjectIcon::Emoji { emoji }) if emoji == "🧠")
    );
    assert_eq!(t3.read_project("okilum").unwrap().id, "okilum");
    let error = t3.read_project("gone").unwrap_err();
    assert!(error.to_string().contains("no project gone"), "{error}");
}

#[test]
fn no_session_is_not_paired_and_asks_for_no_server() {
    let server = fake_t3(Arc::new(Mutex::new(Value::Null)));
    let t3 = gateway(&server, Store::holding(&[("unrelated", "x")]));
    let error = t3
        .set_project_icon("okilum", Some(&calculator()))
        .unwrap_err();
    assert_eq!(
        error,
        GatewayError::NotPaired {
            origin: server.origin.clone()
        }
    );
    let message = error.to_string();
    assert!(
        message.contains(&server.origin) && message.contains("Manager panel"),
        "{message}"
    );
    assert!(!message.contains("not connected to T3"), "{message}");
    assert!(server.requests().is_empty());
    assert_eq!(t3.status(), GatewayStatus::NotConnected);
}

#[test]
fn a_signed_out_session_reads_the_store_again_once_and_then_says_so() {
    let server = fake_t3(Arc::new(Mutex::new(Value::Null)));
    let store = Store::holding(&[("t3_session_abc", "expired")]);
    let t3 = gateway(&server, store.clone());
    let error = t3.list_projects().unwrap_err();
    assert_eq!(error, GatewayError::Revoked);
    assert_eq!(t3.status(), GatewayStatus::Revoked);

    // The user signs in again in the Manager panel: the next call finds it.
    store.set(&[("t3_session_abc", "live")]);
    t3.set_project_icon("okilum", Some(&calculator())).unwrap();
    assert!(matches!(t3.status(), GatewayStatus::Connected { .. }));

    // A cookie T3 drops between calls is replaced from the store once.
    let reads = store.reads.load(Ordering::SeqCst);
    *t3.active.lock().unwrap() = Some(SessionCookie {
        name: "t3_session_abc".into(),
        value: "dropped".into(),
    });
    t3.set_project_icon("okilum", None).unwrap();
    assert_eq!(store.reads.load(Ordering::SeqCst), reads + 1);
}

#[test]
fn t3s_refusal_reaches_the_user_with_t3s_words() {
    let server = fake_t3(Arc::new(Mutex::new(Value::Null)));
    let t3 = gateway(&server, Store::holding(&[("t3_session_abc", "live")]));
    let error = t3
        .set_project_icon("missing", Some(&calculator()))
        .unwrap_err();
    assert_eq!(
        error,
        GatewayError::Tool {
            code: "invalid_request".into(),
            message: "invalid_command".into()
        }
    );
    assert_eq!(
        t3_refusal(
            403,
            r#"{"code":"insufficient_scope","requiredScope":"orchestration:operate"}"#
        ),
        GatewayError::Tool {
            code: "insufficient_scope".into(),
            message: "needs the orchestration:operate scope".into()
        }
    );
    assert_eq!(
        t3_refusal(502, "Bad gateway"),
        GatewayError::Tool {
            code: "http_502".into(),
            message: "Bad gateway".into()
        }
    );
}

#[test]
fn an_unreachable_server_or_store_is_unreachable_not_unpaired() {
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let t3 = SessionGateway::new(
        Environment::new("t3", &closed).unwrap(),
        Store::holding(&[("t3_session_abc", "live")]),
    )
    .unwrap();
    let error = t3.list_projects().unwrap_err();
    assert!(matches!(error, GatewayError::Unreachable(_)), "{error:?}");
    assert!(matches!(t3.status(), GatewayStatus::Unreachable { .. }));

    let server = fake_t3(Arc::new(Mutex::new(Value::Null)));
    let error = gateway(&server, Arc::new(Broken))
        .list_projects()
        .unwrap_err();
    assert!(
        matches!(&error, GatewayError::Unreachable(detail) if detail.contains("within 5 s")),
        "{error:?}"
    );
}

/// The MCP side of a paired connection, for the fallback.
struct Mcp {
    connected: bool,
    icons: AtomicUsize,
}

impl ManagerGateway for Mcp {
    fn status(&self) -> GatewayStatus {
        if self.connected {
            GatewayStatus::Connected {
                expires_at: u64::MAX,
            }
        } else {
            GatewayStatus::NotConnected
        }
    }
    fn server_identity(&self) -> GatewayResult<crate::ServerIdentity> {
        Err(GatewayError::NotConnected)
    }
    fn list_projects(&self) -> GatewayResult<Vec<Project>> {
        Ok(vec![Project {
            id: "via-mcp".into(),
            ..Default::default()
        }])
    }
    fn set_project_icon(
        &self,
        project_id: &str,
        icon: Option<&ProjectIcon>,
    ) -> GatewayResult<Project> {
        self.icons.fetch_add(1, Ordering::SeqCst);
        Ok(Project {
            id: project_id.into(),
            project_icon: icon.cloned(),
            ..Default::default()
        })
    }
    fn list_threads(&self, _: &str) -> GatewayResult<Vec<ThreadSummary>> {
        if self.connected {
            Ok(Vec::new())
        } else {
            Err(GatewayError::NotConnected)
        }
    }
    fn read_thread(&self, _: &ThreadQuery) -> GatewayResult<ThreadPage> {
        Err(GatewayError::NotConnected)
    }
    fn send_message(&self, _: &OutgoingMessage) -> GatewayResult<SendReceipt> {
        Err(GatewayError::NotConnected)
    }
    fn pending_requests(&self, _: &str) -> GatewayResult<Vec<String>> {
        Ok(Vec::new())
    }
    fn capabilities(&self) -> GatewayResult<Capabilities> {
        Err(GatewayError::NotConnected)
    }
    fn thread_configuration(&self, _: &str) -> GatewayResult<ThreadConfiguration> {
        Err(GatewayError::NotConnected)
    }
    fn configure_model(&self, _: &str, _: &ModelSelection) -> GatewayResult<ThreadConfiguration> {
        Err(GatewayError::NotConnected)
    }
}

#[test]
fn a_paired_connection_reads_board_threads_and_saves_icons_with_the_manager_session() {
    let stored = Arc::new(Mutex::new(Value::Null));
    let server = fake_t3(stored.clone());
    let mcp = Arc::new(Mcp {
        connected: false,
        icons: AtomicUsize::new(0),
    });
    let paired: Arc<dyn ManagerGateway> = Arc::new(PairedGateway::new(
        gateway(&server, Store::holding(&[("t3_session_abc", "live")])),
        mcp.clone(),
    ));
    // The whole Save path the dialog runs: list, match, update, read back.
    let vault = std::env::temp_dir().join(format!("okilum-session-icon-{}", uuid::Uuid::new_v4()));
    let ops = vault.join("Dev/Areas/okilum/ops");
    std::fs::create_dir_all(&ops).unwrap();
    std::fs::write(
        ops.join("project.yaml"),
        "name: Okilum\nt3:\n  environment: t3\n  executor: {project_id: okilum}\n",
    )
    .unwrap();
    let okilum = crate::projects::Project::read(&vault.join("Dev/Areas/okilum")).unwrap();
    let saved =
        icon::save_icon(&vault, &okilum, Some(paired.as_ref()), Some(&calculator())).unwrap();
    let Saved::T3(link) = saved else {
        panic!("T3 keeps the icon: {saved:?}")
    };
    assert_eq!(link.project_id, "okilum");
    assert_eq!(link.icon, Some(calculator()));
    assert_eq!(
        *stored.lock().unwrap(),
        json!({"kind": "lucide", "name": "calculator", "color": "green"})
    );
    assert_eq!(mcp.icons.load(Ordering::SeqCst), 0);
    assert!(
        !std::fs::read_to_string(ops.join("project.yaml"))
            .unwrap()
            .contains("icon:"),
        "project.yaml is untouched"
    );
    let threads = paired
        .list_threads("okilum")
        .expect("the Manager cookie works without an MCP token");
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].thread_id, "mcp:launch:executor");
    assert_eq!(threads[0].active_run_id.as_deref(), Some("run-current"));
    assert_eq!(threads[0].status, "running");
    let mut binding =
        crate::board::Binding::parse(include_str!("../../tests/fixtures/board-legacy.yaml"))
            .unwrap();
    let scope = &mut binding.issues[0];
    scope.agent_thread_id = Some(threads[0].thread_id.clone());
    scope.current_task = true;
    let issue: crate::board::Issue =
        serde_json::from_value(json!({"number":scope.number,"title":"Task","state":"open"}))
            .unwrap();
    assert_eq!(
        crate::board::column(scope, Some(&issue), &threads),
        crate::board::Column::Working
    );
    assert!(server
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|r| r.path() == "/api/orchestration/shell" && r.header("cookie") == Some(GOOD)));
    std::fs::remove_dir_all(vault).unwrap();
}

#[test]
fn without_a_paired_session_an_mcp_token_still_serves_icons() {
    let server = fake_t3(Arc::new(Mutex::new(Value::Null)));
    let connected = Arc::new(Mcp {
        connected: true,
        icons: AtomicUsize::new(0),
    });
    let paired = PairedGateway::new(
        gateway(&server, Arc::new(NoSessionSource)),
        connected.clone(),
    );
    assert!(
        paired.list_threads("okilum").unwrap().is_empty(),
        "no browser session preserves the existing MCP read path"
    );
    assert_eq!(paired.list_projects().unwrap()[0].id, "via-mcp");
    paired
        .set_project_icon("okilum", Some(&calculator()))
        .unwrap();
    assert_eq!(connected.icons.load(Ordering::SeqCst), 1);

    // Neither: the precise reason, not "not connected to T3".
    let none = PairedGateway::new(
        gateway(&server, Arc::new(NoSessionSource)),
        Arc::new(Mcp {
            connected: false,
            icons: AtomicUsize::new(0),
        }),
    );
    let error = none.set_project_icon("okilum", None).unwrap_err();
    assert!(matches!(error, GatewayError::NotPaired { .. }), "{error:?}");
    // An unreachable T3 is never masked by the fallback.
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let down = PairedGateway::new(
        SessionGateway::new(
            Environment::new("t3", &closed).unwrap(),
            Store::holding(&[("t3_session_abc", "live")]),
        )
        .unwrap(),
        connected.clone(),
    );
    assert!(matches!(
        down.list_projects(),
        Err(GatewayError::Unreachable(_))
    ));
    assert_eq!(connected.icons.load(Ordering::SeqCst), 1);
}

#[test]
fn board_session_scope_refusal_is_concrete_and_does_not_silently_switch_to_mcp() {
    let server = FakeServer::start(|request| {
        assert_eq!(request.header("cookie"), Some(GOOD));
        match request.path() {
            "/api/auth/session" => Response::json(200, json!({"authenticated":true})),
            "/api/orchestration/shell" => Response::json(
                403,
                json!({"code":"scope_denied","requiredScope":"orchestration:read"}),
            ),
            path => panic!("unexpected request {path}"),
        }
    });
    let paired = PairedGateway::new(
        gateway(&server, Store::holding(&[("t3_session_abc", "live")])),
        Arc::new(Mcp {
            connected: true,
            icons: AtomicUsize::new(0),
        }),
    );
    let error = paired.list_threads("okilum").unwrap_err().to_string();
    assert!(error.contains("scope_denied") && error.contains("orchestration:read"));
    assert!(
        !error.contains("live"),
        "diagnostics never include the session secret"
    );
}

/// #1475: «Create the project in T3» is T3's web app's own `project.create`
/// over the paired session; T3's refusal reaches the user in T3's words.
#[test]
fn a_new_project_is_t3s_project_create_with_the_panels_session() {
    let server = FakeServer::start(|request| {
        let signed_in = request
            .header("cookie")
            .is_some_and(|cookie| cookie.split("; ").any(|part| part == GOOD));
        match (request.method.as_str(), request.path()) {
            ("GET", "/api/auth/session") => {
                Response::json(200, json!({ "authenticated": signed_in }))
            }
            ("POST", "/api/projects/mutate") if signed_in => {
                let command = request.json();
                if command["workspaceRoot"] == "/srv/missing" {
                    return Response::json(
                        400,
                        json!({ "_tag": "ProjectMutationError", "code": "workspace_root_not_found",
                                "message": "Workspace root /srv/missing does not exist" }),
                    );
                }
                let mut project = t3_project("x", Value::Null, false);
                project["id"] = command["projectId"].clone();
                project["title"] = command["title"].clone();
                project["workspaceRoot"] = command["workspaceRoot"].clone();
                Response::json(200, project)
            }
            other => panic!("unexpected request {other:?}"),
        }
    });
    let t3 = gateway(&server, Store::holding(&[("t3_session_abc", "live")]));
    let project = t3.create_project("HomeLab", "/srv/vault/HomeLab").unwrap();
    assert_eq!(project.title, "HomeLab");
    assert_eq!(project.workspace_root, "/srv/vault/HomeLab");

    let requests = server.requests();
    let [create] = mutations(&requests)[..] else {
        panic!("one project.create: {requests:?}")
    };
    assert_eq!(create.header("cookie"), Some(GOOD));
    let command = create.json();
    assert_eq!(command["type"], "project.create");
    assert_eq!(command["title"], "HomeLab");
    assert_eq!(command["workspaceRoot"], "/srv/vault/HomeLab");
    assert_eq!(command["projectId"], project.id.as_str());
    assert!(command["projectId"]
        .as_str()
        .is_some_and(|id| id.len() == 36));
    assert!(command["commandId"]
        .as_str()
        .is_some_and(|id| id.len() == 36));
    assert_ne!(command["projectId"], command["commandId"]);
    assert_eq!(
        command.as_object().unwrap().len(),
        5,
        "T3's create payload, nothing more: {command}"
    );

    let error = t3.create_project("Gone", "/srv/missing").unwrap_err();
    assert_eq!(
        error,
        GatewayError::Tool {
            code: "workspace_root_not_found".into(),
            message: "Workspace root /srv/missing does not exist".into()
        }
    );
}

#[test]
fn board_shell_requires_protocol_two_in_addition_to_the_working_manager_cookie() {
    let server = fake_t3(Arc::new(Mutex::new(Value::Null)));
    let response = Client::new()
        .get(format!("{}/api/orchestration/shell", server.origin))
        .header(COOKIE, GOOD)
        .send()
        .unwrap();
    assert_eq!(
        response.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "positive control: the live route schema rejects the old request even with a valid cookie"
    );
    let session = gateway(&server, Store::holding(&[("t3_session_abc", "live")]));
    let threads = session.list_threads("okilum").unwrap();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].thread_id, "mcp:launch:executor");
    let requests = server.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.path() == "/api/orchestration/shell"
                && r.header("x-t3-orchestration-protocol") == Some("2"))
            .count(),
        1
    );
    assert!(
        requests.iter().any(|r| r.path() == "/api/auth/session"),
        "real paired-session authentication ran"
    );
}

#[test]
fn disabled_module_refuses_session_http_before_cookie_discovery() {
    let access = crate::NetworkAccess::new(true);
    let server = fake_t3(Arc::new(Mutex::new(Value::Null)));
    let source = Store::holding(&[("t3_session_abc", "live")]);
    let client = {
        let _scope = access.enter();
        gateway(&server, source.clone())
    };
    client.connect().unwrap();
    assert!(
        !server.requests().is_empty(),
        "positive control: a valid browser session reaches T3"
    );
    let before = server.requests().len();
    let cookie_reads = source.reads.load(Ordering::SeqCst);
    access.revoke();
    assert!(matches!(client.connect(), Err(GatewayError::NotConnected)));
    assert!(matches!(
        client.list_projects(),
        Err(GatewayError::NotConnected)
    ));
    assert_eq!(server.requests().len(), before);
    assert_eq!(
        source.reads.load(Ordering::SeqCst),
        cookie_reads,
        "disabled access cannot even ask the webview to reconnect"
    );
}
