//! One line that identifies this build in a bug report (#1097): version, build,
//! update channel, OS with its version, architecture, how Okilum was installed
//! and the source commit. About shows a compact form and copies the full line;
//! Help → Copy Version Info and the diagnostic log carry the same line.
use gpui::{App, ClipboardItem, Menu, MenuItem};

gpui::actions!(okilum, [CopyVersionInfo]);

pub(crate) fn install(cx: &mut App) {
    cx.on_action(|_: &CopyVersionInfo, cx| copy(cx));
}

pub(crate) fn copy(cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(report()));
}

pub(crate) fn help_menu() -> Menu {
    Menu {
        name: "Help".into(),
        items: vec![MenuItem::action("Copy Version Info", CopyVersionInfo)],
        disabled: false,
    }
}

/// `Okilum 0.1.10467 (build 10467, beta channel) — macOS 15.6 arm64 — app in
/// /Applications — source beed5ef1`
pub(crate) fn report() -> String {
    format!(
        "Okilum {} (build {}, {}) — {} {} — {} — source {}",
        env!("OKILUM_RELEASE_VERSION"),
        env!("OKILUM_BUILD_VERSION"),
        channel(&crate::updater::channel()),
        os(),
        arch(),
        install_kind(),
        source(env!("OKILUM_SOURCE_COMMIT")),
    )
}

/// The compact form for the visible About line: `macOS arm64`.
pub(crate) fn platform() -> String {
    let os = if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(windows) {
        "Windows"
    } else {
        "Linux"
    };
    format!("{os} {}", arch())
}

fn channel(channel: &str) -> String {
    match channel {
        "Beta" | "Stable" => format!("{} channel", channel.to_lowercase()),
        other => other.to_lowercase(),
    }
}

fn arch() -> &'static str {
    arch_name(std::env::consts::ARCH, cfg!(windows))
}

fn arch_name(arch: &'static str, windows: bool) -> &'static str {
    match arch {
        "aarch64" => "arm64",
        "x86_64" if windows => "x64",
        other => other,
    }
}

fn source(commit: &str) -> String {
    if commit.len() >= 8 && commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        commit[..8].to_owned()
    } else {
        commit.to_owned()
    }
}

#[cfg(target_os = "linux")]
fn os() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|text| os_release_name(&text))
        .unwrap_or_else(|| "Linux".into())
}

/// PRETTY_NAME, else NAME with VERSION_ID; quotes removed.
#[cfg(any(target_os = "linux", test))]
fn os_release_name(text: &str) -> Option<String> {
    let field = |key: &str| {
        text.lines().find_map(|line| {
            let value = line.strip_prefix(key)?.strip_prefix('=')?;
            let value = value.trim().trim_matches('"').trim_matches('\'');
            (!value.is_empty()).then(|| value.to_owned())
        })
    };
    field("PRETTY_NAME").or_else(|| {
        let name = field("NAME")?;
        Some(match field("VERSION_ID") {
            Some(version) => format!("{name} {version}"),
            None => name,
        })
    })
}

#[cfg(target_os = "macos")]
fn os() -> String {
    let version = objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersion();
    if version.patchVersion > 0 {
        format!(
            "macOS {}.{}.{}",
            version.majorVersion, version.minorVersion, version.patchVersion
        )
    } else {
        format!("macOS {}.{}", version.majorVersion, version.minorVersion)
    }
}

#[cfg(windows)]
fn os() -> String {
    let build = windows_value("CurrentBuildNumber");
    let release = windows_value("DisplayVersion");
    windows_name(build.as_deref(), release.as_deref())
}

