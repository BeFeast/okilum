#[cfg(target_os = "linux")]
pub(crate) mod appimage;
// Platform engines retain selection/validation; Okilum owns policy and UI.
#[cfg(any(target_os = "linux", test))]
pub(crate) mod linux_repo;
#[cfg(target_os = "macos")]
mod macos;
mod policy;
#[cfg(any(windows, test))]
mod ready;
mod restart_feedback;
#[cfg(any(target_os = "linux", windows, test))]
mod restart_intent;
mod scheduler;
pub(crate) use policy::{mode, Mode};
pub(crate) mod status;
pub(crate) mod whats_new;
#[cfg(windows)]
mod windows;
#[cfg(any(windows, test))]
mod windows_feed;

use gpui::{App, MenuItem};
use scheduler::{Schedule, Trigger};
use std::{
    hash::BuildHasher as _,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

#[cfg(target_os = "macos")]
gpui::actions!(okilum, [AboutOkilum, CheckForUpdates, ToggleBetaBuilds]);

/// Items for the application menu, ending with `quit`.
pub(crate) fn menu_items(quit: MenuItem) -> Vec<MenuItem> {
    #[cfg(target_os = "macos")]
    return macos_menu_items(quit);
    #[cfg(not(target_os = "macos"))]
    vec![
        MenuItem::action("Settings…", crate::reader_settings::OpenSettings),
        MenuItem::separator(),
        quit,
    ]
}

#[cfg(target_os = "macos")]
fn macos_menu_items(quit: MenuItem) -> Vec<MenuItem> {
    let mut items = vec![
        MenuItem::action("About Okilum", AboutOkilum),
        MenuItem::action("Settings…", crate::reader_settings::OpenSettings),
        MenuItem::separator(),
    ];
    if macos::available() {
        items.extend([
            MenuItem::action("Check for Updates…", CheckForUpdates),
            MenuItem::action("Receive Beta Builds", ToggleBetaBuilds).checked(macos::beta()),
        ]);
    }
    items.extend([MenuItem::separator(), quit]);
    items
}

/// The installed Windows package's version; elsewhere there is none (#1319).
pub(crate) fn installed_package() -> Option<String> {
    #[cfg(windows)]
    return windows::package_version();
    #[cfg(not(windows))]
    None
}

fn recheck_interval() -> Duration {
    // RandomState is independently seeded by the OS. No persisted identity or
    // dependency is needed to spread automatic requests from different installs.
    scheduler::interval(std::collections::hash_map::RandomState::new().hash_one(std::process::id()))
}

fn schedule() -> &'static Mutex<Schedule> {
    static SCHEDULE: OnceLock<Mutex<Schedule>> = OnceLock::new();
    SCHEDULE.get_or_init(|| Mutex::new(Schedule::new(now(), recheck_interval())))
}

