use super::*;
use ::core::prelude::v1::test;

fn fixture(window: &mut Window, cx: &mut Context<SyncSettings>) -> SyncSettings {
    SyncSettings {
        #[cfg(target_os = "linux")]
        runtimes: vec![],
        controller: None,
        snapshot: None,
        inspected: None,
        name: cx.new(|cx| InputState::new(window, cx).placeholder("This computer’s name")),
        invitation: cx.new(|cx| InputState::new(window, cx).placeholder("Paste pairing URL")),
        selected_peer: None,
        busy: false,
        epoch: Epoch::default(),
        error: None,
    }
}

#[gpui::test]
fn reader_settings_sync_validates_before_starting_any_controller(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| fixture(window, cx));
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<SyncSettings>().unwrap()
    });
    visual.run_until_parked();
    for selector in ["sync-add-computer", "sync-connect"] {
        let bounds = visual
            .debug_bounds(selector)
            .expect("production pairing action");
        visual.simulate_click(bounds.center(), Modifiers::default());
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert!(view.error.as_ref().unwrap().contains("name"));
            assert!(
                view.controller.is_none(),
                "validation must precede runtime or network IO"
            );
            assert!(!view.busy);
        });
    }
}

#[test]
fn reader_settings_sync_nonempty_and_expired_errors_have_actionable_recovery() {
    let nonempty = user_error(&anyhow::anyhow!("unknown nonempty destination"));
    assert!(nonempty.contains("Lossless merge preview"));
    assert!(nonempty.contains("empty folder"));
    let expired = user_error(&anyhow::anyhow!("invitation expired"));
    assert!(expired.contains("Create a new one"));
    let secret = "okilum://pair/private-token";
    assert!(!user_error(&anyhow::anyhow!(secret)).contains(secret));
}

fn snapshot(pending: Option<direct::Pending>) -> Snapshot {
    Snapshot {
        offer_url: None,
        preparing: false,
        pending,
        peers: vec![],
        catalog: vec![],
        attached: vec![],
        status: "Waiting for confirmation".into(),
    }
}

#[gpui::test]
fn reader_settings_sync_only_the_inviting_computer_can_confirm(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = fixture(window, cx);
            view.snapshot = Some(snapshot(Some(direct::Pending {
                request_id: uuid::Uuid::new_v4(),
                incoming: false,
                name: "Neo14".into(),
                device_id: "new-device".into(),
                fingerprint: "new-identity".into(),
            })));
            view
        });
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<SyncSettings>().unwrap()
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("sync-cancel-request").is_some(),
        "positive control: outgoing request rendered"
    );
    assert!(visual.debug_bounds("sync-approve-computer").is_none());
    view.update(visual, |view, cx| {
        view.snapshot
            .as_mut()
            .unwrap()
            .pending
            .as_mut()
            .unwrap()
            .incoming = true;
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("sync-approve-computer").is_some(),
        "positive control: incoming request has explicit approval"
    );
    assert!(visual.debug_bounds("sync-reject-computer").is_some());
    assert!(visual.debug_bounds("sync-cancel-request").is_none());
}

#[gpui::test]
fn reader_settings_sync_catalog_requires_a_selected_paired_computer(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let peer = uuid::Uuid::new_v4();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = fixture(window, cx);
            let mut saved = snapshot(None);
            saved.peers.push(direct::Peer {
                id: peer,
                name: "Existing computer".into(),
                device_id: "peer-device".into(),
            });
            saved.catalog.push(direct::Vault {
                id: "vault-id".into(),
                label: "Work notes".into(),
            });
            view.snapshot = Some(saved);
            view
        });
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<SyncSettings>().unwrap()
    });
    visual.run_until_parked();
    // A catalog cannot appear against an unselected or previous computer.
    assert!(
        visual.debug_bounds("sync-inspect-existing").is_some(),
        "positive control: settings rendered"
    );
    assert!(visual.debug_bounds("sync-authorized-catalog").is_none());
    assert!(view.read_with(visual, |view, _| view.selected_peer.is_none()));
    view.update(visual, |view, cx| {
        view.selected_peer = Some(peer);
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("sync-authorized-catalog").is_some(),
        "positive control: selected authorized catalog renders"
    );
    view.read_with(visual, |view, _| {
        assert_eq!(
            view.snapshot.as_ref().unwrap().catalog[0].label,
            "Work notes"
        );
        assert!(
            view.controller.is_none(),
            "displaying the catalog never starts sync"
        );
    });
}

