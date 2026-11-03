//! Regression coverage uses the real HTTP worker and an observable OS-store double.
use super::guided_tests::{guided_click, guided_frame};
use super::*;
use ::core::prelude::v1::test;
use okilum_core::remote_log::fake::{FakeKibana, Scenario};
use std::sync::Mutex;

fn inside(bounds: Bounds<Pixels>, viewport: Size<Pixels>) -> bool {
    bounds.size.width > px(0.)
        && bounds.size.height > px(0.)
        && bounds.left() >= px(0.)
        && bounds.top() >= px(0.)
        && bounds.right() <= viewport.width
        && bounds.bottom() <= viewport.height
}

// Reproduce the original full-width DatePicker in a horizontal row. The same
// predicate used below must detect its invisible Apply button on both viewports.
struct OriginalRange([Entity<gpui_component::date_picker::DatePickerState>; 2]);
impl Render for OriginalRange {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .gap_2()
            .child("From")
            .child(gpui_component::date_picker::DatePicker::new(&self.0[0]).small())
            .child("00")
            .child("00")
            .child("To (exclusive)")
            .child(gpui_component::date_picker::DatePicker::new(&self.0[1]).small())
            .child("00")
            .child("00")
            .child(
                Button::new("original-apply")
                    .debug_selector(|| "original-apply".into())
                    .small()
                    .label("Apply range"),
            )
    }
}

#[gpui::test]
fn custom_range_bounds_fit_default_and_minimum_with_old_layout_positive_control(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(crate::test_init);
    let (_, original) = cx.add_window_view(|window, cx| {
        let old = cx.new(|cx| {
            OriginalRange(std::array::from_fn(|_| {
                cx.new(|cx| gpui_component::date_picker::DatePickerState::new(window, cx))
            }))
        });
        Root::new(old, window, cx)
    });
    for viewport in [size(px(1200.), px(800.)), size(px(900.), px(600.))] {
        original.simulate_resize(viewport);
        guided_frame(original);
        assert!(
            !inside(original.debug_bounds("original-apply").unwrap(), viewport),
            "positive control: the old full-width pickers must push Apply outside the window"
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let mut panel = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| Panel::new(dir.path().into(), dir.path().into(), window, cx));
        panel = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    let panel = panel.unwrap();
    panel.update_in(visual, |panel, window, cx| {
        panel.choose(
            Source::direct("Synthetic", "https://logs.example", "events", "").unwrap(),
            window,
            cx,
        );
        panel.open_custom(window, cx);
        assert_eq!(panel.environment_name(), "Synthetic");
        assert!(!panel.guided.adding && !panel.guided.advanced);
    });
    for viewport in [size(px(1200.), px(800.)), size(px(900.), px(600.))] {
        visual.simulate_resize(viewport);
        guided_frame(visual);
        for selector in [
            "remote-environments",
            "remote-time-controls",
            "remote-date-0",
            "remote-clock-0",
            "remote-clock-1",
            "remote-date-1",
            "remote-clock-2",
            "remote-clock-3",
            "remote-custom-zone",
            "remote-custom-apply",
            "remote-status",
        ] {
            let bounds = visual
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("missing {selector}"));
            assert!(
                inside(bounds, viewport),
                "{selector}: {bounds:?} outside {viewport:?}"
            );
        }
        assert!(
            visual.debug_bounds("remote-date-0").unwrap().bottom()
                <= visual.debug_bounds("remote-date-1").unwrap().top()
        );
        // Expanded source controls stay bounded too, and remain collapsible.
        guided_click(visual, "remote-add-source");
        assert!(inside(
            visual.debug_bounds("remote-import").unwrap(),
            viewport
        ));
        guided_click(visual, "remote-add-source");
        // Connection details from #2437 stay collapsed under status.
        assert!(visual.debug_bounds("remote-connection-section").is_none());
        guided_click(visual, "remote-connection-details");
        let status = visual.debug_bounds("remote-status").unwrap();
        let details = visual.debug_bounds("remote-connection-section").unwrap();
        assert!(details.top() >= status.bottom());
        guided_click(visual, "remote-connection-details");
        // Exercise the actual hour dropdown, then dismiss its retained popup.
        guided_click(visual, "remote-clock-0");
        visual.simulate_keystrokes("down enter escape");
        guided_frame(visual);
    }
    cx.quit();
}

fn assert_no_secret_files(path: &std::path::Path) {
    if !path.exists() {
        return;
    }
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            assert_no_secret_files(&path);
        } else {
            let bytes = std::fs::read(&path).unwrap();
            assert!(
                !bytes
                    .windows(b"test-secret".len())
                    .any(|s| s == b"test-secret"),
                "key leaked into {}",
                path.display()
            );
        }
    }
}

