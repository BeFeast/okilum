//! Synthetic native acceptance setup only; never installed as product code.
#![cfg(target_os = "linux")]
#[path = "support/direct_daemon.rs"]
mod direct_daemon;
use anyhow::{Context, Result};
use std::{fs, path::PathBuf};
#[test]
fn prepare_isolated_native_profiles() -> Result<()> {
    let root = PathBuf::from(std::env::var_os("OKILUM_QA_ROOT").context("set an unused QA root")?);
    fs::create_dir(&root)?;
    let binary = PathBuf::from(std::env::var_os("OKILUM_SYNC_DIRECT_CLIENT").context("set pinned fixture binary")?);
    let mut rows = vec![];
    for name in ["first", "second"] {
        let profile = root.join(name);
        fs::create_dir(&profile)?;
        fs::create_dir(profile.join("sync"))?;
        let (mut peer, _) = direct_daemon::Peer::managed(&profile.join("sync"), "managed", binary.clone())?;
        peer.stop()?;
        rows.push(serde_json::json!({"profile":profile,"config":peer.home.join("config.xml"),"vault":peer.vault,"id":peer.id,"listen":peer.listen}));
    }
    fs::write(root.join("profiles.json"), serde_json::to_vec_pretty(&rows)?)?;
    Ok(())
}
