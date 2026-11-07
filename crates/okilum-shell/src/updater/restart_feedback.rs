//! Restart is an explicit transaction: every click and asynchronous outcome is
//! recorded in the same diagnostic log attached to Feedback.
//!
//! #2679: every update message — «Update ready», restart progress and
//! failures — shares one notification id, so a window shows at most one
//! update notice and a newer message replaces the older one in place.
use gpui::App;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

static TRACE: Mutex<Option<crate::reader_diagnostics::Trace>> = Mutex::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static MESSAGE: Mutex<Option<Message>> = Mutex::new(None);

/// The single update notice's id on every platform.
pub(super) struct UpdateNotice;

#[derive(Clone, Debug)]
struct Message {
    text: String,
    /// A refusal or failure offers one action; progress offers none.
    actionable: bool,
    ready: bool,
}

/// Remembers which ready package was announced, so the same package is not
/// announced twice but a newer one always is (#2679), even after the older
/// notice was closed.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Default)]
pub(super) struct Announced(Option<String>);

#[cfg(any(target_os = "macos", test))]
impl Announced {
    /// True once per distinct ready package.
    pub(super) fn claim(&mut self, package: &str) -> bool {
        if self.0.as_deref() == Some(package) {
            return false;
        }
        self.0 = Some(package.to_owned());
        true
    }

    /// Nothing is ready any more: the same package may be announced again.
    pub(super) fn reset(&mut self) {
        self.0 = None;
    }
}

