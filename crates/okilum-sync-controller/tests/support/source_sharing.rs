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
    source_state: PathBuf,
    first: Controller,
    second: Controller,
    peer: uuid::Uuid,
}
impl Pair {
    fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let binary = PathBuf::from(
            std::env::var_os("OKILUM_SYNC_DIRECT_CLIENT")
                .context("set pinned Syncthing 2.1.6 binary")?,
        )
        .canonicalize()?;
        let (source, prepared) = Peer::managed_lan(root.path(), "managed-source", binary.clone())?;
        let (target, _) = Peer::managed_lan(root.path(), "managed-target", binary)?;
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
        Ok(Self {
            _root: root,
            source,
            target,
            prepared,
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
