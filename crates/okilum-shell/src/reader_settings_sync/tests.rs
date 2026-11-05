#[cfg(target_os = "linux")]
use super::linux_runtime;
use super::{
    destination, destination_name, mac_direct_state, perform, suspend, sync_refresh_interval,
    user_error, BackgroundSync, ControllerGuard, Epoch, Operation, Output, SyncSettings,
    TestWorker,
};
use crate::{
    reader_features, reader_history, reader_open, reader_settings, reader_startup, Opts, Reader,
};
use ::core::prelude::v1::test;
use gpui::{
    point, px, size, AppContext, ClickEvent, Context, Focusable, Modifiers, ScrollHandle,
    TestAppContext, VisualTestContext, Window,
};
use gpui_component::{input::InputState, Root};
use okilum_sync_controller::direct::{self, Controller, ExistingVault, Snapshot};
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::{path::Path, time::Duration};

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
        pending_destination: None,
        destination_error: None,
        receiving: false,
        scroll: ScrollHandle::new(),
        #[cfg(target_os = "linux")]
        conflicts: Default::default(),
    }
}

#[gpui::test]
fn issue2049_external_service_links_never_open_sync(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let directory = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        reader_features::install(directory.path(), cx);
        cx.set_global(reader_history::TestSessionDirectory(
            directory.path().to_owned(),
        ));
    });
    for url in [
        "okilum://sync/approve?request=private",
        "okilum://settings/sync?service=https://not-contacted.invalid",
        "okilum://enroll/private",
    ] {
        assert!(direct::validate_pairing_input(url, "Computer").is_err());
        cx.update(|cx| reader_open::dispatch_urls(vec![url.into()], cx));
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(
                !cx.has_global::<BackgroundSync>(),
                "URL must not mount Sync"
            );
            assert!(!reader_features::enabled(
                reader_features::Feature::Sync,
                cx
            ));
        });
    }
    // Positive control: ordinary note navigation remains a recognized URL.
    assert!(okilum_core::deep_link::parse("okilum://v/Notes/note.md").is_ok());
}

#[cfg(target_os = "linux")]
#[gpui::test]
fn issue2049_previous_connection_message_is_visible_in_settings(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let directory = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        reader_features::install(directory.path(), cx);
        reader_features::set_enabled(reader_features::Feature::Sync, true, cx);
        cx.set_global(TestWorker(std::sync::Arc::new(|_| {
            Err(super::legacy::PreviousConnection.into())
        })));
    });
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| SyncSettings::new(window, cx));
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<SyncSettings>().unwrap()
    });
    visual.run_until_parked();
    assert_visible(visual, "sync-error");
    view.update(visual, |view, _| {
        assert_eq!(view.error.as_deref(), Some(super::legacy::MESSAGE));
        assert!(view.controller.is_none());
    });
}

#[gpui::test]
fn settings_2487_escape_commits_computer_name_before_closing(cx: &mut TestAppContext) {
    settings_2487_close_focused_field(cx, false, false);
}

#[gpui::test]
fn settings_2487_close_button_commits_computer_name_before_closing(cx: &mut TestAppContext) {
    settings_2487_close_focused_field(cx, true, false);
}

#[gpui::test]
fn settings_2487_escape_commits_invitation_before_closing(cx: &mut TestAppContext) {
    settings_2487_close_focused_field(cx, false, true);
}

#[gpui::test]
fn settings_2487_close_button_commits_invitation_before_closing(cx: &mut TestAppContext) {
    settings_2487_close_focused_field(cx, true, true);
}