#[cfg(test)]
pub(super) static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(all(test, target_os = "linux", not(feature = "linux-sprint")))]
pub(super) fn pending_message() -> Option<String> {
    MESSAGE
        .lock()
        .unwrap()
        .as_ref()
        .map(|message| message.text.clone())
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

fn post(outcome: &str, reason: String, actionable: bool) {
    record(outcome, &reason);
    *MESSAGE.lock().unwrap() = Some(Message {
        text: reason,
        actionable,
        ready: false,
    });
}

// Replacement downloads and the macOS bridge share the existing update notice.
pub(super) fn notify(outcome: &str, reason: impl Into<String>) {
    post(outcome, reason.into(), false);
}

/// A completed replacement must not be overwritten by queued download progress.
#[cfg(any(target_os = "linux", windows, test))]
pub(super) fn clear_pending() {
    *MESSAGE.lock().unwrap() = None;
}

/// Replace download progress in every window, including ones that lost focus.
#[cfg(any(target_os = "linux", windows, test))]
pub(super) fn latest_ready() {
    record("ready", "The latest update is ready");
    *MESSAGE.lock().unwrap() = Some(Message {
        text: "Update ready".into(),
        actionable: true,
        ready: true,
    });
}

#[cfg(any(target_os = "linux", windows, test))]
pub(super) fn offer(reason: impl Into<String>) {
    post("available", reason.into(), true);
}

pub(super) fn refused(reason: impl Into<String>) {
    ACTIVE.store(false, Ordering::Release);
    post("refused", reason.into(), true);
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
        2 => refused(format!(
            "{reason} Open Settings → Updates and check for updates again. Okilum is staying open."
        )),
        // Sparkle can offer its retry continuation while GPUI's asynchronous
        // quit is already completing. An empty signal is waiting, not proof
        // of a failed restart. Keep ACTIVE for subsequent install outcomes.
        10 if reason.is_empty() => post(
            "waiting",
            "Finishing restart. If Okilum stays open, choose Restart now again.".into(),
            true,
        ),
        10 | 14 => refused(if reason.is_empty() {
            "Okilum is still open. Save or close any pending dialog, then choose Restart now again."
        } else {
            reason
        }),
        4 => notify(
            "verifying",
            "Verifying the update. Restart will continue automatically; your edits will be protected first.",
        ),
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

/// «Update ready» with Restart (#683). It stays until the user closes it or
/// restarts: a downloaded update must not pass unnoticed. A newer package or
/// any restart outcome replaces it in place (#2679).
pub(super) fn show_ready(
    window: &mut gpui::Window,
    cx: &mut App,
    restart: impl Fn(&mut App) + 'static,
) {
    use gpui::InteractiveElement as _;
    use gpui_component::button::Button;
    use gpui_component::{Sizable, WindowExt, notification::Notification};
    use std::rc::Rc;
    let restart = Rc::new(restart);
    window.defer(cx, move |window, cx| {
        window.push_notification(
            Notification::new()
                .id::<UpdateNotice>()
                .message("Update ready")
                .autohide(false)
                .placement(gpui::Anchor::BottomRight)
                .action(move |_, _, _| {
                    let restart = restart.clone();
                    Button::new("restart-update")
                        .debug_selector(|| "restart-update".into())
                        .small()
                        .label("Restart now")
                        .on_click(move |_, _, cx| restart(cx))
                }),
            cx,
        );
    });
}

fn show_message(window: &mut gpui::Window, cx: &mut App, message: Message) {
    use gpui::InteractiveElement as _;
    use gpui_component::button::Button;
    use gpui_component::{Sizable, WindowExt, notification::Notification};
    let mut notice = Notification::new()
        .id::<UpdateNotice>()
        .message(message.text)
        .autohide(false)
        .placement(gpui::Anchor::BottomRight);
    if message.actionable {
        // Restart now while an update is ready (always the newest one),
        // otherwise a fresh check. Never a second notice.
        notice = notice.action(|_, _, _| {
            Button::new("update-notice-action")
                .debug_selector(|| "update-notice-action".into())
                .small()
                .label(
                    if matches!(
                        super::status::get(),
                        super::status::CheckStatus::Available(_)
                    ) {
                        "Download update"
                    } else {
                        super::action_label()
                    },
                )
                .on_click(|_, _, cx| {
                    if matches!(
                        super::status::get(),
                        super::status::CheckStatus::Available(_)
                    ) {
                        super::install_update();
                    } else {
                        super::activate(cx);
                    }
                })
        });
    }
    window.push_notification(notice, cx);
}

pub(super) fn present(cx: &mut App) {
    let Some(message) = MESSAGE.lock().unwrap().take() else {
        return;
    };
    // Includes Settings/About and windows that lost focus after the click.
    for handle in cx.windows() {
        let message = message.clone();
        let _ = handle.update(cx, |_, window, cx| {
            if message.ready {
                show_ready(window, cx, super::restart_now);
            } else {
                show_message(window, cx, message);
            }
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

    #[cfg(target_os = "macos")]
    #[gpui::test]
    fn termination_wait_does_not_report_a_false_refusal_or_drop_later_install_outcomes(
        cx: &mut gpui::TestAppContext,
    ) {
        let _guard = TEST_LOCK.lock().unwrap();
        let directory = tempfile::tempdir().unwrap();
        *TRACE.lock().unwrap() = Some(crate::reader_diagnostics::Trace::new(
            Some(directory.path().into()),
            None,
        ));
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            crate::test_init(cx);
        });
        let app = cx.clone();
        let (_, visual) = cx.add_window_view(|window, cx| {
            let host = cx.new(|_| NoticeHost);
            Root::new(host, window, cx)
        });
        attempt();
        report(6, "");
        report(10, "");
        app.update(present);
        visual.run_until_parked();
        visual.update(|window, cx| {
            assert_eq!(window.notifications(cx).len(), 1);
            window.draw(cx).clear(cx);
        });
        assert!(visual.debug_bounds("update-notice-action").is_some());
        report(8, "");
        // A later explicit attempt with a real refusal keeps the failure path.
        attempt();
        report(10, "Your latest edits could not be protected.");
        app.update(present);
        visual.run_until_parked();
        visual.update(|window, cx| assert_eq!(window.notifications(cx).len(), 1));
        let log = std::fs::read_to_string(directory.path().join("reader-diagnostic.log")).unwrap();
        let events: Vec<serde_json::Value> = log
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .filter(|event: &serde_json::Value| event["phase"] == "update_restart")
            .collect();
        let outcomes: Vec<_> = events
            .iter()
            .map(|event| event["details"]["outcome"].as_str().unwrap())
            .collect();
        assert_eq!(
            outcomes,
            [
                "attempt",
                "accepted",
                "waiting",
                "relaunched",
                "attempt",
                "refused"
            ]
        );
        assert!(
            events.last().unwrap()["details"]["reason"]
                .as_str()
                .unwrap()
                .contains("latest edits could not be protected")
        );
        *TRACE.lock().unwrap() = None;
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
        assert!(events.iter().any(|e| {
            e["details"]["reason"]
                .as_str()
                .unwrap()
                .contains("Save or close")
        }));
        *TRACE.lock().unwrap() = None;
    }
}