#[gpui::test]
fn reader_settings_sync_suspend_revokes_a_real_listener_and_retained_controller(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let directory = tempfile::tempdir().unwrap();
    let controller = Controller::open(directory.path().join("controller"), vec![]).unwrap();
    let snapshot = controller
        .begin_pairing_at(
            "Existing computer",
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();
    let url = url::Url::parse(snapshot.offer_url.as_ref().unwrap()).unwrap();
    let endpoint: std::net::SocketAddr = url.path().trim_start_matches('/').parse().unwrap();
    // Positive control: the real application listener owns the address before suspend.
    assert!(std::net::TcpListener::bind(endpoint).is_err());
    let retained = controller.clone();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = fixture(window, cx);
            view.controller = Some(controller);
            view.snapshot = Some(snapshot);
            view
        });
        cx.set_global(BackgroundSync(view.clone()));
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<SyncSettings>().unwrap()
    });
    visual.update(|_, cx| suspend(cx));
    assert!(
        retained.snapshot().is_err(),
        "retained background clones lose permission immediately"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if std::net::TcpListener::bind(endpoint).is_ok() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "application listener must release its address"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    view.read_with(visual, |view, _| {
        assert!(view.controller.is_none());
        assert!(view.snapshot.is_none());
        assert!(!view.busy);
    });
    // No Syncthing configuration, process or canonical vault exists in this fixture.
}

#[test]
fn direct_metadata_parent_is_private_without_creating_managed_sync_state() -> anyhow::Result<()> {
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
    let root = tempfile::tempdir()?;
    let managed = root.path().join("Okilum/Sync");
    let direct = mac_direct_state(&managed)?;
    assert_eq!(direct, root.path().join("Okilum/SyncDirect"));
    assert!(!managed.exists());
    assert!(!direct.exists());
    let parent = managed.parent().unwrap();
    assert_eq!(std::fs::metadata(parent)?.mode() & 0o777, 0o700);
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o755))?;
    assert!(mac_direct_state(&managed).is_err());
    assert_eq!(std::fs::metadata(parent)?.mode() & 0o777, 0o755);
    let alias = root.path().join("linked");
    symlink(parent, &alias)?;
    assert!(mac_direct_state(&alias.join("Sync")).is_err());
    assert!(!managed.exists());
    Ok(())
}

#[test]
fn preparing_receive_continues_refresh_after_pairing_finishes() {
    let mut value = snapshot(None);
    assert!(!sync_refresh_needed(&value));
    value.preparing = true;
    assert!(sync_refresh_needed(&value));
    value.preparing = false;
    value.offer_url = Some("fixture-only invitation".into());
    assert!(sync_refresh_needed(&value));
    value.offer_url = None;
    assert!(!sync_refresh_needed(&value));
}

#[test]
fn malformed_pairing_is_rejected_before_locations_or_bootstrap() {
    let error = perform(
        None,
        Operation::Redeem("not a URL".into(), "New computer".into()),
    )
    .err()
    .expect("invalid URL must fail preflight");
    assert!(error.to_string().contains("URL"));
    let error = perform(None, Operation::Begin("bad\nname".into()))
        .err()
        .expect("invalid name must fail preflight");
    assert_eq!(error.to_string(), "invalid computer name");
}

#[gpui::test]
fn invalid_pasted_pairing_url_never_starts_controller(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| fixture(window, cx));
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<SyncSettings>().unwrap()
    });
    view.update_in(visual, |view, window, cx| {
        view.name
            .update(cx, |input, cx| input.set_value("New computer", window, cx));
        view.invitation
            .update(cx, |input, cx| input.set_value("not a URL", window, cx));
        view.start_pairing(true, cx);
        assert!(view.error.is_some());
        assert!(view.controller.is_none());
        assert!(!view.busy);
        assert_eq!(
            view.epoch.current(),
            0,
            "no background operation was dispatched"
        );
    });
}

#[test]
fn controller_opened_after_sync_suspend_is_revoked_before_publication() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let epoch = Epoch::default();
    let ticket = epoch.begin();
    // Suspend while open is in progress, before the view has any handle to stop.
    epoch.invalidate();
    let controller = Controller::open(root.path().join("direct"), vec![])?;
    let retained = controller.clone();
    assert!(
        retained.snapshot().is_ok(),
        "positive control: open is usable"
    );
    {
        let guard = ControllerGuard {
            ticket,
            controller: controller.clone(),
        };
        assert!(guard.check().is_err());
    }
    assert!(
        retained.snapshot().is_err(),
        "stale clones must lose access"
    );
    drop(retained);
    drop(controller);

    let fresh = epoch.begin();
    let controller = Controller::open(root.path().join("direct"), vec![])?;
    {
        let guard = ControllerGuard {
            ticket: fresh,
            controller: controller.clone(),
        };
        guard.check()?;
    }
    assert!(
        controller.snapshot().is_ok(),
        "fresh completion remains usable"
    );
    controller.shutdown();
    Ok(())
}
