//! #2505: actual read-only inspection, using pinned managed fixtures on both computers.
use super::*;
use okilum_sync_controller::merge_preview::Kind;

fn all_hashes(path: &Path) -> Result<BTreeMap<String, String>> {
    fn walk(root: &Path, current: &Path, hashes: &mut BTreeMap<String, String>) -> Result<()> {
        for entry in fs::read_dir(current)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                walk(root, &entry.path(), hashes)?;
            } else {
                hashes.insert(
                    entry
                        .path()
                        .strip_prefix(root)?
                        .to_string_lossy()
                        .into_owned(),
                    format!("{:x}", Sha256::digest(fs::read(entry.path())?)),
                );
            }
        }
        Ok(())
    }
    let mut hashes = BTreeMap::new();
    walk(path, path, &mut hashes)?;
    Ok(hashes)
}
#[test]
#[ignore = "requires checksum-pinned Syncthing 2.1.6; run scripts/ci/sync-source-sharing.sh"]
fn managed_remote_inventory_exact_plan_binary_chunks_staleness_and_zero_writes() -> Result<()> {
    let mut pair = Pair::loopback()?;
    let binary: Vec<u8> = (0..300_007).map(|i| (i % 256) as u8).collect();
    for vault in [&pair.source.vault, &pair.target.vault] {
        fs::create_dir(vault.join("nested"))?;
        fs::write(vault.join("equal.md"), b"equal canonical bytes")?;
        fs::write(vault.join("equal.bin"), [0, 255, 17, 0, 10])?;
    }
    fs::write(pair.source.vault.join("different.md"), b"source version")?;
    fs::write(pair.target.vault.join("different.md"), b"target version")?;
    fs::write(pair.source.vault.join("remote-only.bin"), &binary)?;
    for i in 0..6 {
        fs::write(
            pair.source.vault.join(format!("nested/{i}.md")),
            format!("remote file {i}"),
        )?;
    }
    fs::write(pair.target.vault.join("local-only.md"), b"local version")?;
    fs::write(pair.target.vault.join("nested/local.bin"), [255, 0, 255])?;
    pair.first.share_vault(&pair.prepared, &pair.source.vault)?;
    let id = pair.shared_catalog()?;
    pair.source.api.scan(&id)?;
    wait("source initial scan settled", || {
        Ok(pair.source.api.status(&id)?["state"] == "idle")
    })?;
    let target_state = pair._root.path().join("target-controller/direct.json");
    let source_state = pair.source_state.join("direct.json");
    let before_files = (
        all_hashes(&pair.source.vault)?,
        all_hashes(&pair.target.vault)?,
    );
    let before_configs = (pair.source.api.config()?, pair.target.api.config()?);
    let before_disk = (
        fs::read(pair.source.home.join("config.xml"))?,
        fs::read(pair.target.home.join("config.xml"))?,
    );
    let before_journals = (fs::read(&source_state)?, fs::read(&target_state)?);
    let preview = pair
        .second
        .preview_remote_vault(pair.peer, &id, &pair.target.vault)?;
    assert_eq!(preview.plan().count(Kind::Equal), 2);
    assert_eq!(preview.plan().count(Kind::Different), 1);
    assert_eq!(preview.plan().count(Kind::OnlyHere), 2);
    assert_eq!(preview.plan().count(Kind::OnlyThere), 7);
    let actual: Vec<_> = preview
        .entries()
        .iter()
        .map(|e| (e.path.as_str(), e.kind))
        .collect();
    assert_eq!(
        actual,
        vec![
            ("different.md", Kind::Different),
            ("equal.bin", Kind::Equal),
            ("equal.md", Kind::Equal),
            ("local-only.md", Kind::OnlyHere),
            ("nested/0.md", Kind::OnlyThere),
            ("nested/1.md", Kind::OnlyThere),
            ("nested/2.md", Kind::OnlyThere),
            ("nested/3.md", Kind::OnlyThere),
            ("nested/4.md", Kind::OnlyThere),
            ("nested/5.md", Kind::OnlyThere),
            ("nested/local.bin", Kind::OnlyHere),
            ("remote-only.bin", Kind::OnlyThere)
        ]
    );
    assert_eq!(
        pair.second
            .read_remote_preview_file(&preview, "remote-only.bin")?,
        binary
    );
    pair.second.validate_remote_preview(&preview)?;
    assert!(pair
        .second
        .read_remote_preview_file(&preview, "../config.xml")
        .is_err());
    assert!(pair
        .second
        .preview_remote_vault(uuid::Uuid::new_v4(), &id, &pair.target.vault)
        .is_err());
    assert!(pair
        .second
        .preview_remote_vault(pair.peer, "unapproved-vault", &pair.target.vault)
        .is_err());
    assert_eq!(
        (
            all_hashes(&pair.source.vault)?,
            all_hashes(&pair.target.vault)?
        ),
        before_files
    );
    assert_eq!(
        (pair.source.api.config()?, pair.target.api.config()?),
        before_configs
    );
    assert_eq!(
        (
            fs::read(pair.source.home.join("config.xml"))?,
            fs::read(pair.target.home.join("config.xml"))?
        ),
        before_disk
    );
    assert_eq!(
        (fs::read(&source_state)?, fs::read(&target_state)?),
        before_journals
    );
    assert!(
        pair.target.api.config()?["folders"]
            .as_array()
            .unwrap()
            .is_empty(),
        "inspection enrolled the non-empty destination"
    );
    assert!(
        serde_json::from_slice::<Value>(&fs::read(&source_state)?)?["share_grants"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        pair.second
            .attach(pair.peer, &id, &pair.target.vault)
            .is_err(),
        "non-empty attach must stay refused"
    );
    // Two independently approved signers can hold complete capabilities at once.
    // The deterministic slow-boundary unit complements these real successful reads.
    let third = Controller::open(
        pair._root.path().join("third-controller"),
        vec![pair.target.home.join("config.xml")],
    )?;
    let offer = pair
        .first
        .begin_pairing_at("Source", "127.0.0.1:0".parse()?, "127.0.0.1".parse()?)?
        .offer_url
        .context("pairing offer missing")?;
    third.redeem(&offer, "Third inspector")?;
    pair.first.approve(
        pair.first
            .snapshot()?
            .pending
            .context("pending inspector missing")?
            .request_id,
    )?;
    let third_peer = third.refresh()?.peers[0].id;
    let (left, right) = std::thread::scope(|scope| {
        let left = scope.spawn(|| {
            pair.second
                .preview_remote_vault(pair.peer, &id, &pair.target.vault)
        });
        let right = scope.spawn(|| third.preview_remote_vault(third_peer, &id, &pair.target.vault));
        (left.join().unwrap(), right.join().unwrap())
    });
    let left = left?;
    let right = right?;
    pair.second.validate_remote_preview(&left)?;
    third.validate_remote_preview(&right)?;
    assert_eq!(
        third.read_remote_preview_file(&right, "remote-only.bin")?,
        binary
    );
    pair.second.validate_remote_preview(&preview)?;
    third.shutdown();
    fs::write(
        pair.target.vault.join("local-only.md"),
        b"local edit invalidates preview",
    )?;
    assert!(pair.second.validate_remote_preview(&preview).is_err());
    assert!(pair
        .second
        .read_remote_preview_file(&preview, "remote-only.bin")
        .is_err());
    let fresh = pair
        .second
        .preview_remote_vault(pair.peer, &id, &pair.target.vault)?;
    fs::write(
        pair.source.vault.join("different.md"),
        b"remote edit invalidates preview",
    )?;
    assert!(pair.second.validate_remote_preview(&fresh).is_err());
    assert!(pair
        .second
        .read_remote_preview_file(&fresh, "remote-only.bin")
        .is_err());
    let fresh = pair
        .second
        .preview_remote_vault(pair.peer, &id, &pair.target.vault)?;
    let _new = pair
        .second
        .preview_remote_vault(pair.peer, &id, &pair.target.vault)?;
    assert!(
        pair.second.validate_remote_preview(&fresh).is_ok(),
        "unchanged second inspection invalidated first snapshot"
    );
    for patch in [json!({"paused":true}), json!({"type":"receiveonly"})] {
        pair.source.api.patch_folder(&id, &patch)?;
        let expected = if patch["paused"] == true {
            "paused vault cannot be inspected"
        } else {
            "inspection requires a promoted sendreceive vault"
        };
        assert_eq!(
            pair.second
                .validate_remote_preview(&fresh)
                .err()
                .context("paused/receiving validation succeeded")?
                .to_string(),
            expected
        );
        assert_eq!(
            pair.second
                .preview_remote_vault(pair.peer, &id, &pair.target.vault)
                .err()
                .context("paused/receiving opening succeeded")?
                .to_string(),
            expected
        );
        pair.source
            .api
            .patch_folder(&id, &json!({"paused":false,"type":"sendreceive"}))?;
    }
    let prepared = pair
        .target
        .home
        .parent()
        .context("target preparation missing")?;
    for here in [
        pair._root.path().join("target-controller"),
        prepared.to_owned(),
        pair.target.home.clone(),
        pair._root.path().to_owned(),
    ] {
        let error = pair
            .second
            .preview_remote_vault(pair.peer, &id, &here)
            .err()
            .context("accepted private-state overlap")?;
        assert!(
            error.to_string().contains("overlaps operational state"),
            "{here:?}: {error:#}"
        );
    }
    let empty = pair._root.path().join("empty-inspection");
    fs::create_dir(&empty)?;
    assert!(pair
        .second
        .preview_remote_vault(pair.peer, &id, &empty)
        .is_err());
    std::os::unix::fs::symlink(pair._root.path(), pair.source.vault.join("escape"))?;
    assert!(pair
        .second
        .preview_remote_vault(pair.peer, &id, &pair.target.vault)
        .is_err());
    fs::remove_file(pair.source.vault.join("escape"))?;
    let policy = fs::read(pair.source.vault.join(".stignore"))?;
    let mut changed = policy.clone();
    changed.extend_from_slice(b"secret\n");
    fs::write(pair.source.vault.join(".stignore"), changed)?;
    assert!(pair
        .second
        .preview_remote_vault(pair.peer, &id, &pair.target.vault)
        .is_err());
    fs::write(pair.source.vault.join(".stignore"), policy)?;
    // Positive control after every failure: authorized fresh inventory still works.
    let fresh = pair
        .second
        .preview_remote_vault(pair.peer, &id, &pair.target.vault)?;
    pair.second.validate_remote_preview(&fresh)?;
    pair.source.stop()?;
    pair.source.start()?;
    assert!(
        pair.second.validate_remote_preview(&fresh).is_err(),
        "remote runtime restart retained an old revision"
    );
    let fresh = pair
        .second
        .preview_remote_vault(pair.peer, &id, &pair.target.vault)?;
    pair.target.stop()?;
    pair.target.start()?;
    assert!(
        pair.second.validate_remote_preview(&fresh).is_err(),
        "local runtime restart retained an old revision"
    );
    let fresh = pair
        .second
        .preview_remote_vault(pair.peer, &id, &pair.target.vault)?;
    pair.second.validate_remote_preview(&fresh)?;
    pair.first.shutdown();
    pair.second.shutdown();
    pair.source.stop()?;
    pair.target.stop()?;
    assert!(
        pair.source.child.is_none() && pair.target.child.is_none(),
        "fixture daemons must be killed and reaped"
    );
    Ok(())
}