fn now() -> Duration {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

fn run_check(trigger: Trigger) {
    schedule().lock().unwrap().run(
        now(),
        trigger,
        if trigger.manual() {
            available()
        } else {
            automatic_checks_enabled()
        },
        recheck_interval(),
        || start_check(trigger),
    );
}

fn automatic_checks_enabled() -> bool {
    #[cfg(all(target_os = "linux", feature = "appimage-acceptance-harness"))]
    if crate::appimage_acceptance::active() {
        return false;
    }
    available() && mode().automatic_checks()
}

fn start_check(trigger: Trigger) -> bool {
    #[cfg(target_os = "linux")]
    return appimage::check(mode().download(matches!(trigger, Trigger::Download)));
    #[cfg(not(target_os = "linux"))]
    let _ = trigger;
    #[cfg(target_os = "macos")]
    return macos::check(
        mode().download(matches!(trigger, Trigger::Download)),
        trigger.manual(),
    );
    #[cfg(windows)]
    return windows::check(mode().download(matches!(trigger, Trigger::Download)));
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    false
}

/// The native Sparkle scheduler shares the automatic throttle with focus,
/// startup and explicit checks. Sparkle has already claimed its own session.
#[cfg(target_os = "macos")]
fn admit_sparkle_check(automatic: bool) -> bool {
    schedule().lock().unwrap().run(
        now(),
        if automatic {
            Trigger::Focus
        } else {
            Trigger::Manual
        },
        if automatic {
            automatic_checks_enabled()
        } else {
            available()
        },
        recheck_interval(),
        || true,
    )
}

fn install_schedule(cx: &mut App) {
    let _ = schedule();
    observe_window_focus(cx, || run_check(Trigger::Focus));
    cx.spawn(async move |cx| {
        loop {
            // Ticks only consult the policy; the network runs once per due interval.
            cx.background_executor().timer(Duration::from_secs(1)).await;
            run_check(Trigger::Interval);
            cx.update(|cx| {
                #[cfg(target_os = "linux")]
                appimage::finish_requested_restart(cx);
                #[cfg(windows)]
                windows::finish_requested_restart(cx);
                restart_feedback::present(cx);
            });
        }
    })
    .detach();
}

fn observe_window_focus(cx: &App, on_focus: impl Fn() + Clone + 'static) {
    // Observe every GPUI root, including Settings/About and single-file windows.
    // Switching between two Okilum windows uses the same application-wide gate.
    cx.observe_new::<gpui_component::Root>(move |_, window, cx| {
        if let Some(window) = window {
            let on_focus = on_focus.clone();
            cx.observe_window_activation(window, move |_, window, _| {
                if window.is_window_active() {
                    on_focus();
                }
            })
            .detach();
        }
    })
    .detach();
}

// Startup confirmation requires a constructed GUI window, not keyboard focus.
// A restored/background Linux window (including Xvfb without a window manager)
// may never become active; that must not prevent committing an applied update.
#[cfg(any(target_os = "linux", test))]
fn with_startup_window(cx: &App, confirm: impl FnOnce()) {
    if !cx.windows().is_empty() {
        confirm();
    }
}

/// Start Sparkle before the menus are built, so they reflect its availability.
pub(crate) fn install(_cx: &mut App) {
    restart_feedback::install(_cx);
    crate::reader_settings::install(_cx);
    install_schedule(_cx);
    #[cfg(target_os = "linux")]
    if appimage::packaged() {
        appimage::start();
        _cx.spawn(async move |cx| {
            let mut checked = status::generation();
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(300))
                    .await;
                cx.update(|cx| {
                    with_startup_window(cx, appimage::confirm_startup);
                });
                if status::generation() != checked {
                    checked = status::generation();
                    cx.update(|cx| cx.refresh_windows());
                }
            }
        })
        .detach();
    }
    whats_new::install(_cx);
    #[cfg(windows)]
    {
        windows::start();
        _cx.spawn(async move |cx| {
            let mut previous = None;
            let mut checked = status::generation();
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                let current = windows::ready_package();
                if current != previous || status::generation() != checked {
                    previous = current;
                    checked = status::generation();
                    cx.update(|cx| cx.refresh_windows());
                }
            }
        })
        .detach();
    }
    #[cfg(target_os = "macos")]
    _cx.spawn(async move |cx| {
        // Sparkle reports on the main thread, outside GPUI; redraw the line.
        let mut checked = status::generation();
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(300))
                .await;
            if status::generation() != checked {
                checked = status::generation();
                cx.update(|cx| {
                    macos::finish_requested_restart(cx);
                    cx.refresh_windows();
                });
            }
        }
    })
    .detach();
    #[cfg(target_os = "macos")]
    {
        macos::start();
        _cx.on_action(|_: &AboutOkilum, cx| crate::about::show_from_menu(cx));
        // Explicit update entry overrides the remembered Settings pane (#1496).
        _cx.on_action(|_: &CheckForUpdates, cx| {
            crate::reader_settings::show_updates(cx);
            check();
        });
        _cx.on_action(|_: &ToggleBetaBuilds, cx| {
            set_beta(!macos::beta(), cx);
        });
    }
}

/// Availability reflects the packaged updater, not merely the operating system.
pub(crate) fn available() -> bool {
    #[cfg(target_os = "macos")]
    return macos::available();
    #[cfg(windows)]
    return windows::available();
    #[cfg(target_os = "linux")]
    return appimage::packaged();
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    false
}

/// Windows: why updating is paused because the update helper needs repair.
/// Path-free; shown in Settings instead of calling the install "Portable".
pub(crate) fn repair_needed() -> Option<String> {
    #[cfg(windows)]
    return windows::repair_needed();
    #[cfg(not(windows))]
    None
}

/// Windows: retry helper recovery now and check for updates if it succeeded.
pub(crate) fn retry_repair() {
    #[cfg(windows)]
    if windows::retry_repair() {
        check();
    }
}

