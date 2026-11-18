//! #1422 POC acceptance against a real T3 server, on the same `okilum_t3`
//! paths the Project tab uses. One phase per invocation; every phase prints
//! one JSON object per check on stdout and exits non-zero when a check fails.
//! Runbook and recorded results: `docs/t3-poc-acceptance.md`.
//!
//! The token lives only in the file named by `OKILUM_ACCEPTANCE_TOKEN`
//! (created 0600 by `connect`), a disposable stand-in for the shell's OS
//! keychain entry; the run revokes it on the server at the end.
use anyhow::{bail, ensure, Context, Result};
use okilum_t3::{
    chat::Conversation,
    executors::Snapshot,
    launch::{
        progress::{Outcome, Stage, Tracker},
        Draft, LaunchContext, Request,
    },
    manifest::AreaBinding,
    models::Target,
    oauth::{Authorization, Input, Progress, DEFAULT_WAIT},
    reconnect, Environment, GatewayError, GatewayStatus, ManagerGateway, McpGateway,
    ModelSelection, OutgoingMessage, StoredToken, ThreadQuery,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const USAGE: &str = "usage: poc_acceptance <origin> <area> \
connect | chat | executors | launch <role> <worktree> <branch> [instance/model] | \
model <instance> <model> | drop-resend | revoked <revoke-script>";

fn main() {
    let ok = match run() {
        Ok(ok) => ok,
        Err(error) => {
            emit("error", false, json!({ "error": format!("{error:#}") }));
            false
        }
    };
    std::process::exit(if ok { 0 } else { 1 });
}

fn emit(check: &str, pass: bool, detail: Value) {
    println!(
        "{}",
        json!({ "check": check, "pass": pass, "at": now_ms(), "detail": detail })
    );
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn run() -> Result<bool> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ensure!(args.len() >= 3, "{USAGE}");
    let env = Environment::new("acceptance", &args[0])?;
    let area = PathBuf::from(&args[1]);
    let binding = AreaBinding::read(&area)?.context("the area has no t3 manifest")?;
    let token_file = PathBuf::from(
        std::env::var("OKILUM_ACCEPTANCE_TOKEN").context("set OKILUM_ACCEPTANCE_TOKEN")?,
    );
    let rest = &args[3..];
    match (args[2].as_str(), rest) {
        ("connect", []) => connect(&env, &token_file),
        ("chat", []) => chat(&gateway(&env, &token_file)?, &binding),
        ("executors", []) => executors(&gateway(&env, &token_file)?, &area, &binding),
        ("launch", [role, worktree, branch, model @ ..]) if model.len() <= 1 => launch(
            &gateway(&env, &token_file)?,
            &area,
            &binding,
            role,
            worktree,
            branch,
            model.first().map(String::as_str),
        ),
        ("model", [instance, model]) => {
            configure(&gateway(&env, &token_file)?, &binding, instance, model)
        }
        ("drop-resend", []) => drop_resend(&env, &read_token(&token_file)?, &binding),
        ("revoked", [script]) => revoked(&gateway(&env, &token_file)?, Path::new(script)),
        _ => bail!("{USAGE}"),
    }
}

fn read_token(path: &Path) -> Result<StoredToken> {
    StoredToken::from_secret(&std::fs::read(path).context("no token: run connect first")?)
}

fn gateway(env: &Environment, token_file: &Path) -> Result<McpGateway> {
    let token = read_token(token_file)?;
    // The Project tab's reconnect path: a saved credential, never a new browser sign-in.
    let connected = reconnect::connect(env, Some((env.credential_url(), token)), false, || {
        bail!("the saved credential was refused; this phase never signs in again")
    })?;
    Ok(connected.gateway)
}

fn connect(env: &Environment, token_file: &Path) -> Result<bool> {
    let authorization = Authorization::prepare(env, "Okilum #1422 acceptance")?;
    // The authorization URL is not a secret (PKCE): print it for the approver.
    eprintln!("AUTHORIZE_URL {}", authorization.url());
    let (send, input) = mpsc::channel();
    thread::spawn(move || {
        // A pasted callback URL, for an approval made in a browser elsewhere.
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if !line.trim().is_empty() && send.send(Input::Callback(line)).is_err() {
                break;
            }
        }
        // Keep the channel open: closing it would cancel the attempt.
        loop {
            thread::park();
        }
    });
    let token = authorization.wait(
        input,
        |progress| {
            if let Progress::InvalidCallback(message) = progress {
                eprintln!("callback refused: {message}");
            }
        },
        DEFAULT_WAIT,
    )?;
    write_private(token_file, &token.to_secret())?;
    // Reuse the saved credential exactly like a restart of the app does.
    let reused = reconnect::connect(
        env,
        Some((env.credential_url(), read_token(token_file)?)),
        false,
        || bail!("reconnect asked for a new browser sign-in"),
    )?;
    let identity = reused.gateway.server_identity()?;
    let pass = reused.issued_token.is_none()
        && matches!(reused.gateway.status(), GatewayStatus::Connected { .. });
    emit(
        "connect",
        pass,
        json!({
            "origin": env.origin(),
            "client_id": token.client_id,
            "scope": token.scope,
            "expires_at": token.expires_at,
            "credential_key": env.credential_url(),
            "status": format!("{:?}", reused.gateway.status()),
            "reconnect_without_browser": reused.issued_token.is_none(),
            "server": { "environment_id": identity.environment_id, "label": identity.label },
        }),
    );
    Ok(pass)
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("cannot create {}", path.display()))?;
    file.write_all(bytes)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(_: &Path, _: &[u8]) -> Result<()> {
    bail!("the acceptance runner targets a Unix host")
}

