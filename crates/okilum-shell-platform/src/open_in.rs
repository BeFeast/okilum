//! «Open in ▸» installed code editors (#873). Detection only looks at fixed
//! install locations and `PATH`; launching passes the file as an argument,
//! never through a shell. A dirty note is saved first.
//!
//! The note toolbar carries a split button (#1400): «Open in <default>» with
//! that editor's icon opens the note in one click, its chevron lists every
//! installed editor. The default is the editor last used, remembered across
//! restarts, and [`OpenInDefaultEditor`] opens the note there from the keyboard.
use crate::prelude::*;
use gpui_component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use std::ffi::OsString;
use std::sync::OnceLock;

actions!(okilum, [OpenInDefaultEditor]);

/// Open the note in the default editor (#1400). Not ⌘O / Ctrl+O, the one T3
/// Code uses: Okilum opens a file there. ⌥⌘O on macOS. Linux and Windows
/// may not get a new Ctrl+Alt+letter (#2813, AltGr), and Ctrl+Shift+O is Open
/// Folder, so they use Ctrl+Shift+A, free in Okilum, the toolkit and the
/// common input methods (Ctrl+Shift+U is IBus Unicode entry).
pub const OPEN_IN_EDITOR_KEY: &str = if cfg!(target_os = "macos") {
    "cmd-alt-o"
} else {
    "ctrl-shift-a"
};

mod icons;

/// Start loading the editors' application icons on a background thread, so
/// the first «Open in ▸» already has them (#1400). Never blocks the caller.
pub fn warm_icons() {
    icons::warm();
}

/// How an editor takes "open this file at line N".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineArg {
    /// `--goto path:line` (VS Code family).
    Goto,
    /// `path:line` (Zed, Sublime Text).
    Suffix,
    /// Path only (`open -a` on macOS without a bundled CLI).
    None,
}

struct Known {
    id: &'static str,
    label: &'static str,
    line: LineArg,
    /// Arguments the launcher needs before the file.
    flags: &'static [&'static str],
    /// macOS bundle name and its command-line launcher inside the bundle.
    mac: &'static [(&'static str, &'static str)],
    /// Executable names on `PATH`, `/snap/bin` and Flatpak exports.
    unix: &'static [&'static str],
    /// Relative to `%LOCALAPPDATA%` or `%ProgramFiles%`.
    windows: &'static [&'static str],
    /// Linux desktop-entry ids (`<id>.desktop`) that carry the editor's icon:
    /// distribution packages, Flatpak and Snap (`<snap>_<app>`).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    desktop: &'static [&'static str],
}

