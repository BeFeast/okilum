//! #2367: ordinary source vaults, real pinned managed preparations on both sides.
use super::*;
use anyhow::ensure;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

struct Pair {
    _root: tempfile::TempDir,
    source: Peer,
    target: Peer,
    prepared: PathBuf,
    target_prepared: PathBuf,
    source_state: PathBuf,
    first: Controller,
    second: Controller,
    peer: uuid::Uuid,
}
impl Pair {
    fn new() -> Result<Self> {
        Self::with_lan(true)
    }
    fn loopback() -> Result<Self> {
        Self::with_lan(false)
    }
    fn with_lan(lan: bool) -> Result<Self> {
        let root = tempfile::tempdir()?;
        let binary = PathBuf::from(
            std::env::var_os("OKILUM_SYNC_DIRECT_CLIENT")
                .context("set pinned Syncthing 2.1.6 binary")?,
        )
        .canonicalize()?;
        let prepare = if lan {
            Peer::managed_lan
        } else {
            Peer::managed
        };
        let (source, prepared) = prepare(root.path(), "managed-source", binary.clone())?;
        let (target, target_prepared) = prepare(root.path(), "managed-target", binary)?;
        assert_eq!(source.api.version()?["version"], "v2.1.6");
        assert_eq!(target.api.version()?["version"], "v2.1.6");
        let source_state = root.path().join("source-controller");
        let first = Controller::open(source_state.clone(), vec![source.home.join("config.xml")])?;
        let second = Controller::open(
            root.path().join("target-controller"),
            vec![target.home.join("config.xml")],
        )?;
        let url = first
            .begin_pairing_at("Source", "127.0.0.1:0".parse()?, "127.0.0.1".parse()?)?
            .offer_url
            .unwrap();
        second.redeem(&url, "Target")?;
        let request = first.snapshot()?.pending.unwrap();
        assert_eq!(request.device_id, target.id);
        first.approve(request.request_id)?;
        let approved = second.refresh()?;
        assert!(approved.catalog.is_empty());
        let peer = approved.peers[0].id;
        if !lan {
            // Naming/migration uses real pinned daemons with deterministic routing.
            // The separate LAN test exercises production hint exchange.
            source
                .api
                .add_device(&json!({"deviceID":target.id,"name":"Target fixture",
                "addresses":[format!("tcp://{}",target.listen)]}))?;
            target
                .api
                .add_device(&json!({"deviceID":source.id,"name":"Source fixture",
                "addresses":[format!("tcp://{}",source.listen)]}))?;
        }
        Ok(Self {
            _root: root,
            source,
            target,
            prepared,
            target_prepared,
            source_state,
            first,
            second,
            peer,
        })
    }
    fn shared_catalog(&self) -> Result<String> {
        let catalog = self.second.catalog(self.peer)?;
        ensure!(
            catalog.len() == 1,
            "ordinary source is not shared in the authorized catalog"
        );
        Ok(catalog[0].id.clone())
    }
    fn tick(&self, id: &str) -> Result<()> {
        self.source.api.scan(id)?;
        self.target.api.scan(id)?;
        self.second.refresh()?;
        Ok(())
    }
}
fn hashes(root: &Path, names: &[&str]) -> Result<BTreeMap<String, String>> {
    names
        .iter()
        .map(|name| {
            Ok((
                (*name).into(),
                format!("{:x}", Sha256::digest(fs::read(root.join(name))?)),
            ))
        })
        .collect()
}
fn same_hash(root: &Path, name: &str, expected: &[u8]) -> bool {
    fs::read(root.join(name)).is_ok_and(|b| Sha256::digest(&b) == Sha256::digest(expected))
}