fn settings_2487_close_focused_field(cx: &mut TestAppContext, button: bool, invitation: bool) {
    let directory = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        reader_settings::install(cx);
        crate::reader_ui_state::install(directory.path(), cx);
        reader_features::install(directory.path(), cx);
    });
    let calls = guided_worker(cx, vec![snapshot(None)]);
    cx.update(|cx| reader_settings::show(None, cx));
    cx.run_until_parked();
    let handle = cx.update(|cx| cx.windows()[0]);
    let mut visual = VisualTestContext::from_window(handle, cx);
    if invitation {
        visual.simulate_resize(size(px(820.), px(800.)));
    }
    visual.update(|window, _| window.activate_window());
    visual.run_until_parked();
    let sync = visual.debug_bounds("settings-section-Sync").unwrap();
    visual.simulate_click(sync.center(), Modifiers::default());
    visual.run_until_parked();
    assert_eq!(*calls.lock().unwrap(), vec!["enter"]);
    let view = visual.update(|_, cx| cx.global::<BackgroundSync>().0.clone());

    let field = view.read_with(&visual, |view, _| {
        if invitation {
            view.invitation.clone()
        } else {
            view.name.clone()
        }
    });
    let committed = std::sync::Arc::new(std::sync::Mutex::new(None));
    let saved = committed.clone();
    view.update(&mut visual, |_, cx| {
        cx.subscribe(&field, move |_, input, event, cx| {
            if matches!(event, gpui_component::input::InputEvent::Blur) {
                assert!(
                    cx.windows().contains(&handle),
                    "the focused field must finish before the window is removed"
                );
                *saved.lock().unwrap() = Some(input.read(cx).value().to_string());
            }
        })
        .detach();
    });
    visual.update(|window, cx| {
        field.update(cx, |input, cx| {
            // The name field starts with this host's name; type a fresh value.
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
    });
    visual.run_until_parked();
    // Computer name starts with this host's name. Replace the existing value
    // through the real editing shortcut before testing commit-on-dismissal.
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a"
    } else {
        "ctrl-a"
    });
    visual.run_until_parked();
    visual.simulate_input("Desktop 2487");
    visual.update(|window, cx| {
        let input = field.read(cx);
        assert!(window.is_window_active(), "native focus events are active");
        assert!(input.focus_handle(cx).is_focused(window));
        assert_eq!(input.value().as_ref(), "Desktop 2487");
    });
    assert!(
        committed.lock().unwrap().is_none(),
        "blur has not fired yet"
    );
    if button {
        let close = visual
            .debug_bounds("settings-close")
            .expect("Settings has a visible Close button");
        assert!(close.size.width > px(0.) && close.size.height > px(0.));
        visual.simulate_click(close.center(), Modifiers::default());
    } else {
        visual.simulate_keystrokes("escape");
    }
    visual.run_until_parked();
    assert!(!cx.read(|cx| cx.windows().contains(&handle)));
    assert_eq!(committed.lock().unwrap().as_deref(), Some("Desktop 2487"));

    cx.update(|cx| reader_settings::show(None, cx));
    cx.run_until_parked();
    let reopened = cx.update(|cx| cx.windows()[0]);
    visual = VisualTestContext::from_window(reopened, cx);
    visual.run_until_parked();
    assert_visible(&mut visual, "sync-add-computer");
    visual.update(|_, cx| {
        let reopened = cx.global::<BackgroundSync>().0.read(cx);
        let field = if invitation {
            &reopened.invitation
        } else {
            &reopened.name
        };
        assert_eq!(field.read(cx).value().as_ref(), "Desktop 2487");
    });
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

#[test]
fn issue2589_mac_first_add_mints_url_without_runtime_or_approval() -> anyhow::Result<()> {
    use std::cell::Cell;
    let directory = tempfile::tempdir()?;
    let controller = Controller::open(directory.path().join("direct"), vec![])?;
    let called = Cell::new(false);
    for operation in [
        Operation::EnterPairing,
        Operation::Begin("First Mac".into()),
    ] {
        super::prepare_mac_runtime(&controller, &operation, |_| {
            called.set(true);
            anyhow::bail!("macOS Sync approval required")
        })?;
    }
    assert!(
        !called.get(),
        "Settings and Add computer cannot start a login item"
    );
    let invitation =
        controller.begin_pairing_at("First Mac", "127.0.0.1:0".parse()?, "127.0.0.1".parse()?)?;
    assert!(invitation.offer_url.is_some());
    let error = super::prepare_mac_runtime(
        &controller,
        &Operation::Redeem("private-url".into(), "Mac".into()),
        |_| {
            called.set(true);
            anyhow::bail!("macOS Sync approval required")
        },
    )
    .unwrap_err();
    assert!(
        called.get(),
        "receiving still requires authenticated runtime preparation"
    );
    assert!(user_error(&error).contains("Login Items"));
    assert!(user_error(&anyhow::anyhow!("Sync is disabled")).contains("Features"));
    Ok(())
}

