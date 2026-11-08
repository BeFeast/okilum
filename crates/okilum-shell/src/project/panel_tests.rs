//! Actual Reader visibility and native Details paint, using synthetic state only.
use super::*;
use crate::reader_diagnostics;
use ::core::prelude::v1::test;
use serde_json::{json, Value};
use std::{cell::RefCell, path::Path, time::Instant};

thread_local! {
    static DETAILS_PAINT: RefCell<Vec<Bounds<Pixels>>> = const { RefCell::new(Vec::new()) };
}

/// Observe the real content mask in paint, not an assumed layout intersection.
/// This absolute, drawing-free probe does not participate in flex sizing.
pub(super) fn details_paint_probe() -> impl IntoElement {
    gpui::canvas(
        |_, _, _| (),
        |bounds, _, window, _| {
            let visible = bounds.intersect(&window.content_mask().bounds);
            DETAILS_PAINT.with(|rows| rows.borrow_mut().push(visible));
        },
    )
    .absolute()
    .size_full()
}

struct Fixture {
    reader: Entity<Reader>,
    view: Entity<ProjectView>,
    gateway: Arc<fixture::Fake>,
    trace: reader_diagnostics::Trace,
    files: tempfile::TempDir,
}

fn mounted(cx: &mut TestAppContext, messages: u64) -> (Fixture, &mut VisualTestContext) {
    cx.executor().allow_parking();
    let files = fixture::canonical_tempdir();
    let vault = files.path().join("vault");
    std::fs::create_dir_all(vault.join("ops")).unwrap();
    std::fs::write(vault.join("ops/project.toml"), "kind = 'knowledge'\n").unwrap();
    std::fs::write(vault.join("start.md"), "# Synthetic Manager fixture\n").unwrap();
    let trace = reader_diagnostics::Trace::new(Some(files.path().join("diagnostics")), None);
    cx.update(|cx| {
        crate::test_init(cx);
        reader_features::enable_test_modules(cx);
        cx.set_global(reader_diagnostics::LaunchTrace(trace.clone()));
    });
    let (root, visual) = cx.add_window_view(|window, cx| {
        let reader = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(vault.clone()),
                    note: Some("start.md".into()),
                    index_dir: Some(files.path().join("index")),
                    session_directory: Some(files.path().join("state")),
                    panel_settings_override: Some(files.path().join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        Root::new(reader, window, cx)
    });
    visual.simulate_resize(size(px(1400.), px(800.)));
    visual.run_until_parked();
    let reader = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<Reader>().unwrap()
    });
    let gateway = Arc::new(fixture::Fake::default());
    let view = reader.update_in(visual, |reader, window, cx| {
        let view = cx.new(|cx| {
            let mut view = fixture::view(files.path().join("drafts"), gateway.clone(), window, cx);
            view.area = Some(vault.clone());
            let env = view.environment.as_ref().unwrap();
            let expiry = 4070908800u64;
            let token = okilum_t3::stream::NativeToken::from_secret(env, &serde_json::to_vec(&json!({
                "origin":env.origin(), "access_token":"panel-fixture-only", "expires_at":expiry,
                "scope":"orchestration:read orchestration:operate"
            })).unwrap()).unwrap();
            let client = Arc::new(okilum_t3::stream::NativeClient::new(
                env.clone(), token, Duration::from_secs(1),
            ).unwrap());
            view.native.set_detail_test_client(client);
            view.native.set_detail_test_expiry(expiry);
            view.native.set_panel_test_ready();
            view.identity = Some(ServerIdentity { environment_id:"fixture".into(), label:"Fixture".into() });
            view.gateway = None; // No periodic transport work can repopulate an empty-feed control.
            view.compose.update(cx, |input, cx| input.set_value("Keep this draft", window, cx));
            let items: Vec<okilum_t3::TimelineItem> = (1..=messages).map(|position| {
                serde_json::from_value(json!({
                    "position":position,"itemId":format!("row-{position}"),"messageId":format!("message-{position}"),
                    "type":"assistant_message","status":"completed","text":format!("Synthetic message {position}\n\nRetained content."),"updatedAt":"now"
                })).unwrap()
            }).collect();
            view.feed.update(cx, |feed, cx| feed.apply(items.iter(), None, cx));
            view.ttfm_open(Instant::now(), cx);
            view
        });
        reader.project_navigation.active = Some(okilum_t3::projects::Project::read(&vault).unwrap());
        reader.project = Some(view.clone());
        reader.project_open = true;
        reader.panels.close(reader_layout::Panel::Notes);
        reader.panels.open(reader_layout::Panel::Backlinks);
        reader.panel_widths.backlinks = 380.;
        cx.notify();
        view
    });
    visual.run_until_parked();
    (
        Fixture {
            reader,
            view,
            gateway,
            trace,
            files,
        },
        visual,
    )
}

