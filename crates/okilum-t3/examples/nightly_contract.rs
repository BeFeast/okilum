//! Full native-client proof against an explicitly supplied isolated candidate.
use anyhow::{ensure, Result};
use okilum_t3::{
    stream::{
        shell, CancelSubscription, NativeClient, NativeToken, StreamError, ThreadSubscription,
        ThreadUpdate,
    },
    Environment,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    io::{self, Read},
    sync::mpsc,
    thread,
    time::Duration,
};

#[derive(Deserialize)]
struct Input {
    origin: String,
    credential: String,
    thread_id: String,
    /// Absent for legacy read-only callers; never passes the full mutation gate.
    expected_title: Option<String>,
}

fn bounded<T>(cancel: CancelSubscription, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let (done, deadline) = mpsc::channel();
    let watchdog = thread::spawn(move || {
        if deadline.recv_timeout(Duration::from_secs(30)).is_err() {
            cancel.cancel();
        }
    });
    let result = operation();
    let _ = done.send(());
    watchdog
        .join()
        .map_err(|_| anyhow::anyhow!("watchdog failed"))?;
    result
}

fn idle_projection(projection: &Value, thread_id: &str, expected_title: Option<&str>) -> bool {
    projection["thread"]["id"] == thread_id
        && expected_title.is_none_or(|title| projection["thread"]["title"] == title)
        && projection["runs"].as_array().is_some_and(Vec::is_empty)
        && projection["messages"].as_array().is_some_and(Vec::is_empty)
        && projection["thread"]
            .get("activeRunId")
            .is_none_or(Value::is_null)
}

fn synchronized(
    subscription: &mut ThreadSubscription,
    snapshot: bool,
    thread_id: &str,
    expected_title: Option<&str>,
) -> Result<u64> {
    bounded(subscription.cancellation()?, || {
        let mut snapshots = 0;
        let mut synced = false;
        for _ in 0..32 {
            subscription.receive(|updates| {
                for update in updates {
                    match update {
                        ThreadUpdate::Snapshot { projection, .. } => {
                            if !idle_projection(projection, thread_id, expected_title) {
                                return Err(StreamError::Protocol(
                                    "not the expected idle scratch fixture",
                                ));
                            }
                            snapshots += 1;
                        }
                        ThreadUpdate::Synchronized => synced = true,
                        // No activity is expected; even an unrelated command race fails.
                        ThreadUpdate::Event { .. } => {
                            return Err(StreamError::Protocol("event in idle fixture"))
                        }
                    }
                }
                Ok(())
            })?;
            if synced {
                break;
            }
        }
        ensure!(
            synced && snapshots == u64::from(snapshot),
            "invalid snapshot/resume proof"
        );
        Ok(snapshots)
    })
}

fn shell_synchronized(subscription: &mut shell::Subscription, id: &str, title: &str) -> Result<()> {
    bounded(subscription.cancellation()?, || {
        let mut snapshots = 0;
        let mut fixture = false;
        let mut marker = false;
        for _ in 0..32 {
            subscription.receive(|updates| {
                for update in updates {
                    match update {
                        shell::Update::Snapshot(threads) => {
                            snapshots += 1;
                            fixture = threads.iter().any(|row| {
                                row.id == id
                                    && row.title == title
                                    && row.active_run_id.is_none()
                                    && row.archived_at.is_none()
                                    && row.deleted_at.is_none()
                            });
                        }
                        shell::Update::Synchronized => marker = true,
                        // Other users' shell metadata may change; it does not prove
                        // this scratch fixture or replace the required real marker.
                        _ => {}
                    }
                }
                Ok(())
            })?;
            if marker {
                break;
            }
        }
        ensure!(snapshots == 1 && fixture && marker, "invalid shell proof");
        Ok(())
    })
}