#[gpui::test]
fn issue2589_action_failure_is_logged_with_chain_and_visible_cause(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let directory = tempfile::tempdir().unwrap();
    let trace = crate::reader_diagnostics::Trace::new(Some(directory.path().to_owned()), None);
    cx.update(|cx| {
        reader_features::install(directory.path(), cx);
        reader_features::set_enabled(reader_features::Feature::Sync, true, cx);
        cx.set_global(crate::reader_diagnostics::LaunchTrace(trace));
        cx.set_global(TestWorker(std::sync::Arc::new(|_| {
            Err(anyhow::Error::new(std::io::Error::from_raw_os_error(13))
                .context("cannot start LAN pairing listener"))
        })));
    });
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| fixture(window, cx));
        view.update(cx, |view, cx| {
            view.run(Operation::Begin("First Mac".into()), cx)
        });
        Root::new(view, window, cx)
    });
    visual.run_until_parked();
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<SyncSettings>().unwrap()
    });
    view.read_with(visual, |view, _| {
        assert!(view.error.as_deref().unwrap().contains("Permission denied"))
    });
    assert_visible(visual, "sync-error");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let log = std::fs::read_to_string(directory.path().join("reader-diagnostic.log"))
            .unwrap_or_default();
        if let Some(event) = log
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|event| event["phase"] == "sync_action_failed")
        {
            assert_eq!(event["details"]["action"], "add_computer");
            assert_eq!(event["details"]["causes"].as_array().unwrap().len(), 2);
            assert!(event["details"]["error"]
                .as_str()
                .unwrap()
                .contains("os error 13"));
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "sync_action_failed must reach the app log"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
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
    // #2589: the platform-created 0755 app folder is tightened, not rejected.
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o755))?;
    assert_eq!(
        mac_direct_state(&managed)?,
        root.path().join("Okilum/SyncDirect")
    );
    assert_eq!(std::fs::metadata(parent)?.mode() & 0o777, 0o700);
    let alias = root.path().join("linked");
    symlink(parent, &alias)?;
    assert!(mac_direct_state(&alias.join("Sync")).is_err());
    assert!(!managed.exists());
    Ok(())
}

#[test]
fn preparing_receive_continues_refresh_after_pairing_finishes() {
    let mut value = snapshot(None);
    assert!(!sync_refresh_interval(&value).is_some());
    value.preparing = true;
    assert!(sync_refresh_interval(&value).is_some());
    value.preparing = false;
    value.offer_url = Some("fixture-only invitation".into());
    assert!(sync_refresh_interval(&value).is_some());
    value.offer_url = None;
    assert!(!sync_refresh_interval(&value).is_some());
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
                connection_error: None,
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
                    managed: true,
                    selected: true,
                    network: None,
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
    let viewport = visual.update(|window, _| window.viewport_size());
    assert!(
        panel.top() >= px(0.)
            && panel.left() >= px(0.)
            && panel.bottom() <= viewport.height
            && panel.right() <= viewport.width,
        "panel {panel:?} exceeds window viewport {viewport:?}"
    );
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
    visual.simulate_resize(size(px(700.), px(440.)));
    visual.run_until_parked();
    assert_eq!(
        visual.update(|window, _| window.viewport_size()),
        size(px(700.), px(440.)),
        "positive control: minimum viewport resize reached GPUI layout"
    );
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
    visual.executor().advance_clock(Duration::from_secs(1));
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
    // Completed pairing returns to the normal interval.
    visual.executor().advance_clock(Duration::from_secs(2));
    visual.run_until_parked();
    assert_eq!(calls.lock().unwrap().len(), 2);
    visual.executor().advance_clock(Duration::from_secs(28));
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
    let vault = directory.path().join("local-replica");
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
        assert_eq!(
            super::super::vault_name::shown(&vault, cx),
            "Received notes"
        );
        assert!(reader_startup::recent_label(&vault, cx).starts_with("Received notes — "));
        assert!(
            !vault.join(".okilum").exists(),
            "display names never modify received files"
        );
    });
    let handle = cx.update(|cx| {
        reader_open::reader_of(&vault, cx)
            .unwrap()
            .upgrade()
            .unwrap()
            .read(cx)
            .reader_window
    });
    assert!(gpui::VisualTestContext::from_window(handle, cx)
        .window_title()
        .unwrap()
        .ends_with(" — Received notes"));
    assert_eq!(*calls.lock().unwrap(), vec!["enter", "other", "read"]);
}