/// Windows, before `VelopackApp::run`: whether a pending package may be applied
/// at this start (#1318).
#[cfg(windows)]
pub(crate) fn apply_pending_at_start() -> bool {
    windows::apply_pending_at_start()
}

#[cfg(any(target_os = "linux", windows))]
pub(super) fn startup_check() {
    run_check(Trigger::Focus);
}

pub(crate) fn set_mode(value: Mode, cx: &mut App) {
    if let Err(error) = policy::set_mode(value) {
        status::set(status::CheckStatus::Failed(format!(
            "Could not save update mode: {error}"
        )));
    }
    #[cfg(target_os = "macos")]
    macos::configure_mode();
    cx.refresh_windows();
}

pub(crate) fn check() {
    run_check(Trigger::Manual);
}

pub(crate) fn channel() -> &'static str {
    if crate::linux_sprint::enabled() {
        return "Sprint";
    }
    #[cfg(target_os = "macos")]
    return if !macos::available() {
        "Development"
    } else if macos::beta() {
        "Beta"
    } else {
        "Stable"
    };
    #[cfg(target_os = "windows")]
    return if windows::repair_needed().is_some() {
        "Needs repair"
    } else if !windows::available() {
        "Portable"
    } else if windows::beta() {
        "Beta"
    } else {
        "Stable"
    };
    #[cfg(target_os = "linux")]
    if appimage::packaged() {
        return appimage::channel();
    }
    #[cfg(target_os = "linux")]
    return if env!("OKILUM_BUILD_VERSION") == "development" {
        "Development"
    } else {
        // The pacman repository that delivers updates (#1036).
        match linux_repo::current() {
            linux_repo::Source::Beta => "Beta",
            linux_repo::Source::Stable => "Stable",
            linux_repo::Source::Both => "Beta and Stable",
            linux_repo::Source::Unknown => "System packages",
        }
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    if env!("OKILUM_BUILD_VERSION") == "development" {
        "Development"
    } else {
        "System packages"
    }
}

/// Shares Sparkle's persisted channel preference with Settings and the menu.
pub(crate) fn set_beta(enabled: bool, cx: &mut App) {
    #[cfg(any(target_os = "macos", windows))]
    if status::in_flight() {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, cx| {
                crate::reader_toast::error(
                    "Finish the current update before changing channels.",
                    window,
                    cx,
                )
            });
        }
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let changed = macos::beta() != enabled;
        macos::set_beta(enabled);
        if changed {
            check();
        }
        #[cfg(feature = "brain")]
        crate::brain::app_quit::set_menus(cx);
        #[cfg(not(feature = "brain"))]
        crate::reader_app_menu::set_menus(cx);
        cx.refresh_windows();
    }
    #[cfg(windows)]
    {
        let previous = windows::beta();
        windows::set_beta(enabled);
        if windows::beta() != previous {
            check();
        }
        cx.refresh_windows();
    }
    #[cfg(target_os = "linux")]
    {
        if appimage::packaged() {
            appimage::set_beta(enabled);
            cx.refresh_windows();
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    let _ = (enabled, cx);
}

/// The same explicit action is exposed in About, Settings and the ready toast.
pub(crate) fn action_label() -> &'static str {
    #[cfg(target_os = "linux")]
    if appimage::ready() {
        return "Restart to update";
    }
    #[cfg(target_os = "macos")]
    if macos::ready() {
        return "Restart now";
    }
    #[cfg(windows)]
    if windows::ready() {
        return "Restart now";
    }
    "Check for Updates…"
}
pub(crate) fn restart_now(cx: &mut App) {
    #[cfg(target_os = "linux")]
    appimage::restart(cx);
    #[cfg(windows)]
    windows::restart(cx);
    #[cfg(target_os = "macos")]
    macos::restart(cx);
    let _ = cx;
}

pub(crate) fn activate(cx: &mut App) {
    #[cfg(target_os = "linux")]
    if appimage::ready() {
        appimage::restart(cx);
        return;
    }
    #[cfg(windows)]
    if windows::ready() {
        windows::restart(cx);
        return;
    }
    #[cfg(target_os = "macos")]
    if macos::ready() {
        macos::restart(cx);
        return;
    }
    let _ = cx;
    check();
}

/// Explicit download intent from About/Settings. Installation is a separate click.
pub(crate) fn install_update() {
    run_check(Trigger::Download);
}