const KNOWN: &[Known] = &[
    Known {
        id: "zed",
        label: "Zed",
        line: LineArg::Suffix,
        flags: &[],
        mac: &[("Zed.app", "Contents/MacOS/cli")],
        unix: &["zed", "zeditor", "dev.zed.Zed"],
        windows: &["Programs\\Zed\\zed.exe"],
        desktop: &["dev.zed.Zed", "zed", "dev.zed.Zed-Preview"],
    },
    Known {
        id: "cursor",
        label: "Cursor",
        line: LineArg::Goto,
        // Cursor 3 opens a file in the last active window, and when that is
        // its Agents window the note lands in a side panel. A new window is a
        // classic editor window; `--reuse-window` would pick the Agents one
        // (#1398).
        flags: &["--new-window"],
        mac: &[("Cursor.app", "Contents/Resources/app/bin/cursor")],
        unix: &["cursor"],
        windows: &["Programs\\cursor\\Cursor.exe"],
        desktop: &["cursor", "co.anysphere.cursor"],
    },
    Known {
        id: "vscode",
        label: "VS Code",
        line: LineArg::Goto,
        flags: &[],
        mac: &[("Visual Studio Code.app", "Contents/Resources/app/bin/code")],
        unix: &["code", "com.visualstudio.code"],
        windows: &[
            "Programs\\Microsoft VS Code\\Code.exe",
            "Microsoft VS Code\\Code.exe",
        ],
        desktop: &[
            "code",
            "com.visualstudio.code",
            "code_code",
            "visual-studio-code",
        ],
    },
    Known {
        id: "vscode-insiders",
        label: "VS Code Insiders",
        line: LineArg::Goto,
        flags: &[],
        mac: &[(
            "Visual Studio Code - Insiders.app",
            "Contents/Resources/app/bin/code-insiders",
        )],
        unix: &["code-insiders"],
        windows: &["Programs\\Microsoft VS Code Insiders\\Code - Insiders.exe"],
        desktop: &["code-insiders", "code-insiders_code-insiders"],
    },
    Known {
        id: "windsurf",
        label: "Windsurf",
        line: LineArg::Goto,
        flags: &[],
        mac: &[("Windsurf.app", "Contents/Resources/app/bin/windsurf")],
        unix: &["windsurf"],
        windows: &["Programs\\Windsurf\\Windsurf.exe"],
        desktop: &["windsurf"],
    },
    Known {
        id: "sublime",
        label: "Sublime Text",
        line: LineArg::Suffix,
        flags: &[],
        mac: &[("Sublime Text.app", "Contents/SharedSupport/bin/subl")],
        unix: &["subl", "com.sublimetext.three"],
        windows: &["Sublime Text\\subl.exe", "Sublime Text\\sublime_text.exe"],
        desktop: &["sublime_text", "com.sublimetext.three", "sublime-text_subl"],
    },
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Editor {
    pub id: &'static str,
    pub label: &'static str,
    program: PathBuf,
    /// The `.app` bundle (macOS) or executable (Windows) whose application
    /// icon the menu shows; Linux looks the icon up by desktop-entry id.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    icon_path: PathBuf,
    /// Arguments before the file (`-a App` for `open`, launcher flags).
    prefix: Vec<OsString>,
    line: LineArg,
}

impl Editor {
    /// The full argument list, without a shell.
    fn args(&self, path: &Path, line: Option<usize>) -> Vec<OsString> {
        let mut args = self.prefix.clone();
        match (self.line, line) {
            (LineArg::Goto, Some(line)) => {
                args.push("--goto".into());
                args.push(with_line(path, line));
            }
            (LineArg::Suffix, Some(line)) => args.push(with_line(path, line)),
            _ => args.push(path.into()),
        }
        args
    }
}

impl Editor {
    /// The program and arguments that open `path`, without a line.
    pub fn command(&self, path: &Path) -> (PathBuf, Vec<OsString>) {
        (self.program.clone(), self.args(path, None))
    }
}

/// The default editor: the one last used to open a note (remembered across
/// restarts) while it is still installed, else the first known one installed
/// (Zed before the others).
pub fn preferred(cx: &App) -> Option<&'static Editor> {
    pick_default(editors(), reader_ui_state::open_in_editor(cx))
}

fn pick_default<'a>(editors: &'a [Editor], saved: &str) -> Option<&'a Editor> {
    editors
        .iter()
        .find(|e| e.id == saved)
        .or_else(|| editors.first())
}

fn with_line(path: &Path, line: usize) -> OsString {
    let mut arg = path.as_os_str().to_owned();
    arg.push(format!(":{line}"));
    arg
}

/// Where to look; injectable for tests.
#[derive(Default)]
struct Probe {
    mac_apps: Vec<PathBuf>,
    unix_dirs: Vec<PathBuf>,
    windows_roots: Vec<PathBuf>,
}

impl Probe {
    #[cfg_attr(test, allow(dead_code))]
    fn current() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut probe = Probe::default();
        if cfg!(target_os = "macos") {
            probe.mac_apps.push("/Applications".into());
            probe
                .mac_apps
                .extend(home.as_ref().map(|h| h.join("Applications")));
        }
        if cfg!(windows) {
            for var in ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"] {
                probe
                    .windows_roots
                    .extend(std::env::var_os(var).map(PathBuf::from));
            }
        } else {
            probe.unix_dirs.extend(
                std::env::var_os("PATH")
                    .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
                    .unwrap_or_default(),
            );
            probe.unix_dirs.push("/snap/bin".into());
            probe.unix_dirs.push("/var/lib/flatpak/exports/bin".into());
            probe.unix_dirs.extend(
                home.as_ref()
                    .map(|h| h.join(".local/share/flatpak/exports/bin")),
            );
        }
        if cfg!(windows) {
            probe.unix_dirs.extend(
                std::env::var_os("PATH")
                    .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
                    .unwrap_or_default(),
            );
        }
        probe
    }
}

fn executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

