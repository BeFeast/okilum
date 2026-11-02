use super::*;
use ::core::prelude::v1::test;
use std::sync::atomic::AtomicUsize;

fn identity() -> Identity {
    Identity {
        origin: Environment::new("fixture", "https://rotation.invalid")
            .unwrap()
            .origin(),
        project: "p/../🧠".into(),
        manager: "m/one".into(),
    }
}
fn configuration() -> Configuration {
    Configuration {
        enabled: true,
        timezone: "Asia/Jerusalem".into(),
        at: ["08:30".into(), "20:30".into()],
        reviewed_source: Some("Reviewed source 🧠\r\n  keep spaces  \n".into()),
    }
}
fn fixture() -> (tempfile::TempDir, Store, Loaded) {
    let state = tempfile::tempdir().unwrap();
    let store = Store::new(state.path(), identity()).unwrap();
    let loaded = store
        .configure(0, configuration(), 20758 * 86400, &resolver(), || true)
        .unwrap();
    (state, store, loaded)
}
fn bytes(store: &Store) -> Vec<(String, Vec<u8>)> {
    let mut result: Vec<_> = fs::read_dir(&store.directory)
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".owner"))
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                fs::read(e.path()).unwrap(),
            )
        })
        .collect();
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

#[test]
fn rotation_policy_1473_read_is_noncreating_and_lifecycle_is_not_checkpoint_revision() {
    let state = tempfile::tempdir().unwrap();
    let store = Store::new(state.path(), identity()).unwrap();
    assert!(store.load().unwrap().is_none());
    assert!(
        !store.directory.exists(),
        "read must not allocate authority or a namespace"
    );
    let loaded = store
        .configure(0, configuration(), 20758 * 86400, &resolver(), || true)
        .unwrap();
    assert_eq!(loaded.record.lifecycle_revision, 1);
    let saved = bytes(&store);
    for _ in 0..4 {
        store
            .with_current(&loaded.record.owner(), |policy| {
                let mut checkpoint = policy.load()?.unwrap();
                let revision = checkpoint.revision;
                checkpoint.through += 1;
                policy.save(revision, checkpoint).map(|_| ())
            })
            .unwrap();
        assert_eq!(
            Store::new(state.path(), identity())
                .unwrap()
                .load()
                .unwrap()
                .unwrap()
                .record,
            loaded.record
        );
    }
    assert_eq!(
        bytes(&store),
        saved,
        "checkpoint CAS and restart never churn authority"
    );
    let mut changed = configuration();
    changed.at = ["09:00".into(), "21:00".into()];
    let next = store
        .configure(
            loaded.record.sequence,
            changed,
            20759 * 86400,
            &resolver(),
            || true,
        )
        .unwrap();
    assert_eq!(next.record.lifecycle_revision, 2);
    assert_ne!(next.record.lifecycle_revision, next.checkpoint.revision);
    assert_eq!(
        next.record.configuration.reviewed_source,
        loaded.record.configuration.reviewed_source
    );
    assert_eq!(
        store.directory.file_name().unwrap().to_string_lossy().len(),
        64
    );
}

#[test]
fn rotation_policy_1473_preparing_restart_and_cancellation_never_mint_ready_authority() {
    let state = tempfile::tempdir().unwrap();
    let store = Store::new(state.path(), identity()).unwrap();
    let checks = AtomicUsize::new(0);
    assert!(store
        .configure(0, configuration(), 20758 * 86400, &resolver(), || {
            checks.fetch_add(1, Ordering::SeqCst) == 0
        })
        .is_err());
    assert!(store.directory.join("1.owner").exists());
    assert!(!store.directory.join("2.owner").exists());
    let saved = bytes(&store);
    let restarted = Store::new(state.path(), identity()).unwrap();
    assert!(restarted.load().is_err());
    assert!(restarted
        .with_current(&identity().owner(1), |_| Ok(()))
        .is_err());
    assert_eq!(bytes(&store), saved);
    // Only an explicit CAS against the allocated Preparing record can repair it.
    let repaired = restarted
        .configure(1, configuration(), 20758 * 86400, &resolver(), || true)
        .unwrap();
    assert_eq!(repaired.record.lifecycle_revision, 2);
    assert!(restarted
        .with_current(&identity().owner(1), |_| Ok(()))
        .is_err());
}