pub(crate) fn ready_notice(window: &mut gpui::Window, cx: &mut App) {
    // Every platform's notice restarts into the newest ready update, not the
    // package that was current when the notice was drawn (#2679).
    whats_new::notice(window, cx);
    #[cfg(target_os = "linux")]
    let package = appimage::ready_package();
    #[cfg(windows)]
    let package = windows::ready_package();
    #[cfg(target_os = "macos")]
    let package = macos::ready_package();
    #[cfg(any(target_os = "linux", windows, target_os = "macos"))]
    {
        let package = package.map(|package| format!("{}:{package}", channel()));
        announce_ready(&status::get(), package.as_deref(), window, cx, restart_now);
    }
    #[cfg(all(target_os = "linux", feature = "updater-ui-harness"))]
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        static SHOWN: AtomicBool = AtomicBool::new(false);
        if std::env::var_os("OKILUM_UPDATE_READY_FIXTURE").is_some()
            && !SHOWN.swap(true, Ordering::SeqCst)
        {
            show_ready_notice(window, cx, |_| {});
        }
    }
    let _ = (window, cx);
}

/// A Ready announcement belongs to a window, not to the whole application.
/// Otherwise the first restored window consumes the other windows' notice.
#[cfg(any(target_os = "linux", windows, target_os = "macos", test))]
#[derive(Default)]
struct ReadyAnnouncements {
    windows: std::collections::HashMap<gpui::WindowId, restart_feedback::Announced>,
}

#[cfg(any(target_os = "linux", windows, target_os = "macos", test))]
impl gpui::Global for ReadyAnnouncements {}

/// Each engine supplies only its verified pending package identity. Every
/// window gets that package, including inactive and newly opened windows.
/// A dismissal stays local; redraws do not resurrect the same notice.
#[cfg(any(target_os = "linux", windows, target_os = "macos", test))]
fn announce_ready(
    status: &status::CheckStatus,
    package: Option<&str>,
    window: &mut gpui::Window,
    cx: &mut App,
    restart: impl Fn(&mut App) + 'static,
) {
    use status::CheckStatus;
    let live_windows: Vec<_> = cx
        .windows()
        .iter()
        .map(|handle| handle.window_id())
        .collect();
    let window_id = window.window_handle().window_id();
    let announcements = cx.default_global::<ReadyAnnouncements>();
    // Keep the current window even when GPUI temporarily takes it out of the
    // application during its update. Closed windows retain no announcement state.
    announcements
        .windows
        .retain(|id, _| *id == window_id || live_windows.contains(id));
    let announced = announcements.windows.entry(window_id).or_default();
    let show = match status {
        CheckStatus::Ready | CheckStatus::RestartRetry(_) => {
            package.is_some_and(|package| announced.claim(package))
        }
        // Transient states while a ready package is verified or replaced.
        CheckStatus::Checking
        | CheckStatus::Available(_)
        | CheckStatus::Downloading(_)
        | CheckStatus::Preparing
        | CheckStatus::Restarting => false,
        _ => {
            announced.reset();
            false
        }
    };
    if show {
        show_ready_notice(window, cx, restart);
    }
}