fn events(path: &Path, phase: &str) -> Vec<Value> {
    std::fs::read_to_string(path.join("diagnostics/reader-diagnostic.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event["phase"] == phase)
        .collect()
}

fn barrier(fixture: &Fixture) {
    let before = events(fixture.files.path(), "panel_fixture_barrier").len();
    fixture
        .trace
        .event("panel_fixture_barrier", json!({"drain":true}));
    let until = Instant::now() + Duration::from_secs(5);
    while events(fixture.files.path(), "panel_fixture_barrier").len() <= before {
        assert!(Instant::now() < until, "diagnostic writer did not run");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn draw(visual: &mut VisualTestContext) {
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    visual.run_until_parked();
}

fn toggle(fixture: &Fixture, visual: &mut VisualTestContext) {
    fixture.reader.update_in(visual, |reader, window, cx| {
        reader.toggle_panel(reader_layout::Panel::Backlinks, window, cx);
    });
    visual.run_until_parked();
}

#[gpui::test]
fn manager_2693_reader_toggle_rearms_retained_cached_paint_once(cx: &mut TestAppContext) {
    let _cache = reader_side_panel::tests::CachedPanels::on();
    let (fixture, visual) = mounted(cx, 2);
    barrier(&fixture);
    assert_eq!(
        events(fixture.files.path(), "manager_ttfm").len(),
        1,
        "mounted message actually painted"
    );
    let client = fixture
        .view
        .read_with(visual, |view, _| view.native.connection().unwrap().0);
    toggle(&fixture, visual);
    assert!(
        fixture.view.read_with(visual, |view, _| view.active),
        "Reader toggle bypasses selection activation"
    );
    toggle(&fixture, visual);
    barrier(&fixture);
    let warm = events(fixture.files.path(), "manager_ttfm");
    assert_eq!(
        warm.len(),
        2,
        "actual Reader reopen needs one fresh visible-paint event"
    );
    let details = &warm[1]["details"];
    assert_eq!(details["reused_connection"], true);
    for phase in ["auth_ms", "connect_ms", "shell_ms", "thread_ms"] {
        assert!(
            details[phase].is_null(),
            "unobserved warm phase {phase}: {details}"
        );
    }
    assert!(details["open_to_first_message_ms"].as_f64().unwrap() >= 0.);
    for _ in 0..3 {
        draw(visual);
    }
    barrier(&fixture);
    assert_eq!(
        events(fixture.files.path(), "manager_ttfm").len(),
        2,
        "redraw cannot create a new open"
    );
    fixture.view.read_with(visual, |view, _| {
        assert!(Arc::ptr_eq(&client, &view.native.connection().unwrap().0));
    });
    fixture.reader.update_in(visual, |reader, window, cx| {
        reader.close_project(window, cx)
    });
}

#[gpui::test]
fn manager_2693_reader_close_before_paint_and_empty_feed_never_succeed(cx: &mut TestAppContext) {
    let _cache = reader_side_panel::tests::CachedPanels::on();
    let (fixture, visual) = mounted(cx, 2);
    barrier(&fixture);
    let initial = events(fixture.files.path(), "manager_ttfm").len();
    assert_eq!(initial, 1);
    toggle(&fixture, visual);
    fixture.view.update(visual, |view, cx| {
        view.feed.update(cx, |feed, cx| feed.reset(cx))
    });
    toggle(&fixture, visual);
    let pending = fixture.view.read_with(visual, |view, _| view.ttfm.clone());
    assert!(pending.needs_paint(), "empty feed cannot paint a message");
    toggle(&fixture, visual);
    assert!(
        !pending.needs_paint(),
        "actual Reader close cancels a pending open"
    );
    pending.mark(ttfm::Phase::Paint, Instant::now()); // stale callback from the closed frame
    for _ in 0..3 {
        draw(visual);
    }
    barrier(&fixture);
    assert_eq!(events(fixture.files.path(), "manager_ttfm").len(), initial);
    fixture.reader.update_in(visual, |reader, window, cx| {
        reader.close_project(window, cx)
    });
}

#[gpui::test]
fn manager_2694_expanded_details_clip_before_history_at_narrow_short_sizes(
    cx: &mut TestAppContext,
) {
    let (fixture, visual) = mounted(cx, 64);
    let draft = fixture.view.read_with(visual, |view, cx| {
        view.composer.read(cx).typed_source_text(cx)
    });
    assert_eq!(draft, "Keep this draft");
    let client = fixture
        .view
        .read_with(visual, |view, _| view.native.connection().unwrap().0);
    for (width, height) in [(320., 440.), (380., 600.), (560., 700.)] {
        visual.simulate_resize(size(px(1400.), px(height)));
        fixture.reader.update(visual, |reader, cx| {
            reader.panel_widths.backlinks = width;
            cx.notify();
        });
        fixture.view.update(visual, |view, cx| {
            view.model_error = Some(
                "Synthetic long diagnostic text that wraps without changing the connection. "
                    .repeat(24),
            );
            view.connection_details = false;
            view.feed.update(cx, |_, cx| cx.notify());
            cx.notify();
        });
        visual.run_until_parked();
        let collapsed = visual
            .debug_bounds("project-history")
            .expect("mounted history");
        visual.simulate_event(gpui::ScrollWheelEvent {
            position: collapsed.center(),
            delta: gpui::ScrollDelta::Pixels(point(px(0.), px(1600.))),
            ..Default::default()
        });
        visual.run_until_parked();
        assert!(
            !fixture.view.read_with(visual, |view, cx| view
                .feed
                .read(cx)
                .panel_test_follows_tail(cx)),
            "positive control: real wheel input leaves the live edge"
        );
        let toggle = visual
            .debug_bounds("manager-details-toggle")
            .expect("real Details control");
        DETAILS_PAINT.with(|rows| rows.borrow_mut().clear());
        visual.simulate_click(toggle.center(), Modifiers::default());
        fixture
            .view
            .update(visual, |view, cx| view.feed.update(cx, |_, cx| cx.notify()));
        visual.run_until_parked();
        let history = visual
            .debug_bounds("project-history")
            .expect("expanded history viewport");
        let connection = visual
            .debug_bounds("project-connection-panel")
            .expect("bounded connection viewport");
        assert!(
            history.size.height >= px(32.),
            "usable history at {width}x{height}: {history:?}; connection {connection:?}; collapsed {collapsed:?}"
        );
        assert!(
            connection.bottom() <= history.top() + px(1.),
            "connection overlaps history: {connection:?} {history:?}"
        );
        let paints = DETAILS_PAINT.with(|rows| rows.borrow().clone());
        assert!(
            !paints.is_empty(),
            "positive control: real Details paint ran"
        );
        for painted in paints {
            assert!(painted.size.width > px(0.) && painted.size.height > px(0.));
            assert!(
                painted.bottom() <= history.top() + px(1.),
                "actual unclipped Details paint overlaps messages: {painted:?} {history:?}"
            );
        }
        let toggle = visual.debug_bounds("manager-details-toggle").unwrap();
        visual.simulate_click(toggle.center(), Modifiers::default());
        fixture
            .view
            .update(visual, |view, cx| view.feed.update(cx, |_, cx| cx.notify()));
        visual.run_until_parked();
        let restored = visual.debug_bounds("project-history").unwrap();
        assert!(
            restored.size.height >= collapsed.size.height - px(1.),
            "collapse restores transcript space"
        );
        fixture.view.read_with(visual, |view, cx| {
            assert!(!view.connection_details);
            assert_eq!(view.composer.read(cx).typed_source_text(cx), draft);
            assert!(
                !view.feed.read(cx).panel_test_follows_tail(cx),
                "Details preserves the older-history anchor"
            );
            assert!(Arc::ptr_eq(&client, &view.native.connection().unwrap().0));
            assert_eq!(
                view.binding.as_ref().unwrap().manager.thread_id.as_deref(),
                Some("mcp:fixture-manager")
            );
        });
        assert!(fixture.gateway.sends.lock().unwrap().is_empty());
    }
    fixture.reader.update_in(visual, |reader, window, cx| {
        reader.close_project(window, cx)
    });
}
