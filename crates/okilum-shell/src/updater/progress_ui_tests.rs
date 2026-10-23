use super::status::CheckStatus;
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
        crate::reader_settings::render_update_status(
            "fixture",
            self.status.clone(),
            move |_| restarted.set(true),
            cx,
        )
    }
}
#[gpui::test]
fn settings_download_progress_becomes_an_adjacent_explicit_restart(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        gpui_component::init(cx);
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
    ] {
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
            visual.simulate_click(button.center(), gpui::Modifiers::default());
        }
    }
    assert!(
        restarted.get(),
        "Restart now invokes the update/relaunch action"
    );
}
