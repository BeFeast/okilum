use super::*;
use std::{fs, os::unix::fs::symlink};

fn header(files: usize, bytes: usize) -> Header {
    Header {
        id: Uuid::new_v4(),
        revision: "revision".into(),
        manifest: String::new(),
        files,
        bytes,
        binding: Binding {
            pair: Uuid::new_v4(),
            owner: "owner".into(),
            reader: "reader".into(),
            vault: "vault".into(),
            root: "/fixture".into(),
            root_identity: (1, 2),
            device: "device".into(),
            certificate: "certificate".into(),
            config: "config".into(),
            generation: Uuid::new_v4(),
            started: "start".into(),
            policy: "policy".into(),
        },
    }
}
fn entries() -> Vec<Entry> {
    (0..7)
        .map(|i| Entry {
            path: format!("{i}.md"),
            size: 1,
            sha256: wire::fingerprint(&[i]),
        })
        .collect()
}
fn page(header: &Header, offset: usize, entries: &[Entry]) -> Response {
    Response::Page {
        header: header.clone(),
        offset,
        entries: entries.to_vec(),
    }
}
#[test]
fn complete_pages_are_required_and_mixed_bindings_fail_closed() -> Result<()> {
    let entries = entries();
    let mut h = header(entries.len(), entries.len());
    h.manifest = digest(&entries)?;
    let mut good = Collector::new(h.clone());
    good.page(page(&h, 0, &entries[..4]))?;
    good.page(page(&h, 4, &entries[4..]))?;
    assert_eq!(good.finish()?, entries);
    let mut missing = Collector::new(h.clone());
    missing.page(page(&h, 0, &entries[..4]))?;
    assert!(missing.finish().is_err());
    let mut skipped = Collector::new(h.clone());
    assert!(skipped.page(page(&h, 4, &entries[4..])).is_err());
    let mut duplicate = Collector::new(h.clone());
    duplicate.page(page(&h, 0, &entries[..4]))?;
    assert!(duplicate.page(page(&h, 0, &entries[..4])).is_err());
    assert!(Collector::new(h.clone())
        .page(page(&h, 0, &entries[..3]))
        .is_err());
    let mut forged = vec![h.clone(); 8];
    forged[0].id = Uuid::new_v4();
    forged[1].binding.pair = Uuid::new_v4();
    forged[2].binding.owner = "another computer".into();
    forged[3].binding.reader = "another reader".into();
    forged[4].binding.root_identity = (3, 4);
    forged[5].binding.policy = "other excludes".into();
    forged[6].binding.generation = Uuid::new_v4();
    forged[7].binding.started = "restarted".into();
    for forged in forged {
        let mut collector = Collector::new(h.clone());
        collector.page(page(&h, 0, &entries[..4]))?;
        assert!(collector.page(page(&forged, 4, &entries[4..])).is_err());
    }
    let mut incorrect = entries.clone();
    incorrect[5].sha256 = "0".repeat(64);
    let mut collector = Collector::new(h.clone());
    collector.page(page(&h, 0, &incorrect[..4]))?;
    collector.page(page(&h, 4, &incorrect[4..]))?;
    assert!(collector.finish().is_err());
    Ok(())
}
#[test]
fn chunks_bind_offsets_revision_identity_and_content_hash_with_frame_controls() -> Result<()> {
    let bytes: Vec<_> = (0..CHUNK_BYTES + 27).map(|i| (i % 256) as u8).collect();
    let entry = Entry {
        path: "binary.dat".into(),
        size: bytes.len(),
        sha256: wire::fingerprint(&bytes),
    };
    let h = header(1, bytes.len());
    let response = |header: Header, index, offset, hex: String| Response::Content {
        header,
        index,
        offset,
        sha256: entry.sha256.clone(),
        hex,
    };
    let mut collected = vec![];
    append_chunk(
        &mut collected,
        &h,
        0,
        &entry,
        response(h.clone(), 0, 0, wire::hex(&bytes[..CHUNK_BYTES])),
    )?;
    append_chunk(
        &mut collected,
        &h,
        0,
        &entry,
        response(h.clone(), 0, CHUNK_BYTES, wire::hex(&bytes[CHUNK_BYTES..])),
    )?;
    assert_eq!(collected, bytes);
    let hex = wire::hex(&bytes[..CHUNK_BYTES]);
    for bad in [
        response(h.clone(), 1, 0, hex.clone()),
        response(h.clone(), 0, 1, hex.clone()),
        response(h.clone(), 0, 0, hex[..hex.len() - 2].into()),
        response(header(1, bytes.len()), 0, 0, hex.clone()),
    ] {
        assert!(append_chunk(&mut vec![], &h, 0, &entry, bad).is_err());
    }
    let scope = h.binding.pair;
    let seed = wire::random()?;
    let key = wire::random()?;
    let frame = wire::seal(
        scope,
        &key,
        &seed,
        &super::super::Response {
            nonce: Uuid::new_v4(),
            transport: None,
            result: Reply::Inspection {
                response: response(h.clone(), 0, 0, hex),
            },
        },
    )?;
    assert!(serde_json::to_vec(&frame)?.len() < wire::MAX_FRAME);
    let (_opened, _): (super::super::Response, _) =
        wire::open(frame, &key, Some(&wire::public(&seed)?))?;
    let mut worst = entries()[..4].to_vec();
    for (i, e) in worst.iter_mut().enumerate() {
        e.path = format!("{i}{}", "z".repeat(1023));
    }
    let frame = wire::seal(
        scope,
        &key,
        &seed,
        &super::super::Response {
            nonce: Uuid::new_v4(),
            transport: None,
            result: Reply::Inspection {
                response: page(&h, 0, &worst),
            },
        },
    )?;
    assert!(serde_json::to_vec(&frame)?.len() < wire::MAX_FRAME);
    Ok(())
}
#[test]
fn no_follow_tree_rejects_unsafe_paths_special_files_hardlinks_and_custom_ignores() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let vault = temp.path().join("vault");
    fs::create_dir(&vault)?;
    fs::write(vault.join("safe.md"), b"positive control")?;
    let root = tree::open_root(&vault)?;
    let before = Tree::collect(&root, &vault, true)?;
    assert_eq!(before.files["safe.md"], b"positive control");
    for path in [
        "../secret",
        "/secret",
        "nested/../secret",
        "a\\b",
        "C:secret",
        "a\0b",
        "",
        "a//b",
    ] {
        assert!(tree::validate_path(path).is_err(), "accepted {path:?}");
    }
    symlink(temp.path(), vault.join("link"))?;
    assert!(Tree::collect(&root, &vault, true).is_err());
    fs::remove_file(vault.join("link"))?;
    symlink(&vault, temp.path().join("alias"))?;
    assert!(tree::open_root(&temp.path().join("alias")).is_err());
    fs::hard_link(vault.join("safe.md"), vault.join("hardlink"))?;
    assert!(Tree::collect(&root, &vault, true).is_err());
    fs::remove_file(vault.join("hardlink"))?;
    rustix::fs::mkfifoat(
        &root,
        "fifo",
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )?;
    assert!(Tree::collect(&root, &vault, true).is_err());
    fs::remove_file(vault.join("fifo"))?;
    fs::write(
        vault.join(".stignore"),
        format!("{}\nsecret\n", source::IGNORES.join("\n")),
    )?;
    assert!(Tree::collect(&root, &vault, true).is_err());
    fs::write(
        vault.join(".stignore"),
        format!("{}\n", source::IGNORES.join("\n")),
    )?;
    fs::create_dir(vault.join(".okilum-index"))?;
    fs::write(
        vault.join(".okilum-index/private"),
        b"must never be exposed",
    )?;
    let valid = Tree::collect(&root, &vault, true)?;
    assert_eq!(valid.files.len(), 1);
    fs::write(vault.join("safe.md"), b"positive control changed")?;
    let after = Tree::collect(&root, &vault, false)?;
    assert_ne!(valid.stamps, after.stamps);
    fs::rename(&vault, temp.path().join("previous"))?;
    fs::create_dir(&vault)?;
    assert!(Tree::collect(&root, &vault, true).is_err());
    Ok(())
}
#[test]
fn oversized_inventories_are_errors_not_truncated_success() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = tree::open_root(temp.path())?;
    let file = File::create(temp.path().join("oversize"))?;
    file.set_len(MAX_BYTES as u64 + 1)?;
    assert!(Tree::collect(&root, temp.path(), true).is_err());
    Ok(())
}
#[test]
fn unapproved_peers_and_vaults_fail_before_any_filesystem_access() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let controller = Controller::open(temp.path().join("controller"), vec![])?;
    let mut db = controller.shared.db.lock().unwrap();
    let scope = Uuid::new_v4();
    let signer = wire::public(&wire::random()?)?;
    assert!(controller
        .inspect_request(
            &db,
            scope,
            &signer,
            Request::Open {
                vault: "secret".into()
            }
        )
        .is_err());
    db.peers.insert(
        scope,
        Paired {
            peer: Peer {
                id: scope,
                name: "Fixture".into(),
                device_id: "fingerprint".into(),
            },
            public: signer.clone(),
            key: wire::random()?,
            endpoint: "127.0.0.1:1".parse()?,
            allowed: vec![],
            syncthing_device: Some("approved device".into()),
            addresses: vec![],
        },
    );
    assert!(controller
        .inspect_request(
            &db,
            scope,
            &signer,
            Request::Open {
                vault: "secret".into()
            }
        )
        .is_err());
    assert!(controller
        .inspect_request(
            &db,
            scope,
            &[9; 32],
            Request::Open {
                vault: "secret".into()
            }
        )
        .is_err());
    Ok(())
}