#[gpui::test]
fn guided_first_run_folder_picker_survives_refresh_and_revokes_on_disable(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let directory = tempfile::tempdir().unwrap();
    let peer = uuid::Uuid::new_v4();
    let mut ready = snapshot(None);
    ready.peers.push(direct::Peer {
        id: peer,
        name: "Desktop".into(),
        device_id: "device".into(),
    });
    ready.catalog.push(direct::Vault {
        id: "notes".into(),
        label: "Work notes".into(),
    });
    let calls = guided_worker(cx, vec![ready]);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = fixture(window, cx);
            view.run(Operation::Read, cx);
            view
        });
        cx.set_global(BackgroundSync(view.clone()));
        Root::new(view, window, cx)
    });
    visual.run_until_parked();
    let pick = visual.debug_bounds("sync-new-vault-folder-0").unwrap();
    visual.simulate_click(pick.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual.did_prompt_for_new_path(),
        "positive control: real folder dialog opened"
    );
    visual.executor().advance_clock(Duration::from_secs(30));
    visual.run_until_parked();
    visual.simulate_new_path_selection(|_| Some(directory.path().join("Work notes")));
    visual.run_until_parked();
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["read", "read", "other"],
        "background refresh must not discard the accepted local folder"
    );
    let pick = visual.debug_bounds("sync-new-vault-folder-0").unwrap();
    visual.simulate_click(pick.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.did_prompt_for_new_path());
    visual.update(|_, cx| reader_features::set_enabled(reader_features::Feature::Sync, false, cx));
    visual.simulate_new_path_selection(|_| Some(directory.path().join("Late choice")));
    visual.run_until_parked();
    assert_eq!(
        calls.lock().unwrap().len(),
        3,
        "disabled Sync must ignore a late dialog answer"
    );
}

#[cfg(target_os = "linux")]
#[path = "labels_tests.rs"]
mod labels_tests;

#[cfg(target_os = "linux")]
fn network_choice(managed: bool, local: bool) -> linux_runtime::Choice {
    linux_runtime::Choice {
        identity: okilum_sync_controller::daemon::DaemonIdentity {
            config_file: PathBuf::from("/fixture/config.xml"),
            device_id: "fixture-device".into(),
            certificate_hash: "fixture-certificate".into(),
        },
        folders: 2,
        managed,
        selected: true,
        network: Some(okilum_sync_controller::network_policy::State {
            global_discovery: !local,
            relays: !local,
            nat: !local,
            stun: !local,
        }),
    }
}

#[cfg(target_os = "linux")]
#[gpui::test]
fn sync_network_switch_dispatches_selected_identity_and_publishes_readback(
    cx: &mut TestAppContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let controller = Controller::open(directory.path().join("direct"), vec![]).unwrap();
    let calls = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let recorded = calls.clone();
    cx.update(|cx| {
        crate::test_init(cx);
        reader_features::install(directory.path(), cx);
        reader_features::set_enabled(reader_features::Feature::Sync, true, cx);
        cx.set_global(TestWorker(std::sync::Arc::new(move |operation| {
            let Operation::Network(identity, policy) = operation else {
                panic!("unexpected operation")
            };
            assert_eq!(identity, network_choice(true, false).identity);
            recorded.lock().unwrap().push(policy);
            let local =
                policy == okilum_sync_controller::network_policy::Policy::LocalNetworkAndVpn;
            Ok(Output {
                connection_error: None,
                controller: controller.clone(),
                snapshot: snapshot(None),
                inspected: None,
                selected_destination: None,
                runtimes: vec![network_choice(true, local)],
                vaults: vec![],
            })
        })));
    });
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = fixture(window, cx);
            view.runtimes = vec![network_choice(true, false)];
            view
        });
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<SyncSettings>().unwrap()
    });
    for local in [true, false] {
        visual.run_until_parked();
        let switch = visual
            .debug_bounds("sync-network-local-only")
            .expect("managed switch");
        visual.simulate_click(switch.center(), Modifiers::default());
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert!(!view.busy);
            assert_eq!(
                view.runtimes[0].network.as_ref().unwrap().local_only(),
                local
            );
        });
    }
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            okilum_sync_controller::network_policy::Policy::LocalNetworkAndVpn,
            okilum_sync_controller::network_policy::Policy::PublicDiscoveryAndRelays,
        ]
    );
}

