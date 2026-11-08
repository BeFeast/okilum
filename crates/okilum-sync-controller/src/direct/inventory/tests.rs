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
            remote: true,
            root_binding: "opaque fixture".into(),
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
    forged[4].binding.root_binding = "another opaque root".into();
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
    let scope = Uuid::new_v4();
    let signer = wire::public(&wire::random()?)?;
    assert!(controller
        .inspect_request(
            scope,
            &signer,
            Request::Open {
                vault: "secret".into()
            }
        )
        .is_err());
    controller.shared.db.lock().unwrap().peers.insert(
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
            scope,
            &signer,
            Request::Open {
                vault: "secret".into()
            }
        )
        .is_err());
    assert!(controller
        .inspect_request(
            scope,
            &[9; 32],
            Request::Open {
                vault: "secret".into()
            }
        )
        .is_err());
    Ok(())
}

#[test]
fn full_validation_detects_changed_bytes_even_if_metadata_revision_collides() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = Arc::new(tree::open_root(temp.path())?);
    fs::write(temp.path().join("note.md"), b"positive control")?;
    let mut binding = header(1, 16).binding;
    binding.policy = tree::policy(&root)?;
    let mut original = local(binding, temp.path().to_owned(), root.clone())?;
    let good = Tree::collect(&root, temp.path(), true)?;
    assert!(original.matches(&good, true));
    fs::write(temp.path().join("note.md"), b"negative control")?;
    let changed = Tree::collect(&root, temp.path(), true)?;
    // Model coarse timestamps explicitly rather than depending on host clock
    // granularity: metadata alone reports the same revision, bytes do not.
    original.tree.stamps = changed.stamps.clone();
    assert!(original.matches(&changed, false));
    assert!(!original.matches(&changed, true));
    Ok(())
}