fn manager(binding: &AreaBinding) -> Result<String> {
    binding
        .manager
        .thread_id
        .clone()
        .context("the area has no manager thread")
}

fn item_summary(item: &okilum_t3::TimelineItem) -> Value {
    json!({
        "position": item.position,
        "item_id": item.item_id,
        "type": item.kind,
        "status": item.status,
        "chars": item.text.as_deref().map_or(0, |t| t.chars().count()),
    })
}

fn chat(gateway: &McpGateway, binding: &AreaBinding) -> Result<bool> {
    let manager = manager(binding)?;
    let mut conversation = Conversation::new(&manager);
    let mut pages = 0;
    while conversation.has_more && pages < 50 {
        conversation.poll(gateway)?;
        pages += 1;
    }
    let history_ok = !conversation.items.is_empty() && !conversation.has_more;
    emit(
        "chat.history",
        history_ok,
        json!({
            "thread_id": manager,
            "items": conversation.items.len(),
            "pages": pages,
            "first": conversation.items.values().next().map(item_summary),
            "last": conversation.items.values().last().map(item_summary),
            "pending_questions": conversation.pending_questions,
        }),
    );
    let before = conversation.cursor;
    let message = OutgoingMessage::queue(
        &manager,
        "Okilum #1422 acceptance (long growing reply): without using any tools, reply with \
         the numbers 1 to 120, one per line, each followed by one short English word. \
         Nothing else.",
    );
    let receipt = gateway.send_message(&message)?;
    emit(
        "chat.send",
        !receipt.message_id.is_empty(),
        json!({ "client_request_id": message.client_request_id, "receipt": receipt }),
    );
    // Follow the reply as the Project tab does: poll, re-read growing items.
    let started = Instant::now();
    let mut lengths: BTreeMap<String, Vec<(u128, String, usize)>> = BTreeMap::new();
    let mut settled = None;
    while started.elapsed() < Duration::from_secs(15 * 60) {
        conversation.poll(gateway)?;
        for item in conversation
            .items
            .values()
            .filter(|i| before.is_none_or(|b| i.position > b))
            .filter(|i| i.kind.contains("assistant"))
        {
            let len = item.text.as_deref().map_or(0, |t| t.chars().count());
            let seen = lengths.entry(item.item_id.clone()).or_default();
            if seen.last().map(|(_, s, l)| (s.as_str(), *l)) != Some((item.status.as_str(), len)) {
                seen.push((started.elapsed().as_millis(), item.status.clone(), len));
            }
        }
        let mut query = ThreadQuery::messages(&manager);
        query.limit = Some(1);
        query.max_chars_per_item = Some(1);
        let thread = gateway.read_thread(&query)?.thread;
        let reply_done = conversation
            .items
            .values()
            .filter(|i| before.is_none_or(|b| i.position > b))
            .any(|i| i.kind.contains("assistant") && i.status == "completed");
        if thread.active_run_id.is_none() && reply_done {
            settled = Some(started.elapsed().as_millis());
            break;
        }
        thread::sleep(Duration::from_millis(400));
    }
    let (grown, growth) = lengths
        .iter()
        .max_by_key(|(_, seen)| seen.len())
        .map(|(id, seen)| {
            let distinct_growing = seen.windows(2).filter(|w| w[1].2 > w[0].2).count();
            (
                distinct_growing >= 2,
                json!({ "item_id": id, "observations": seen }),
            )
        })
        .unwrap_or((false, Value::Null));
    let final_chars = lengths
        .values()
        .filter_map(|seen| seen.last().map(|(_, _, l)| *l))
        .max()
        .unwrap_or(0);
    let pass = settled.is_some() && grown;
    emit(
        "chat.growing_reply",
        pass,
        json!({ "settled_after_ms": settled, "final_chars": final_chars, "growth": growth }),
    );
    Ok(history_ok && pass)
}

