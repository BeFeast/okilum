use super::*;
use ::core::prelude::v1::test;

fn fixture(window: &mut Window, cx: &mut Context<SyncSettings>) -> SyncSettings {
    SyncSettings {
        #[cfg(target_os = "linux")]
        runtimes: vec![],
        #[cfg(target_os = "linux")]
        vaults: vec![],
        controller: None,
        snapshot: None,
        inspected: None,
        name: cx.new(|cx| InputState::new(window, cx).placeholder("This computer’s name")),
        invitation: cx.new(|cx| InputState::new(window, cx).placeholder("Paste pairing URL")),
        selected_peer: None,
        busy: false,
        epoch: Epoch::default(),
        lifetime: Epoch::default(),
        error: None,
        awaiting_open: None,
        receiving: false,
        scroll: ScrollHandle::new(),
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
        #[cfg(target_os = "linux")]
        shared: vec![],
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

#[cfg(target_os = "linux")]
#[gpui::test]
fn reader_settings_sync_lists_an_ordinary_vault_and_shared_state(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let path = PathBuf::from("/ordinary-vault");
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = fixture(window, cx);
            view.vaults = vec![path.clone()];
            view.snapshot = Some(snapshot(None));
            view
        });
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<SyncSettings>().unwrap()
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("sync-share-vault-0").is_some(),
        "ordinary vault action missing"
    );
    assert!(
        visual.debug_bounds("sync-inspect-existing").is_some(),
        "existing replica action missing"
    );
    view.update(visual, |view, cx| {
        view.snapshot.as_mut().unwrap().shared.push(path.clone());
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("sync-share-vault-0").is_some(),
        "shared state disappeared"
    );
    view.read_with(visual, |view, _| {
        assert!(view.snapshot.as_ref().unwrap().shared.contains(&path));
        assert!(!view.busy);
    });
}

// Replace only the blocking platform/controller boundary. Clicks, completion
// publication, timer scheduling, feature consent and Settings layout are real.
fn guided_worker(
    cx: &mut TestAppContext,
    states: Vec<Snapshot>,
) -> std::sync::Arc<std::sync::Mutex<Vec<&'static str>>> {
    let directory = tempfile::tempdir().unwrap();
    let controller = Controller::open(directory.path().join("direct"), vec![]).unwrap();
    let states = std::sync::Mutex::new(std::collections::VecDeque::from(states));
    let calls = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let recorded = calls.clone();
    cx.update(|cx| {
        cx.set_global(TestWorker(std::sync::Arc::new(move |operation| {
            let _keep_directory = &directory;
            let selected_destination = match &operation {
                Operation::Attach(_, _, path) | Operation::AttachNew(_, _, path) => {
                    Some(path.clone())
                }
                _ => None,
            };
            let tag = match operation {
                Operation::EnterPairing => "enter",
                Operation::Read => "read",
                Operation::Begin(_) => "offer",
                Operation::Redeem(_, _) => "connect",
                Operation::Approve(_) => "approve",
                _ => "other",
            };
            recorded.lock().unwrap().push(tag);
            let mut states = states.lock().unwrap();
            let snapshot = if states.len() > 1 {
                states.pop_front().unwrap()
            } else {
                states.front().unwrap().clone()
            };
            Ok(Output {
                controller: controller.clone(),
                snapshot,
                inspected: None,
                selected_destination,
                #[cfg(target_os = "linux")]
                runtimes: vec![linux_runtime::Choice {
                    identity: okilum_sync_controller::daemon::DaemonIdentity {
                        config_file: PathBuf::from("/fixture-managed/config.xml"),
                        device_id: "fixture-device".into(),
                        certificate_hash: "fixture-certificate".into(),
                    },
                    folders: 0,
                    selected: true,
                }],
                #[cfg(target_os = "linux")]
                vaults: (0..12)
                    .map(|i| PathBuf::from(format!("/fixture-vault-{i}")))
                    .collect(),
            })
        })))
    });
    calls
}

fn assert_visible(visual: &mut VisualTestContext, selector: &'static str) {
    let panel = visual
        .debug_bounds("sync-panel")
        .expect("Sync panel is rendered");
    let element = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"));
    assert!(
        element.size.height > px(0.) && element.size.width > px(0.),
        "empty {selector}"
    );
    assert!(
        element.top() >= panel.top()
            && element.bottom() <= panel.bottom()
            && element.left() >= panel.left()
            && element.right() <= panel.right(),
        "{selector} {element:?} outside panel {panel:?}"
    );
}

