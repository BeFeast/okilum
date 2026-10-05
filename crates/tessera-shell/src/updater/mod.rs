//! macOS updates through stock Sparkle 2: its standard UI does the checking,
//! downloading, installing and relaunching. The app only adds menu items and
//! the beta channel preference.
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

use gpui::{App, MenuItem};

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
    windows::start();
    #[cfg(target_os = "macos")]
    {
        macos::start();
        _cx.on_action(|_: &AboutTessera, cx| crate::about::show_from_menu(cx));
        _cx.on_action(|_: &CheckForUpdates, _| macos::check());
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
