//! Restart is an explicit transaction: every click and asynchronous outcome is
//! recorded in the same diagnostic log attached to Feedback.
use gpui::App;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

static TRACE: Mutex<Option<crate::reader_diagnostics::Trace>> = Mutex::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static MESSAGE: Mutex<Option<String>> = Mutex::new(None);

#[cfg(test)]
pub(super) static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(all(test, target_os = "linux", not(feature = "linux-sprint")))]
pub(super) fn pending_message() -> Option<String> {
    MESSAGE.lock().unwrap().clone()
}

pub(super) fn install(cx: &App) {
    if let Some(trace) = crate::reader_diagnostics::trace(cx) {
        *TRACE.lock().unwrap() = Some(trace);
    }
}

pub(super) fn record(outcome: &str, reason: &str) {
    if let Some(trace) = TRACE.lock().unwrap().as_ref() {
        // The accepted outcome can immediately terminate the process.
        trace.event_now(
            "update_restart",
            serde_json::json!({
                "outcome": outcome, "reason": reason, "platform": std::env::consts::OS,
            }),
        );
    }
}

pub(super) fn attempt() {
    ACTIVE.store(true, Ordering::Release);
    record("attempt", "Restart now clicked");
}

pub(super) fn notify(outcome: &str, reason: impl Into<String>) {
    let reason = reason.into();
    record(outcome, &reason);
    *MESSAGE.lock().unwrap() = Some(reason);
}

pub(super) fn refused(reason: impl Into<String>) {
    ACTIVE.store(false, Ordering::Release);
    notify("refused", reason);
}

#[cfg(target_os = "macos")]
pub(super) fn report(kind: i32, reason: &str) {
    if kind == 2 {
        record("Sparkle error", reason);
    }
    if !ACTIVE.load(Ordering::Acquire) && !matches!(kind, 13 | 14) {
        return;
    }
    match kind {
        2 => refused(format!("{reason} Open Settings → Updates and check for updates again. Okilum is staying open.")),
        10 | 14 => refused(if reason.is_empty() {
            "Okilum is still open. Save or close any pending dialog, then choose Restart now again."
        } else { reason }),
        4 => notify("verifying", "Verifying the update. Restart will continue automatically; your edits will be protected first."),
        6 => notify("accepted", "Restarting to install the update…"),
        7 => refused("Update cancelled. Check for updates and download it again."),
        8 => {
            ACTIVE.store(false, Ordering::Release);
            record("relaunched", "Sparkle confirmed installation and relaunch");
        }
        13 => notify("waiting", reason),
        _ => {}
    }
}

pub(super) fn present(cx: &mut App) {
    let Some(message) = MESSAGE.lock().unwrap().take() else {
        return;
    };
    // Includes Settings/About and windows that lost focus after the click.
    for handle in cx.windows() {
        let message = message.clone();
        let _ = handle.update(cx, |_, window, cx| {
            use gpui_component::{notification::Notification, WindowExt};
            struct RestartFeedback;
            window.push_notification(
                Notification::new()
                    .id::<RestartFeedback>()
                    .message(message)
                    .autohide(false)
                    .placement(gpui::Anchor::BottomRight),
                cx,
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, Render};
    use gpui_component::{Root, WindowExt};

    struct NoticeHost;
    impl Render for NoticeHost {
        fn render(
            &mut self,
            window: &mut gpui::Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            use gpui::{ParentElement, Styled};
            gpui::div()
                .size_full()
                .children(Root::render_notification_layer(window, cx))
        }
    }

    #[gpui::test]
    fn repeated_refusals_are_visible_after_focus_changes_and_recorded_in_feedback_log(
        cx: &mut gpui::TestAppContext,
    ) {
        let _guard = TEST_LOCK.lock().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let trace = crate::reader_diagnostics::Trace::new(Some(directory.path().into()), None);
        *TRACE.lock().unwrap() = Some(trace);
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            crate::test_init(cx);
        });
        let app = cx.clone();
        let (_, visual) = cx.add_window_view(|window, cx| {
            let host = cx.new(|_| NoticeHost);
            Root::new(host, window, cx)
        });
        for _ in 0..2 {
            visual.deactivate_window();
            attempt();
            refused("Save or close the note first, then choose Restart now again.");
            // The production scheduler owns App, without borrowing a Window.
            // Holding this window while the presenter updates every window
            // would correctly reject its reentrant handle.update call.
            app.update(present);
            visual.run_until_parked();
            visual.update(|window, cx| {
                window.activate_window();
                assert_eq!(
                    window.notifications(cx).len(),
                    1,
                    "the persistent outcome replaces the previous attempt"
                );
            });
        }
        visual
            .executor()
            .advance_clock(std::time::Duration::from_secs(60));
        visual.run_until_parked();
        visual.update(|window, cx| assert_eq!(window.notifications(cx).len(), 1));
        let log = std::fs::read_to_string(directory.path().join("reader-diagnostic.log")).unwrap();
        let events: Vec<serde_json::Value> = log
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .filter(|event: &serde_json::Value| event["phase"] == "update_restart")
            .collect();
        assert_eq!(
            events
                .iter()
                .filter(|e| e["details"]["outcome"] == "attempt")
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e["details"]["outcome"] == "refused")
                .count(),
            2
        );
        assert!(events.iter().any(|e| e["details"]["reason"]
            .as_str()
            .unwrap()
            .contains("Save or close")));
        *TRACE.lock().unwrap() = None;
    }
}