fn executors(gateway: &McpGateway, area: &Path, binding: &AreaBinding) -> Result<bool> {
    let identity = gateway.server_identity()?;
    let snapshot = Snapshot::load(gateway, area, binding)?;
    let rows: Vec<Value> = snapshot
        .registered
        .iter()
        .map(|row| {
            json!({
                "role": row.entry.role,
                "thread_id": row.entry.thread_id,
                "listed": row.thread.is_some(),
                "status": row.thread.as_ref().map(|t| t.status.clone()),
                "item_count": row.thread.as_ref().map(|t| t.item_count),
                "pull_requests": row.thread.as_ref().map(|t| t.pull_request_urls()),
                "pending_questions": row.pending_questions,
                "question_error": row.question_error,
                "open_in_t3": identity.thread_url(&row.entry.thread_id),
            })
        })
        .collect();
    let pass = !rows.is_empty()
        && snapshot
            .registered
            .iter()
            .all(|row| row.thread.is_some() && row.pending_questions.is_some());
    emit(
        "executors.tree",
        pass,
        json!({
            "manager": snapshot.manager,
            "registered": rows,
            "unregistered": snapshot.unregistered.iter().map(|t| json!({
                "thread_id": t.thread_id, "status": t.status, "item_count": t.item_count,
            })).collect::<Vec<_>>(),
        }),
    );
    Ok(pass)
}

fn parse_model(value: &str) -> Result<ModelSelection> {
    let (instance, model) = value
        .split_once('/')
        .context("model override is instance/model")?;
    Ok(ModelSelection {
        instance_id: instance.into(),
        model: model.into(),
        options: Vec::new(),
    })
}

fn launch(
    gateway: &McpGateway,
    area: &Path,
    binding: &AreaBinding,
    role: &str,
    worktree: &str,
    branch: &str,
    model: Option<&str>,
) -> Result<bool> {
    let manager = manager(binding)?;
    let context = LaunchContext::load(gateway, binding)?;
    let draft = Draft {
        role: role.into(),
        brief_path: "specs/executor-check.md".into(),
        worktree_path: worktree.into(),
        branch: branch.into(),
        model_override: model.map(parse_model).transpose()?,
    };
    let request = Request::prepare(area, binding, &context, &draft)?;
    let baseline = gateway.list_threads(&binding.executor.project_id)?;
    let message = OutgoingMessage::queue(&manager, request.message()?);
    let receipt = gateway.send_message(&message)?;
    emit(
        "launch.request",
        true,
        json!({
            "request_id": request.request_id,
            "client_request_id": message.client_request_id,
            "model": request.model_selection,
            "worktree": worktree, "branch": branch,
            "baseline_threads": baseline.len(),
            "receipt": receipt,
        }),
    );
    let mut tracker = Tracker::new(
        &request,
        message.client_request_id.clone(),
        manager.clone(),
        Some(&baseline),
    );
    let started = Instant::now();
    let mut steps = vec![(0u128, tracker.progress.line())];
    while started.elapsed() < Duration::from_secs(25 * 60) {
        match tracker.observe(gateway, Some(&receipt)) {
            Ok(Some(observation)) => {
                if tracker.apply(observation) {
                    let line = tracker.progress.line();
                    eprintln!("launch: {line}");
                    steps.push((started.elapsed().as_millis(), line));
                }
            }
            Ok(None) => {}
            Err(error) => eprintln!("launch observation failed: {error:#}"),
        }
        if tracker.progress.finished() || tracker.progress.reached == Stage::Running {
            break;
        }
        thread::sleep(Duration::from_secs(3));
    }
    let progress = &tracker.progress;
    let pass = progress.reached == Stage::Running
        && matches!(progress.outcome, None | Some(Outcome::Completed));
    emit(
        "launch.progress",
        pass,
        json!({
            "steps": steps,
            "reached": progress.reached.label(),
            "outcome": progress.outcome.map(|o| o.label()),
            "executor": progress.executor.as_ref().map(|e| json!({
                "thread_id": e.thread_id, "title": e.title, "status": e.status,
            })),
            "note": progress.note,
        }),
    );
    Ok(pass)
}