#[gpui::test]
fn guided_first_run_welcome_opens_pairing_and_enables_sync(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("state");
    std::fs::create_dir(&state).unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        reader_open::install(cx);
        reader_features::install(&state, cx);
        cx.set_global(reader_history::TestSessionDirectory(state.clone()));
    });
    let calls = guided_worker(cx, vec![snapshot(None)]);
    cx.update(|cx| {
        assert!(!reader_features::enabled(
            reader_features::Feature::Sync,
            cx
        ));
        reader_startup::launch(
            Opts {
                session_directory: Some(state.clone()),
                ..Default::default()
            },
            cx,
        );
    });
    cx.run_until_parked();
    let entry = cx.update(|cx| {
        assert_eq!(cx.windows().len(), 1);
        cx.windows()[0]
    });
    let mut visual = VisualTestContext::from_window(entry, cx);
    visual.run_until_parked();
    let button = visual
        .debug_bounds("startup-sync-pairing")
        .expect("welcome entry");
    visual.simulate_click(button.center(), Modifiers::default());
    visual.run_until_parked();
    let settings = visual.update(|_, cx| {
        assert!(reader_features::enabled(reader_features::Feature::Sync, cx));
        assert!(!reader_features::enabled(
            reader_features::Feature::Agents,
            cx
        ));
        assert!(reader_open::readers(cx).is_empty(), "no dummy vault needed");
        cx.windows()
            .into_iter()
            .find(|w| *w != entry)
            .expect("Settings window")
    });
    let mut settings = VisualTestContext::from_window(settings, &visual);
    settings.run_until_parked();
    assert_visible(&mut settings, "sync-connect");
    assert_visible(&mut settings, "sync-status");
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["enter"],
        "entry dispatches preparation without Set up or Refresh"
    );
    settings.update(|_, cx| {
        let view = &cx.global::<BackgroundSync>().0;
        assert!(view.read(cx).receiving);
        #[cfg(target_os = "linux")]
        assert!(
            view.read(cx).runtimes[0].selected,
            "setup result published immediately"
        );
    });
}

#[gpui::test]
fn guided_first_run_offer_incoming_confirmation_update_without_refresh_and_stay_visible(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let mut offer = snapshot(None);
    offer.status = "Waiting for the new computer".into();
    offer.offer_url = Some(format!("okilum://pair/{}", "fixture".repeat(60)));
    let id = uuid::Uuid::new_v4();
    let mut incoming = offer.clone();
    incoming.pending = Some(direct::Pending {
        request_id: id,
        incoming: true,
        name: "Laptop".into(),
        device_id: "A".repeat(63),
        fingerprint: "B".repeat(64),
    });
    let mut approved = snapshot(None);
    approved.status = "Ready".into();
    approved.peers.push(direct::Peer {
        id,
        name: "Laptop".into(),
        device_id: "device".into(),
    });
    let calls = guided_worker(cx, vec![snapshot(None), offer, incoming, approved]);
    // The real Settings window uses its minimum supported 700x440 viewport below.
    let owner = cx.add_window(|_, _| gpui::Empty);
    owner
        .update(cx, |_, window, cx| {
            reader_settings::show_sync_pairing(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    let settings = cx.update(|cx| {
        cx.windows()
            .into_iter()
            .find(|w| *w != owner.into())
            .unwrap()
    });
    let mut visual = VisualTestContext::from_window(settings, cx);
    visual.update(|window, _| window.resize(size(px(700.), px(440.))));
    visual.run_until_parked();
    let view = visual.update(|_, cx| cx.global::<BackgroundSync>().0.clone());
    view.update_in(&mut visual, |view, window, cx| {
        view.receiving = false;
        view.name
            .update(cx, |input, cx| input.set_value("Desktop", window, cx));
        cx.notify();
    });
    visual.run_until_parked();
    let add = visual.debug_bounds("sync-add-computer").unwrap();
    visual.simulate_click(add.center(), Modifiers::default());
    visual.run_until_parked();
    assert_visible(&mut visual, "sync-copy-url");
    assert_visible(&mut visual, "sync-pairing-url");
    view.update(&mut visual, |view, cx| {
        view.scroll.set_offset(point(px(0.), px(-1000.)));
        cx.notify();
    });
    visual.executor().advance_clock(Duration::from_secs(2));
    visual.run_until_parked();
    assert_visible(&mut visual, "sync-approve-computer");
    assert!(
        visual.debug_bounds("sync-copy-url").is_none(),
        "incoming replaces offer, with positive approval control above"
    );
    let confirm = visual.debug_bounds("sync-approve-computer").unwrap();
    visual.simulate_click(confirm.center(), Modifiers::default());
    visual.run_until_parked();
    assert_visible(&mut visual, "sync-status");
    assert_visible(&mut visual, "sync-catalog-0");
    assert!(visual.debug_bounds("sync-approve-computer").is_none());
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["enter", "offer", "read", "approve"]
    );
    // Errors are pinned even when the long list has been scrolled to its end.
    view.update(&mut visual, |view, cx| {
        view.scroll.set_offset(point(px(0.), px(-1000.)));
        view.error = Some("Could not connect. Check both computers and try again.".into());
        cx.notify();
    });
    visual.run_until_parked();
    assert_visible(&mut visual, "sync-error");
}

#[gpui::test]
fn guided_first_run_waiting_then_catalog_arrives_from_timer(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let id = uuid::Uuid::new_v4();
    let waiting = snapshot(Some(direct::Pending {
        request_id: id,
        incoming: false,
        name: "Laptop".into(),
        device_id: "device".into(),
        fingerprint: "key".into(),
    }));
    let mut ready = snapshot(None);
    ready.status = "Ready".into();
    ready.peers.push(direct::Peer {
        id,
        name: "Desktop".into(),
        device_id: "device".into(),
    });
    ready.catalog.push(direct::Vault {
        id: "notes".into(),
        label: "Work notes".into(),
    });
    let calls = guided_worker(cx, vec![waiting, ready]);
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = fixture(window, cx);
            view.run(Operation::Read, cx);
            view
        });
        Root::new(view, window, cx)
    });
    visual.run_until_parked();
    assert_visible(visual, "sync-cancel-request");
    assert!(visual.debug_bounds("sync-authorized-catalog").is_none());
    visual.executor().advance_clock(Duration::from_secs(2));
    visual.run_until_parked();
    assert_visible(visual, "sync-authorized-catalog");
    assert_visible(visual, "sync-new-vault-folder-0");
    root.read_with(visual, |root, cx| {
        let view = root.view().clone().downcast::<SyncSettings>().unwrap();
        assert_eq!(view.read(cx).selected_peer, Some(id));
    });
    assert_eq!(*calls.lock().unwrap(), vec!["read", "read"]);
    // Polling survives completed confirmation; later changes need no Refresh.
    visual.executor().advance_clock(Duration::from_secs(2));
    visual.run_until_parked();
    assert_eq!(calls.lock().unwrap().len(), 3);
}