/// «Update ready» with Restart, in the one update notice (#683, #2679).
#[cfg(any(windows, test, target_os = "linux", target_os = "macos"))]
fn show_ready_notice(
    window: &mut gpui::Window,
    cx: &mut App,
    restart: impl Fn(&mut App) + 'static,
) {
    restart_feedback::show_ready(window, cx, restart);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::{AppContext, TestAppContext};
    use gpui_component::{Root, WindowExt as _};

    /// A window whose content draws the notification layer, as Reader does.
    struct Empty;
    impl gpui::Render for Empty {
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
    fn window_activation_reaches_the_shared_scheduler_without_polling(cx: &mut TestAppContext) {
        use std::{cell::RefCell, rc::Rc};
        let elapsed = Rc::new(std::cell::Cell::new(Duration::ZERO));
        let starts = Rc::new(std::cell::Cell::new(0));
        let schedule = Rc::new(RefCell::new(Schedule::new(
            Duration::ZERO,
            scheduler::MAX_INTERVAL,
        )));
        // Startup has already checked. New roots and focus storms share that
        // timestamp, independent of GPUI's test executor clock.
        schedule.borrow_mut().run(
            Duration::ZERO,
            Trigger::Manual,
            true,
            scheduler::MAX_INTERVAL,
            || true,
        );
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            crate::test_init(cx);
            observe_window_focus(cx, {
                let elapsed = elapsed.clone();
                let starts = starts.clone();
                move || {
                    schedule.borrow_mut().run(
                        elapsed.get(),
                        Trigger::Focus,
                        true,
                        scheduler::MAX_INTERVAL,
                        || {
                            starts.set(starts.get() + 1);
                            true
                        },
                    );
                }
            });
        });
        let (_, first) = cx.add_window_view(|window, cx| {
            let view = cx.new(|_| Empty);
            Root::new(view, window, cx)
        });
        first.run_until_parked();
        assert_eq!(starts.get(), 0);
        elapsed.set(Duration::from_secs(5 * 60));
        first.deactivate_window();
        first.run_until_parked();
        assert_eq!(starts.get(), 0, "deactivation must not request an update");
        first.update(|window, _| window.activate_window());
        first.run_until_parked();
        assert_eq!(starts.get(), 1, "positive control: activation really fired");
        let (_, second) = cx.add_window_view(|window, cx| {
            let view = cx.new(|_| Empty);
            Root::new(view, window, cx)
        });
        second.update(|window, _| window.activate_window());
        second.run_until_parked();
        assert_eq!(starts.get(), 1, "a second window shares the throttle");
        elapsed.set(Duration::from_secs(10 * 60));
        second.deactivate_window();
        second.update(|window, _| window.activate_window());
        second.run_until_parked();
        assert_eq!(starts.get(), 2);
    }

    #[gpui::test]
    fn gui_startup_invokes_receipt_writer_after_window_creation(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let receipt = directory.path().join("started");
        cx.update(|cx| {
            with_startup_window(cx, || {
                std::fs::write(&receipt, b"startup confirmed\n").unwrap()
            });
            assert!(
                !receipt.exists(),
                "headless startup cannot confirm an update"
            );
            cx.set_reduce_motion(true);
            crate::test_init(cx);
        });
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|_| Empty);
            Root::new(view, window, cx)
        });
        // Exercise the same ordinary GUI startup boundary; no focus or native
        // activation event is supplied. The real receipt writer's permissions,
        // uid/path and compiled identity are covered by the producer roundtrip.
        visual.update(|_, cx| {
            with_startup_window(cx, || {
                std::fs::write(&receipt, b"startup confirmed\n").unwrap()
            });
        });
        assert_eq!(std::fs::read(receipt).unwrap(), b"startup confirmed\n");
    }

    /// #683: the notice used to vanish after 4 s, so an update downloaded while
    /// the user looked elsewhere passed unnoticed. It now stays until closed.
    #[gpui::test]
    fn update_ready_notice_stays_until_the_user_closes_it(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            crate::test_init(cx);
        });
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|_| Empty);
            Root::new(view, window, cx)
        });
        let restarted = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = restarted.clone();
        visual.update(|window, cx| show_ready_notice(window, cx, move |_| flag.set(true)));
        visual.run_until_parked();
        visual.update(|window, cx| assert_eq!(window.notifications(cx).len(), 1));
        visual
            .executor()
            .advance_clock(std::time::Duration::from_secs(60));
        visual.run_until_parked();
        visual.update(|window, cx| {
            assert_eq!(
                window.notifications(cx).len(),
                1,
                "still there after a minute"
            )
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let restart = visual
            .debug_bounds("restart-update")
            .expect("the Restart action is drawn")
            .center();
        visual.simulate_click(restart, gpui::Modifiers::default());
        visual.run_until_parked();
        assert!(restarted.get(), "Restart runs the restart");
    }

    fn notice_window(cx: &mut TestAppContext) -> &mut gpui::VisualTestContext {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            crate::test_init(cx);
        });
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|_| Empty);
            Root::new(view, window, cx)
        });
        visual.update(|window, _| window.activate_window());
        visual.run_until_parked();
        visual
    }

    fn notice_count(visual: &mut gpui::VisualTestContext) -> usize {
        visual.update(|window, cx| window.notifications(cx).len())
    }

    /// #2679: beta ships on every merge, so several updates become ready while
    /// Okilum is open. Each replaces the one notice; none stacks, and a newer
    /// package is announced even after the older notice was closed.
    #[gpui::test]
    fn consecutive_ready_updates_show_one_notice_for_the_newest(cx: &mut TestAppContext) {
        use std::{cell::RefCell, rc::Rc};
        let visual = notice_window(cx);
        let restarted = Rc::new(RefCell::new(Vec::<&str>::new()));
        let ready = status::CheckStatus::Ready;
        let announce = |visual: &mut gpui::VisualTestContext,
                        status: &status::CheckStatus,
                        package: &'static str| {
            let restarted = restarted.clone();
            visual.update(|window, cx| {
                announce_ready(status, Some(package), window, cx, move |_| {
                    restarted.borrow_mut().push(package)
                })
            });
            visual.run_until_parked();
        };
        announce(visual, &ready, "1");
        assert_eq!(notice_count(visual), 1);
        announce(visual, &ready, "1");
        assert_eq!(
            notice_count(visual),
            1,
            "the same package is announced once"
        );
        // Verification and the replacement download pass through transient
        // states; none of them stacks a second notice.
        for transient in [
            status::CheckStatus::Preparing,
            status::CheckStatus::Checking,
            status::CheckStatus::Downloading(Some(40)),
        ] {
            announce(visual, &transient, "1");
        }
        announce(visual, &ready, "2");
        assert_eq!(
            notice_count(visual),
            1,
            "a newer update replaces the notice in place"
        );
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let restart = visual
            .debug_bounds("restart-update")
            .expect("the newest notice offers Restart now")
            .center();
        visual.simulate_click(restart, gpui::Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            *restarted.borrow(),
            ["2"],
            "Restart targets the newest update"
        );

        // Closing a notice keeps that package quiet, never a newer one.
        visual
            .update(|window, cx| window.remove_notification::<restart_feedback::UpdateNotice>(cx));
        visual
            .executor()
            .advance_clock(std::time::Duration::from_secs(1));
        visual.run_until_parked();
        announce(visual, &ready, "2");
        assert_eq!(
            notice_count(visual),
            0,
            "a closed notice stays closed for its package"
        );
        announce(visual, &ready, "3");
        assert_eq!(
            notice_count(visual),
            1,
            "a newer package is announced after a closed one"
        );
    }

    /// #2719: one restored window must not consume another window's Ready.
    #[gpui::test]
    fn ready_announcements_belong_to_each_window(cx: &mut TestAppContext) {
        use std::{cell::Cell, rc::Rc};
        let app = cx.clone();
        let mut first_app = cx.clone();
        let mut later_app = cx.clone();
        let first = notice_window(&mut first_app);
        let second = notice_window(cx);
        first.deactivate_window();
        assert!(!first.update(|window, _| window.is_window_active()));
        second.update(|window, cx| {
            announce_ready(&status::CheckStatus::Ready, None, window, cx, |_| {
                panic!("an unverified package cannot authorize Restart")
            });
        });
        second.run_until_parked();
        assert_eq!(
            notice_count(second),
            0,
            "status alone cannot announce an unverified package"
        );
        let restarted = Rc::new(Cell::new(0));
        let announce = |visual: &mut gpui::VisualTestContext, package: &'static str| {
            let restarted = restarted.clone();
            visual.update(|window, cx| {
                announce_ready(
                    &status::CheckStatus::Ready,
                    Some(package),
                    window,
                    cx,
                    move |_| restarted.set(if package == "2" { 2 } else { 1 }),
                );
            });
            visual.run_until_parked();
        };
        announce(second, "1");
        announce(first, "1");
        assert_eq!(
            notice_count(first),
            1,
            "each restored window must show its ready notice"
        );
        assert_eq!(notice_count(second), 1);

        first.update(|window, cx| window.remove_notification::<restart_feedback::UpdateNotice>(cx));
        first.executor().advance_clock(Duration::from_secs(1));
        first.run_until_parked();
        announce(first, "1");
        assert_eq!(notice_count(first), 0, "dismissal stays local and stable");
        assert_eq!(notice_count(second), 1);

        let later = notice_window(&mut later_app);
        announce(later, "1");
        assert_eq!(
            notice_count(later),
            1,
            "a window opened while Ready gets its own notice"
        );
        for visual in [&mut *first, &mut *second, &mut *later] {
            announce(visual, "2");
            assert_eq!(notice_count(visual), 1, "the new package replaces in place");
            visual.update(|window, cx| window.draw(cx).clear(cx));
            assert!(visual.debug_bounds("restart-update").is_some());
        }
        first.update(|window, _| window.activate_window());
        first.run_until_parked();
        let restart = first.debug_bounds("restart-update").unwrap().center();
        first.simulate_click(restart, gpui::Modifiers::default());
        first.run_until_parked();
        assert_eq!(restarted.get(), 2, "the current action is wired to Restart");

        later.update(|window, _| window.remove_window());
        first.run_until_parked();
        announce(first, "2");
        assert_eq!(
            app.read(|cx| cx.global::<ReadyAnnouncements>().windows.len()),
            2,
            "closed windows retain no announcement state"
        );
    }

    /// #2679: «Update ready» and «This update is no longer ready» were stacked.
    /// Restart progress and failures now render into the same single notice.
    #[gpui::test]
    fn replacement_ready_is_not_overwritten_by_queued_download_progress(cx: &mut TestAppContext) {
        let _guard = restart_feedback::TEST_LOCK.lock().unwrap();
        let app = cx.clone();
        let mut other_app = cx.clone();
        let other = notice_window(&mut other_app);
        let visual = notice_window(cx);
        visual.update(|window, cx| show_ready_notice(window, cx, |_| {}));
        other.update(|window, cx| show_ready_notice(window, cx, |_| {}));
        visual.run_until_parked();
        other.deactivate_window();
        restart_feedback::notify("downloading", "Downloading the latest update…");
        app.update(restart_feedback::present);
        visual.run_until_parked();
        other.update(|window, cx| window.draw(cx).clear(cx));
        assert!(other.debug_bounds("restart-update").is_none());
        restart_feedback::notify("downloading", "Downloading the latest update…");
        // The worker can complete before the one-second presenter tick.
        restart_feedback::latest_ready();
        app.update(restart_feedback::present);
        visual.run_until_parked();
        for window in [visual, other] {
            assert_eq!(notice_count(window), 1);
            window.update(|window, cx| window.draw(cx).clear(cx));
            assert!(
                window.debug_bounds("restart-update").is_some(),
                "the newest Restart action must replace progress even in an inactive window"
            );
            assert!(window.debug_bounds("update-notice-action").is_none());
        }
    }

    #[gpui::test]
    fn restart_outcomes_replace_the_ready_notice_in_place(cx: &mut TestAppContext) {
        let _guard = restart_feedback::TEST_LOCK.lock().unwrap();
        let app = cx.clone();
        let visual = notice_window(cx);
        visual.update(|window, cx| show_ready_notice(window, cx, |_| {}));
        visual.run_until_parked();
        assert_eq!(notice_count(visual), 1);

        restart_feedback::attempt();
        restart_feedback::notify("waiting", "Downloading the latest update.");
        app.update(restart_feedback::present);
        visual.run_until_parked();
        assert_eq!(
            notice_count(visual),
            1,
            "progress replaces the ready notice"
        );
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(visual.debug_bounds("restart-update").is_none());
        assert!(
            visual.debug_bounds("update-notice-action").is_none(),
            "progress offers no action"
        );

        restart_feedback::refused("Could not restart to update: disk full.");
        app.update(restart_feedback::present);
        visual.run_until_parked();
        assert_eq!(
            notice_count(visual),
            1,
            "a failure stays in the same notice"
        );
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            visual.debug_bounds("update-notice-action").is_some(),
            "a failure offers exactly one action"
        );

        visual.update(|window, cx| show_ready_notice(window, cx, |_| {}));
        visual.run_until_parked();
        assert_eq!(
            notice_count(visual),
            1,
            "a newer ready update replaces the failure"
        );
    }
}

#[cfg(test)]
mod progress_ui_tests;

#[cfg(all(test, target_os = "macos"))]
pub(crate) use macos::protect_verified_install;

pub(crate) fn can_cancel_update() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::can_cancel()
    }
    #[cfg(any(target_os = "linux", windows))]
    return policy::DOWNLOAD.active();
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    false
}
pub(crate) fn cancel_update() {
    // Cancellation must revoke explicit restart consent even when the worker
    // won the download-completion race and already cleared its token.
    #[cfg(target_os = "linux")]
    appimage::cancel_requested_restart();
    #[cfg(windows)]
    windows::cancel_requested_restart();
    #[cfg(any(target_os = "linux", windows))]
    policy::DOWNLOAD.cancel();
    #[cfg(target_os = "macos")]
    macos::cancel();
}
