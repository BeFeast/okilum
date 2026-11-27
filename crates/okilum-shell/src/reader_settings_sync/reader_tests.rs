use crate::{
    reader_features, reader_history, reader_open, reader_settings, reader_startup, Opts, Reader,
};
use ::core::prelude::v1::test;
use gpui::{
    point, px, size, AppContext, ClickEvent, Focusable, Modifiers, TestAppContext,
    VisualTestContext,
};
use gpui_component::{Root, WindowExt};
use okilum_shell_sync::reader_settings_sync::{
    test_support::{
        assert_visible, background_sync, draw_sync_fixture, fixture, guided_worker,
        has_background_sync, install_error_worker, snapshot, Step,
    },
    SyncSettings,
};
use okilum_sync_controller::direct::{self, ExistingVault};
use std::path::Path;
use std::time::Duration;

fn sync_fixture_init(cx: &mut gpui::App) {
    crate::test_init(cx);
    cx.set_reduce_motion(true);
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
            assert!(!has_background_sync(cx), "URL must not mount Sync");
            assert!(!reader_features::enabled(
                reader_features::Feature::Sync,
                cx
            ));
        });
    }
    // Positive control: ordinary note navigation remains a recognized URL.
    assert!(okilum_core::deep_link::parse("okilum://v/Notes/note.md").is_ok());
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
        sync_fixture_init(cx);
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
    let view = visual.update(|_, cx| background_sync(cx));

    view.update(&mut visual, |view, cx| {
        view.test_set_wizard(Some(if invitation { Step::Paste } else { Step::Start }));
        cx.notify();
    });
    visual.run_until_parked();
    let field = view.read_with(&visual, |view, _| {
        if invitation {
            view.test_invitation().unwrap()
        } else {
            view.test_name().unwrap()
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
        visual.run_until_parked();
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
        let reopened = background_sync(cx);
        let reopened = reopened.read(cx);
        let field = if invitation {
            reopened.test_invitation().unwrap()
        } else {
            reopened.test_name().unwrap()
        };
        assert_eq!(field.read(cx).value().as_ref(), "Desktop 2487");
    });
}