#[test]
fn rotation_policy_1473_missing_corrupt_and_hash_collision_identity_hold_without_rewrite() {
    let (_state, store, loaded) = fixture();
    let saved = bytes(&store);
    let mut collision = store.clone();
    collision.identity.manager = "another-manager".into();
    assert!(
        collision.load().is_err(),
        "full identity validation must survive a forced hash collision"
    );
    assert!(collision
        .configure(0, configuration(), 20759 * 86400, &resolver(), || true)
        .is_err());
    assert_eq!(bytes(&store), saved);
    fs::write(store.directory.join("2.owner"), b"{broken").unwrap();
    assert!(store.load().is_err());
    assert!(store
        .with_current(&loaded.record.owner(), |_| Ok(()))
        .is_err());
    assert_eq!(
        fs::read(store.directory.join("2.owner")).unwrap(),
        b"{broken"
    );
    for (name, _) in saved {
        fs::remove_file(store.directory.join(name)).unwrap();
    }
    assert!(
        store.load().is_err(),
        "checkpoint cannot synthesize missing authority"
    );
    assert!(store
        .configure(0, configuration(), 20759 * 86400, &resolver(), || true)
        .is_err());
    assert!(!store.directory.join("1.owner").exists());
}

#[test]
fn rotation_policy_1473_stale_configuration_and_replaced_authority_reject_old_cas() {
    let (_state, store, first) = fixture();
    let saved = bytes(&store);
    assert!(store
        .configure(0, configuration(), 20759 * 86400, &resolver(), || true)
        .is_err());
    assert_eq!(bytes(&store), saved);
    let second = store
        .configure(
            first.record.sequence,
            configuration(),
            20759 * 86400,
            &resolver(),
            || true,
        )
        .unwrap();
    assert!(store
        .with_current::<()>(&first.record.owner(), |_| panic!(
            "stale owner reached checkpoint CAS"
        ))
        .is_err());
    store
        .with_current(&second.record.owner(), |policy| {
            assert_eq!(policy.load()?.unwrap(), second.checkpoint);
            Ok(())
        })
        .unwrap();
    let before = bytes(&store);
    fs::rename(
        store.directory.join("2.owner"),
        store.directory.join("22.owner"),
    )
    .unwrap();
    assert!(
        store.load().is_err(),
        "ambiguous or incomplete immutable history is held"
    );
    assert!(store
        .with_current(&second.record.owner(), |_| Ok(()))
        .is_err());
    assert_eq!(bytes(&store).len(), before.len());
}

#[test]
fn rotation_policy_1473_configure_and_held_ack_share_one_lock_through_existing_cas() {
    let (_state, store, first) = fixture();
    let owner = first.record.owner();
    let pending = store
        .with_current(&owner, |policy| {
            let mut checkpoint = policy.load()?.unwrap();
            let revision = checkpoint.revision;
            checkpoint.pending = Some(okilum_t3::rotation::Slot {
                day: 20758,
                minute: 510,
                utc: Some(checkpoint.through - 1),
            });
            policy.save(revision, checkpoint)
        })
        .unwrap();
    assert!(
        store
            .configure(
                first.record.sequence,
                configuration(),
                20759 * 86400,
                &resolver(),
                || true
            )
            .is_err(),
        "pending claim must be reconciled first"
    );
    let worker = store.clone();
    let worker_owner = owner.clone();
    let (entered, waiting) = std::sync::mpsc::channel();
    let (release, held) = std::sync::mpsc::channel();
    let ack = std::thread::spawn(move || {
        worker.with_current(&worker_owner, |policy| {
            assert_eq!(policy.load()?.unwrap(), pending);
            entered.send(()).unwrap();
            held.recv().unwrap();
            policy.acknowledge(pending.revision, &worker_owner)
        })
    });
    waiting.recv().unwrap();
    let before = bytes(&store);
    assert!(
        store
            .configure(
                first.record.sequence,
                configuration(),
                20759 * 86400,
                &resolver(),
                || true
            )
            .is_err(),
        "Configure must not bypass the held acknowledgement lock"
    );
    assert_eq!(bytes(&store), before);
    release.send(()).unwrap();
    assert!(ack.join().unwrap().unwrap().pending.is_none());
    let next = store
        .configure(
            first.record.sequence,
            configuration(),
            20759 * 86400,
            &resolver(),
            || true,
        )
        .unwrap();
    assert!(store
        .with_current::<()>(&owner, |_| panic!(
            "old acknowledgement crossed replacement"
        ))
        .is_err());
    assert_eq!(store.load().unwrap().unwrap().record, next.record);
}