fn source(path: &Path) -> ExistingVault {
    ExistingVault {
        daemon: daemon::DaemonIdentity {
            config_file: path.join("prepared/syncthing/config.xml"),
            device_id: "device".into(),
            certificate_hash: "certificate".into(),
        },
        version: daemon::CLIENT_VERSION.into(),
        folder_id: "vault".into(),
        path: path.into(),
        label: "Vault".into(),
        paused: false,
        folder_type: "sendreceive".into(),
        peers: vec![],
        root_identity: Some((1, 2)),
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        stable_root_identity: None,
    }
}
fn stored(path: &Path, peer: Uuid, vault: &str) -> Result<Stored> {
    let root = Arc::new(tree::open_root(path)?);
    let mut b = header(0, 0).binding;
    b.pair = peer;
    b.vault = vault.into();
    b.policy = tree::policy(&root)?;
    Ok(Stored {
        local: local(b, path.into(), root)?,
        source: source(path),
        expires: Instant::now() + Duration::from_secs(LIFETIME_SECONDS),
    })
}
#[test]
fn cache_is_peer_vault_revision_scoped_bounded_and_reaps_expired_bytes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("note.md"), b"cache positive control")?;
    let mut cache = Cache::default();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    cache.admit(a, Some("vault"))?;
    assert!(cache.admit(a, Some("vault")).is_err());
    cache.admit(b, Some("vault"))?;
    let first = cache.insert(stored(temp.path(), a, "vault")?);
    let second = cache.insert(stored(temp.path(), b, "vault")?);
    cache.busy.clear();
    // A stable binding and revision reuses the original capability.
    let mut same = stored(temp.path(), a, "vault")?;
    same.local.header.binding = first.binding.clone();
    assert_eq!(cache.insert(same), first);
    assert_eq!(cache.entries.len(), 2);
    assert_eq!(
        cache.entries[&(b, "vault".into(), second.revision.clone())]
            .local
            .header,
        second
    );
    cache.admit(a, Some("other"))?;
    cache.insert(stored(temp.path(), a, "other")?);
    cache.busy.clear();
    assert!(cache.admit(a, Some("third")).is_err());
    let c = Uuid::new_v4();
    cache.admit(c, Some("vault"))?;
    assert!(
        cache.admit(Uuid::new_v4(), Some("vault")).is_err(),
        "open reservations must count toward memory bound"
    );
    cache.insert(stored(temp.path(), c, "vault")?);
    cache.busy.clear();
    assert!(cache.admit(Uuid::new_v4(), Some("vault")).is_err());
    for stored in cache.entries.values_mut() {
        Arc::get_mut(stored).unwrap().expires = Instant::now();
    }
    let weak = Arc::downgrade(cache.entries.values().next().unwrap());
    cache.prune();
    assert!(cache.entries.is_empty());
    assert!(weak.upgrade().is_none(), "expired allocation retained");
    cache.admit(a, Some("vault"))?;
    Ok(())
}
#[test]
fn unchanged_reopening_retains_capability_but_installs_the_fresh_validated_snapshot() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("note.md"), b"unchanged canonical bytes")?;
    let peer = Uuid::new_v4();
    let mut cache = Cache::default();
    let mut first = stored(temp.path(), peer, "vault")?;
    first.expires = Instant::now() + Duration::from_secs(LIFETIME_SECONDS / 2);
    let original_expiry = first.expires;
    let header = cache.insert(first);
    let key = (peer, "vault".into(), header.revision.clone());
    let in_flight = cache.entries[&key].clone();
    let mut fresh = stored(temp.path(), peer, "vault")?;
    fresh.local.header.binding = header.binding.clone();
    let fresh_expiry = fresh.expires;
    let fresh_root = fresh.local.root.clone();
    assert!(fresh_expiry > original_expiry);
    assert!(!Arc::ptr_eq(&in_flight.local.root, &fresh_root));
    assert_eq!(cache.insert(fresh), header);
    let reopened = &cache.entries[&key];
    assert_eq!(
        reopened.expires, fresh_expiry,
        "fresh Open discarded its newly validated snapshot's expiry"
    );
    assert!(Arc::ptr_eq(&reopened.local.root, &fresh_root));
    assert_eq!(in_flight.expires, original_expiry);
    assert_eq!(cache.entries.len(), 1);
    // Once expired, even an identical full reopening needs a new capability.
    Arc::get_mut(cache.entries.get_mut(&key).unwrap())
        .unwrap()
        .expires = Instant::now();
    let mut after_expiry = stored(temp.path(), peer, "vault")?;
    after_expiry.local.header.binding = header.binding.clone();
    assert_ne!(cache.insert(after_expiry).id, header.id);
    Ok(())
}
#[test]
fn paused_receiving_and_missing_folder_state_are_explicit_refusals() -> Result<()> {
    use serde_json::json;
    inspectable(&json!({"paused":false,"type":"sendreceive"}))?;
    for (folder, expected) in [
        (json!({"paused":true,"type":"sendreceive"}), PAUSED),
        (json!({"paused":false,"type":"receiveonly"}), NOT_PROMOTED),
    ] {
        let error = inspectable(&folder).unwrap_err();
        assert_eq!(public_refusal(&error), Some(expected));
    }
    assert!(public_refusal(&anyhow::anyhow!("private path /home/private-user/vault")).is_none());
    for folder in [
        json!({"paused":true,"type":"sendreceive"}),
        json!({"paused":false,"type":"receiveonly"}),
        json!({"paused":false,"type":"sendonly"}),
        json!({}),
    ] {
        assert!(inspectable(&folder).is_err());
    }
    Ok(())
}
#[test]
fn opaque_binding_frames_hide_local_paths_usernames_and_raw_identity() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("home/private-user/vault");
    fs::create_dir_all(&path)?;
    let root = tree::open_root(&path)?;
    let key = wire::random()?;
    let mut h = header(1, 1);
    let scope = h.binding.pair;
    let opaque = opaque_root(&key, scope, &path, &root)?;
    assert_eq!(opaque_root(&key, scope, &path, &root)?, opaque);
    assert_ne!(opaque_root(&wire::random()?, scope, &path, &root)?, opaque);
    assert_ne!(opaque_root(&key, Uuid::new_v4(), &path, &root)?, opaque);
    h.binding.root_binding = opaque;
    let responses = [
        Response::Open { header: h.clone() },
        page(&h, 0, &entries()[..1]),
        Response::Content {
            header: h.clone(),
            index: 0,
            offset: 0,
            sha256: wire::fingerprint(b"x"),
            hex: wire::hex(b"x"),
        },
        Response::Valid { header: h.clone() },
    ];
    for response in responses {
        let frame = wire::seal(scope, &key, &key, &response)?;
        let (opened, _): (Response, _) = wire::open(frame, &key, Some(&wire::public(&key)?))?;
        let clear = serde_json::to_string(&opened)?;
        assert!(
            clear.contains("root_binding"),
            "positive frame decoding control"
        );
        assert!(!clear.contains(path.to_str().unwrap()));
        assert!(!clear.contains("private-user"));
        assert!(!clear.contains("root_identity"));
        assert!(!clear.contains("\"root\""));
    }
    Ok(())
}
#[test]
fn destination_overlap_is_refused_in_both_directions_before_runtime_access() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    let controller = Controller::open(state.clone(), vec![])?;
    fs::create_dir_all(state.join("nested"))?;
    let ordinary = temp.path().join("ordinary");
    fs::create_dir(&ordinary)?;
    disjoint(&ordinary, &state)?;
    for path in [&state, &state.join("nested"), &temp.path().to_owned()] {
        assert!(disjoint(path, &state).is_err());
    }
    for path in [&state, &state.join("nested"), &temp.path().to_owned()] {
        let error = controller
            .preview_remote_vault(Uuid::new_v4(), "vault", path)
            .err()
            .unwrap();
        assert!(
            error.to_string().contains("overlaps operational state"),
            "{error:#}"
        );
    }
    Ok(())
}

