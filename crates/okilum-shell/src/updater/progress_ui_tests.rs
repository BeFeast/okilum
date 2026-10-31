use super::status::CheckStatus;
use gpui::prelude::*;
use gpui::{AppContext, Context, IntoElement, Render, TestAppContext, Window};
use gpui_component::Root;
use std::{cell::Cell, rc::Rc};

struct UpdateControls {
    status: CheckStatus,
    restarted: Rc<Cell<bool>>,
}
impl Render for UpdateControls {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let restarted = self.restarted.clone();
        gpui::div().children(crate::reader_settings::render_update_status(
            "fixture",
            self.status.clone(),
            move |_| restarted.set(true),
            cx,
        ))
    }
}
#[gpui::test]
fn settings_download_progress_becomes_an_adjacent_explicit_restart(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        crate::test_init(cx);
    });
    let restarted = Rc::new(Cell::new(false));
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let controls = cx.new(|_| UpdateControls {
            status: CheckStatus::Idle,
            restarted: restarted.clone(),
        });
        entity = Some(controls.clone());
        Root::new(controls, window, cx)
    });
    let controls = entity.unwrap();
    visual.run_until_parked();
    assert!(visual.debug_bounds("fixture-restart-update").is_none());
    for status in [
        CheckStatus::Downloading(Some(25)),
        CheckStatus::Preparing,
        CheckStatus::Ready,
        CheckStatus::Restarting,
        CheckStatus::RestartRetry(String::new()),
        CheckStatus::RestartRetry("Your latest edits could not be protected.".into()),
    ] {
        restarted.set(false);
        let actionable = matches!(status, CheckStatus::Ready | CheckStatus::RestartRetry(_));
        controls.update(visual, |controls, cx| {
            controls.status = status;
            cx.notify();
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let progress = visual
            .debug_bounds("fixture-update-progress")
            .expect("positive control: real progress indicator drawn");
        if let Some(button) = visual.debug_bounds("fixture-restart-update") {
            assert!(
                button.origin.x >= progress.right(),
                "Restart now belongs beside the progress bar"
            );
            assert!(
                !restarted.get(),
                "ready does not restart without user input"
            );
            assert!(actionable);
            visual.simulate_click(button.center(), gpui::Modifiers::default());
        } else {
            assert!(
                !actionable,
                "a refused/delayed restart must restore About's action"
            );
        }
        assert_eq!(restarted.get(), actionable);
    }
    assert!(
        restarted.get(),
        "Restart now invokes the update/relaunch action"
    );
}

// Use the same status renderer as About and Updates, without a live updater or
// shared global status. The following row detects overlapping/clipped height.
struct WrappedStatus {
    status: CheckStatus,
    width: f32,
}
impl Render for WrappedStatus {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        gpui_component::v_flex()
            .debug_selector(|| "status-content".into())
            .w(gpui::px(self.width))
            .children(crate::reader_settings::render_update_status(
                "wrap",
                self.status.clone(),
                |_| panic!("layout must not restart the app"),
                cx,
            ))
            .child(
                gpui::div()
                    .debug_selector(|| "after-status".into())
                    .child("Next setting"),
            )
    }
}

#[gpui::test]
fn long_update_error_wraps_within_settings_content(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|_| WrappedStatus {
            status: CheckStatus::Note("Short status".into()),
            width: 556.,
        });
        entity = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = entity.unwrap();
    // Feedback's Settings window is 780px wide (556px content), even though
    // its Reader metadata reports 1580x850. Cover both and a narrower window.
    for window_width in [1580., 780., 600.] {
        visual.simulate_resize(gpui::size(gpui::px(window_width), gpui::px(850.)));
        view.update(visual, |view, cx| {
            view.width = window_width - 224.;
            view.status = CheckStatus::Note("Short status".into());
            cx.notify();
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let short = visual
            .debug_bounds("wrap-update-status")
            .expect("short status drawn");
        assert!(short.size.height > gpui::px(0.));
        view.update(visual, |view, cx| {
            view.status = CheckStatus::Failed(
                "An error occurred while downloading the update. Please try again later. "
                    .repeat(4),
            );
            cx.notify();
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let text = visual
            .debug_bounds("wrap-update-status")
            .expect("long error drawn");
        let content = visual
            .debug_bounds("status-content")
            .expect("content drawn");
        let next = visual
            .debug_bounds("after-status")
            .expect("next setting drawn");
        assert!(text.size.width > gpui::px(0.));
        assert!(
            text.origin.x >= content.origin.x && text.right() <= content.right(),
            "status overflows content at {window_width}px: {text:?}, content {content:?}"
        );
        assert!(
            text.size.height >= short.size.height * 2.,
            "long error must wrap at {window_width}px: {text:?}, short {short:?}"
        );
        assert!(
            next.origin.y >= text.bottom(),
            "following setting overlaps wrapped text"
        );
        assert!(text.bottom() <= content.bottom());
    }
}