#[test]
fn guided_first_run_destination_proposal_is_one_safe_home_component() {
    assert_eq!(destination_name("Work notes"), "Work notes");
    assert_eq!(destination_name("../../Work\\notes\n"), "Worknotes");
    assert_eq!(destination_name(".."), "Notes");
    assert_eq!(
        Path::new(&destination_name("/absolute/path"))
            .components()
            .count(),
        1
    );
}

#[gpui::test]
fn guided_first_run_received_vault_opens_only_after_readiness_and_keeps_owner_alive(
    cx: &mut TestAppContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("state");
    let vault = directory.path().join("Received notes");
    std::fs::create_dir(&state).unwrap();
    std::fs::create_dir(&vault).unwrap();
    std::fs::write(vault.join("First.md"), "# Received note").unwrap();
    let vault = vault.canonicalize().unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        reader_open::install(cx);
        cx.set_global(reader_history::TestSessionDirectory(state.clone()));
    });
    let mut receiving = snapshot(None);
    receiving.preparing = true;
    receiving.attached.push(ExistingVault {
        daemon: okilum_sync_controller::daemon::DaemonIdentity {
            config_file: directory.path().join("config.xml"),
            device_id: "fixture-device".into(),
            certificate_hash: "fixture-certificate".into(),
        },
        version: "v2.1.6".into(),
        folder_id: "fixture-vault".into(),
        path: vault.clone(),
        label: "Received notes".into(),
        paused: false,
        folder_type: "receiveonly".into(),
        peers: vec![],
        root_identity: None,
    });
    let mut ready = receiving.clone();
    ready.preparing = false;
    ready.attached[0].folder_type = "sendreceive".into();
    let calls = guided_worker(cx, vec![snapshot(None), receiving, ready]);
    cx.update(|cx| {
        reader_startup::launch(
            Opts {
                session_directory: Some(state),
                index_dir: Some(directory.path().join("index")),
                ..Default::default()
            },
            cx,
        )
    });
    cx.run_until_parked();
    let entry = cx.update(|cx| cx.windows()[0]);
    let mut visual = VisualTestContext::from_window(entry, cx);
    visual.run_until_parked();
    let button = visual.debug_bounds("startup-sync-pairing").unwrap();
    visual.simulate_click(button.center(), Modifiers::default());
    visual.run_until_parked();
    let view = visual.update(|_, cx| cx.global::<BackgroundSync>().0.clone());
    view.update(&mut visual, |view, cx| {
        view.run(
            Operation::Attach(uuid::Uuid::new_v4(), "fixture-vault".into(), vault.clone()),
            cx,
        )
    });
    visual.run_until_parked();
    visual.update(|_, cx| {
        assert!(
            reader_open::readers(cx).is_empty(),
            "receiveonly is not ready to open"
        );
        assert!(
            cx.windows().contains(&entry),
            "Settings alone never retires welcome"
        );
    });
    visual.executor().advance_clock(Duration::from_secs(2));
    visual.run_until_parked();
    cx.update(|cx| {
        assert!(
            reader_open::reader_of(&vault, cx).is_some(),
            "ready vault opened automatically"
        );
        assert!(
            !cx.windows().contains(&entry),
            "only the real reader replaces welcome"
        );
    });
    assert_eq!(*calls.lock().unwrap(), vec!["enter", "other", "read"]);
}