#[test]
fn slow_inspection_does_not_block_second_peer_pairing_or_controller_locks() -> Result<()> {
    use std::sync::mpsc;
    let temp = tempfile::tempdir()?;
    let server = Controller::open(temp.path().join("server"), vec![])?;
    let offer = server
        .begin_pairing_at("Server", "127.0.0.1:0".parse()?, "127.0.0.1".parse()?)?
        .offer_url
        .unwrap();
    let endpoint = decode_url(&offer)?.endpoint;
    let server_public = wire::public(&server.shared.db.lock().unwrap().seed)?;
    let clients = [
        Controller::open(temp.path().join("first"), vec![])?,
        Controller::open(temp.path().join("second"), vec![])?,
    ];
    let ids = [Uuid::new_v4(), Uuid::new_v4()];
    let keys: [[u8; 32]; 2] = [wire::random()?, wire::random()?];
    {
        let mut db = server.shared.db.lock().unwrap();
        db.attached
            .push(source(&temp.path().join("unavailable-vault")));
        for ((id, key), client) in ids.iter().zip(&keys).zip(&clients) {
            db.peers.insert(
                *id,
                Paired {
                    peer: Peer {
                        id: *id,
                        name: format!("Client {id}"),
                        device_id: "fixture".into(),
                    },
                    public: wire::public(&client.shared.db.lock().unwrap().seed)?,
                    key: *key,
                    endpoint,
                    allowed: vec!["vault".into()],
                    syncthing_device: Some("approved".into()),
                    addresses: vec![],
                },
            );
        }
    }
    let (entered, observed) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    *server.shared.inspection_hook.lock().unwrap() = Some(Arc::new(move |id| {
        entered.send(id).unwrap();
        if id == ids[0] {
            // Deterministic slow-I/O boundary, after actual wire auth/admission.
            // Timeout also releases the worker if an assertion fails.
            let _ = released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5));
        }
    }));
    let first = clients[0].clone();
    let public = server_public.clone();
    let slow = thread::spawn(move || {
        first.call(
            endpoint,
            ids[0],
            &keys[0],
            &public,
            Operation::Inspect {
                request: Box::new(Request::Open {
                    vault: "vault".into(),
                }),
            },
        )
    });
    assert_eq!(
        observed.recv_timeout(Duration::from_secs(1))?,
        ids[0],
        "positive control: inspection really entered slow boundary"
    );
    let second = clients[1].clone();
    let public = server_public.clone();
    let server_clone = server.clone();
    let third = Controller::open(temp.path().join("pairing"), vec![])?;
    let (done, result) = mpsc::channel();
    let progress = thread::spawn(move || {
        let work = (|| -> Result<()> {
            assert!(
                server_clone.shared.db.try_lock().is_ok(),
                "db held during inspection"
            );
            assert!(
                server_clone.shared.inventory.try_lock().is_ok(),
                "cache held during inspection"
            );
            server_clone.snapshot()?;
            assert!(matches!(
                second.call(
                    endpoint,
                    ids[1],
                    &keys[1],
                    &public,
                    Operation::Inspect {
                        request: Box::new(Request::Open {
                            vault: "vault".into()
                        })
                    }
                )?,
                Reply::Error { .. }
            ));
            third.redeem(&offer, "New computer")?;
            assert!(server_clone.snapshot()?.pending.is_some());
            Ok(())
        })();
        done.send(work).unwrap();
    });
    let outcome = result.recv_timeout(Duration::from_secs(2));
    release.send(())?;
    progress.join().unwrap();
    assert!(matches!(slow.join().unwrap()?, Reply::Error { .. }));
    outcome??;
    assert_eq!(
        observed.recv_timeout(Duration::from_secs(1))?,
        ids[1],
        "second inspection must really be admitted, not merely rejected by the worker limit"
    );
    assert!(
        server.shared.inventory.lock().unwrap().busy.is_empty(),
        "failed requests leaked peer permits"
    );
    server.shutdown();
    Ok(())
}

#[test]
fn opened_inventory_descriptor_must_match_the_approved_root_identity() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("vault");
    fs::create_dir(&path)?;
    fs::write(path.join("note.md"), b"approved root")?;
    let original = tree::open_root(&path)?;
    let mut approved = source(&path);
    approved.root_identity = Some(tree::identity(&original)?);
    let pinned = authorized_root(&approved)?;
    assert_eq!(tree::identity(&pinned)?, tree::identity(&original)?);
    fs::rename(&path, temp.path().join("previous"))?;
    fs::create_dir(&path)?;
    fs::write(path.join("note.md"), b"unapproved replacement")?;
    assert!(authorized_root(&approved).is_err());
    assert_ne!(
        tree::identity(&tree::open_root(&path)?)?,
        tree::identity(&original)?
    );
    Ok(())
}
