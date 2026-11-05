//! Real gateway requests on both native platforms; no WKWebView is created.
use super::*;
use ::core::prelude::v1::test;
use okilum_t3::{GatewayError, GatewayStatus};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Mutex;

struct Server {
    origin: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let log = requests.clone();
        let stopped = stop.clone();
        let worker = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopped.load(SeqCst) {
                    break;
                }
                let mut stream = stream.unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut headers = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    headers.push_str(&line);
                }
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                log.lock().unwrap().push(headers.clone());
                let command: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let project = json!({"id":"p","title":"Project","workspaceRoot":"/srv/project", "projectIcon":command["projectIcon"]});
                let (status, response) = if !headers.lines().any(|line| {
                    line.eq_ignore_ascii_case("authorization: Bearer synthetic-native-2520")
                }) {
                    (401, json!({}))
                } else if headers.starts_with("POST /api/projects/mutate ") {
                    (200, project)
                } else {
                    (200, json!({"projects":[project],"threads":[]}))
                };
                let body = response.to_string();
                write!(stream, "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        Self {
            origin,
            requests,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        let _ = TcpStream::connect(self.origin.trim_start_matches("http://"));
        let result = self.worker.take().unwrap().join();
        if !std::thread::panicking() {
            result.unwrap();
        }
    }
}

#[test]
fn board_projects_and_icon_gateway_use_native_token_without_cookies_2520() {
    let server = Server::start();
    let env = okilum_t3::Environment::new("fixture", &server.origin).unwrap();
    let token = okilum_t3::stream::NativeToken::from_secret(
        &env,
        &serde_json::to_vec(&json!({
            "origin":env.origin(),"access_token":"synthetic-native-2520", "expires_at":u64::MAX,
            "scope":"orchestration:read orchestration:operate"
        }))
        .unwrap(),
    )
    .unwrap();
    // No MCP token either: every successful call must exercise the native path.
    let mcp = Arc::new(okilum_t3::McpGateway::new(env.clone(), None).unwrap());
    let gateway =
        manager_gateway(env, mcp, Some(token), okilum_t3::NetworkAccess::default()).unwrap();
    assert_eq!(gateway.list_projects().unwrap()[0].id, "p");
    assert_eq!(gateway.read_project("p").unwrap().id, "p");
    let icon = ProjectIcon::Lucide {
        name: "rocket".into(),
        color: okilum_t3::projects::icon::IconColor::Violet,
    };
    assert_eq!(
        gateway
            .set_project_icon("p", Some(&icon))
            .unwrap()
            .project_icon,
        Some(icon)
    );
    assert_eq!(
        gateway.set_project_icon("p", None).unwrap().project_icon,
        None
    );
    assert!(gateway.list_threads("p").unwrap().is_empty());
    assert!(matches!(gateway.status(), GatewayStatus::Connected { .. }));
    let requests = server.requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        5,
        "positive control: all five operations reached HTTP"
    );
    for request in requests.iter() {
        assert!(!request.to_ascii_lowercase().contains("cookie:"));
        assert!(!request.contains("/api/auth/session"));
        assert!(!request.contains("/mcp"));
    }
    assert!(requests[4]
        .to_ascii_lowercase()
        .contains("x-t3-orchestration-protocol: 2"));
}

#[test]
fn webview_only_install_gets_the_shared_connect_state_2520() {
    let env = okilum_t3::Environment::new("fixture", "https://unpaired.example.test").unwrap();
    let mcp = Arc::new(okilum_t3::McpGateway::new(env.clone(), None).unwrap());
    let gateway = manager_gateway(env, mcp, None, okilum_t3::NetworkAccess::default()).unwrap();
    assert_eq!(gateway.status(), GatewayStatus::NotConnected);
    assert_eq!(
        gateway.list_projects().unwrap_err(),
        GatewayError::NotConnected
    );
    assert_eq!(
        gateway.read_project("p").unwrap_err(),
        GatewayError::NotConnected
    );
    assert_eq!(
        gateway.set_project_icon("p", None).unwrap_err(),
        GatewayError::NotConnected
    );
    assert_eq!(
        gateway.list_threads("p").unwrap_err(),
        GatewayError::NotConnected
    );
}