fn configure(
    gateway: &McpGateway,
    binding: &AreaBinding,
    instance: &str,
    model: &str,
) -> Result<bool> {
    let target = Target {
        thread_id: manager(binding)?,
        project_id: binding
            .manager
            .project_id
            .clone()
            .context("no manager project")?,
    };
    let before = target.load(gateway)?;
    let selection = before.selection(instance, model);
    let configured = target.configure(gateway, &selection)?;
    // An independent read-back, as the picker does after a refresh.
    let read_back = gateway.thread_configuration(&target.thread_id)?;
    let pass = read_back.model_selection.instance_id == instance
        && read_back.model_selection.model == model
        && configured.configuration.model_selection == read_back.model_selection;
    emit(
        "model.change",
        pass,
        json!({
            "thread_id": target.thread_id,
            "before": before.configuration.model_selection,
            "requested": selection,
            "configure_readback": configured.configuration.model_selection,
            "independent_readback": read_back.model_selection,
        }),
    );
    Ok(pass)
}

/// A loopback HTTP → HTTPS forwarder that drops the client connection after
/// T3 has processed one chosen request: the reply never reaches Okilum.
struct DroppingProxy {
    port: u16,
    drop_marker: Arc<Mutex<Option<String>>>,
    dropped: Arc<AtomicBool>,
}

impl DroppingProxy {
    fn start(origin: String) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let drop_marker = Arc::new(Mutex::new(None::<String>));
        let dropped = Arc::new(AtomicBool::new(false));
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .build()?;
        let (marker, flag) = (drop_marker.clone(), dropped.clone());
        thread::spawn(move || {
            for socket in listener.incoming().map_while(Result::ok) {
                let (client, origin, marker, flag) =
                    (client.clone(), origin.clone(), marker.clone(), flag.clone());
                thread::spawn(move || {
                    if let Err(error) = forward(socket, &client, &origin, &marker, &flag) {
                        eprintln!("proxy: {error:#}");
                    }
                });
            }
        });
        Ok(Self {
            port,
            drop_marker,
            dropped,
        })
    }
}

