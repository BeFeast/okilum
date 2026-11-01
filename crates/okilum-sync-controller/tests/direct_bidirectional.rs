//! Actual isolated 2.1.x peers, encrypted pairing and guarded initial promotion.
#![cfg(target_os = "linux")]
#[path = "support/direct_daemon.rs"]
mod direct_daemon;
use anyhow::{Context, Result};
use direct_daemon::{wait, Peer};
use okilum_sync_controller::direct::{inspect_existing, Controller};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn scan(peer: &Peer) -> Result<()> {
    peer.api.scan("direct-fixture")
}
fn contains(root: &Path, content: &str) -> Result<bool> {
    if !root.try_exists()? {
        return Ok(false);
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if contains(&entry.path(), content)? {
                return Ok(true);
            }
        } else if fs::read_to_string(entry.path()).is_ok_and(|s| s == content) {
            return Ok(true);
        }
    }
    Ok(false)
}
fn phase(state: &Path, value: &str) -> Result<()> {
    let file = state.join("direct.json");
    let mut journal: Value = serde_json::from_slice(&fs::read(&file)?)?;
    journal["transfers"][0]["phase"] = json!(value);
    fs::write(file, serde_json::to_vec(&journal)?)?;
    Ok(())
}

#[test]
#[ignore = "requires explicit unmodified Syncthing 2.1.7 source and 2.1.6 receiver binaries"]
fn actual_paired_daemons_promote_safely_and_replicate_in_both_directions() -> Result<()> {
    let root = tempfile::tempdir()?;
    let source_binary = PathBuf::from(
        std::env::var_os("OKILUM_SYNC_DIRECT_SOURCE")
            .context("set isolated Syncthing 2.1.7 binary")?,
    );
    let target_binary = PathBuf::from(
        std::env::var_os("OKILUM_SYNC_DIRECT_CLIENT")
            .context("set isolated Syncthing 2.1.6 binary")?,
    );
    let source = Peer::new(root.path(), "source", source_binary)?;
    let mut target = Peer::new(root.path(), "target", target_binary)?;
    assert_eq!(source.api.version()?["version"], "v2.1.7");
    assert_eq!(target.api.version()?["version"], "v2.1.6");
    fs::create_dir(source.vault.join("nested"))?;
    fs::write(
        source.vault.join("nested/seed.md"),
        "original source content",
    )?;
    source.api.add_device(&json!({"deviceID":target.id,"addresses":[format!("tcp://{}",target.listen)],"paused":true}))?;
    target.api.add_device(
        &json!({"deviceID":source.id,"addresses":[format!("tcp://{}",source.listen)]}),
    )?;
    source.api.add_paused_folder(&json!({"id":"direct-fixture","label":"Source keeps its settings","path":source.vault,
        "type":"sendreceive","paused":true,"devices":[{"deviceID":source.id}],"rescanIntervalS":0,"fsWatcherEnabled":false}))?;
    source
        .api
        .patch_folder("direct-fixture", &json!({"paused":false}))?;
    scan(&source)?;
    let original = source.api.folder("direct-fixture")?;
    let source_configs = vec![source.home.join("config.xml")];
    let target_configs = vec![target.home.join("config.xml")];
    let source_state = root.path().join("source-controller");
    let target_state = root.path().join("target-controller");
    let first = Controller::open(source_state, source_configs.clone())?;
    first.attach_existing(&inspect_existing(&source_configs, &source.vault)?, true)?;
    let mut second = Controller::open(target_state.clone(), target_configs.clone())?;
    let url = first
        .begin_pairing_at("Source", "127.0.0.1:0".parse()?, "127.0.0.1".parse()?)?
        .offer_url
        .unwrap();
    second.redeem(&url, "Receiving computer")?;
    let request = first.snapshot()?.pending.unwrap();
    assert_eq!(
        request.device_id, target.id,
        "old computer must confirm the exact receiving daemon identity"
    );
    first.approve(request.request_id)?;
    let approved = second.refresh()?;
    let paired = approved.peers[0].id;
    assert_eq!(approved.catalog.len(), 1);
    let nonempty = root.path().join("unknown-nonempty");
    fs::create_dir(&nonempty)?;
    fs::write(nonempty.join("sentinel.md"), "never overwrite")?;
    let before = source.api.folder("direct-fixture")?;
    assert!(second
        .attach(paired, "direct-fixture", &nonempty)
        .unwrap_err()
        .to_string()
        .contains("nonempty"));
    assert_eq!(
        source.api.folder("direct-fixture")?,
        before,
        "nonempty rejection mutated the source share"
    );
    assert_eq!(
        fs::read_to_string(nonempty.join("sentinel.md"))?,
        "never overwrite"
    );
    assert!(
        second
            .attach(paired, "direct-fixture", &target.vault)?
            .preparing
    );
    assert_eq!(target.api.folder("direct-fixture")?["type"], "receiveonly");
    let local = target.vault.join("local-before-ready.md");
    fs::write(&local, "user-created before readiness")?;
    scan(&target)?;
    wait("receive-only local delta positive control", || {
        scan(&target)?;
        Ok(
            target.api.status("direct-fixture")?["receiveOnlyTotalItems"]
                .as_u64()
                .is_some_and(|n| n > 0),
        )
    })?;
    assert!(second.refresh()?.preparing);
    second.shutdown();
    drop(second);
    // Durable promotion intent alone, without successful PATCH or fresh readiness,
    // must never turn a restarted receiving replica bidirectional.
    phase(&target_state, "promoting")?;
    target.stop()?;
    target.start()?;
    second = Controller::open(target_state.clone(), target_configs.clone())?;
    assert!(second.refresh()?.preparing);
    assert_eq!(target.api.folder("direct-fixture")?["type"], "receiveonly");
    source.pause_remote(&target.id, false)?;
    wait("initial subdirectory content received", || {
        scan(&source)?;
        scan(&target)?;
        second.refresh()?;
        Ok(fs::read_to_string(target.vault.join("nested/seed.md"))
            .is_ok_and(|s| s == "original source content"))
    })?;
    assert!(
        second.refresh()?.preparing,
        "local receive-only delta was ignored"
    );
    assert_eq!(target.api.folder("direct-fixture")?["type"], "receiveonly");
    assert_eq!(fs::read_to_string(&local)?, "user-created before readiness");
    assert!(
        !source.vault.join("local-before-ready.md").exists(),
        "positive connected control published unapproved local edits"
    );
    // Only remove this test-created file after the preservation assertion.
    fs::remove_file(&local)?;
    wait("fresh direct interval automatically promotes", || {
        scan(&source)?;
        scan(&target)?;
        Ok(!second.refresh()?.preparing)
    })?;
    assert_eq!(target.api.folder("direct-fixture")?["type"], "sendreceive");
    let current = source.api.folder("direct-fixture")?;
    assert_eq!(current["type"], original["type"]);
    assert_eq!(current["label"], original["label"]);
    assert_eq!(current["versioning"], original["versioning"]);
    assert_eq!(
        target.api.folder("direct-fixture")?["versioning"]["params"]["maxAge"],
        "2592000"
    );
    fs::write(source.vault.join("from-source.md"), "source creates")?;
    fs::write(target.vault.join("from-target.md"), "target creates")?;
    wait("both computers create notes", || {
        scan(&source)?;
        scan(&target)?;
        Ok(fs::read_to_string(target.vault.join("from-source.md"))
            .is_ok_and(|s| s == "source creates")
            && fs::read_to_string(source.vault.join("from-target.md"))
                .is_ok_and(|s| s == "target creates"))
    })?;
    fs::write(
        source.vault.join("nested/seed.md"),
        "source edits existing note",
    )?;
    wait("source edit reaches receiver", || {
        scan(&source)?;
        scan(&target)?;
        Ok(fs::read_to_string(target.vault.join("nested/seed.md"))
            .is_ok_and(|s| s == "source edits existing note"))
    })?;
    assert!(
        contains(&target.vault.join(".stversions"), "original source content")?,
        "remote overwrite lost previous version"
    );
    fs::write(
        target.vault.join("nested/seed.md"),
        "target edits existing note",
    )?;
    wait("receiver edit reaches source", || {
        scan(&source)?;
        scan(&target)?;
        Ok(fs::read_to_string(source.vault.join("nested/seed.md"))
            .is_ok_and(|s| s == "target edits existing note"))
    })?;
    second.shutdown();
    drop(second);
    // Lost successful PATCH acknowledgement recovers by owned read-back, even
    // while the source application is offline; no repeat mutation is needed.
    phase(&target_state, "promoting")?;
    first.shutdown();
    drop(first);
    second = Controller::open(target_state.clone(), target_configs.clone())?;
    assert!(!second.refresh()?.preparing);
    assert_eq!(target.api.folder("direct-fixture")?["type"], "sendreceive");
    // An arbitrary external receiveonly replica has no promotion journal at all.
    let external = target.home.join("external-receiveonly");
    fs::create_dir(&external)?;
    fs::write(external.join("user.md"), "untouched external file")?;
    target
        .api
        .add_paused_folder(&json!({"id":"external","label":"external","path":external,
        "type":"receiveonly","paused":true,"devices":[{"deviceID":target.id}]}))?;
    second.attach_existing(&inspect_existing(&target_configs, &external)?, true)?;
    let external_config = target.api.folder("external")?;
    second.refresh()?;
    assert_eq!(target.api.folder("external")?, external_config);
    assert_eq!(
        fs::read_to_string(external.join("user.md"))?,
        "untouched external file"
    );
    second.shutdown();
    drop(second);
    let file = target_state.join("direct.json");
    let mut legacy: Value = serde_json::from_slice(&fs::read(&file)?)?;
    let transfer = legacy["transfers"][0].as_object_mut().unwrap();
    for key in ["owner", "phase", "witness", "destination_identity"] {
        transfer.remove(key);
    }
    fs::write(&file, serde_json::to_vec(&legacy)?)?;
    second = Controller::open(target_state, target_configs)?;
    assert!(
        second.refresh()?.preparing,
        "legacy record silently acquired promotion authority"
    );
    second.shutdown();
    Ok(())
}