fn detect_with(probe: &Probe) -> Vec<Editor> {
    KNOWN
        .iter()
        .filter_map(|known| {
            let flags = || known.flags.iter().map(OsString::from).collect::<Vec<_>>();
            let editor = |program: PathBuf, prefix: Vec<OsString>, line| Editor {
                id: known.id,
                label: known.label,
                icon_path: program.clone(),
                program,
                prefix,
                line,
            };
            for dir in &probe.mac_apps {
                for (bundle, cli) in known.mac {
                    let app = dir.join(bundle);
                    if !app.is_dir() {
                        continue;
                    }
                    let cli = app.join(cli);
                    let mut detected = if executable(&cli) {
                        editor(cli, flags(), known.line)
                    } else {
                        // The bundle without its launcher: Finder's `open`.
                        editor(
                            "/usr/bin/open".into(),
                            vec!["-a".into(), app.clone().into_os_string()],
                            LineArg::None,
                        )
                    };
                    detected.icon_path = app;
                    return Some(detected);
                }
            }
            for root in &probe.windows_roots {
                for rel in known.windows {
                    let exe = rel.split('\\').fold(root.clone(), |p, part| p.join(part));
                    if exe.is_file() {
                        return Some(editor(exe, flags(), known.line));
                    }
                }
            }
            for dir in &probe.unix_dirs {
                for name in known.unix {
                    let name = if cfg!(windows) {
                        format!("{name}.exe")
                    } else {
                        (*name).to_owned()
                    };
                    let bin = dir.join(name);
                    if executable(&bin) {
                        return Some(editor(bin, flags(), known.line));
                    }
                }
            }
            None
        })
        .collect()
}

/// Detected once per run: a newly installed editor appears after a restart.
#[cfg(not(any(test, feature = "test-support")))]
pub fn editors() -> &'static [Editor] {
    static EDITORS: OnceLock<Vec<Editor>> = OnceLock::new();
    EDITORS.get_or_init(|| detect_with(&Probe::current()))
}

/// Tests see only the editors they install, never the host's.
#[cfg(any(test, feature = "test-support"))]
pub fn editors() -> &'static [Editor] {
    TEST_EDITORS.with(|editors| editors.get())
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static TEST_EDITORS: std::cell::Cell<&'static [Editor]> = const { std::cell::Cell::new(&[]) };
    static BUILT_ROWS: std::cell::RefCell<Vec<(&'static str, Option<String>)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The rows the last editor list was built with: editor id and shortcut.
#[cfg(any(test, feature = "test-support"))]
pub fn built_rows() -> Vec<(&'static str, Option<String>)> {
    BUILT_ROWS.with(|rows| rows.borrow().clone())
}

/// Pretend `ids` (known editor ids) are installed on this test thread, each
/// launched as `program`.
#[cfg(any(test, feature = "test-support"))]
pub fn install_test_editors(ids: &[&str], program: &Path) {
    let editors = ids
        .iter()
        .map(|id| {
            let known = KNOWN.iter().find(|k| k.id == *id).expect("known editor");
            Editor {
                id: known.id,
                label: known.label,
                program: program.to_owned(),
                icon_path: PathBuf::new(),
                // Launched like a detected editor: its flags come first.
                prefix: known.flags.iter().map(OsString::from).collect(),
                line: known.line,
            }
        })
        .collect::<Vec<_>>();
    TEST_EDITORS.with(|cell| cell.set(Vec::leak(editors)));
}

/// The default editor first, then the others in known order.
fn ordered(cx: &App) -> Vec<&'static Editor> {
    let default = preferred(cx).map(|e| e.id);
    let mut list: Vec<_> = editors().iter().collect();
    list.sort_by_key(|e| Some(e.id) != default);
    list
}

/// «Open in ▸» with the installed editors; nothing when none is installed.
pub fn submenu(
    menu: PopupMenu,
    root: PathBuf,
    rel: String,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    if editors().is_empty() {
        return menu;
    }
    menu.submenu("Open in", window, cx, move |menu, _, cx| {
        editor_rows(menu, root.clone(), rel.clone(), cx)
    })
}

