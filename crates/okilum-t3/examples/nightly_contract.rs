//! Hosted-only diagnostic using the unchanged public native client API.
use anyhow::{bail, ensure, Result};
use okilum_t3::{
    stream::{NativeClient, NativeToken, StreamError, ThreadSubscription, ThreadUpdate},
    Environment,
};
use serde::Deserialize;
use serde_json::json;
use std::{io, sync::mpsc, thread, time::Duration};

#[derive(Deserialize)]
struct Input {
    origin: String,
    credential: String,
    thread_id: String,
}

fn synchronized(subscription: &mut ThreadSubscription, expected_snapshot: bool) -> Result<u64> {
    let cancel = subscription.cancellation()?;
    let (done, deadline) = mpsc::channel();
    let watchdog = thread::spawn(move || {
        if deadline.recv_timeout(Duration::from_secs(30)).is_err() {
            cancel.cancel();
        }
    });
    let result = (|| {
        let mut snapshots = 0;
        let mut synced = false;
        for _ in 0..32 {
            subscription.receive(|updates| {
                for update in updates {
                    match update {
                        ThreadUpdate::Snapshot { .. } => snapshots += 1,
                        ThreadUpdate::Synchronized => synced = true,
                        // An idle fixture has no provider activity. Surface every event instead
                        // of silently treating a changed wire stream as a passing idle resume.
                        ThreadUpdate::Event { .. } => {
                            return Err(StreamError::Protocol(
                                "unexpected event in idle contract fixture",
                            ));
                        }
                    }
                }
                Ok(())
            })?;
            if synced {
                break;
            }
        }
        ensure!(synced, "missing subscription completion marker");
        ensure!(
            snapshots == u64::from(expected_snapshot),
            "unexpected snapshot/resume mode"
        );
        Ok(snapshots)
    })();
    let _ = done.send(());
    watchdog
        .join()
        .map_err(|_| anyhow::anyhow!("watchdog failed"))?;
    result
}

fn contract(stage: &mut &'static str) -> Result<serde_json::Value> {
    ensure!(
        std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
            && std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted"),
        "nightly server probe requires GitHub-hosted Actions"
    );
    // Secret arrives via stdin, never an argv, env variable, file or log.
    let input: Input = serde_json::from_reader(io::stdin())?;
    let environment = Environment::new("disposable-nightly", &input.origin)?;
    ensure!(
        input.origin.starts_with("http://127.0.0.1:"),
        "non-loopback nightly origin"
    );
    *stage = "native_pair";
    let token = NativeToken::pair(&environment, &input.credential, Duration::from_secs(10))?;
    let client = NativeClient::new(environment, token, Duration::from_secs(5))?;
    *stage = "initial_subscribe";
    let mut first = client.subscribe(&input.thread_id, None)?;
    *stage = "initial_snapshot_decode_and_completion";
    synchronized(&mut first, true)?;
    let cursor = first
        .cursor()
        .ok_or_else(|| anyhow::anyhow!("snapshot cursor missing"))?;
    let sequence = cursor.sequence();
    first.close();
    *stage = "resume_subscribe";
    let mut resumed = client.subscribe(&input.thread_id, Some(cursor))?;
    *stage = "resume_decode_and_completion";
    synchronized(&mut resumed, false)?;
    if resumed.cursor().map(|c| c.sequence()) != Some(sequence) {
        bail!("idle resume changed the committed cursor");
    }
    resumed.close();
    Ok(json!({"paired":true,"snapshot":true,"resume":true,
        "resume_without_snapshot":true,"cursor":sequence,
        "client":"okilum_t3::stream::NativeClient"}))
}

fn main() {
    let mut stage = "hosted_preflight";
    match contract(&mut stage) {
        Ok(receipt) => println!("{receipt}"),
        Err(_) => {
            // Only a fixed boundary label crosses the subprocess boundary, never peer
            // payloads, credentials, panic output or model content.
            println!("{}", json!({"status":"FAIL","stage":stage}));
            std::process::exit(1);
        }
    }
}
