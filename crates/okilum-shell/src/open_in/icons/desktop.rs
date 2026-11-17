//! Linux: the editor's desktop entry names its icon; the icon theme turns the
//! name into a file (freedesktop Icon Theme spec, current theme then its
//! `Inherits` then `hicolor`, then `pixmaps`). Entries are only read, never
//! executed.
use std::path::{Path, PathBuf};

/// Preferred menu raster: 16 px at up to 4× scale.
const WANTED: u32 = 64;
/// Fallback layout when a theme folder has no readable `index.theme`
/// (Flatpak and Snap exports ship bare `hicolor` trees).
const HICOLOR_DIRS: &[&str] = &[
    "64x64/apps",
    "scalable/apps",
    "48x48/apps",
    "128x128/apps",
    "256x256/apps",
    "512x512/apps",
    "32x32/apps",
    "24x24/apps",
    "16x16/apps",
];

/// Where to look; injectable for tests.
pub(super) struct Search {
    /// `$XDG_DATA_HOME`, `$XDG_DATA_DIRS`, Flatpak and Snap exports.
    pub data_dirs: Vec<PathBuf>,
    /// The user's icon theme, if configured.
    pub theme: Option<String>,
}

impl Search {
    pub(super) fn current() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut data_dirs = Vec::new();
        match std::env::var_os("XDG_DATA_HOME") {
            Some(dir) if !dir.is_empty() => data_dirs.push(PathBuf::from(dir)),
            _ => data_dirs.extend(home.as_ref().map(|h| h.join(".local/share"))),
        }
        data_dirs.extend(
            home.as_ref()
                .map(|h| h.join(".local/share/flatpak/exports/share")),
        );
        let system = std::env::var_os("XDG_DATA_DIRS")
            .filter(|dirs| !dirs.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        data_dirs.extend(std::env::split_paths(&system));
        data_dirs.push("/var/lib/flatpak/exports/share".into());
        data_dirs.push("/var/lib/snapd/desktop".into());
        let config = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
            _ => home.map(|h| h.join(".config")),
        };
        Search {
            data_dirs,
            theme: config.as_deref().and_then(configured_theme),
        }
    }
}

/// GTK 4/3 settings, then KDE's `kdeglobals`.
fn configured_theme(config: &Path) -> Option<String> {
    let read = |rel: &str, group: &str, key: &str| {
        let text = std::fs::read_to_string(config.join(rel)).ok()?;
        ini_value(&text, group, key).map(str::to_owned)
    };
    read("gtk-4.0/settings.ini", "Settings", "gtk-icon-theme-name")
        .or_else(|| read("gtk-3.0/settings.ini", "Settings", "gtk-icon-theme-name"))
        .or_else(|| read("kdeglobals", "Icons", "Theme"))
        .filter(|theme| plain_name(theme))
}

/// `key=value` of `[group]`, ignoring localized keys and other groups.
fn ini_value<'a>(text: &'a str, group: &str, key: &str) -> Option<&'a str> {
    let mut inside = false;
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            inside = name == group;
        } else if inside {
            if let Some((k, v)) = line.split_once('=') {
                if k.trim() == key {
                    let v = v.trim();
                    return (!v.is_empty()).then_some(v);
                }
            }
        }
    }
    None
}

/// A theme or icon name is a basename, never a path or traversal.
fn plain_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != ".."
}

/// The icon file of the first desktop entry found for `ids`; when no entry
/// names a readable icon, the ids themselves are tried as icon names.
pub(super) fn resolve(ids: &[&str], search: &Search) -> Option<PathBuf> {
    let themes = themes(search);
    let mut names = Vec::new();
    for id in ids {
        for dir in &search.data_dirs {
            let entry = dir.join("applications").join(format!("{id}.desktop"));
            if let Some(icon) = std::fs::read_to_string(entry)
                .ok()
                .as_deref()
                .and_then(|text| ini_value(text, "Desktop Entry", "Icon"))
            {
                names.push(icon.to_owned());
            }
        }
    }
    names.extend(ids.iter().map(|id| (*id).to_owned()));
    names
        .iter()
        .find_map(|name| lookup(name, &themes, &search.data_dirs))
}