/// One row per installed editor: its icon and name, and on the default
/// editor's row the [`OpenInDefaultEditor`] shortcut, right-aligned.
fn editor_rows(mut menu: PopupMenu, root: PathBuf, rel: String, cx: &App) -> PopupMenu {
    let editors = ordered(cx);
    let default = editors.first().map(|e| e.id);
    let key = crate::hooks::shortcut(&OpenInDefaultEditor, cx);
    #[cfg(any(test, feature = "test-support"))]
    BUILT_ROWS.with(|rows| rows.borrow_mut().clear());
    for editor in editors {
        let (root, rel) = (root.clone(), rel.clone());
        let key = if Some(editor.id) == default {
            key.clone()
        } else {
            None
        };
        #[cfg(any(test, feature = "test-support"))]
        BUILT_ROWS.with(|rows| rows.borrow_mut().push((editor.id, key.clone())));
        menu = menu.item(
            PopupMenuItem::element(move |_, cx| {
                h_flex()
                    .debug_selector(move || format!("open-in-{}", editor.id))
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(icon_element(icons::get(editor.id), cx))
                    .child(div().flex_1().child(editor.label))
                    .children(key.clone().map(|key| {
                        div()
                            .debug_selector(move || format!("open-in-{}-key", editor.id))
                            .ml_4()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(key)
                    }))
            })
            .on_click(move |_, window, cx| open(editor, &root, &rel, window, cx)),
        );
    }
    menu
}