#[cfg(target_os = "linux")]
#[gpui::test]
fn sync_network_external_connection_has_no_mutating_switch(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = fixture(window, cx);
            view.runtimes = vec![network_choice(false, false)];
            view
        });
        Root::new(view, window, cx)
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("sync-network-local-only").is_none());
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<SyncSettings>().unwrap()
    });
    view.update(visual, |view, cx| {
        view.runtimes[0].managed = true;
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("sync-network-local-only").is_some(),
        "positive control: same rendered connection offers switch only when managed"
    );
}

#[gpui::test]
fn sync_new_destination_picker_answer_during_refresh_is_not_dropped(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let root_dir = tempfile::tempdir().unwrap();
    let peer = uuid::Uuid::new_v4();
    let calls = guided_worker(cx, vec![snapshot(None)]);
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| fixture(window, cx));
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<SyncSettings>().unwrap()
    });
    view.update(visual, |view, cx| {
        view.choose_new_folder(peer, "notes".into(), "Notes", cx);
        // The periodic Read can be in flight when the native dialog answers.
        view.busy = true;
    });
    assert!(visual.did_prompt_for_new_path(), "production picker opened");
    visual.simulate_new_path_selection(|_| Some(root_dir.path().join("Notes")));
    visual.run_until_parked();
    assert!(
        calls.lock().unwrap().is_empty(),
        "selection waits for current IO"
    );
    view.update(visual, |view, cx| {
        view.busy = false;
        view.run(Operation::Read, cx);
    });
    visual.run_until_parked();
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["read", "other"],
        "completion of Read must dispatch the accepted destination exactly once"
    );
    visual.update(|_, cx| cx.set_global(BackgroundSync(view.clone())));
    view.update(visual, |view, cx| {
        view.choose_new_folder(peer, "notes".into(), "Notes", cx);
        view.busy = true;
    });
    assert!(visual.did_prompt_for_new_path());
    visual.simulate_new_path_selection(|_| Some(root_dir.path().join("Revoked notes")));
    visual.run_until_parked();
    view.read_with(visual, |view, _| {
        assert!(view.pending_destination.is_some())
    });
    visual.update(|_, cx| reader_features::set_enabled(reader_features::Feature::Sync, false, cx));
    view.read_with(visual, |view, _| {
        assert!(view.pending_destination.is_none())
    });
    assert_eq!(*calls.lock().unwrap(), vec!["read", "other"]);
    assert!(!root_dir.path().join("Revoked notes").exists());
}

#[gpui::test]
fn sync_new_destination_failure_is_visible_beside_vault_after_refresh(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let directory = tempfile::tempdir().unwrap();
    let peer = uuid::Uuid::new_v4();
    let mut ready = snapshot(None);
    ready.peers.push(direct::Peer {
        id: peer,
        name: "Source".into(),
        device_id: "source".into(),
    });
    ready.catalog.push(direct::Vault {
        id: "notes".into(),
        label: "Notes".into(),
    });
    let controller = Controller::open(directory.path().join("state"), vec![]).unwrap();
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    cx.update(|cx| {
        let calls = calls.clone();
        cx.set_global(TestWorker(std::sync::Arc::new(move |operation| {
            if let Operation::AttachNew(_, _, path) = operation {
                calls.lock().unwrap().push("attach");
                destination::prepare(&path)?;
                anyhow::bail!("positive control: invalid destination unexpectedly accepted");
            }
            calls.lock().unwrap().push("read");
            Ok(Output {
                connection_error: None,
                controller: controller.clone(),
                snapshot: ready.clone(),
                inspected: None,
                selected_destination: None,
                #[cfg(target_os = "linux")]
                runtimes: vec![],
                #[cfg(target_os = "linux")]
                vaults: vec![],
            })
        })));
    });
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = fixture(window, cx);
            view.run(Operation::Read, cx);
            view
        });
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<SyncSettings>().unwrap()
    });
    visual.run_until_parked();
    let pick = visual.debug_bounds("sync-new-vault-folder-0").unwrap();
    visual.simulate_click(pick.center(), Modifiers::default());
    assert!(visual.did_prompt_for_new_path());
    visual.simulate_new_path_selection(|_| Some(directory.path().join("absent/Notes")));
    visual.run_until_parked();
    assert_eq!(*calls.lock().unwrap(), vec!["read", "attach"]);
    assert!(visual.debug_bounds("sync-destination-error-0").is_some());
    view.read_with(visual, |view, _| {
        assert!(view.error.as_ref().unwrap().contains("local folder"));
        assert!(!view.busy);
        assert!(view.awaiting_open.is_none());
    });
    view.update(visual, |view, cx| view.run(Operation::Read, cx));
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("sync-destination-error-0").is_some(),
        "refresh preserves actionable failure"
    );
    assert!(!directory.path().join("absent").exists());
}