#[test]
#[ignore = "requires checksum-pinned Syncthing 2.1.6; run scripts/ci/sync-source-sharing.sh"]
fn managed_source_share_empty_receive_and_bidirectional_hashes() -> Result<()> {
    let mut pair = Pair::new()?;
    let initial = [
        "seed.md",
        "nested/keep.md",
        ".claude/skills/example/SKILL.md",
        ".okilum-index/private",
        "worktrees/private",
        ".okilum-save-fixture",
    ];
    for name in initial {
        let path = pair.source.vault.join(name);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, format!("original bytes for {name}"))?;
    }
    let before = hashes(&pair.source.vault, &initial)?;
    assert!(pair.source.api.config()?["folders"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(!pair.source.vault.join(".stfolder").exists());
    let shared = pair.first.share_vault(&pair.prepared, &pair.source.vault)?;
    assert_eq!(shared.shared, vec![pair.source.vault.clone()]);
    let id = pair.shared_catalog()?;
    let source_config = pair.source.api.config()?;
    pair.first.share_vault(&pair.prepared, &pair.source.vault)?;
    assert_eq!(pair.shared_catalog()?, id, "retry changed stable folder ID");
    assert_eq!(
        pair.source.api.config()?,
        source_config,
        "retry rewrote configuration"
    );
    pair.first.shutdown();
    drop(pair.first);
    // Lost completion acknowledgement: the folder was unpaused but only its
    // earlier durable creation/witness reached disk. Resume the original scope.
    let journal_file = pair.source_state.join("direct.json");
    let mut interrupted: Value = serde_json::from_slice(&fs::read(&journal_file)?)?;
    interrupted["sources"][0]["active"] = json!(false);
    interrupted["attached"] = json!([]);
    for peer in interrupted["peers"].as_object_mut().unwrap().values_mut() {
        peer["allowed"] = json!([]);
    }
    fs::write(&journal_file, serde_json::to_vec(&interrupted)?)?;
    pair.first = Controller::open(
        pair.source_state.clone(),
        vec![pair.source.home.join("config.xml")],
    )?;
    pair.first.refresh()?;
    pair.first.share_vault(&pair.prepared, &pair.source.vault)?;
    assert_eq!(
        pair.shared_catalog()?,
        id,
        "source share or authorization was not durable"
    );
    pair.second.attach(pair.peer, &id, &pair.target.vault)?;
    wait(
        "ordinary source initial receive and automatic promotion",
        || {
            pair.tick(&id)?;
            Ok(!pair.second.snapshot()?.preparing)
        },
    )?;
    assert_eq!(pair.target.api.folder(&id)?["type"], "sendreceive");
    assert_eq!(
        hashes(&pair.source.vault, &initial)?,
        before,
        "initial receive deleted/changed source files"
    );
    let canonical = &initial[..3];
    assert_eq!(
        hashes(&pair.source.vault, canonical)?,
        hashes(&pair.target.vault, canonical)?
    );
    for name in &initial[3..] {
        assert!(
            !pair.target.vault.join(name).exists(),
            "per-machine data was transported: {name}"
        );
    }
    assert_eq!(
        fs::read(pair.source.vault.join(".stignore"))?,
        fs::read(pair.target.vault.join(".stignore"))?
    );
    for (origin, remote, name) in [
        (&pair.source, &pair.target, "from-source.md"),
        (&pair.target, &pair.source, "from-target.md"),
    ] {
        fs::write(origin.vault.join(name), b"created note")?;
        wait("create hashes both ways", || {
            pair.tick(&id)?;
            Ok(same_hash(&remote.vault, name, b"created note"))
        })?;
        fs::write(origin.vault.join(name), b"modified note")?;
        wait("modify hashes both ways", || {
            pair.tick(&id)?;
            Ok(same_hash(&remote.vault, name, b"modified note"))
        })?;
        let renamed = format!("renamed-{name}");
        fs::rename(origin.vault.join(name), origin.vault.join(&renamed))?;
        wait("rename hashes both ways", || {
            pair.tick(&id)?;
            Ok(!remote.vault.join(name).exists()
                && same_hash(&remote.vault, &renamed, b"modified note"))
        })?;
        fs::remove_file(origin.vault.join(&renamed))?;
        wait("intentional delete both ways", || {
            pair.tick(&id)?;
            Ok(!remote.vault.join(&renamed).exists())
        })?;
    }
    assert_eq!(
        hashes(&pair.source.vault, &initial)?,
        before,
        "original source files were lost"
    );
    assert_eq!(
        hashes(&pair.source.vault, canonical)?,
        hashes(&pair.target.vault, canonical)?
    );
    Ok(())
}

#[test]
#[ignore = "requires checksum-pinned Syncthing 2.1.6; run scripts/ci/sync-source-sharing.sh"]
fn managed_source_skipped_share_negative_control() -> Result<()> {
    let pair = Pair::new()?;
    fs::write(pair.source.vault.join("sentinel.md"), "source stays here")?;
    let before = pair.source.api.config()?;
    assert!(
        pair.shared_catalog().is_err(),
        "acceptance observer passed without sharing"
    );
    assert!(pair
        .second
        .attach(pair.peer, "unshared-vault", &pair.target.vault)
        .is_err());
    assert_eq!(pair.source.api.config()?, before);
    assert_eq!(fs::read_dir(&pair.target.vault)?.count(), 0);
    assert_eq!(
        fs::read_to_string(pair.source.vault.join("sentinel.md"))?,
        "source stays here"
    );
    // A selected external config cannot be taken over by supplying a managed
    // receipt for a different identity. No second daemon is bootstrapped.
    let pid = pair.source.child.as_ref().unwrap().id();
    pair.first
        .set_configurations(vec![pair.target.home.join("config.xml")])?;
    assert!(pair
        .first
        .share_vault(&pair.prepared, &pair.source.vault)
        .is_err());
    assert_eq!(pair.source.api.config()?, before);
    assert!(!pair.source.vault.join(".stignore").exists());
    pair.first
        .set_configurations(vec![pair.source.home.join("config.xml")])?;
    fs::write(
        pair.source.vault.join(".stignore"),
        "private custom rule\r\n",
    )?;
    assert!(pair
        .first
        .share_vault(&pair.prepared, &pair.source.vault)
        .is_err());
    assert_eq!(
        fs::read_to_string(pair.source.vault.join(".stignore"))?,
        "private custom rule\r\n"
    );
    assert_eq!(pair.source.api.config()?, before);
    // Remove only the fixture-created policy after asserting exact preservation.
    fs::remove_file(pair.source.vault.join(".stignore"))?;
    // Positive control for the exact observer that rejected the skipped step.
    pair.first.share_vault(&pair.prepared, &pair.source.vault)?;
    pair.shared_catalog()?;
    assert_eq!(pair.source.child.as_ref().unwrap().id(), pid);
    let shared_config = pair.source.api.config()?;
    let previous = pair.source.vault.with_file_name("preserved-source");
    fs::rename(&pair.source.vault, &previous)?;
    fs::create_dir(&pair.source.vault)?;
    fs::write(
        pair.source.vault.join("replacement.md"),
        "replacement bytes",
    )?;
    assert!(pair
        .first
        .share_vault(&pair.prepared, &pair.source.vault)
        .is_err());
    assert_eq!(pair.source.api.config()?, shared_config);
    assert_eq!(fs::read_dir(&pair.source.vault)?.count(), 1);
    assert_eq!(
        fs::read_to_string(previous.join("sentinel.md"))?,
        "source stays here"
    );
    Ok(())
}

#[test]
#[ignore = "requires checksum-pinned Syncthing 2.1.6; run scripts/ci/sync-source-sharing.sh"]
fn managed_source_nonempty_destination_refused_without_writes() -> Result<()> {
    let pair = Pair::new()?;
    fs::write(pair.source.vault.join("source.md"), "source original")?;
    pair.first.share_vault(&pair.prepared, &pair.source.vault)?;
    let id = pair.shared_catalog()?;
    fs::write(pair.target.vault.join("local.md"), "local original")?;
    let before_source = pair.source.api.config()?;
    let before_target = pair.target.api.config()?;
    let source_bytes = hashes(&pair.source.vault, &["source.md", ".stignore"])?;
    let target_bytes = hashes(&pair.target.vault, &["local.md"])?;
    let error = pair
        .second
        .attach(pair.peer, &id, &pair.target.vault)
        .unwrap_err();
    assert!(error.to_string().contains("nonempty"), "{error:#}");
    assert_eq!(pair.source.api.config()?, before_source);
    assert_eq!(pair.target.api.config()?, before_target);
    assert_eq!(
        hashes(&pair.source.vault, &["source.md", ".stignore"])?,
        source_bytes
    );
    assert_eq!(hashes(&pair.target.vault, &["local.md"])?, target_bytes);
    assert_eq!(fs::read_dir(&pair.target.vault)?.count(), 1);
    assert!(!pair.second.snapshot()?.preparing);
    Ok(())
}

#[test]
#[ignore = "requires checksum-pinned Syncthing 2.1.6; run scripts/ci/sync-source-sharing.sh"]
fn managed_peer_addresses_without_discovery_negative_then_hashes_and_restart() -> Result<()> {
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let root = tempfile::tempdir()?;
    let binary = PathBuf::from(
        std::env::var_os("OKILUM_SYNC_DIRECT_CLIENT")
            .context("set pinned Syncthing 2.1.6 binary")?,
    )
    .canonicalize()?;
    let (mut source, source_prepared) =
        Peer::managed_lan(root.path(), "address-source", binary.clone())?;
    let (target, target_prepared) = Peer::managed_lan(root.path(), "address-target", binary)?;
    let first = Controller::open(
        root.path().join("source-controller"),
        vec![source.home.join("config.xml")],
    )?;
    let second = Controller::open(
        root.path().join("target-controller"),
        vec![target.home.join("config.xml")],
    )?;
    let content = b"explicit authenticated addresses carry these original bytes";
    fs::write(source.vault.join("address-proof.md"), content)?;
    first.share_vault(&source_prepared, &source.vault)?;
    for runtime in [&source, &target] {
        let options = runtime.api.config()?["options"].clone();
        for key in [
            "localAnnounceEnabled",
            "globalAnnounceEnabled",
            "relaysEnabled",
            "natEnabled",
        ] {
            assert_eq!(options[key], false, "fixture permits fallback: {key}");
        }
    }
    // Simulate the legacy channel: temporarily hide only the fixture's managed
    // receipts so no transport hints are advertised/applied. Daemons, identities,
    // trust, folders and the authenticated pairing path are otherwise identical.
    for prepared in [&source_prepared, &target_prepared] {
        fs::rename(
            prepared.join("prepared.json"),
            prepared.join("held-prepared.json"),
        )?;
    }
    let url = first
        .begin_pairing_at("Desktop", "127.0.0.1:0".parse()?, "127.0.0.1".parse()?)?
        .offer_url
        .unwrap();
    second.redeem(&url, "Laptop")?;
    first.approve(first.snapshot()?.pending.unwrap().request_id)?;
    let peer = second.refresh()?.peers[0].id;
    let id = second.catalog(peer)?[0].id.clone();
    second.attach(peer, &id, &target.vault)?;
    // Syncthing fills an empty device name from the first remote handshake.
    // Pin fixture names before comparing the complete device configuration so
    // that asynchronous daemon metadata cannot look like a controller mutation.
    for (runtime, remote, name) in [
        (&source, &target, "Target fixture"),
        (&target, &source, "Source fixture"),
    ] {
        reqwest::blocking::Client::builder()
            .no_proxy()
            .build()?
            .patch(format!(
                "http://{}/rest/config/devices/{}",
                runtime.rest, remote.id
            ))
            .header("X-API-Key", &runtime.api_key)
            .json(&json!({"name":name}))
            .send()?
            .error_for_status()?;
        assert_eq!(runtime.api.device(&remote.id)?["name"], name);
    }
    let connected = || -> Result<bool> {
        Ok(
            source.api.connections()?["connections"][&target.id]["connected"] == true
                || target.api.connections()?["connections"][&source.id]["connected"] == true,
        )
    };
    let negative = Instant::now();
    while negative.elapsed() < Duration::from_secs(5) {
        assert!(
            !connected()?,
            "negative control connected without discovery or hints"
        );
        assert!(
            !target.vault.join("address-proof.md").exists(),
            "negative observer accepted file arrival"
        );
        thread::sleep(Duration::from_millis(200));
    }
    for (runtime, remote) in [(&source, &target), (&target, &source)] {
        assert_eq!(
            runtime.api.device(&remote.id)?["addresses"],
            json!(["dynamic"])
        );
    }
    for prepared in [&source_prepared, &target_prepared] {
        fs::rename(
            prepared.join("held-prepared.json"),
            prepared.join("prepared.json"),
        )?;
    }
    // Positive control: the very same catalog call exchanges both directions.
    // No fixture REST device-address override is allowed on this success path.
    let before_source_folder = source.api.folder(&id)?;
    let before_target_folder = target.api.folder(&id)?;
    let mut before_source_device = source.api.device(&target.id)?;
    let mut before_target_device = target.api.device(&source.id)?;
    second.catalog(peer)?;
    let mut after_source_device = source.api.device(&target.id)?;
    let mut after_target_device = target.api.device(&source.id)?;
    for value in [
        &mut before_source_device,
        &mut before_target_device,
        &mut after_source_device,
        &mut after_target_device,
    ] {
        value.as_object_mut().unwrap().remove("addresses");
    }
    assert_eq!(
        before_source_device, after_source_device,
        "address exchange changed device trust/settings"
    );
    assert_eq!(
        before_target_device, after_target_device,
        "address exchange changed device trust/settings"
    );
    assert_eq!(source.api.folder(&id)?, before_source_folder);
    assert_eq!(target.api.folder(&id)?, before_target_folder);
    for (runtime, remote) in [(&source, &target), (&target, &source)] {
        let addresses = runtime.api.device(&remote.id)?["addresses"]
            .as_array()
            .unwrap()
            .clone();
        assert!(addresses.contains(&json!("dynamic")));
        assert!(
            addresses
                .iter()
                .any(|a| a.as_str().is_some_and(|s| s.starts_with("tcp://")
                    && s.ends_with(&format!(":{}", remote.listen.port())))),
            "no explicit dial address: {addresses:?}"
        );
    }
    wait(
        "exchanged explicit addresses connect and deliver hash",
        || {
            source.api.scan(&id)?;
            target.api.scan(&id)?;
            second.refresh()?;
            Ok(connected()? && same_hash(&target.vault, "address-proof.md", content))
        },
    )?;
    let before = source.api.identity()?["connectionServiceStatus"].clone();
    source.stop()?;
    source.start()?;
    wait("managed listener comes back on the same port", || {
        Ok(source.api.identity()?["connectionServiceStatus"] == before)
    })?;
    second.catalog(peer)?;
    fs::write(
        source.vault.join("after-restart.md"),
        b"stable port after restart",
    )?;
    wait("stable managed port reconnects after restart", || {
        source.api.scan(&id)?;
        target.api.scan(&id)?;
        second.refresh()?;
        Ok(same_hash(
            &target.vault,
            "after-restart.md",
            b"stable port after restart",
        ))
    })?;
    Ok(())
}

#[test]
#[ignore = "requires checksum-pinned Syncthing 2.1.6; run scripts/ci/sync-source-sharing.sh"]
fn managed_receiver_catalog_name_and_7504_label_reconcile() -> Result<()> {
    let mut pair = Pair::loopback()?;
    pair.first.share_vault(&pair.prepared, &pair.source.vault)?;
    let catalog = pair.second.catalog(pair.peer)?;
    let id = &catalog[0].id;
    let label = &catalog[0].label;
    let attached = pair.second.attach(pair.peer, id, &pair.target.vault)?;
    assert_eq!(attached.attached[0].label, *label);
    assert_eq!(pair.target.api.folder(id)?["label"], *label);
    let target_state = pair._root.path().join("target-controller");
    assert_eq!(
        okilum_sync_controller::direct::received_vault_names(&target_state)?,
        vec![(pair.target.vault.clone(), label.clone())]
    );
    pair.second.shutdown();
    drop(pair.second);
    let file = target_state.join("direct.json");
    let mut db: Value = serde_json::from_slice(&fs::read(&file)?)?;
    let old_label = format!(
        "Okilum Sync {}",
        db["transfers"][0]["owner"].as_str().unwrap()
    );
    // A 7504 receiver with a complete ownership witness, already bidirectional.
    pair.target
        .api
        .patch_folder(id, &json!({"label":old_label,"type":"sendreceive"}))?;
    let before = pair.target.api.folder(id)?;
    db["transfers"][0].as_object_mut().unwrap().remove("label");
    db["transfers"][0]["phase"] = json!("bidirectional");
    db["transfers"][0]["witness"] = before.clone();
    db["attached"][0]["label"] = json!(old_label);
    // Another computer's selected catalog must stay selected during migration.
    db["catalog_peer"] = json!(uuid::Uuid::new_v4());
    db["catalog"] = json!([{"id":"other-vault","label":"Other selected catalog"}]);
    fs::write(&file, serde_json::to_vec(&db)?)?;
    fs::write(
        pair.target.vault.join("local-sentinel.md"),
        "preserved local bytes",
    )?;
    pair.second = Controller::open(
        target_state.clone(),
        vec![pair.target.home.join("config.xml")],
    )?;
    let migrated = pair
        .second
        .refresh()
        .context("reconcile legacy receiver label")?;
    let mut expected = before.clone();
    expected["label"] = json!(label);
    assert_eq!(
        pair.target.api.folder(id)?,
        expected,
        "migration changed more than label"
    );
    assert_eq!(migrated.attached[0].label, *label);
    assert_eq!(migrated.catalog[0].label, "Other selected catalog");
    assert_eq!(
        fs::read_to_string(pair.target.vault.join("local-sentinel.md"))?,
        "preserved local bytes"
    );
    pair.second.shutdown();
    drop(pair.second);
    // Lost label PATCH acknowledgement: new label on daemon, old witness on disk.
    let mut db: Value = serde_json::from_slice(&fs::read(&file)?)?;
    db["transfers"][0]["witness"] = before;
    fs::write(&file, serde_json::to_vec(&db)?)?;
    pair.second = Controller::open(
        target_state.clone(),
        vec![pair.target.home.join("config.xml")],
    )?;
    assert_eq!(
        pair.second.snapshot()?.attached[0].label,
        *label,
        "restart must preserve the catalog name before daemon refresh"
    );
    assert_eq!(pair.second.refresh()?.attached[0].label, *label);
    assert_eq!(pair.target.api.folder(id)?, expected);
    Ok(())
}

/// #2589: a receiver that knew only its inviter keeps syncing with the
/// inviter's other computers while the inviter is offline.
#[test]
#[ignore = "requires checksum-pinned Syncthing 2.1.6; run scripts/ci/sync-source-sharing.sh"]
fn managed_receiver_meshes_through_its_share_peer_introducer() -> Result<()> {
    let mut pair = Pair::loopback()?;
    let binary = PathBuf::from(
        std::env::var_os("OKILUM_SYNC_DIRECT_CLIENT")
            .context("set pinned Syncthing 2.1.6 binary")?,
    )
    .canonicalize()?;
    let (third, _) = Peer::managed(pair._root.path(), "managed-third", binary)?;
    fs::write(pair.source.vault.join("seed.md"), b"seed from the source")?;
    pair.first.share_vault(&pair.prepared, &pair.source.vault)?;
    let id = pair.shared_catalog()?;
    // Fixture-only setup: the source already shares this vault with a third
    // computer, which treats the source as its introducer.
    pair.source
        .api
        .add_device(&json!({"deviceID":third.id,"name":"Third fixture",
        "addresses":[format!("tcp://{}",third.listen)]}))?;
    let mut members = pair.source.api.folder(&id)?["devices"]
        .as_array()
        .context("source members")?
        .clone();
    members.push(json!({"deviceID":third.id}));
    pair.source
        .api
        .patch_folder(&id, &json!({"devices":members}))?;
    third
        .api
        .add_device(&json!({"deviceID":pair.source.id,"name":"Source fixture",
        "addresses":[format!("tcp://{}",pair.source.listen)],"introducer":true}))?;
    third
        .api
        .add_paused_folder(&json!({"id":id,"label":"Third replica","path":third.vault,
        "type":"sendreceive","paused":true,
        "devices":[{"deviceID":third.id},{"deviceID":pair.source.id}]}))?;
    third.api.patch_folder(&id, &json!({"paused":false}))?;
    pair.second.attach(pair.peer, &id, &pair.target.vault)?;
    wait("receive and automatic promotion", || {
        pair.tick(&id)?;
        Ok(!pair.second.snapshot()?.preparing)
    })?;
    assert_eq!(
        pair.target.api.device(&pair.source.id)?["introducer"],
        true,
        "bidirectional receiver did not mark its share peer as introducer"
    );
    // Like an existing pairing after an update: the next cluster config from
    // the share peer (here, after a receiver restart) carries the introduction.
    pair.target.stop()?;
    pair.target.start()?;
    wait("share peer introduces its third computer", || {
        pair.second.refresh()?;
        let folder = pair.target.api.folder(&id)?;
        Ok(folder["devices"].as_array().is_some_and(|members| {
            members
                .iter()
                .any(|m| m["deviceID"] == third.id && m["introducedBy"] == pair.source.id)
        }))
    })?;
    assert_eq!(
        pair.target.api.device(&third.id)?["introducedBy"],
        pair.source.id
    );
    assert!(
        !pair.second.refresh()?.preparing,
        "an introduced member reopened the first receive"
    );
    assert_eq!(pair.target.api.folder(&id)?["type"], "sendreceive");
    // The share peer goes to sleep: application and daemon both offline.
    pair.first.shutdown();
    pair.source.stop()?;
    let content = b"written while the share peer is offline";
    fs::write(third.vault.join("from-third.md"), content)?;
    wait("third computer reaches the receiver directly", || {
        third.api.scan(&id)?;
        pair.target.api.scan(&id)?;
        pair.second.refresh()?;
        Ok(same_hash(&pair.target.vault, "from-third.md", content))
    })?;
    // Negative control: a member that the share peer did not introduce.
    let stranger = "AIR6LPZ-7K4PTTV-UXQSMUU-CPQ5YWH-OEDFIIQ-JUG777G-2YQXXR5-YD6AWQR";
    pair.target
        .api
        .add_device(&json!({"deviceID":stranger,"addresses":["dynamic"]}))?;
    let mut members = pair.target.api.folder(&id)?["devices"]
        .as_array()
        .context("receiver members")?
        .clone();
    members.push(json!({"deviceID":stranger}));
    pair.target
        .api
        .patch_folder(&id, &json!({"devices":members}))?;
    let error = pair.second.refresh().unwrap_err();
    assert!(format!("{error:#}").contains("membership"), "{error:#}");
    Ok(())
}

#[path = "remote_inventory.rs"]
mod remote_inventory;

/// Driven in separate processes by CI: observe two live daemons before release,
/// then exercise success, assertion unwinding, and SIGKILL without Drop.
#[test]
#[ignore = "requires pinned Syncthing and the fixture lifecycle driver"]
fn fixture_lifecycle_control() -> Result<()> {
    let ready = PathBuf::from(
        std::env::var_os("OKILUM_SYNC_FIXTURE_READY").context("lifecycle driver required")?,
    );
    let pair = Pair::loopback()?;
    fs::write(
        ready.with_extension("pending"),
        format!(
            "{} {}",
            pair.source.child.as_ref().unwrap().id(),
            pair.target.child.as_ref().unwrap().id()
        ),
    )?;
    fs::rename(ready.with_extension("pending"), &ready)?;
    wait("lifecycle control release", || {
        Ok(ready.with_extension("release").exists())
    })?;
    assert_ne!(
        std::env::var("OKILUM_SYNC_FIXTURE_CONTROL").as_deref(),
        Ok("panic"),
        "forced fixture lifecycle failure (#2498)"
    );
    Ok(())
}

#[test]
#[ignore = "requires checksum-pinned Syncthing 2.1.6; run scripts/ci/sync-source-sharing.sh"]
fn managed_history_2045_restore_retention_and_migration() -> Result<()> {
    use okilum_core::{
        file_editor::{FileEditor, Save},
        source_history,
    };
    use okilum_sync_controller::direct::RetentionDays;
    let mut pair = Pair::loopback()?;
    let old = b"\xef\xbb\xbf# Previous source\r\nExact bytes\r\n";
    fs::write(pair.source.vault.join("history.md"), old)?;
    pair.first.share_vault(&pair.prepared, &pair.source.vault)?;
    let id = pair.shared_catalog()?;
    pair.second.attach(pair.peer, &id, &pair.target.vault)?;
    wait("history initial hash and promotion", || {
        pair.tick(&id)?;
        Ok(!pair.second.snapshot()?.preparing && same_hash(&pair.target.vault, "history.md", old))
    })?;
    for peer in [&pair.source, &pair.target] {
        assert_eq!(peer.api.folder(&id)?["versioning"]["type"], "staggered");
        assert_eq!(
            peer.api.folder(&id)?["versioning"]["params"]["maxAge"],
            "2592000"
        );
    }
    // Exercise real migration of both existing ownership records, including a
    // receiver witness. No unrelated fields may change, and the second refresh
    // must produce exactly the same daemon configuration.
    pair.first.shutdown();
    drop(pair.first);
    pair.second.shutdown();
    drop(pair.second);
    let target_state = pair._root.path().join("target-controller");
    for (peer, state, records) in [
        (&pair.source, &pair.source_state, "sources"),
        (&pair.target, &target_state, "transfers"),
    ] {
        let mut folder = peer.api.folder(&id)?;
        folder["versioning"]["type"] = json!("");
        folder["versioning"]["params"] = json!({});
        peer.api
            .patch_folder(&id, &json!({"versioning":folder["versioning"]}))?;
        let observed = peer.api.folder(&id)?;
        let file = state.join("direct.json");
        let mut journal: Value = serde_json::from_slice(&fs::read(&file)?)?;
        journal[records][0]
            .as_object_mut()
            .unwrap()
            .remove("history");
        journal[records][0]["witness"] = observed;
        fs::write(&file, serde_json::to_vec(&journal)?)?;
    }
    pair.first = Controller::open(
        pair.source_state.clone(),
        vec![pair.source.home.join("config.xml")],
    )?;
    pair.second = Controller::open(
        target_state.clone(),
        vec![pair.target.home.join("config.xml")],
    )?;
    let source_before = pair.source.api.folder(&id)?;
    let target_before = pair.target.api.folder(&id)?;
    pair.first.refresh()?;
    pair.second.refresh()?;
    for (peer, mut before) in [(&pair.source, source_before), (&pair.target, target_before)] {
        before["versioning"]["type"] = json!("staggered");
        before["versioning"]["params"] = json!({"maxAge":"2592000"});
        assert_eq!(
            peer.api.folder(&id)?,
            before,
            "migration changed unrelated fields"
        );
    }
    let source_config = pair.source.api.config()?;
    let target_config = pair.target.api.config()?;
    pair.first.refresh()?;
    pair.second.refresh()?;
    assert_eq!(pair.source.api.config()?, source_config);
    assert_eq!(pair.target.api.config()?, target_config);

    // Delete on A -> discover from B's Note history store -> exact-byte Restore
    // -> both canonical hashes. The archive is not consumed by restoration.
    fs::remove_file(pair.source.vault.join("history.md"))?;
    wait("remote deletion and actual archive", || {
        pair.tick(&id)?;
        Ok(!pair.target.vault.join("history.md").exists()
            && source_history::sync::list(&pair.target.vault, Some(Path::new("history.md")))?
                .versions
                .iter()
                .any(|v| v.bytes() == old))
    })?;
    let listing = source_history::sync::list(&pair.target.vault, None)?;
    let version = listing
        .versions
        .iter()
        .find(|v| v.bytes() == old)
        .context("deleted note absent from recovery history")?;
    let drafts = pair._root.path().join("editor-drafts");
    source_history::sync::restore_deleted(
        &pair.target.vault,
        Path::new("history.md"),
        &drafts,
        version.bytes(),
    )?;
    assert!(source_history::sync::restore_deleted(
        &pair.target.vault,
        Path::new("history.md"),
        &drafts,
        b"must not replace"
    )
    .is_err());
    wait("restored hashes on both devices", || {
        pair.tick(&id)?;
        Ok(same_hash(&pair.source.vault, "history.md", old)
            && same_hash(&pair.target.vault, "history.md", old))
    })?;
    assert!(
        source_history::sync::list(&pair.target.vault, Some(Path::new("history.md")))?
            .versions
            .iter()
            .any(|v| v.bytes() == old)
    );

    // Remote overwrite on the sharing side also has history. A stale reviewed
    // preimage refuses a newer local edit; successful restore backs it up.
    fs::write(pair.target.vault.join("history.md"), b"remote replacement")?;
    wait("overwrite history on sharing computer", || {
        pair.tick(&id)?;
        Ok(same_hash(
            &pair.source.vault,
            "history.md",
            b"remote replacement",
        ))
    })?;
    let versions = source_history::sync::list(&pair.source.vault, Some(Path::new("history.md")))?;
    assert!(versions.versions.iter().any(|v| v.bytes() == old));
    fs::write(pair.source.vault.join("history.md"), b"newer local edit")?;
    assert_eq!(
        FileEditor::replace_bytes(
            &pair.source.vault.join("history.md"),
            &drafts,
            b"remote replacement",
            old
        )?,
        Save::Conflict
    );
    assert!(same_hash(
        &pair.source.vault,
        "history.md",
        b"newer local edit"
    ));
    assert_eq!(
        FileEditor::replace_bytes(
            &pair.source.vault.join("history.md"),
            &drafts,
            b"newer local edit",
            old
        )?,
        Save::Saved
    );
    assert!(source_history::list(&drafts, &pair.source.vault)?
        .versions
        .iter()
        .any(|v| v.bytes() == b"newer local edit"));

    // Time-shifted control: the same real versioner preserves a 31-day archive
    // with 90-day retention, then trims it at 30 days while retaining a 29-day
    // sibling. Triggering an actual remote overwrite invokes staggered Clean.
    pair.second
        .set_history_retention(&pair.target.vault, RetentionDays::try_from(90)?)?;
    let archive = pair.target.vault.join(".stversions");
    fs::create_dir_all(&archive)?;
    let stamp = |days: u64| -> Result<String> {
        let output = std::process::Command::new("date")
            .env("TZ", "UTC")
            .args(["-u", "-d", &format!("{days} days ago"), "+%Y%m%d-%H%M%S"])
            .output()?;
        ensure!(output.status.success(), "fixture date failed");
        Ok(String::from_utf8(output.stdout)?.trim().into())
    };
    let expired = archive.join(format!("trim~{}.md", stamp(31)?));
    let retained = archive.join(format!("trim~{}.md", stamp(29)?));
    fs::write(&expired, b"31-day version")?;
    fs::write(&retained, b"29-day version")?;
    fs::write(pair.source.vault.join("trim.md"), b"initial trim source")?;
    wait("trim positive initial hash", || {
        pair.tick(&id)?;
        Ok(same_hash(
            &pair.target.vault,
            "trim.md",
            b"initial trim source",
        ))
    })?;
    fs::write(
        pair.source.vault.join("trim.md"),
        b"90-day control overwrite",
    )?;
    wait("90-day versioner fired", || {
        pair.tick(&id)?;
        Ok(same_hash(
            &pair.target.vault,
            "trim.md",
            b"90-day control overwrite",
        ))
    })?;
    assert_eq!(
        fs::read(&expired)?,
        b"31-day version",
        "control did not preserve the older version"
    );
    assert_eq!(fs::read(&retained)?, b"29-day version");
    pair.second
        .set_history_retention(&pair.target.vault, RetentionDays::default())?;
    fs::write(
        pair.source.vault.join("trim.md"),
        b"30-day trimming overwrite",
    )?;
    wait("30-day trim fired with hash control", || {
        pair.tick(&id)?;
        Ok(!expired.exists()
            && same_hash(&pair.target.vault, "trim.md", b"30-day trimming overwrite"))
    })?;
    assert_eq!(fs::read(&retained)?, b"29-day version");

    // Existing external folder, even in the same managed daemon, is never
    // adopted for retention. Its original policy remains byte-for-byte intact.
    let external = pair.source.home.join("external-vault");
    fs::create_dir(&external)?;
    pair.source.api.add_paused_folder(&json!({"id":"external-history","path":external,"type":"sendreceive","paused":true,"devices":[{"deviceID":pair.source.id}],"versioning":{"type":"simple","params":{"keep":"7"}}}))?;
    pair.first.attach_existing(
        &inspect_existing(&[pair.source.home.join("config.xml")], &external)?,
        true,
    )?;
    assert!(
        !pair
            .first
            .snapshot()?
            .history_retention
            .iter()
            .any(|s| s.path == external),
        "existing-vault Sync consent must not grant versioning authority"
    );
    let before = pair.source.api.folder("external-history")?;
    assert!(pair
        .first
        .set_history_retention(&external, RetentionDays::default())
        .is_err());
    pair.first.refresh()?;
    assert_eq!(pair.source.api.folder("external-history")?, before);
    // A configurable retention value survives controller restart.
    pair.second
        .set_history_retention(&pair.target.vault, RetentionDays::try_from(90)?)?;
    pair.second.shutdown();
    drop(pair.second);
    pair.second = Controller::open(target_state, vec![pair.target.home.join("config.xml")])?;
    assert_eq!(pair.second.refresh()?.history_retention[0].days.days(), 90);
    assert_eq!(
        pair.target.api.folder(&id)?["versioning"]["params"]["maxAge"],
        "7776000"
    );
    Ok(())
}

#[test]
#[ignore = "requires checksum-pinned Syncthing 2.1.6; run scripts/ci/sync-source-sharing.sh"]
fn managed_history_2045_broken_folder_warns_while_other_vault_receives() -> Result<()> {
    let pair = Pair::loopback()?;
    fs::write(pair.source.vault.join("healthy.md"), b"initial")?;
    pair.first.share_vault(&pair.prepared, &pair.source.vault)?;
    let id = pair.shared_catalog()?;
    pair.second.attach(pair.peer, &id, &pair.target.vault)?;
    wait("healthy initial receive and promotion", || {
        pair.tick(&id)?;
        Ok(!pair.second.snapshot()?.preparing
            && same_hash(&pair.target.vault, "healthy.md", b"initial"))
    })?;
    let broken = pair.target.home.join("broken-vault");
    fs::create_dir(&broken)?;
    pair.second.share_vault(&pair.target_prepared, &broken)?;
    let folder = pair.target.api.config()?["folders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"].as_str().is_some_and(|p| Path::new(p) == broken))
        .context("broken source was not actually enrolled")?
        .clone();
    let broken_id = folder["id"].as_str().unwrap();
    let moved = pair.target.home.join("moved-vault");
    fs::rename(&broken, &moved)?;
    let snapshot = pair.second.refresh()?;
    assert!(snapshot.history_warnings.iter().any(|w| w.path == broken));
    fs::write(pair.source.vault.join("healthy.md"), b"after moved source")?;
    wait("healthy vault receives despite moved source", || {
        pair.tick(&id)?;
        Ok(same_hash(
            &pair.target.vault,
            "healthy.md",
            b"after moved source",
        ))
    })?;
    fs::rename(&moved, &broken)?;
    assert!(
        pair.second.refresh()?.history_warnings.is_empty(),
        "a repaired binding must clear its warning"
    );
    pair.target.api.remove_folder(broken_id)?;
    let snapshot = pair.second.refresh()?;
    assert!(snapshot.history_warnings.iter().any(|w| w.path == broken));
    assert!(
        pair.target.api.folder(broken_id).is_err(),
        "refresh must not recreate a removed folder"
    );
    fs::write(
        pair.source.vault.join("healthy.md"),
        b"after removed source",
    )?;
    wait("healthy vault receives despite removed source", || {
        pair.tick(&id)?;
        Ok(same_hash(
            &pair.target.vault,
            "healthy.md",
            b"after removed source",
        ))
    })?;
    // Deselecting a runtime is also a per-folder history failure, never an
    // authorization to reconnect to it or fail the entire settings refresh.
    pair.second.set_configurations(vec![])?;
    let snapshot = pair.second.refresh()?;
    assert!(snapshot.history_warnings.iter().any(|w| w.path == broken));
    assert!(snapshot
        .history_warnings
        .iter()
        .any(|w| w.path == pair.target.vault));
    pair.second
        .set_configurations(vec![pair.target.home.join("config.xml")])?;
    pair.second.refresh()?;
    // A removed receiver must likewise stay isolated, without a second error
    // from transfer reconciliation re-attempting the failed history binding.
    pair.target.api.remove_folder(&id)?;
    assert!(pair
        .second
        .refresh()?
        .history_warnings
        .iter()
        .any(|w| w.path == pair.target.vault));
    Ok(())
}