fn toolbar_text_width(text: &str, weight: FontWeight, window: &Window) -> f32 {
    let mut font = window.text_style().font();
    font.weight = weight;
    let run = gpui::TextRun {
        len: text.len(),
        font,
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(
            text.to_owned().into(),
            px(brand::READER_CHROME_FONT_SIZE),
            &[run],
            None,
        )
        .width()
        .as_f32()
}

/// The toolbar split button's two widths, labelled and collapsed (#1400);
/// `None` when no editor is installed.
pub fn toolbar_widths(cx: &App, window: &Window) -> Option<(f32, f32)> {
    let editor = preferred(cx)?;
    // A small toolkit button: 8px padding each side, a 16px icon, a 4px gap;
    // the chevron part is 18px wide.
    let text = toolbar_text_width(
        &format!("Open in {}", editor.label),
        FontWeight::MEDIUM,
        window,
    ) * window.rem_size().as_f32()
        / brand::READER_CHROME_FONT_SIZE;
    Some((text + 36. + TOOLBAR_CHEVRON, 28. + TOOLBAR_CHEVRON))
}

const TOOLBAR_CHEVRON: f32 = 18.;

/// «Open in <Editor>» split button for the note toolbar (#1400): the main
/// part opens the note in the default editor, the chevron lists them all.
pub fn toolbar_button(root: PathBuf, rel: String, labelled: bool, cx: &App) -> Option<AnyElement> {
    let editor = preferred(cx)?;
    let label: SharedString = format!("Open in {}", editor.label).into();
    let tooltip = crate::hooks::hint(&label, &OpenInDefaultEditor, cx);
    let (open_root, open_rel) = (root.clone(), rel.clone());
    let main = Button::new("viewer-open-in")
        .ghost()
        .small()
        .h(px(28.))
        .rounded(px(6.))
        .px_2()
        .when(!labelled, |b| b.w(px(28.)).px_0())
        .child(icon_element(icons::get(editor.id), cx))
        .when(labelled, |b| b.child(label.clone()))
        .accessibility_label(label)
        .tooltip(tooltip)
        .debug_selector(|| "viewer-open-in".into())
        .on_click(move |_, window, cx| {
            if let Some(editor) = preferred(cx) {
                open(editor, &open_root, &open_rel, window, cx);
            }
        });
    let chevron = Button::new("viewer-open-in-list")
        .ghost()
        .small()
        .h(px(28.))
        .w(px(TOOLBAR_CHEVRON))
        .px_0()
        .rounded(px(6.))
        .dropdown_caret(true)
        .accessibility_label("Choose the editor")
        .tooltip("Open in…")
        .debug_selector(|| "viewer-open-in-list".into())
        .dropdown_menu_with_anchor(Anchor::TopRight, move |m, _, cx| {
            editor_rows(m, root.clone(), rel.clone(), cx)
        });
    Some(
        h_flex()
            .flex_none()
            .items_center()
            .child(main)
            .child(chevron)
            .into_any_element(),
    )
}

/// The editor's application icon, or a neutral file glyph while icons are
/// still loading or when none could be read.
fn icon_element(image: Option<Arc<RenderImage>>, cx: &App) -> AnyElement {
    match image {
        Some(image) => img(ImageSource::Render(image))
            .size_4()
            .flex_none()
            .into_any_element(),
        None => Icon::new(IconName::File)
            .size_4()
            .flex_none()
            .text_color(cx.theme().muted_foreground)
            .into_any_element(),
    }
}

pub fn open(editor: &'static Editor, root: &Path, rel: &str, window: &mut Window, cx: &mut App) {
    let result = (|| -> anyhow::Result<()> {
        let path = crate::hooks::checked_path(root, rel, cx)?;
        // Never hand an editor a stale file: save the open draft first.
        let line = match crate::hooks::prepare_open_note(root, rel, cx) {
            Ok(line) => line,
            Err(error) => anyhow::bail!("{error}"),
        };
        let mut child = std::process::Command::new(&editor.program)
            .args(editor.args(&okilum_core::vault::plain_path(&path), line))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        // Reap on its own thread: some launchers (Flatpak, `open -a`) stay
        // as long as the editor, which must not hold an executor thread.
        std::thread::Builder::new()
            .name("open-in-reaper".into())
            .spawn(move || {
                let _ = child.wait();
            })?;
        reader_ui_state::set_open_in_editor(editor.id, cx);
        Ok(())
    })();
    if let Err(error) = result {
        crate::hooks::toast_error(
            format!("Cannot open in {}: {error:#}", editor.label),
            window,
            cx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{detect_with, pick_default, Editor, LineArg, Probe, OPEN_IN_EDITOR_KEY};
    use std::path::{Path, PathBuf};

    /// #1400: the default is the remembered editor while it is installed,
    /// else the first installed one; nothing when none is installed.
    #[test]
    fn default_editor_is_the_remembered_one_while_installed() {
        let editor = |id| Editor {
            id,
            label: id,
            program: "/bin/x".into(),
            icon_path: "/bin/x".into(),
            prefix: vec![],
            line: LineArg::None,
        };
        let installed = [editor("zed"), editor("cursor"), editor("vscode")];
        let pick = |saved| pick_default(&installed, saved).map(|e| e.id);
        assert_eq!(pick("cursor"), Some("cursor"), "remembered and installed");
        assert_eq!(pick("vscode"), Some("vscode"));
        assert_eq!(pick(""), Some("zed"), "nothing remembered: first installed");
        assert_eq!(pick("sublime"), Some("zed"), "remembered one uninstalled");
        assert_eq!(pick_default(&[], "cursor").map(|e| e.id), None);
    }

    /// #1400: ⌘O / Ctrl+O stays Open File; the default-editor key differs.
    #[test]
    fn default_editor_key_is_not_open_file() {
        let key = gpui::Keystroke::parse(OPEN_IN_EDITOR_KEY).unwrap();
        assert_eq!(key.key, if cfg!(target_os = "macos") { "o" } else { "a" });
        assert!(
            key.modifiers.shift || key.modifiers.alt,
            "{OPEN_IN_EDITOR_KEY}"
        );
    }

    fn touch(path: &Path, exec: bool) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        if exec {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let _ = exec;
    }

    fn ids(editors: &[Editor]) -> Vec<&str> {
        editors.iter().map(|e| e.id).collect()
    }

    #[cfg(unix)]
    #[test]
    fn unix_detection_lists_only_installed_executables() {
        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("bin");
        let flatpak = temp.path().join("flatpak");
        touch(&bin.join("cursor"), true);
        touch(&bin.join("subl"), false); // not executable: not an editor
        touch(&flatpak.join("dev.zed.Zed"), true);
        let probe = Probe {
            unix_dirs: vec![bin.clone(), flatpak.clone()],
            ..Default::default()
        };
        let editors = detect_with(&probe);
        assert_eq!(
            ids(&editors),
            ["zed", "cursor"],
            "known order, installed only"
        );
        assert_eq!(editors[0].program, flatpak.join("dev.zed.Zed"));
        // Positive control: an empty probe finds nothing.
        assert!(detect_with(&Probe::default()).is_empty());
    }

    #[test]
    fn mac_bundles_prefer_their_launcher_and_fall_back_to_open() {
        let temp = tempfile::tempdir().unwrap();
        let apps = temp.path().join("Applications");
        touch(
            &apps.join("Visual Studio Code.app/Contents/Resources/app/bin/code"),
            true,
        );
        std::fs::create_dir_all(apps.join("Zed.app/Contents")).unwrap();
        let probe = Probe {
            mac_apps: vec![apps.clone()],
            ..Default::default()
        };
        let editors = detect_with(&probe);
        assert_eq!(ids(&editors), ["zed", "vscode"]);
        let zed = &editors[0];
        assert_eq!(zed.program, PathBuf::from("/usr/bin/open"));
        // #1400: the menu icon comes from the bundle, launcher or not.
        assert_eq!(zed.icon_path, apps.join("Zed.app"));
        assert_eq!(editors[1].icon_path, apps.join("Visual Studio Code.app"));
        let file = Path::new("/v/Note.md");
        assert_eq!(
            zed.args(file, Some(12)),
            [
                "-a".into(),
                apps.join("Zed.app").into_os_string(),
                file.into()
            ],
            "without its CLI a bundle opens the file, no line"
        );
        assert_eq!(
            editors[1].args(file, Some(12)),
            ["--goto", "/v/Note.md:12"].map(std::ffi::OsString::from)
        );
    }

    #[test]
    fn windows_known_directories_are_found() {
        let temp = tempfile::tempdir().unwrap();
        let local = temp.path().join("Local");
        touch(
            &local.join("Programs").join("cursor").join("Cursor.exe"),
            true,
        );
        let probe = Probe {
            windows_roots: vec![local.clone()],
            ..Default::default()
        };
        let editors = detect_with(&probe);
        assert_eq!(ids(&editors), ["cursor"]);
        assert_eq!(
            editors[0].icon_path,
            local.join("Programs").join("cursor").join("Cursor.exe"),
            "#1400: the executable carries the icon"
        );
    }

    /// #1398: Cursor gets `--new-window` before `--goto`, so the note opens
    /// in an editor window, not in Cursor's Agents window; the others are
    /// launched exactly as before.
    #[test]
    fn cursor_opens_the_note_in_an_editor_window() {
        let temp = tempfile::tempdir().unwrap();
        let apps = temp.path().join("Applications");
        for cli in [
            "Cursor.app/Contents/Resources/app/bin/cursor",
            "Visual Studio Code.app/Contents/Resources/app/bin/code",
            "Zed.app/Contents/MacOS/cli",
        ] {
            touch(&apps.join(cli), true);
        }
        let local = temp.path().join("Local");
        touch(
            &local.join("Programs").join("cursor").join("Cursor.exe"),
            true,
        );
        let file = Path::new("/v/A note.md");
        let args = |editor: &Editor| {
            editor
                .args(file, Some(7))
                .into_iter()
                .map(|a| a.into_string().unwrap())
                .collect::<Vec<_>>()
        };
        let mac = detect_with(&Probe {
            mac_apps: vec![apps.clone()],
            ..Default::default()
        });
        assert_eq!(ids(&mac), ["zed", "cursor", "vscode"]);
        assert_eq!(
            args(&mac[1]),
            ["--new-window", "--goto", "/v/A note.md:7"],
            "macOS bundled CLI"
        );
        assert_eq!(
            args(&mac[2]),
            ["--goto", "/v/A note.md:7"],
            "VS Code unchanged"
        );
        assert_eq!(args(&mac[0]), ["/v/A note.md:7"], "Zed unchanged");
        let windows = detect_with(&Probe {
            windows_roots: vec![local],
            ..Default::default()
        });
        assert_eq!(
            args(&windows[0]),
            ["--new-window", "--goto", "/v/A note.md:7"],
            "Windows Cursor.exe"
        );
        assert_eq!(
            windows[0].args(file, None),
            ["--new-window", "/v/A note.md"].map(std::ffi::OsString::from),
            "no line: still an editor window"
        );
    }

    #[test]
    fn line_arguments_match_each_editor() {
        let editor = |line| Editor {
            id: "x",
            label: "X",
            program: "/bin/x".into(),
            icon_path: "/bin/x".into(),
            prefix: vec![],
            line,
        };
        let file = Path::new("/v/A note.md");
        assert_eq!(
            editor(LineArg::Suffix).args(file, Some(3)),
            ["/v/A note.md:3"].map(std::ffi::OsString::from)
        );
        assert_eq!(
            editor(LineArg::Goto).args(file, None),
            ["/v/A note.md"].map(std::ffi::OsString::from),
            "no line: just the path, spaces kept in one argument"
        );
    }
}
