//! Disposable real protocol receiver for signed-beta Mac UI acceptance.
#![cfg(target_os = "macos")]
use anyhow::{Context, Result};
use okilum_sync_controller::direct::Controller;
use serde_json::{json, Value};
use std::{io::{self, BufRead, Write}, path::PathBuf};
#[test]
fn native_peer() -> Result<()> {
    let state = PathBuf::from(std::env::var_os("OKILUM_QA_PEER_STATE").context("peer state")?);
    let config = PathBuf::from(std::env::var_os("OKILUM_QA_PEER_CONFIG").context("peer config")?);
    let controller = Controller::open(state, vec![config])?;
    println!("PEER {}", json!({"ready":true})); io::stdout().flush()?;
    for line in io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line?)?;
        let result = (|| -> Result<Value> {
            Ok(match request["op"].as_str().context("op")? {
                "redeem" => serde_json::to_value(controller.redeem(request["url"].as_str().context("url")?, "Disposable headless peer")?)?,
                "refresh" => serde_json::to_value(controller.refresh()?)?,
                "catalog" => serde_json::to_value(controller.catalog(request["peer"].as_str().context("peer")?.parse()?)?)?,
                "attach" => serde_json::to_value(controller.attach(request["peer"].as_str().context("peer")?.parse()?, request["vault"].as_str().context("vault")?, &PathBuf::from(request["path"].as_str().context("path")?))?)?,
                _ => anyhow::bail!("unknown operation"),
            })
        })();
        let reply = match result { Ok(value) => json!({"ok":value}), Err(error) => json!({"error":format!("{error:#}")}) };
        println!("PEER {}", reply); io::stdout().flush()?;
    }
    controller.shutdown(); Ok(())
}