pub(in crate::project) fn ready(view: &mut ProjectView, cx: &mut Context<ProjectView>) {
    // These fixtures mount the composer, not ProjectView itself. Publish the
    // typed owner through the same hook as ProjectView::render after restore.
    view.typed_sync(cx);
    assert!(
        view.typed_scope().is_some(),
        "positive control: real parent typed owner is ready"
    );
    view.draft_handoff_surface_visible(true, cx);
    assert!(
        exact_identity(view, cx).is_ok(),
        "positive control: current native identity passes the unchanged readiness guard"
    );
}

pub(in crate::project) fn seed(
    view: &mut ProjectView,
    cx: &mut Context<ProjectView>,
) -> Arc<PolicyStore> {
    ready(view, cx);
    let store = Store::new(
        view.state.as_deref().unwrap(),
        exact_identity(view, cx).unwrap(),
    )
    .unwrap();
    let mut config = configuration();
    config.reviewed_source = Some("Reviewed source\n".into());
    let loaded = store
        .configure(0, config, 20758 * 86400, &resolver(), || true)
        .unwrap();
    let policy = loaded.policy.clone();
    view.rotation_policy.loaded = Some(loaded);
    policy
}

fn init(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
}

#[gpui::test]
fn rotation_policy_1473_explicit_save_review_and_restart_keep_authority_and_unsent_draft(
    cx: &mut TestAppContext,
) {
    init(cx);
    let state = tempfile::tempdir().unwrap();
    let mut owner = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let project = cx.new(|cx| {
            discuss_handoff::native_tests::view_with_draft(
                state.path(),
                "Keep authored 🧠\r\n",
                window,
                cx,
            )
        });
        let composer = project.read(cx).composer.clone();
        owner = Some(project);
        Root::new(composer, window, cx)
    });
    let project = owner.unwrap();
    visual.run_until_parked();
    project.update_in(visual, |view, window, cx| {
        ready(view, cx);
        view.open_rotation_policy(window, cx);
    });
    visual.run_until_parked();
    project.update_in(visual, |view, window, cx| {
        let form = view.rotation_policy.form.as_mut().unwrap();
        assert!(!form.enabled, "opening does not opt in");
        form.enabled = true;
        form.timezone.update(cx, |input, cx| {
            input.set_value("Asia/Jerusalem", window, cx)
        });
        form.source.update(cx, |input, cx| {
            input.set_value("Reviewed source\n", window, cx)
        });
        view.save_rotation_policy(window, cx);
    });
    visual.run_until_parked();
    let (authority, record) = project.update_in(visual, |view, _, cx| {
        assert!(
            view.rotation_policy.error.is_none(),
            "{:?}",
            view.rotation_policy.error
        );
        let loaded = view.rotation_policy.loaded.as_ref().unwrap();
        assert_eq!(loaded.record.lifecycle_revision, 1);
        let authority = loaded.authority.clone();
        let record = loaded.record.clone();
        // Synthetic earlier saved cursor represents an offline application gap.
        authority
            .with_current(&record.owner(), |policy| {
                let mut checkpoint = policy.load()?.unwrap();
                let revision = checkpoint.revision;
                checkpoint.through -= 2 * 86400;
                policy.save(revision, checkpoint).map(|_| ())
            })
            .unwrap();
        assert_eq!(
            view.composer.read(cx).typed_source_text(cx),
            "Keep authored 🧠\r\n"
        );
        assert!(view.records.is_empty());
        (authority, record)
    });
    let saved = bytes(&authority);
    project.update_in(visual, |view, window, cx| {
        view.reload_rotation_policy(window, cx)
    });
    visual.run_until_parked();
    project.update_in(visual, |view, window, cx| {
        view.review_rotation_window(window, cx)
    });
    visual.run_until_parked();
    project.update_in(visual, |view, _, cx| {
        assert!(
            view.rotation_policy.error.is_none(),
            "{:?}",
            view.rotation_policy.error
        );
        assert!(
            view.rotation.is_some(),
            "positive control: real pending reconciliation mounted"
        );
        assert!(authority
            .load()
            .unwrap()
            .unwrap()
            .checkpoint
            .pending
            .is_some());
        assert_eq!(authority.load().unwrap().unwrap().record, record);
        assert_eq!(
            bytes(&authority),
            saved,
            "Review changes only the checkpoint, never authority"
        );
        assert_eq!(
            view.composer.read(cx).typed_source_text(cx),
            "Keep authored 🧠\r\n"
        );
        assert!(view.records.is_empty());
    });
}

