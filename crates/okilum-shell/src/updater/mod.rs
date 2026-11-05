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
    return if appimage::ready() {
        // The staged AppImage holds the installation lock until restart. Keep
        // Ready intact rather than trying to stage the same archive again.
        false
    } else {
        appimage::check(mode().download(matches!(trigger, Trigger::Download)))
    };
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
    cx.spawn(async move |cx| loop {
        // Ticks only consult the policy; the network runs once per due interval.
        cx.background_executor().timer(Duration::from_secs(1)).await;
        run_check(Trigger::Interval);
        cx.update(restart_feedback::present);
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

/// Windows, before `VelopackApp::run`: whether a pending package may be applied
/// at this start (#1318).
#[cfg(windows)]
pub(crate) fn apply_pending_at_start() -> bool {
    windows::apply_pending_at_start()
}

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
    return if !windows::available() {
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
    windows::restart(None, cx);
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
        windows::restart(None, cx);
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
    #[cfg(target_os = "linux")]
    if window.is_window_active() && appimage::take_announcement() {
        show_ready_notice(window, cx, appimage::restart);
    }
    whats_new::notice(window, cx);
    #[cfg(windows)]
    if window.is_window_active() {
        if let Some(id) = windows::take_announcement() {
            show_ready_notice(window, cx, move |cx| windows::restart(Some(&id), cx));
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        static ANNOUNCED: AtomicBool = AtomicBool::new(false);
        if !matches!(
            status::get(),
            status::CheckStatus::Ready | status::CheckStatus::RestartRetry(_)
        ) {
            ANNOUNCED.store(false, Ordering::Relaxed);
        } else if window.is_window_active() && !ANNOUNCED.swap(true, Ordering::Relaxed) {
            show_ready_notice(window, cx, macos::restart);
        }
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

/// «Update ready» with Restart (#683). It stays until the user closes it or
/// restarts: a downloaded update must not pass unnoticed. Closed, it does not
/// return for the same package until the next launch.
#[cfg(any(windows, test, target_os = "linux", target_os = "macos"))]
fn show_ready_notice(
    window: &mut gpui::Window,
    cx: &mut App,
    restart: impl Fn(&mut App) + 'static,
) {
    use gpui::InteractiveElement as _;
    use gpui_component::button::Button;
    use gpui_component::{notification::Notification, Sizable, WindowExt};
    use std::rc::Rc;
    struct UpdateNotice;
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
}

#[cfg(test)]
mod progress_ui_tests;

#[cfg(all(test, target_os = "macos"))]
pub(crate) use macos::protect_verified_install;

pub(crate) fn can_cancel_update() -> bool {
    #[cfg(target_os = "macos")]
    {
        return macos::can_cancel();
    }
    #[cfg(any(target_os = "linux", windows))]
    return policy::DOWNLOAD.active();
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    false
}
pub(crate) fn cancel_update() {
    #[cfg(any(target_os = "linux", windows))]
    policy::DOWNLOAD.cancel();
    #[cfg(target_os = "macos")]
    macos::cancel();
}
