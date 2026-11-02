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
        let (source, prepared) = Peer::managed(root.path(), "managed-source", binary.clone())?;
        let (target, _) = Peer::managed(root.path(), "managed-target", binary)?;
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
        // Fixture-only loopback routing replaces LAN discovery, never folder enrollment.
        source.api.add_device(
            &json!({"deviceID":target.id,"addresses":[format!("tcp://{}",target.listen)]}),
        )?;
        target.api.add_device(
            &json!({"deviceID":source.id,"addresses":[format!("tcp://{}",source.listen)]}),
        )?;
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
    pair.first = Controller::open(
        pair.source_state.clone(),
        vec![pair.source.home.join("config.xml")],
    )?;
    pair.first.refresh()?;
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
    // Positive control for the exact observer that rejected the skipped step.
    pair.first.share_vault(&pair.prepared, &pair.source.vault)?;
    pair.shared_catalog()?;
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