#[gpui::test]
fn sync_2419_sidebar_is_visible_while_off_and_first_click_enables_only_sync(
    cx: &mut TestAppContext,
) {
    let directory = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        reader_features::install(directory.path(), cx);
    });
    let calls = guided_worker(cx, vec![snapshot(None)]);
    cx.update(|cx| reader_settings::show(None, cx));
    cx.run_until_parked();
    let settings = cx.update(|cx| cx.windows()[0]);
    let mut visual = VisualTestContext::from_window(settings, cx);
    visual.run_until_parked();
    visual.update(|_, cx| {
        assert!(!reader_features::enabled(
            reader_features::Feature::Sync,
            cx
        ))
    });
    assert!(
        calls.lock().unwrap().is_empty(),
        "visibility alone grants no network access"
    );
    let entry = visual
        .debug_bounds("settings-section-Sync")
        .expect("Sync discoverable with module off");
    visual.simulate_click(entry.center(), Modifiers::default());
    visual.run_until_parked();
    assert_visible(&mut visual, "sync-add-computer");
    visual.update(|_, cx| {
        assert!(reader_features::enabled(reader_features::Feature::Sync, cx));
        assert!(!reader_features::enabled(
            reader_features::Feature::Agents,
            cx
        ));
        assert!(!reader_features::enabled(
            reader_features::Feature::SecondBrain,
            cx
        ));
        reader_features::install(directory.path(), cx);
        assert!(
            reader_features::enabled(reader_features::Feature::Sync, cx),
            "choice persisted"
        );
    });
    assert_eq!(*calls.lock().unwrap(), vec!["enter"]);
}

#[gpui::test]
fn sync_2419_preparing_shows_setup_and_existing_incompatible_connection_shows_error(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = fixture(window, cx);
            view.busy = true;
            view
        });
        Root::new(view, window, cx)
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("sync-pending").is_some());
    assert!(visual.debug_bounds("sync-setup").is_some());
    assert!(visual.debug_bounds("sync-error").is_none());
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<SyncSettings>().unwrap()
    });
    visual.update(|_, cx| {
        cx.set_global(TestWorker(std::sync::Arc::new(|_| {
            anyhow::bail!("unsupported Syncthing version")
        })))
    });
    view.update(visual, |view, cx| {
        view.busy = false;
        view.run(Operation::EnterPairing, cx);
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("sync-error").is_some(),
        "positive control: incompatible existing Syncthing reports a real error"
    );
    view.read_with(visual, |view, _| {
        assert!(view
            .error
            .as_ref()
            .unwrap()
            .starts_with("No compatible Syncthing"))
    });
}

#[test]
fn sync_2419_pairing_cadence_is_fast_and_idle_returns_to_normal() {
    let mut value = snapshot(None);
    assert_eq!(sync_refresh_interval(&value), None);
    value.offer_url = Some("fixture".into());
    assert_eq!(sync_refresh_interval(&value), Some(Duration::from_secs(1)));
    value.offer_url = None;
    value.pending = Some(direct::Pending {
        request_id: uuid::Uuid::new_v4(),
        incoming: false,
        name: "Laptop".into(),
        device_id: "device".into(),
        fingerprint: "key".into(),
    });
    assert_eq!(sync_refresh_interval(&value), Some(Duration::from_secs(1)));
    value.pending = None;
    value.peers.push(direct::Peer {
        id: uuid::Uuid::new_v4(),
        name: "Laptop".into(),
        device_id: "device".into(),
    });
    assert_eq!(sync_refresh_interval(&value), Some(Duration::from_secs(30)));
    value.preparing = true;
    assert_eq!(sync_refresh_interval(&value), Some(Duration::from_secs(2)));
}