#[gpui::test]
async fn refused_store_session_only_rows_auth_replacement_and_retry_save(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let server = FakeKibana::start("127.0.0.1:0", 3, Scenario::default()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&vault).unwrap();
    let store = Arc::new(Mutex::new(credentials::TestStore {
        refuse: true,
        ..Default::default()
    }));
    cx.update(|cx| {
        crate::test_init(cx);
        crate::bind_keys(cx);
        reader_ui_state::install(&state, cx);
    });
    let mut reopen_cx = cx.clone();
    let mut panel = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| Panel::new(vault.clone(), state.clone(), window, cx));
        panel = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    visual.simulate_resize(size(px(1200.), px(800.)));
    let panel = panel.unwrap();
    visual
        .condition(&panel, |panel, _| panel.sources_loaded)
        .await;
    guided_click(visual, "remote-add-source");
    panel.update_in(visual, |panel, window, cx| {
        panel.credentials.store = Some(store.clone());
        panel
            .name
            .update(cx, |input, cx| input.set_value("Synthetic", window, cx));
        panel.url.update(cx, |input, cx| {
            input.set_value(&server.endpoint, window, cx)
        });
        panel.pattern.update(cx, |input, cx| {
            input.set_value("remote-*:events", window, cx)
        });
    });
    guided_click(visual, "remote-import");
    panel.update_in(visual, |panel, window, cx| {
        assert_eq!(
            panel.environment_name(),
            "Synthetic",
            "environment is visible before the first successful query"
        );
        assert!(!panel.guided.adding);
        panel
            .key
            .update(cx, |input, cx| input.set_value("test-secret", window, cx));
        panel.from.update(cx, |input, cx| {
            input.set_value("2026-11-01T00:00:00Z", window, cx)
        });
        panel.to.update(cx, |input, cx| {
            input.set_value("2026-11-05T00:00:00Z", window, cx)
        });
    });
    guided_click(visual, "remote-refresh");
    visual
        .condition(&panel, |panel, _| {
            !panel.busy && panel.credentials.failure.is_some()
        })
        .await;
    panel.read_with(visual, |panel, cx| {
        assert!(panel
            .error
            .as_deref()
            .unwrap()
            .contains(credentials::recovery_message()));
        assert!(panel
            .credentials
            .failure
            .as_deref()
            .unwrap()
            .contains("-25308"));
        assert!(panel.key.read(cx).value().is_empty());
        assert!(panel.data.is_none());
    });
    assert_eq!(
        store.lock().unwrap().writes,
        1,
        "positive control: a credential write was attempted"
    );
    assert_eq!(server.state.lock().unwrap().searches, 0);
    guided_click(visual, "remote-session-key");
    visual
        .condition(&panel, |panel, _| {
            !panel.busy
                && panel.data.as_ref().is_some_and(|d| d.events.len() == 3)
                && panel.guided.catalog.is_some()
        })
        .await;
    panel.read_with(visual, |panel, _| {
        assert!(panel
            .credential_status()
            .contains("Key not saved — session only"));
        assert!(!panel.auth_error);
    });
    assert!(store.lock().unwrap().entries.is_empty());
    assert_eq!(
        store.lock().unwrap().writes,
        1,
        "session-only bypasses the refused OS store"
    );
    assert_eq!(
        remote_log::load_sources(&state, &vault).unwrap().len(),
        1,
        "positive control: the credential-free source really was saved"
    );
    assert_no_secret_files(&state);
    let data = panel.read_with(visual, |panel, _| panel.data.clone().unwrap());
    server.state.lock().unwrap().scenario.failure_status = Some(401);
    guided_click(visual, "remote-refresh");
    visual
        .condition(&panel, |panel, _| !panel.busy && panel.auth_error)
        .await;
    assert!(panel.read_with(visual, |panel, _| Arc::ptr_eq(
        panel.data.as_ref().unwrap(),
        &data
    )));
    server.state.lock().unwrap().scenario.failure_status = None;
    panel.update_in(visual, |panel, window, cx| {
        panel
            .key
            .update(cx, |input, cx| input.set_value("test-secret", window, cx));
    });
    guided_click(visual, "remote-replace-key");
    visual
        .condition(&panel, |panel, _| {
            !panel.busy
                && !panel.auth_error
                && panel.data.as_ref().is_some_and(|d| !Arc::ptr_eq(d, &data))
        })
        .await;
    assert_eq!(
        store.lock().unwrap().writes,
        1,
        "401 replacement preserves the explicit session-only choice"
    );
    assert_no_secret_files(&state);

    // A fresh panel has no process-memory key: it must prompt again, even with
    // the saved source present. This double has no stored entry to supply.
    let mut reopened = None;
    let (_, other_visual) = reopen_cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| Panel::new(vault.clone(), state.clone(), window, cx));
        reopened = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    let reopened = reopened.unwrap();
    other_visual
        .condition(&reopened, |panel, _| panel.sources_loaded)
        .await;
    reopened.update_in(other_visual, |panel, window, cx| {
        panel.credentials.store = Some(store.clone());
        panel.select_source(panel.sources[0].clone(), window, cx);
    });
    other_visual
        .condition(&reopened, |panel, _| !panel.busy && panel.guided.key_needed)
        .await;
    assert!(
        reopened.read_with(other_visual, |panel, _| panel.data.is_none()
            && panel.credentials.key.is_none())
    );

    store.lock().unwrap().refuse = false;
    guided_click(visual, "remote-retry-saving");
    visual
        .condition(&panel, |panel, _| {
            !panel.busy && !panel.credentials.session_only && panel.credentials.failure.is_none()
        })
        .await;
    panel.read_with(visual, |panel, _| {
        let identity = panel.applied.as_ref().unwrap().credential_url(&vault);
        assert_eq!(
            store.lock().unwrap().entries[&identity].as_bytes(),
            b"test-secret",
            "positive control: Retry saving creates the actual store entry"
        );
        assert!(panel.credentials.key.is_none());
    });
    assert_eq!(store.lock().unwrap().writes, 2);
    assert_no_secret_files(&state);
    cx.quit();
}