fn contract(stage: &mut &'static str) -> Result<Value> {
    // Secret arrives via stdin, never argv, environment or persisted token file.
    let input: Input = serde_json::from_reader(io::stdin().take(64 * 1024))?;
    let environment = Environment::new("isolated-contract-candidate", &input.origin)?;
    *stage = "descriptor";
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()?
        .get(format!(
            "{}/.well-known/t3/environment",
            environment.origin()
        ))
        .send()?;
    ensure!(response.status().is_success(), "descriptor unavailable");
    let mut descriptor = Vec::new();
    response.take(65537).read_to_end(&mut descriptor)?;
    ensure!(descriptor.len() <= 65536, "oversized descriptor");
    let descriptor: Value = serde_json::from_slice(&descriptor)?;
    ensure!(
        descriptor["orchestrationProtocolVersion"].as_u64() == Some(2),
        "protocol v2 required"
    );
    *stage = "native_pair";
    let token = NativeToken::pair(&environment, &input.credential, Duration::from_secs(10))?;
    let client = NativeClient::new(environment, token, Duration::from_secs(5))?;
    *stage = "scratch_identity";
    if let Some(title) = &input.expected_title {
        let identifier = title
            .strip_prefix("Okilum contract scratch ")
            .ok_or_else(|| anyhow::anyhow!("not a contract scratch title"))?;
        ensure!(
            uuid::Uuid::parse_str(identifier).is_ok(),
            "invalid scratch identifier"
        );
    }
    *stage = "initial_subscribe";
    let mut first = client.subscribe(&input.thread_id, None)?;
    *stage = "initial_snapshot_decode_and_completion";
    synchronized(
        &mut first,
        true,
        &input.thread_id,
        input.expected_title.as_deref(),
    )?;
    let cursor = first
        .cursor()
        .ok_or_else(|| anyhow::anyhow!("snapshot cursor missing"))?;
    let sequence = cursor.sequence();
    first.close();
    *stage = "resume_subscribe";
    let mut resumed = client.subscribe(&input.thread_id, Some(cursor))?;
    *stage = "resume_decode_and_completion";
    synchronized(
        &mut resumed,
        false,
        &input.thread_id,
        input.expected_title.as_deref(),
    )?;
    ensure!(
        resumed.cursor().map(|c| c.sequence()) == Some(sequence),
        "idle cursor changed"
    );
    resumed.close();
    let mut receipt = json!({"pair":true,"subscribe_thread":true,"resume_thread":true,
        "subscribe_shell":false,"dispatch_command":false,
        "paired":true,"snapshot":true,"resume":true,"resume_without_snapshot":true,
        "cursor":sequence,"client":"okilum_t3::stream::NativeClient"});
    if let Some(title) = &input.expected_title {
        *stage = "shell_subscribe_and_completion";
        let mut shell = client.subscribe_shell()?;
        shell_synchronized(&mut shell, &input.thread_id, title)?;
        shell.cancellation()?.cancel();
        drop(shell);
        receipt["subscribe_shell"] = true.into();
        *stage = "scratch_dispatch";
        let renamed = format!("{title} verified");
        let acknowledged = client.rename_thread(
            &input.thread_id,
            &uuid::Uuid::new_v4().to_string(),
            &renamed,
        )?;
        *stage = "dispatch_snapshot_verification";
        let mut final_snapshot = client.subscribe(&input.thread_id, None)?;
        synchronized(&mut final_snapshot, true, &input.thread_id, Some(&renamed))?;
        ensure!(
            final_snapshot
                .cursor()
                .is_some_and(|c| c.sequence() >= acknowledged),
            "dispatch not observed"
        );
        final_snapshot.close();
        receipt["dispatch_command"] = true.into();
    }
    Ok(receipt)
}

fn main() {
    let mut stage = "preflight";
    match contract(&mut stage) {
        Ok(receipt) => println!("{receipt}"),
        Err(_) => {
            // Only a fixed label crosses the subprocess boundary. No peer payload,
            // credential, exception source or panic output enters a public receipt.
            println!("{}", json!({"status":"FAIL","stage":stage}));
            std::process::exit(1);
        }
    }
}