#[test]
#[ignore = "requires checksum-pinned Syncthing 2.1.6; run scripts/ci/sync-source-sharing.sh"]
fn managed_real_conflicts_resolve_each_choice_with_hashes() -> Result<()> {
    use okilum_sync_controller::conflicts::{self, ApplyOutcome, Choice};
    use std::time::{Duration, SystemTime};
    let pair = Pair::loopback()?;
    pair.first.share_vault(&pair.prepared, &pair.source.vault)?;
    let id = pair.shared_catalog()?;
    pair.second.attach(pair.peer, &id, &pair.target.vault)?;
    wait("conflict receiver promotion", || {
        pair.tick(&id)?;
        Ok(!pair.second.snapshot()?.preparing)
    })?;
    for vault in [&pair.source.vault, &pair.target.vault] {
        let policy = fs::read_to_string(vault.join(".stignore"))?;
        assert!(
            policy.lines().any(|line| line == ".okilum-save-*"),
            "managed enrollment must seed local retention policy on both computers"
        );
    }
    for (index, choice) in [Choice::KeepThis, Choice::KeepThat, Choice::KeepBoth]
        .into_iter()
        .enumerate()
    {
        let name = format!("decision-{index}.md");
        fs::write(pair.source.vault.join(&name), b"shared base\r\n")?;
        wait("shared conflict base hash", || {
            pair.tick(&id)?;
            Ok(same_hash(&pair.target.vault, &name, b"shared base\r\n"))
        })?;
        pair.source.pause_remote(&pair.target.id, true)?;
        pair.target.pause_remote(&pair.source.id, true)?;
        let versions = [
            b"\xef\xbb\xbfsource offline\r\n".as_slice(),
            b"target offline\n".as_slice(),
        ];
        for (peer, bytes, offset) in [
            (&pair.source, versions[0], 2),
            (&pair.target, versions[1], 4),
        ] {
            fs::write(peer.vault.join(&name), bytes)?;
            fs::File::options()
                .write(true)
                .open(peer.vault.join(&name))?
                .set_times(
                    fs::FileTimes::new()
                        .set_modified(SystemTime::now() + Duration::from_secs(offset)),
                )?;
            peer.api.scan(&id)?;
        }
        pair.source.pause_remote(&pair.target.id, false)?;
        pair.target.pause_remote(&pair.source.id, false)?;
        let mut actual = None;
        wait(
            "real Syncthing creates conflict copy with both original hashes",
            || {
                pair.tick(&id)?;
                for copy in conflicts::inspect(&pair.target.vault)?.copies {
                    if conflicts::canonical_path(&copy).as_deref() != Some(Path::new(&name)) {
                        continue;
                    }
                    let p = conflicts::preview(&pair.target.vault, &copy)?;
                    let hashes = [
                        Sha256::digest(p.this().bytes()),
                        Sha256::digest(p.that().bytes()),
                    ];
                    if versions
                        .iter()
                        .all(|bytes| hashes.contains(&Sha256::digest(bytes)))
                    {
                        actual = Some(copy);
                        return Ok(true);
                    }
                }
                Ok(false)
            },
        )?;
        // The decision acts on actual synced inputs; no hand-made conflict copy.
        let p = conflicts::preview(&pair.target.vault, actual.as_ref().unwrap())?;
        let before = [p.this().bytes().to_vec(), p.that().bytes().to_vec()];
        let state = pair._root.path().join(format!("resolution-{index}"));
        fs::create_dir(&state)?;
        let ApplyOutcome::Applied(receipt) = conflicts::apply(&p, choice, &state)? else {
            anyhow::bail!("real conflict decision was not completed");
        };
        for original in &receipt.originals {
            assert!(pair.target.vault.join(original).is_file());
            assert!(!pair.source.vault.join(original).exists());
        }
        let expected = &before[usize::from(choice == Choice::KeepThat)];
        assert!(same_hash(&pair.target.vault, &name, expected));
        assert!(!pair.target.vault.join(actual.as_ref().unwrap()).exists());
        for (version, expected) in conflicts::retained_versions(&receipt.recovery_record)?
            .iter()
            .zip(&before)
        {
            assert_eq!(Sha256::digest(&version.bytes), Sha256::digest(expected));
        }
        if let Some(path) = receipt.kept_both.as_ref() {
            assert!(same_hash(
                &pair.target.vault,
                &path.to_string_lossy(),
                &before[1]
            ));
        }
        wait(
            "resolved hashes and conflict removal reach other runtime",
            || {
                pair.tick(&id)?;
                Ok(same_hash(&pair.source.vault, &name, expected)
                    && !pair.source.vault.join(actual.as_ref().unwrap()).exists()
                    && receipt.kept_both.as_ref().is_none_or(|path| {
                        same_hash(&pair.source.vault, &path.to_string_lossy(), &before[1])
                    }))
            },
        )?;
        for (original, bytes) in receipt.originals.iter().zip(&before) {
            assert!(same_hash(
                &pair.target.vault,
                &original.to_string_lossy(),
                bytes
            ));
            assert!(
                !pair.source.vault.join(original).exists(),
                "retained original must stay on the resolving computer after both scans"
            );
            for api in [&pair.source.api, &pair.target.api] {
                assert!(api.ignores(&id)?["ignore"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|line| line == ".okilum-save-*"));
            }
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires checksum-pinned Syncthing 2.1.6; run scripts/ci/sync-source-sharing.sh"]
fn managed_create_2569_artifacts_and_guarded_existing_policy_migration() -> Result<()> {
    let mut pair = Pair::loopback()?;
    let artifact = ".okilum-create-8db23a26-cb93-4484-aafe-923e716e2f5c";
    let bytes = b"# Ordinary note is the fired-path control\r\n";
    fs::write(
        pair.source.vault.join(artifact),
        b"interrupted exclusive create",
    )?;
    fs::write(pair.source.vault.join("ordinary.md"), bytes)?;
    pair.first.share_vault(&pair.prepared, &pair.source.vault)?;
    let id = pair.shared_catalog()?;
    pair.second.attach(pair.peer, &id, &pair.target.vault)?;
    wait("new policy ordinary bytes and promotion", || {
        pair.tick(&id)?;
        Ok(!pair.second.snapshot()?.preparing
            && same_hash(&pair.target.vault, "ordinary.md", bytes))
    })?;
    assert!(!pair.target.vault.join(artifact).exists());
    let current: Vec<String> = [
        ".okilum-index",
        ".okilum-save-*",
        "/.claude/worktrees",
        "/worktrees",
        ".okilum-create-*",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let old = current[..4].to_vec();
    let old_bytes = format!("{}\n", old.join("\n")).into_bytes();
    let current_bytes = format!("{}\n", current.join("\n")).into_bytes();
    for peer in [&pair.source, &pair.target] {
        assert_eq!(fs::read(peer.vault.join(".stignore"))?, current_bytes);
    }
    // Reconstruct a pre-upgrade installation with both folders paused, so an
    // old policy cannot transport the artifact before the upgrade is exercised.
    pair.first.shutdown();
    drop(pair.first);
    pair.second.shutdown();
    drop(pair.second);
    let target_state = pair._root.path().join("target-controller");
    for (peer, state, records) in [
        (&pair.source, &pair.source_state, "sources"),
        (&pair.target, &target_state, "transfers"),
    ] {
        peer.api.patch_folder(&id, &json!({"paused":true}))?;
        peer.api.replace_managed_ignores(&id, &current, &old)?;
        assert_eq!(fs::read(peer.vault.join(".stignore"))?, old_bytes);
        let file = state.join("direct.json");
        let mut journal: Value = serde_json::from_slice(&fs::read(&file)?)?;
        let record = &mut journal[records][0];
        record.as_object_mut().unwrap().remove("create_ignores");
        record["witness"] = peer.api.folder(&id)?;
        if records == "sources" {
            record["ignores"] = json!(old_bytes);
        } else {
            record["connection"]["ignores"] = json!(old);
        }
        fs::write(file, serde_json::to_vec(&journal)?)?;
    }
    pair.first = Controller::open(
        pair.source_state.clone(),
        vec![pair.source.home.join("config.xml")],
    )?;
    pair.second = Controller::open(
        target_state.clone(),
        vec![pair.target.home.join("config.xml")],
    )?;
    let before_source = pair.source.api.config()?;
    let before_target = pair.target.api.config()?;
    for controller in [&pair.first, &pair.second] {
        assert!(controller.refresh()?.policy_warnings.is_empty());
        assert!(controller.refresh()?.policy_warnings.is_empty());
    }
    assert_eq!(
        pair.source.api.config()?,
        before_source,
        "source unrelated settings changed"
    );
    assert_eq!(
        pair.target.api.config()?,
        before_target,
        "receiver unrelated settings changed"
    );
    for (peer, state, records) in [
        (&pair.source, &pair.source_state, "sources"),
        (&pair.target, &target_state, "transfers"),
    ] {
        assert_eq!(fs::read(peer.vault.join(".stignore"))?, current_bytes);
        assert_eq!(
            peer.api.folder(&id)?["paused"],
            true,
            "user pause preserved"
        );
        let journal: Value = serde_json::from_slice(&fs::read(state.join("direct.json"))?)?;
        assert_eq!(journal[records][0]["create_ignores"]["complete"], true);
        assert!(journal[records][0]["create_ignores"]["pending"].is_null());
        peer.api.patch_folder(&id, &json!({"paused":false}))?;
    }
    // Lost acknowledgement: disk/daemon already have the new policy, while
    // the old ignore witness and a pending before/after intent remain durable.
    pair.first.shutdown();
    drop(pair.first);
    pair.second.shutdown();
    drop(pair.second);
    for (peer, state, records) in [
        (&pair.source, &pair.source_state, "sources"),
        (&pair.target, &target_state, "transfers"),
    ] {
        let file = state.join("direct.json");
        let mut journal: Value = serde_json::from_slice(&fs::read(&file)?)?;
        let record = &mut journal[records][0];
        record["create_ignores"] = json!({"complete":false,"pending":{
            "folder":peer.api.folder(&id)?,"before":old_bytes,"after":current_bytes}});
        if records == "sources" {
            record["ignores"] = json!(old_bytes);
        } else {
            record["connection"]["ignores"] = json!(old);
        }
        fs::write(file, serde_json::to_vec(&journal)?)?;
    }
    pair.first = Controller::open(
        pair.source_state.clone(),
        vec![pair.source.home.join("config.xml")],
    )?;
    pair.second = Controller::open(
        target_state.clone(),
        vec![pair.target.home.join("config.xml")],
    )?;
    assert!(pair.first.refresh()?.policy_warnings.is_empty());
    assert!(pair.second.refresh()?.policy_warnings.is_empty());
    for peer in [&pair.source, &pair.target] {
        assert_eq!(fs::read(peer.vault.join(".stignore"))?, current_bytes);
        assert_eq!(peer.api.folder(&id)?["paused"], false);
    }
    let second_artifact = ".okilum-create-0489e797-b0b7-4e83-a0af-8cb53a1547ae";
    fs::write(
        pair.target.vault.join(second_artifact),
        b"receiver interrupted create",
    )?;
    let return_bytes = b"# Return direction after existing-policy migration\n";
    fs::write(pair.target.vault.join("return.md"), return_bytes)?;
    wait("migrated policy ordinary bytes in return direction", || {
        pair.tick(&id)?;
        Ok(same_hash(&pair.source.vault, "return.md", return_bytes))
    })?;
    assert!(!pair.target.vault.join(artifact).exists());
    assert!(!pair.source.vault.join(second_artifact).exists());
    assert_eq!(
        fs::read(pair.source.vault.join(artifact))?,
        b"interrupted exclusive create"
    );
    assert_eq!(
        fs::read(pair.target.vault.join(second_artifact))?,
        b"receiver interrupted create"
    );

    // An existing custom/negated policy has no automatic migration authority,
    // even when the durable source bytes match it. It must remain visible.
    pair.first.shutdown();
    drop(pair.first);
    let custom = format!(
        "// keep exact comments\r\n!important.md\r\n{}\r\n",
        old.join("\r\n")
    )
    .into_bytes();
    fs::write(pair.source.vault.join(".stignore"), &custom)?;
    let file = pair.source_state.join("direct.json");
    let mut journal: Value = serde_json::from_slice(&fs::read(&file)?)?;
    journal["sources"][0]
        .as_object_mut()
        .unwrap()
        .remove("create_ignores");
    journal["sources"][0]["ignores"] = json!(custom);
    fs::write(file, serde_json::to_vec(&journal)?)?;
    pair.first = Controller::open(
        pair.source_state.clone(),
        vec![pair.source.home.join("config.xml")],
    )?;
    let warning = pair.first.refresh()?.policy_warnings;
    assert_eq!(warning.len(), 1);
    assert_eq!(warning[0].path, pair.source.vault);
    assert!(warning[0].message.contains("custom rules were preserved"));
    assert_eq!(fs::read(pair.source.vault.join(".stignore"))?, custom);
    Ok(())
}

#[test]
#[ignore = "requires checksum-pinned Syncthing 2.1.6; run scripts/ci/sync-source-sharing.sh"]
fn managed_shared_2046_include_propagation_local_review_and_initial_seed() -> Result<()> {
    use okilum_sync_controller::direct::shared_ignores::POLICY_PATH;
    let pair = Pair::loopback()?;
    fs::write(
        pair.source.vault.join("seed.md"),
        b"# Initial positive control\n",
    )?;
    pair.first.share_vault(&pair.prepared, &pair.source.vault)?;
    let id = pair.shared_catalog()?;
    pair.second.attach(pair.peer, &id, &pair.target.vault)?;
    wait("shared policy initial note and promotion", || {
        pair.tick(&id)?;
        Ok(!pair.second.snapshot()?.preparing
            && same_hash(
                &pair.target.vault,
                "seed.md",
                b"# Initial positive control\n",
            ))
    })?;
    // Legacy template is portable source. A device's private additions are not
    // silently promoted into the fleet policy when that template is adopted.
    fs::create_dir(pair.source.vault.join("_Schema"))?;
    fs::write(
        pair.source.vault.join("_Schema/stignore.template"),
        b"// shared template\n.git\n/.obsidian\n",
    )?;
    let generated = fs::read_to_string(pair.source.vault.join(".stignore"))?;
    let local = format!(
        "// local bytes stay exact\r\n{}/source-only.md\r\n",
        generated.replace('\n', "\r\n")
    );
    fs::write(pair.source.vault.join(".stignore"), local.as_bytes())?;
    let preview = pair.first.preview_ignore_policy(&pair.source.vault)?;
    assert!(preview.text.contains("// shared template"));
    assert!(!preview.text.contains("/source-only.md"));
    assert!(preview.proposed_local.ends_with(&local));
    let text = format!("{}\n/excluded.md\n", preview.text);
    pair.first.apply_ignore_policy(&preview, &text)?;
    assert!(fs::read(pair.source.vault.join(".stignore"))?.ends_with(local.as_bytes()));
    wait("shared file and automatic include reach receiver", || {
        pair.tick(&id)?;
        Ok(fs::read(pair.target.vault.join(POLICY_PATH))
            .is_ok_and(|bytes| bytes == text.as_bytes())
            && fs::read_to_string(pair.target.vault.join(".stignore"))
                .is_ok_and(|s| s.starts_with("#include .okilum/sync-ignore\n")))
    })?;
    fs::write(
        pair.source.vault.join("excluded.md"),
        b"must stay on source",
    )?;
    fs::write(
        pair.source.vault.join("source-only.md"),
        b"local source rule",
    )?;
    fs::write(
        pair.source.vault.join("visible-after.md"),
        b"# Positive after policy\n",
    )?;
    wait("ordinary note fires the policy transfer path", || {
        pair.tick(&id)?;
        Ok(same_hash(
            &pair.target.vault,
            "visible-after.md",
            b"# Positive after policy\n",
        ))
    })?;
    assert!(!pair.target.vault.join("excluded.md").exists());
    assert!(!pair.target.vault.join("source-only.md").exists());
    // A second computer edits the same canonical file, without rewriting either
    // computer's local block. Real file transport proves the shared authority.
    let receiver_local = fs::read(pair.target.vault.join(".stignore"))?;
    let preview = pair.second.preview_ignore_policy(&pair.target.vault)?;
    let newer = format!("{}\n/later.md\n", preview.text);
    pair.second.apply_ignore_policy(&preview, &newer)?;
    wait("receiver policy edit returns to sharing computer", || {
        pair.first.refresh()?;
        pair.tick(&id)?;
        Ok(fs::read(pair.source.vault.join(POLICY_PATH)).is_ok_and(|b| b == newer.as_bytes()))
    })?;
    assert_eq!(
        fs::read(pair.target.vault.join(".stignore"))?,
        receiver_local
    );
    assert!(fs::read(pair.source.vault.join(".stignore"))?.ends_with(local.as_bytes()));
    fs::write(pair.source.vault.join("later.md"), b"new shared exclusion")?;
    fs::write(
        pair.source.vault.join("visible-second.md"),
        b"second positive control",
    )?;
    wait("ordinary transfer after returned policy edit", || {
        pair.first.refresh()?;
        pair.tick(&id)?;
        Ok(same_hash(
            &pair.target.vault,
            "visible-second.md",
            b"second positive control",
        ))
    })?;
    assert!(!pair.target.vault.join("later.md").exists());

    // A locally changed file is reported, never silently overwritten. Explicit
    // preview/apply acknowledges those exact bytes and retains the new block.
    let changed = [receiver_local.as_slice(), b"\r\n/receiver-private\r\n"].concat();
    fs::write(pair.target.vault.join(".stignore"), &changed)?;
    let warnings = pair.second.refresh()?.policy_warnings;
    assert!(warnings
        .iter()
        .any(|w| w.path == pair.target.vault && w.message.contains("review")));
    assert_eq!(fs::read(pair.target.vault.join(".stignore"))?, changed);
    let preview = pair.second.preview_ignore_policy(&pair.target.vault)?;
    pair.second.apply_ignore_policy(&preview, &preview.text)?;
    assert!(pair.second.refresh()?.policy_warnings.is_empty());
    assert_eq!(fs::read(pair.target.vault.join(".stignore"))?, changed);
    let stale = pair.second.preview_ignore_policy(&pair.target.vault)?;
    let raced = [changed.as_slice(), b"// raced\n"].concat();
    fs::write(pair.target.vault.join(".stignore"), &raced)?;
    assert!(pair
        .second
        .apply_ignore_policy(&stale, &format!("{}\n/another\n", stale.text))
        .is_err());
    assert_eq!(
        fs::read(pair.target.vault.join(POLICY_PATH))?,
        newer.as_bytes()
    );
    assert_eq!(fs::read(pair.target.vault.join(".stignore"))?, raced);

    // A new receiver gets the included file from the authenticated share reply
    // while its folder is still paused, before .stignore can refer to it.
    let binary =
        PathBuf::from(std::env::var_os("OKILUM_SYNC_DIRECT_CLIENT").unwrap()).canonicalize()?;
    let (third, _) = Peer::managed(pair._root.path(), "policy-third", binary)?;
    let controller = Controller::open(
        pair._root.path().join("policy-third-controller"),
        vec![third.home.join("config.xml")],
    )?;
    let url = pair
        .first
        .begin_pairing_at("Source", "127.0.0.1:0".parse()?, "127.0.0.1".parse()?)?
        .offer_url
        .unwrap();
    controller.redeem(&url, "Third")?;
    pair.first
        .approve(pair.first.snapshot()?.pending.unwrap().request_id)?;
    let snapshot = controller.refresh()?;
    let peer = snapshot.peers[0].id;
    controller.catalog(peer)?;
    pair.source
        .api
        .add_device(&json!({"deviceID":third.id,"addresses":[format!("tcp://{}",third.listen)]}))?;
    third.api.add_device(
        &json!({"deviceID":pair.source.id,"addresses":[format!("tcp://{}",pair.source.listen)]}),
    )?;
    controller.attach(peer, &id, &third.vault)?;
    assert_eq!(fs::read(third.vault.join(POLICY_PATH))?, newer.as_bytes());
    assert!(!fs::read_to_string(third.vault.join(".stignore"))?.contains("source-only"));
    assert!(third.api.ignores(&id)?["error"].is_null() || third.api.ignores(&id)?["error"] == "");
    wait("new receiver positive bytes with shared include", || {
        pair.source.api.scan(&id)?;
        third.api.scan(&id)?;
        controller.refresh()?;
        Ok(!controller.snapshot()?.preparing
            && same_hash(
                &third.vault,
                "visible-second.md",
                b"second positive control",
            ))
    })?;
    assert!(!third.vault.join("excluded.md").exists());
    assert!(!third.vault.join("later.md").exists());
    // Pinned negative control: a missing include is a real parser error. Restore
    // this fixture's own file and prove that error clears again.
    let policy = third.vault.join(POLICY_PATH);
    let held = third.vault.join(".okilum/held-fixture-policy");
    fs::rename(&policy, &held)?;
    let error = third.api.ignores(&id)?["error"].clone();
    assert!(
        error
            .as_str()
            .is_some_and(|s| s.contains("failed to load include")),
        "{error}"
    );
    fs::rename(&held, &policy)?;
    assert!(third.api.ignores(&id)?["error"].is_null() || third.api.ignores(&id)?["error"] == "");
    controller.shutdown();
    Ok(())
}