/// The configured theme, everything it inherits, then `hicolor`.
fn themes(search: &Search) -> Vec<String> {
    let mut themes: Vec<String> = Vec::new();
    let mut queue: Vec<String> = search.theme.iter().cloned().collect();
    while let Some(theme) = queue.pop() {
        if themes.contains(&theme) || themes.len() >= 8 {
            continue;
        }
        let inherits = search
            .data_dirs
            .iter()
            .find_map(|dir| {
                std::fs::read_to_string(dir.join("icons").join(&theme).join("index.theme")).ok()
            })
            .and_then(|text| {
                ini_value(&text, "Icon Theme", "Inherits").map(|list| {
                    list.split(',')
                        .map(str::trim)
                        .filter(|t| plain_name(t))
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
            })
            .unwrap_or_default();
        themes.push(theme);
        // Depth-first in declared order.
        queue.extend(inherits.into_iter().rev());
    }
    if !themes.iter().any(|t| t == "hicolor") {
        themes.push("hicolor".into());
    }
    themes
}

fn lookup(name: &str, themes: &[String], data_dirs: &[PathBuf]) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.is_absolute() {
        return path.is_file().then(|| path.to_owned());
    }
    if !plain_name(name) {
        return None;
    }
    for theme in themes {
        for dir in data_dirs {
            let root = dir.join("icons").join(theme);
            if !root.is_dir() {
                continue;
            }
            for sub in theme_dirs(&root) {
                if let Some(found) = in_dir(&root.join(sub), name) {
                    return Some(found);
                }
            }
        }
    }
    data_dirs
        .iter()
        .find_map(|dir| in_dir(&dir.join("pixmaps"), name))
}

/// Application directories of a theme, closest to [`WANTED`] first;
/// `scalable` counts as an exact match.
fn theme_dirs(root: &Path) -> Vec<String> {
    let Some(text) = std::fs::read_to_string(root.join("index.theme")).ok() else {
        return HICOLOR_DIRS.iter().map(|d| (*d).to_owned()).collect();
    };
    let Some(list) = ini_value(&text, "Icon Theme", "Directories") else {
        return HICOLOR_DIRS.iter().map(|d| (*d).to_owned()).collect();
    };
    let mut dirs: Vec<(u32, String)> = list
        .split(',')
        .map(str::trim)
        .filter(|dir| !dir.is_empty() && !dir.split('/').any(|p| p == ".."))
        .filter_map(|dir| {
            let context = ini_value(&text, dir, "Context");
            let apps = context.map_or(dir.contains("apps"), |c| c == "Applications");
            if !apps {
                return None;
            }
            let distance = if ini_value(&text, dir, "Type") == Some("Scalable") {
                0
            } else {
                ini_value(&text, dir, "Size")
                    .and_then(|s| s.parse::<u32>().ok())
                    .map_or(u32::MAX, |size| size.abs_diff(WANTED))
            };
            Some((distance, dir.to_owned()))
        })
        .collect();
    dirs.sort_by_key(|(distance, _)| *distance);
    dirs.into_iter().map(|(_, dir)| dir).collect()
}

fn in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    ["png", "svg"]
        .iter()
        .map(|ext| dir.join(format!("{name}.{ext}")))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn search(data: &Path, theme: Option<&str>) -> Search {
        Search {
            data_dirs: vec![data.to_owned()],
            theme: theme.map(str::to_owned),
        }
    }

    #[test]
    fn desktop_entry_icon_ignores_actions_and_never_runs_exec() {
        let entry = "[Desktop Entry]\nName=Zed\nIcon[de]=other\nExec=never-run-me\nIcon=dev.zed.Zed\n[Desktop Action New]\nIcon=action";
        assert_eq!(
            ini_value(entry, "Desktop Entry", "Icon"),
            Some("dev.zed.Zed")
        );
        assert_eq!(
            ini_value(
                "[Desktop Entry]\nName=Zed\n[Desktop Action New]\nIcon=x",
                "Desktop Entry",
                "Icon"
            ),
            None
        );
        assert_eq!(
            ini_value("[Desktop Entry]\nIcon=", "Desktop Entry", "Icon"),
            None
        );
    }

    #[test]
    fn hicolor_icon_resolves_from_the_desktop_entry() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path();
        let icon = data.join("icons/hicolor/48x48/apps/zed-logo.png");
        write(&icon, "png");
        write(
            &data.join("icons/hicolor/16x16/apps/zed-logo.png"),
            "smaller",
        );
        write(
            &data.join("applications/dev.zed.Zed.desktop"),
            "[Desktop Entry]\nIcon=zed-logo\nExec=never-run-me",
        );
        let ids = ["zed", "dev.zed.Zed"];
        assert_eq!(resolve(&ids, &search(data, None)), Some(icon.clone()));
        // Without the entry the ids themselves are icon names: none exists.
        std::fs::remove_file(data.join("applications/dev.zed.Zed.desktop")).unwrap();
        assert_eq!(resolve(&ids, &search(data, None)), None, "fallback");
    }

    #[test]
    fn current_theme_and_its_parents_win_over_hicolor() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path();
        write(
            &data.join("applications/code.desktop"),
            "[Desktop Entry]\nIcon=vscode",
        );
        write(&data.join("icons/hicolor/64x64/apps/vscode.png"), "hicolor");
        // Breeze-style layout, found only through index.theme.
        write(
            &data.join("icons/Child/index.theme"),
            "[Icon Theme]\nInherits=Parent\nDirectories=apps/16\n[apps/16]\nSize=16\nContext=Applications",
        );
        let parent = data.join("icons/Parent/apps/48/vscode.svg");
        write(&parent, "<svg/>");
        write(&data.join("icons/Parent/apps/16/vscode.svg"), "<svg/>");
        write(
            &data.join("icons/Parent/index.theme"),
            "[Icon Theme]\nDirectories=apps/16,apps/48,mimetypes/64\n[apps/16]\nSize=16\nContext=Applications\n[apps/48]\nSize=48\nContext=Applications\n[mimetypes/64]\nSize=64\nContext=MimeTypes",
        );
        write(&data.join("icons/Parent/mimetypes/64/vscode.svg"), "<svg/>");
        assert_eq!(
            resolve(&["code"], &search(data, Some("Child"))),
            Some(parent),
            "inherited theme, application context, nearest size"
        );
        assert_eq!(
            resolve(&["code"], &search(data, None)),
            Some(data.join("icons/hicolor/64x64/apps/vscode.png")),
            "positive control: without a theme, hicolor"
        );
    }

    #[test]
    fn absolute_snap_icons_pixmaps_and_traversal() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path();
        let snap = data.join("snap/code/current/code.png");
        write(&snap, "png");
        write(
            &data.join("applications/code_code.desktop"),
            &format!("[Desktop Entry]\nIcon={}", snap.display()),
        );
        assert_eq!(
            resolve(&["code", "code_code"], &search(data, None)),
            Some(snap)
        );
        let pixmap = data.join("pixmaps/sublime-text.png");
        write(&pixmap, "png");
        write(
            &data.join("applications/sublime_text.desktop"),
            "[Desktop Entry]\nIcon=sublime-text",
        );
        assert_eq!(
            resolve(&["sublime_text"], &search(data, None)),
            Some(pixmap)
        );
        write(&data.join("icons/hicolor/64x64/escape.png"), "png");
        write(
            &data.join("applications/evil.desktop"),
            "[Desktop Entry]\nIcon=../escape",
        );
        assert_eq!(resolve(&["evil"], &search(data, None)), None);
        assert!(!plain_name("../theme") && plain_name("Papirus-Dark"));
    }

    #[test]
    fn configured_theme_comes_from_gtk_then_kde() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path();
        assert_eq!(configured_theme(config), None);
        write(&config.join("kdeglobals"), "[Icons]\nTheme=breeze-dark");
        assert_eq!(configured_theme(config).as_deref(), Some("breeze-dark"));
        write(
            &config.join("gtk-3.0/settings.ini"),
            "[Settings]\ngtk-icon-theme-name = Papirus\n",
        );
        assert_eq!(configured_theme(config).as_deref(), Some("Papirus"));
    }
}