fn forward(
    socket: TcpStream,
    client: &reqwest::blocking::Client,
    origin: &str,
    marker: &Mutex<Option<String>>,
    dropped: &AtomicBool,
) -> Result<()> {
    let mut reader = BufReader::new(socket.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().context("no method")?.to_owned();
    let path = parts.next().context("no path")?.to_owned();
    let mut headers = Vec::new();
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':').context("bad header")?;
        let (name, value) = (name.trim().to_owned(), value.trim().to_owned());
        if name.eq_ignore_ascii_case("content-length") {
            length = value.parse()?;
        }
        if !["host", "connection", "content-length", "accept-encoding"]
            .iter()
            .any(|skip| name.eq_ignore_ascii_case(skip))
        {
            headers.push((name, value));
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let mut request = client.request(method.parse()?, format!("{origin}{path}"));
    for (name, value) in &headers {
        request = request.header(name, value);
    }
    let response = request.body(body.clone()).send()?;
    let status = response.status();
    let response_headers = response.headers().clone();
    let response_body = response.bytes()?;
    let drop_this = marker
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|m| String::from_utf8_lossy(&body).contains(m.as_str()));
    let mut socket = socket;
    if drop_this && !dropped.swap(true, Ordering::SeqCst) {
        // T3 has answered; Okilum never sees it.
        socket.shutdown(Shutdown::Both)?;
        return Ok(());
    }
    write!(
        socket,
        "HTTP/1.1 {} {}\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    )?;
    for (name, value) in &response_headers {
        if ["content-length", "transfer-encoding", "connection"]
            .iter()
            .any(|skip| name.as_str().eq_ignore_ascii_case(skip))
        {
            continue;
        }
        socket.write_all(format!("{name}: ").as_bytes())?;
        socket.write_all(value.as_bytes())?;
        socket.write_all(b"\r\n")?;
    }
    write!(
        socket,
        "content-length: {}\r\nconnection: close\r\n\r\n",
        response_body.len()
    )?;
    socket.write_all(&response_body)?;
    Ok(())
}

fn user_copies(gateway: &McpGateway, thread_id: &str, text: &str) -> Result<usize> {
    let mut query = ThreadQuery::messages(thread_id).from_start();
    query.limit = Some(100);
    query.max_chars_per_item = Some(4_000);
    let mut copies = 0;
    for _ in 0..100 {
        let page = gateway.read_thread(&query)?;
        copies += page
            .items
            .iter()
            .filter(|i| i.text.as_deref() == Some(text))
            .count();
        match page.next_position {
            Some(next) if page.has_more => query = query.after(next),
            _ => break,
        }
    }
    Ok(copies)
}

fn drop_resend(env: &Environment, token: &StoredToken, binding: &AreaBinding) -> Result<bool> {
    let manager = manager(binding)?;
    let project = binding
        .manager
        .project_id
        .clone()
        .context("no manager project")?;
    let direct = McpGateway::new(env.clone(), Some(token.clone()))?;
    direct.connect()?;
    let threads_before = direct.list_threads(&project)?.len();
    let proxy = DroppingProxy::start(env.origin().trim_end_matches('/').to_owned())?;
    let proxied_env = Environment::new(
        "acceptance-drop",
        &format!("http://127.0.0.1:{}", proxy.port),
    )?;
    let proxied = McpGateway::new(proxied_env, Some(token.clone()))?;
    proxied.connect()?;
    let message = OutgoingMessage::queue(
        &manager,
        format!(
            "Okilum #1422 acceptance (connection drop during send, {}): reply with the single word OK, no tools.",
            uuid_suffix()
        ),
    );
    *proxy.drop_marker.lock().unwrap() = Some(message.client_request_id.clone());
    let first = proxied.send_message(&message);
    let first_lost = first.is_err() && proxy.dropped.load(Ordering::SeqCst);
    emit(
        "drop.first_send",
        first_lost,
        json!({
            "client_request_id": message.client_request_id,
            "result": match &first { Ok(r) => json!({ "receipt": r }), Err(e) => json!({ "error": e.to_string() }) },
            "proxy_dropped_after_server_reply": proxy.dropped.load(Ordering::SeqCst),
            "status_after_drop": format!("{:?}", proxied.status()),
        }),
    );
    // The journal's Retry: the very same message and clientRequestId.
    let resent = direct.send_message(&message)?;
    let again = direct.send_message(&message)?;
    thread::sleep(Duration::from_secs(2));
    let copies = user_copies(&direct, &manager, &message.text)?;
    let threads_after = direct.list_threads(&project)?.len();
    let pass = first_lost
        && resent.message_id == again.message_id
        && copies == 1
        && threads_after == threads_before;
    emit(
        "drop.resend",
        pass,
        json!({
            "resend_receipt": resent,
            "second_resend_message_id": again.message_id,
            "copies_in_timeline": copies,
            "manager_project_threads": { "before": threads_before, "after": threads_after },
        }),
    );
    Ok(pass)
}

fn uuid_suffix() -> String {
    format!("{:x}", now_ms())
}

fn revoked(gateway: &McpGateway, script: &Path) -> Result<bool> {
    let page = gateway.list_projects()?;
    let before = format!("{:?}", gateway.status());
    emit(
        "revoke.before",
        matches!(gateway.status(), GatewayStatus::Connected { .. }),
        json!({ "status": before, "projects_visible": page.len() }),
    );
    let output = std::process::Command::new(script)
        .output()
        .context("cannot run the revoke script")?;
    let revoked_at = Instant::now();
    ensure!(
        output.status.success(),
        "the revoke script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The next request after the server-side revoke, with no retry or wait.
    let result = gateway.list_projects();
    let elapsed = revoked_at.elapsed().as_millis();
    let status = gateway.status();
    let refused = matches!(
        result,
        Err(GatewayError::Revoked | GatewayError::InvalidCredential)
    );
    let pass = refused && status == GatewayStatus::Revoked;
    emit(
        "revoke.after",
        pass,
        json!({
            "first_request_error": result.err().map(|e| e.to_string()),
            "status": format!("{status:?}"),
            "ms_after_revoke": elapsed,
            "ui_text": "Your T3 connection was revoked. Reconnect to continue. (okilum-shell project.rs status_note)",
        }),
    );
    Ok(pass)
}
