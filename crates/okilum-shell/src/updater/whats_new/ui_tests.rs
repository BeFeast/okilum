use super::*;
use ::core::prelude::v1::test;

fn current() -> Build {
    Build {
        build: 15053,
        source: "a".repeat(40),
    }
}
fn offline_view(cx: &mut App) -> Entity<NotesView> {
    cx.new(|_| NotesView {
        current: current(),
        notes: Some(Range {
            incomplete: true,
            ..Range::default()
        }),
        _task: None,
    })
}
struct Surface {
    focus: FocusHandle,
}
impl Render for Surface {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus)
            .size_full()
            .child("Document")
            .children(Root::render_sheet_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

#[gpui::test]
fn update_toast_does_not_steal_focus_and_expires(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_reduce_motion(true);
    });
    let (_, visual) = cx.add_window_view(|window, cx| {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let view = cx.new(|_| Surface { focus });
        Root::new(view, window, cx)
    });
    visual.run_until_parked();
    let before = visual.update(|window, cx| window.focused(cx));
    visual.update(|window, cx| announce(current(), 15001, window, cx));
    visual.run_until_parked();
    visual.update(|window, cx| {
        assert_eq!(
            window.notifications(cx).len(),
            1,
            "positive control: visible toast"
        );
        assert_eq!(window.focused(cx), before);
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("updated-whats-new").is_some());
    visual.executor().advance_clock(Duration::from_secs(5));
    visual.run_until_parked();
    visual.update(|window, cx| {
        assert!(window.notifications(cx).is_empty());
        assert_eq!(window.focused(cx), before);
    });
}

#[gpui::test]
fn actual_reader_and_about_render_the_sheet_and_offline_fallback(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
        cx.set_reduce_motion(true);
    });
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("start.md"), "# Start\n").unwrap();
    let (_, visual) = cx.add_window_view(|window, cx| {
        let reader = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(temp.path().to_owned()),
                    note: Some("start.md".into()),
                    session_directory: Some(temp.path().join("state")),
                    index_dir: Some(temp.path().join("index")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        Root::new(reader, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| {
        let view = offline_view(cx);
        open(view, window, cx);
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        visual.debug_bounds("whats-new-content").is_some(),
        "actual Reader sheet layer"
    );
    assert!(visual.debug_bounds("whats-new-offline").is_some());
    // Open the real Settings/About window through the application's public path.
    visual.update(|_, cx| crate::reader_settings::show_about(cx));
    visual.run_until_parked();
    let settings = visual
        .windows()
        .into_iter()
        .find(|&handle| {
            VisualTestContext::from_window(handle, visual)
                .window_title()
                .as_deref()
                == Some("Okilum Settings")
        })
        .unwrap();
    let mut settings = VisualTestContext::from_window(settings, visual);
    settings.run_until_parked();
    settings.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        settings.debug_bounds("desktop-about").is_some(),
        "positive control: About drawn"
    );
    // Populate the real About window with the same sheet and a known offline
    // result, so this render regression never contacts a server.
    settings.update(|window, cx| {
        let view = offline_view(cx);
        open(view, window, cx);
    });
    settings.run_until_parked();
    settings.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        settings.debug_bounds("whats-new-content").is_some(),
        "actual Settings sheet layer"
    );
    assert!(settings.debug_bounds("whats-new-offline").is_some());
    assert!(
        settings.debug_bounds("whats-new-releases").is_some(),
        "offline release link retained"
    );
}
