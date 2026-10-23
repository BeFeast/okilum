//! macOS updates through stock Sparkle 2: its standard UI downloads, installs
//! and relaunches. A manual check only probes and reports inline (#995). The
//! app adds menu items and the beta channel preference.
#[cfg(any(target_os = "linux", test))]
pub(crate) mod linux_repo;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(windows, test))]
mod ready;
pub(crate) mod status;
pub(crate) mod whats_new;
#[cfg(windows)]
mod windows;
#[cfg(any(windows, test))]
mod windows_feed;

use gpui::{App, MenuItem};

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

/// How often a running Windows install checks its channel again.
#[cfg(windows)]
const RECHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// Start Sparkle before the menus are built, so they reflect its availability.
pub(crate) fn install(_cx: &mut App) {
    crate::reader_settings::install(_cx);
    whats_new::install(_cx);
    #[cfg(windows)]
    {
        windows::start();
        // An Okilum left open for days still learns of a new release (#683).
        _cx.spawn(async move |cx| loop {
            cx.background_executor().timer(RECHECK_EVERY).await;
            windows::check(false);
        })
        .detach();
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
        // The result is inline, so the menu opens About to show it (#995).
        _cx.on_action(|_: &CheckForUpdates, cx| {
            crate::about::show_from_menu(cx);
            macos::check();
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
    #[cfg(not(any(target_os = "macos", windows)))]
    false
}

/// Windows, before `VelopackApp::run`: whether a pending package may be applied
/// at this start (#1318).
#[cfg(windows)]
pub(crate) fn apply_pending_at_start() -> bool {
    windows::apply_pending_at_start()
}

pub(crate) fn check() {
    #[cfg(target_os = "macos")]
    macos::check();
    #[cfg(windows)]
    windows::check(true);
}

pub(crate) fn channel() -> &'static str {
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
            macos::check();
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
            windows::check(true);
        }
        cx.refresh_windows();
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = (enabled, cx);
}

/// The same explicit action is exposed in About, Settings and the ready toast.
pub(crate) fn action_label() -> &'static str {
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
    #[cfg(windows)]
    windows::restart(None, cx);
    #[cfg(target_os = "macos")]
    macos::restart(cx);
    let _ = cx;
}

pub(crate) fn activate(cx: &mut App) {
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

/// The status row's «Install Update…» for a found update: Sparkle's own window
/// downloads, installs and relaunches.
pub(crate) fn install_update() {
    #[cfg(target_os = "macos")]
    macos::install();
}

pub(crate) fn ready_notice(window: &mut gpui::Window, cx: &mut App) {
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
        if !macos::ready() {
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
#[cfg(any(
    windows,
    target_os = "macos",
    test,
    all(target_os = "linux", feature = "updater-ui-harness")
))]
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

    /// #683: the notice used to vanish after 4 s, so an update downloaded while
    /// the user looked elsewhere passed unnoticed. It now stays until closed.
    #[gpui::test]
    fn update_ready_notice_stays_until_the_user_closes_it(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
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