/// The registry's ProductName still says "Windows 10" on Windows 11, so the
/// name comes from the build number (Windows 11 starts at 22000).
#[cfg(any(windows, test))]
fn windows_name(build: Option<&str>, release: Option<&str>) -> String {
    let number = build.and_then(|b| b.parse::<u32>().ok());
    let mut name = match number {
        Some(n) if n >= 22000 => "Windows 11".to_owned(),
        Some(_) => "Windows 10".to_owned(),
        None => "Windows".to_owned(),
    };
    if let Some(release) = release.filter(|r| !r.is_empty()) {
        name.push(' ');
        name.push_str(release);
    }
    if let Some(build) = build {
        name.push_str(&format!(" (build {build})"));
    }
    name
}

#[cfg(windows)]
fn windows_value(name: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let key = wide(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    let value = wide(name);
    let mut buffer = [0u16; 128];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    // SAFETY: both names are NUL-terminated; the buffer and its byte size match.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..len]))
}

/// How this copy was installed, without personal paths.
fn install_kind() -> String {
    #[cfg(windows)]
    {
        if crate::updater::channel() == "Portable" {
            "portable".into()
        } else {
            "installer".into()
        }
    }
    #[cfg(target_os = "linux")]
    {
        use crate::updater::linux_repo::{current, Source};
        let exe = std::env::current_exe().ok();
        if exe.as_deref() != Some(std::path::Path::new("/usr/bin/okilum")) {
            return "standalone binary".into();
        }
        match current() {
            Source::Beta => "pacman okilum-beta".into(),
            Source::Stable => "pacman okilum-stable".into(),
            Source::Both => "pacman okilum-beta and okilum-stable".into(),
            Source::Unknown => "pacman package".into(),
        }
    }
    #[cfg(target_os = "macos")]
    {
        let exe = std::env::current_exe().ok();
        // Okilum.app/Contents/MacOS/okilum
        match exe.as_deref().and_then(|exe| exe.ancestors().nth(3)) {
            Some(app) if app.extension().is_some_and(|e| e == "app") => {
                if app.starts_with("/Applications") {
                    "app in /Applications".into()
                } else {
                    "app outside /Applications".into()
                }
            }
            _ => "standalone binary".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn report_is_one_line_with_every_part() {
        let line = report();
        assert!(!line.contains('\n'), "{line}");
        assert!(line.starts_with("Okilum "), "{line}");
        for part in [" (build ", " — ", " — source "] {
            assert!(line.contains(part), "{part}: {line}");
        }
        assert!(line.contains(arch()), "{line}");
        assert!(platform().ends_with(arch()));
    }

    #[test]
    fn parts_are_named_for_people() {
        assert_eq!(channel("Beta"), "beta channel");
        assert_eq!(channel("Development"), "development");
        assert_eq!(arch_name("aarch64", false), "arm64");
        assert_eq!(arch_name("x86_64", true), "x64");
        assert_eq!(arch_name("x86_64", false), "x86_64");
        assert_eq!(
            source("beed5ef1a2b3c4d5e6f708192a3b4c5d6e7f8091"),
            "beed5ef1"
        );
        assert_eq!(source("unknown (development)"), "unknown (development)");
    }

    #[test]
    fn windows_11_is_named_from_the_build_not_the_product_name() {
        assert_eq!(
            windows_name(Some("22631"), Some("23H2")),
            "Windows 11 23H2 (build 22631)"
        );
        // Positive control: Windows 10 builds stay Windows 10.
        assert_eq!(
            windows_name(Some("19045"), Some("22H2")),
            "Windows 10 22H2 (build 19045)"
        );
        assert_eq!(windows_name(None, None), "Windows");
    }

    #[test]
    fn linux_names_come_from_os_release() {
        assert_eq!(
            os_release_name("NAME=\"Arch Linux\"\nPRETTY_NAME=\"Arch Linux\"\nID=arch\n")
                .as_deref(),
            Some("Arch Linux")
        );
        assert_eq!(
            os_release_name("NAME=Fedora\nVERSION_ID=40\n").as_deref(),
            Some("Fedora 40")
        );
        assert_eq!(os_release_name("ID=unknown\n"), None);
    }
}