#[gpui::test]
fn issue2589_action_failure_is_logged_with_chain_and_visible_cause(cx: &mut TestAppContext) {
    cx.update(sync_fixture_init);
    let directory = tempfile::tempdir().unwrap();
    let trace = crate::reader_diagnostics::Trace::new(Some(directory.path().to_owned()), None);
    cx.update(|cx| {
        reader_features::install(directory.path(), cx);
        reader_features::set_enabled(reader_features::Feature::Sync, true, cx);
        cx.set_global(crate::reader_diagnostics::LaunchTrace(trace));
        install_error_worker(cx, || {
            anyhow::Error::new(std::io::Error::from_raw_os_error(13))
                .context("cannot start LAN pairing listener")
        });
    });
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| fixture(window, cx));
        view.update(cx, |view, cx| view.test_run_begin("First Mac".into(), cx));
        Root::new(view, window, cx)
    });
    visual.run_until_parked();
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<SyncSettings>().unwrap()
    });
    view.read_with(visual, |view, _| {
        assert!(!view.test_error().unwrap().contains("Permission denied"))
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

#[gpui::test]
fn guided_first_run_welcome_opens_pairing_and_enables_sync(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("state");
    std::fs::create_dir(&state).unwrap();
    cx.update(|cx| {
        sync_fixture_init(cx);
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
        let view = background_sync(cx);
        assert!(view.read(cx).test_receiving());
        #[cfg(target_os = "linux")]
        assert!(
            view.read(cx).test_first_runtime_selected(),
            "setup result published immediately"
        );
    });
}

#[gpui::test]
fn guided_first_run_offer_incoming_confirmation_update_without_refresh_and_stay_visible(
    cx: &mut TestAppContext,
) {
    cx.update(sync_fixture_init);
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
    draw_sync_fixture(&mut visual);
    assert_eq!(
        visual.update(|window, _| window.viewport_size()),
        size(px(700.), px(440.)),
        "positive control: minimum viewport resize reached GPUI layout"
    );
    let view = visual.update(|_, cx| background_sync(cx));
    view.update_in(&mut visual, |view, window, cx| {
        view.test_set_receiving(false);
        view.test_set_wizard(None);
        view.test_name()
            .unwrap()
            .update(cx, |input, cx| input.set_value("Desktop", window, cx));
        cx.notify();
    });
    draw_sync_fixture(&mut visual);
    let add = visual.debug_bounds("sync-add-computer").unwrap();
    visual.simulate_click(add.center(), Modifiers::default());
    draw_sync_fixture(&mut visual);
    let create = visual.debug_bounds("sync-create-link").unwrap();
    visual.simulate_click(create.center(), Modifiers::default());
    draw_sync_fixture(&mut visual);
    assert_visible(&mut visual, "sync-copy-url");
    assert_visible(&mut visual, "sync-pairing-url");
    view.update(&mut visual, |view, cx| {
        view.test_set_scroll_offset(point(px(0.), px(-1000.)));
        cx.notify();
    });
    visual.executor().advance_clock(Duration::from_secs(1));
    draw_sync_fixture(&mut visual);
    assert_visible(&mut visual, "sync-approve-computer");
    assert!(
        visual.debug_bounds("sync-copy-url").is_none(),
        "incoming replaces offer, with positive approval control above"
    );
    let confirm = visual.debug_bounds("sync-approve-computer").unwrap();
    visual.simulate_click(confirm.center(), Modifiers::default());
    draw_sync_fixture(&mut visual);
    assert_visible(&mut visual, "sync-status");
    assert_visible(&mut visual, "sync-catalog-0");
    assert!(visual.debug_bounds("sync-approve-computer").is_none());
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["enter", "offer", "read", "approve"]
    );
    // Errors are pinned even when the long list has been scrolled to its end.
    view.update(&mut visual, |view, cx| {
        view.test_set_scroll_offset(point(px(0.), px(-1000.)));
        view.test_set_error(Some(
            "Could not connect. Check both computers and try again.".into(),
        ));
        cx.notify();
    });
    draw_sync_fixture(&mut visual);
    assert_visible(&mut visual, "sync-error");
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        stable_root_identity: None,
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
    let view = visual.update(|_, cx| background_sync(cx));
    view.update(&mut visual, |view, cx| {
        view.test_run_attach(
            uuid::Uuid::new_v4(),
            "fixture-vault".into(),
            vault.clone(),
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
fn sync_2419_sidebar_is_visible_while_off_and_first_click_enables_only_sync(
    cx: &mut TestAppContext,
) {
    let directory = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        sync_fixture_init(cx);
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
fn sync_2419_share_menu_opens_sync_with_module_off(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        sync_fixture_init(cx);
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

#[gpui::test]
fn issue2045_retention_reduction_requires_explicit_confirmation(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        crate::test_init(cx);
        super::super::bind_keys(cx);
        reader_settings::install(cx);
        crate::reader_ui_state::install(directory.path(), cx);
        reader_features::install(directory.path(), cx);
    });
    let state = |days| {
        let mut state = snapshot(None);
        state.history_retention.push(direct::RetentionSetting {
            path: Path::new("/fixture-history-vault").into(),
            days: direct::RetentionDays::try_from(days).unwrap(),
        });
        state
    };
    let calls = guided_worker(cx, vec![state(365), state(7), state(90)]);
    cx.update(|cx| reader_settings::show(None, cx));
    cx.run_until_parked();
    let handle = cx.update(|cx| cx.windows()[0]);
    let mut visual = VisualTestContext::from_window(handle, cx);
    visual.simulate_resize(size(px(900.), px(900.)));
    visual.run_until_parked();
    let sync = visual.debug_bounds("settings-section-Sync").unwrap();
    visual.simulate_click(sync.center(), Modifiers::default());
    visual.run_until_parked();
    assert_eq!(*calls.lock().unwrap(), vec!["enter"]);
    let view = visual.update(|_, cx| background_sync(cx));
    // The worker supplies twelve local vault rows. Scroll the real overview
    // before clicking; offscreen debug bounds do not exercise a button.
    view.update(&mut visual, |view, cx| {
        view.test_set_scroll_offset(point(px(0.), px(-4000.)));
        cx.notify();
    });
    visual.run_until_parked();
    assert_visible(&mut visual, "sync-history-days-0-7");
    // Opening a reduced preset must not invoke the blocking controller.
    for dismissal in ["escape", "cancel"] {
        let reduce = visual.debug_bounds("sync-history-days-0-7").unwrap();
        visual.simulate_click(reduce.center(), Modifiers::default());
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            visual.update(|window, cx| window.has_active_dialog(cx)),
            "the visible retention button must open the confirmation"
        );
        assert!(visual.debug_bounds("confirm-action").is_some());
        assert_eq!(*calls.lock().unwrap(), vec!["enter"]);
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert_eq!(*calls.lock().unwrap(), vec!["enter"]);
        assert!(visual.update(|window, cx| window.has_active_dialog(cx)));
        if dismissal == "escape" {
            visual.simulate_keystrokes("escape");
        } else {
            let cancel = visual.debug_bounds("cancel-confirm").unwrap();
            visual.simulate_click(cancel.center(), Modifiers::default());
        }
        visual.run_until_parked();
        assert!(!visual.update(|window, cx| window.has_active_dialog(cx)));
        assert_eq!(*calls.lock().unwrap(), vec!["enter"]);
        view.read_with(&visual, |view, _| {
            assert_eq!(
                view.test_snapshot().unwrap().history_retention[0]
                    .days
                    .days(),
                365
            )
        });
    }
    let reduce = visual.debug_bounds("sync-history-days-0-7").unwrap();
    visual.simulate_click(reduce.center(), Modifiers::default());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let confirm = visual.debug_bounds("confirm-action").unwrap();
    visual.simulate_click(confirm.center(), Modifiers::default());
    visual.run_until_parked();
    assert_eq!(*calls.lock().unwrap(), vec!["enter", "retention"]);
    view.read_with(&visual, |view, _| {
        assert_eq!(
            view.test_snapshot().unwrap().history_retention[0]
                .days
                .days(),
            7
        )
    });
    // The completed worker publishes a new snapshot and re-enables the row.
    // Draw that frame before reading button bounds; the modal's previous frame
    // still carries debug bounds until then.
    view.update(&mut visual, |view, cx| {
        assert!(
            !view.test_busy(),
            "completed retention worker must release its controls"
        );
        view.test_set_scroll_offset(point(px(0.), px(-4000.)));
        cx.notify();
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert_visible(&mut visual, "sync-history-days-0-90");
    // Increasing retention remains immediate and provides a fired-path control.
    let increase = visual.debug_bounds("sync-history-days-0-90").unwrap();
    visual.simulate_click(increase.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(!visual.update(|window, cx| window.has_active_dialog(cx)));
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["enter", "retention", "retention"]
    );
}

#[cfg(target_os = "linux")]
mod labels_tests {
    use ::core::prelude::v1::test;
    use gpui::TestAppContext;

    #[gpui::test]
    fn sync_labels_catalog_fallback_keeps_explicit_name_and_reset(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let vault = root.path().join("local-replica");
        std::fs::create_dir(&vault).unwrap();
        cx.update(|cx| {
            crate::vault_name::remember_sync(vec![(vault.clone(), "Catalog notes".into())], cx);
            assert_eq!(crate::vault_name::shown(&vault, cx), "Catalog notes");
            assert!(!vault.join(".okilum").exists());
            crate::vault_name::set(&vault, "My notes", cx);
            crate::vault_name::remember_sync(vec![(vault.clone(), "Catalog notes".into())], cx);
            assert_eq!(crate::vault_name::shown(&vault, cx), "My notes");
            crate::vault_name::set(&vault, "", cx);
            assert_eq!(crate::vault_name::shown(&vault, cx), "local-replica");
        });
    }
}
