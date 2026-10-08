//! Platform updates: macOS manual-check results are inline; Sparkle retains installation UI.
mod check_status;
pub(crate) use check_status::CheckStatus;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(windows, test))]
mod ready;
#[cfg(windows)]
mod windows;
#[cfg(any(windows, test))]
mod windows_feed;

use gpui::{App, MenuItem};

#[cfg(target_os = "macos")]
#[derive(Default)]
struct CheckWatcher(bool);
#[cfg(target_os = "macos")]
impl gpui::Global for CheckWatcher {}

#[cfg(target_os = "macos")]
gpui::actions!(tessera, [AboutTessera, CheckForUpdates, ToggleBetaBuilds]);

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
        MenuItem::action("About Tessera", AboutTessera),
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

/// Start Sparkle before the menus are built, so they reflect its availability.
pub(crate) fn install(_cx: &mut App) {
    crate::reader_settings::install(_cx);
    #[cfg(windows)]
    {
        windows::start();
        _cx.spawn(async move |cx| {
            let mut previous = None;
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                let current = windows::ready_package();
                if current != previous {
                    previous = current;
                    cx.update(|cx| cx.refresh_windows());
                }
            }
        })
        .detach();
    }
    #[cfg(target_os = "macos")]
    {
        _cx.set_global(CheckWatcher::default());
        macos::start();
        _cx.on_action(|_: &AboutTessera, cx| crate::about::show_from_menu(cx));
        _cx.on_action(|_: &CheckForUpdates, cx| activate(cx));
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
        "Windows diagnostic"
    } else if windows::beta() {
        "Beta"
    } else {
        "Stable"
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    if env!("TESSERA_BUILD_VERSION") == "development" {
        "Development"
    } else {
        "System packages"
    }
}

/// Shares Sparkle's persisted channel preference with Settings and the menu.
pub(crate) fn set_beta(enabled: bool, cx: &mut App) {
    #[cfg(target_os = "macos")]
    {
        macos::set_beta(enabled);
        #[cfg(feature = "brain")]
        crate::brain::app_quit::set_menus(cx);
        #[cfg(not(feature = "brain"))]
        crate::reader_app_menu::set_menus(cx);
        cx.refresh_windows();
    }
    #[cfg(windows)]
    {
        windows::set_beta(enabled);
        cx.refresh_windows();
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = (enabled, cx);
}

/// The same explicit action is exposed in About, Settings and the ready toast.
pub(crate) fn action_label() -> &'static str {
    #[cfg(target_os = "macos")]
    match check_status() {
        CheckStatus::Checking | CheckStatus::Busy => return "Checking for updates…",
        CheckStatus::Failed => return "Retry update check",
        _ => {}
    }
    #[cfg(windows)]
    if windows::ready() {
        return "Restart to update";
    }
    "Check for Updates…"
}
pub(crate) fn activate(cx: &mut App) {
    #[cfg(windows)]
    if windows::ready() {
        windows::restart(None, cx);
        return;
    }
    #[cfg(target_os = "macos")]
    {
        crate::reader_settings::show_updates(cx);
        if check_status().checking() {
            return;
        }
        check();
        cx.refresh_windows();
        if cx.global::<CheckWatcher>().0 {
            return;
        }
        cx.global_mut::<CheckWatcher>().0 = true;
        cx.spawn(async move |cx| {
            let mut previous = CheckStatus::Idle;
            loop {
                let current = cx.update(|cx| {
                    let current = check_status();
                    if current != previous {
                        cx.refresh_windows();
                    }
                    current
                });
                previous = current;
                if !current.tracking() {
                    cx.update(|cx| cx.global_mut::<CheckWatcher>().0 = false);
                    break;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(250))
                    .await;
            }
        })
        .detach();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = cx;
        check();
    }
}

pub(crate) fn check_status() -> CheckStatus {
    #[cfg(all(target_os = "linux", feature = "updater-ui-harness"))]
    if let Some(value) = std::env::var("TESSERA_UPDATE_CHECK_FIXTURE")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return CheckStatus::from_native(value);
    }
    #[cfg(target_os = "macos")]
    return macos::status();
    #[cfg(not(target_os = "macos"))]
    CheckStatus::from_native(0)
}

pub(crate) fn ready_notice(window: &mut gpui::Window, cx: &mut App) {
    #[cfg(windows)]
    if window.is_window_active() {
        if let Some(id) = windows::take_announcement() {
            show_ready_notice(window, cx, move |cx| windows::restart(Some(&id), cx));
        }
    }
    #[cfg(all(target_os = "linux", feature = "updater-ui-harness"))]
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        static SHOWN: AtomicBool = AtomicBool::new(false);
        if std::env::var_os("TESSERA_UPDATE_READY_FIXTURE").is_some()
            && !SHOWN.swap(true, Ordering::SeqCst)
        {
            show_ready_notice(window, cx, |_| {});
        }
    }
    let _ = (window, cx);
}

#[cfg(any(windows, all(target_os = "linux", feature = "updater-ui-harness")))]
fn show_ready_notice(
    window: &mut gpui::Window,
    cx: &mut App,
    restart: impl Fn(&mut App) + 'static,
) {
    use gpui::AppContext;
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
                        .small()
                        .label("Restart")
                        .on_click(move |_, _, cx| restart(cx))
                }),
            cx,
        );
        let handle = window.window_handle();
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(4))
                .await;
            let _ = cx.update_window(handle, |_, window, cx| {
                window.remove_notification::<UpdateNotice>(cx);
            });
        })
        .detach();
    });
}