#[gpui::test]
fn sync_2419_share_menu_opens_sync_with_module_off(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        reader_features::install(directory.path(), cx);
    });
    let calls = guided_worker(cx, vec![snapshot(None)]);
    let (root, visual) = cx.add_window_view(|window, cx| {
        let reader = cx.new(|cx| Reader::new(Opts::default(), window, cx));
        Root::new(reader, window, cx)
    });
    let reader = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<Reader>().unwrap()
    });
    visual.update(|window, cx| {
        assert!(!reader_features::enabled(
            reader_features::Feature::Sync,
            cx
        ));
        let gpui_component::menu::PopupMenuItem::Item {
            label,
            handler: Some(handler),
            disabled,
            ..
        } = reader_settings::share_sync_menu_item(reader.downgrade())
        else {
            panic!("ordinary actionable menu entry required")
        };
        assert_eq!(label.as_ref(), "Share with another computer…");
        assert!(!disabled);
        handler(&ClickEvent::default(), window, cx);
    });
    visual.run_until_parked();
    let settings = visual.update(|window, cx| {
        assert!(reader_features::enabled(reader_features::Feature::Sync, cx));
        cx.windows()
            .into_iter()
            .find(|w| *w != window.window_handle())
            .unwrap()
    });
    let mut settings = VisualTestContext::from_window(settings, &*visual);
    settings.run_until_parked();
    assert_visible(&mut settings, "sync-add-computer");
    assert_eq!(*calls.lock().unwrap(), vec!["enter"]);
}

#[gpui::test]
fn sync_2419_first_use_keeps_sync_off_when_preferences_cannot_be_saved(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        reader_features::install(directory.path(), cx);
    });
    std::fs::create_dir(directory.path().join("features.json")).unwrap();
    let calls = guided_worker(cx, vec![snapshot(None)]);
    cx.update(|cx| reader_settings::show(None, cx));
    cx.run_until_parked();
    let settings = cx.update(|cx| cx.windows()[0]);
    let mut visual = VisualTestContext::from_window(settings, cx);
    visual.run_until_parked();
    let entry = visual.debug_bounds("settings-section-Sync").unwrap();
    visual.simulate_click(entry.center(), Modifiers::default());
    visual.run_until_parked();
    visual.update(|_, cx| {
        assert!(!reader_features::enabled(
            reader_features::Feature::Sync,
            cx
        ));
        assert!(reader_features::error(cx).is_some());
    });
    assert!(
        visual.debug_bounds("settings-feature-sync").is_some(),
        "failed enable opens Features with the save error"
    );
    assert!(
        calls.lock().unwrap().is_empty(),
        "failed consent must not reach the controller"
    );
}

#[cfg(target_os = "linux")]
#[gpui::test]
fn sync_2419_real_pairing_timer_skips_full_runtime_inspection(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let directory = tempfile::tempdir().unwrap();
    let prepared = directory.path().join("prepared");
    std::fs::create_dir(&prepared).unwrap();
    std::fs::write(prepared.join("prepared.json"), b"invalid runtime receipt").unwrap();
    let controller = Controller::open(directory.path().join("direct"), vec![]).unwrap();
    let offer = controller
        .begin_pairing_at(
            "Desktop",
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();
    controller
        .set_configurations(vec![prepared.join("syncthing/config.xml")])
        .unwrap();
    assert!(
        controller.refresh().is_err(),
        "positive control: full runtime inspection detects the invalid receipt"
    );
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = fixture(window, cx);
            view.controller = Some(controller.clone());
            view.snapshot = Some(offer);
            view.schedule_refresh(cx);
            view
        });
        Root::new(view, window, cx)
    });
    visual.run_until_parked();
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<SyncSettings>().unwrap()
    });
    visual.executor().advance_clock(Duration::from_millis(999));
    visual.run_until_parked();
    view.read_with(visual, |view, _| assert_eq!(view.epoch.current(), 0));
    visual.executor().advance_clock(Duration::from_millis(1));
    visual.run_until_parked();
    view.read_with(visual, |view, _| {
        assert_eq!(view.epoch.current(), 1, "timer really dispatched a worker");
        assert!(!view.busy, "real worker completed");
        assert!(
            view.error.is_none(),
            "pairing must not wait for unrelated runtime inspection"
        );
        assert!(view.snapshot.as_ref().unwrap().offer_url.is_some());
    });
    assert_visible(visual, "sync-copy-url");
    controller.shutdown();
}