#[gpui::test]
fn rotation_policy_1473_stale_open_menu_refuses_same_entity_rebind_and_client_replacement(
    cx: &mut TestAppContext,
) {
    init(cx);
    let state = tempfile::tempdir().unwrap();
    let mut owner = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let project = cx.new(|cx| discuss_handoff::native_tests::view(state.path(), window, cx));
        let composer = project.read(cx).composer.clone();
        owner = Some(project);
        Root::new(composer, window, cx)
    });
    let project = owner.unwrap();
    visual.run_until_parked();
    project.update_in(visual, |view, window, cx| {
        ready(view, cx);
        let ticket = MenuTicket::capture(view, cx).unwrap();
        view.draft_handoff_connection_changing();
        view.generation += 1;
        view.draft_handoff_connection_ready(cx);
        ticket.open(view, window, cx);
        assert!(view.rotation_policy.form.is_none());
        assert!(view.rotation_notice.is_some());
        assert!(!state.path().join("manager-rotation-v1").exists());
        let ticket = MenuTicket::capture(view, cx).unwrap();
        let replacement = discuss_handoff::native_tests::client(
            view.environment.as_ref().unwrap(),
            okilum_t3::NetworkAccess::new(true),
        );
        view.native.set_detail_test_client(replacement);
        ticket.open(view, window, cx);
        assert!(
            view.rotation_policy.form.is_none(),
            "same IDs cannot substitute another client Arc"
        );
        let fresh = MenuTicket::capture(view, cx).unwrap();
        fresh.open(view, window, cx);
        assert!(
            view.rotation_policy.form.is_some(),
            "positive control: fresh menu opens configuration"
        );
    });
    visual.run_until_parked();
    assert!(
        !state.path().join("manager-rotation-v1").exists(),
        "fresh open still allocates no authority"
    );
}

#[gpui::test]
fn rotation_policy_1473_close_hide_and_rebind_cancel_queued_save_and_preserve_draft(
    cx: &mut TestAppContext,
) {
    init(cx);
    for action in ["close", "hide", "rebind"] {
        let state = tempfile::tempdir().unwrap();
        let mut owner = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let project = cx.new(|cx| {
                discuss_handoff::native_tests::view_with_draft(
                    state.path(),
                    "Retained draft\n",
                    window,
                    cx,
                )
            });
            let composer = project.read(cx).composer.clone();
            owner = Some(project);
            Root::new(composer, window, cx)
        });
        let project = owner.unwrap();
        visual.run_until_parked();
        project.update_in(visual, |view, window, cx| {
            ready(view, cx);
            view.open_rotation_policy(window, cx);
        });
        visual.run_until_parked();
        let identity = project.update_in(visual, |view, window, cx| {
            let identity = exact_identity(view, cx).unwrap();
            let form = view.rotation_policy.form.as_mut().unwrap();
            form.enabled = true;
            form.timezone.update(cx, |input, cx| {
                input.set_value("Asia/Jerusalem", window, cx)
            });
            view.save_rotation_policy(window, cx);
            match action {
                "close" => view.close_rotation_policy(cx),
                "hide" => view.draft_handoff_surface_visible(false, cx),
                _ => {
                    view.draft_handoff_connection_changing();
                    view.generation += 1;
                }
            }
            identity
        });
        visual.run_until_parked();
        assert!(Store::new(state.path(), identity)
            .unwrap()
            .load()
            .unwrap()
            .is_none());
        project.update_in(visual, |view, _, cx| {
            assert_eq!(
                view.composer.read(cx).typed_source_text(cx),
                "Retained draft\n"
            );
            assert!(view.records.is_empty());
            assert!(view.rotation_policy.pending.is_none());
        });
    }
}
