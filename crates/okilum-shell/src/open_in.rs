//! «Open in ▸» installed code editors (#873). Detection only looks at fixed
//! install locations and `PATH`; launching passes the file as an argument,
//! never through a shell. A dirty note is saved first.
use super::*;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use std::ffi::OsString;
use std::sync::{Mutex, OnceLock};

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
    /// macOS bundle name and its command-line launcher inside the bundle.
    mac: &'static [(&'static str, &'static str)],
    /// Executable names on `PATH`, `/snap/bin` and Flatpak exports.
    unix: &'static [&'static str],
    /// Relative to `%LOCALAPPDATA%` or `%ProgramFiles%`.
    windows: &'static [&'static str],
}

const KNOWN: &[Known] = &[
    Known {
        id: "zed",
        label: "Zed",
        line: LineArg::Suffix,
        mac: &[("Zed.app", "Contents/MacOS/cli")],
        unix: &["zed", "zeditor", "dev.zed.Zed"],
        windows: &["Programs\\Zed\\zed.exe"],
    },
    Known {
        id: "cursor",
        label: "Cursor",
        line: LineArg::Goto,
        mac: &[("Cursor.app", "Contents/Resources/app/bin/cursor")],
        unix: &["cursor"],
        windows: &["Programs\\cursor\\Cursor.exe"],
    },
    Known {
        id: "vscode",
        label: "VS Code",
        line: LineArg::Goto,
        mac: &[("Visual Studio Code.app", "Contents/Resources/app/bin/code")],
        unix: &["code", "com.visualstudio.code"],
        windows: &[
            "Programs\\Microsoft VS Code\\Code.exe",
            "Microsoft VS Code\\Code.exe",
        ],
    },
    Known {
        id: "vscode-insiders",
        label: "VS Code Insiders",
        line: LineArg::Goto,
        mac: &[(
            "Visual Studio Code - Insiders.app",
            "Contents/Resources/app/bin/code-insiders",
        )],
        unix: &["code-insiders"],
        windows: &["Programs\\Microsoft VS Code Insiders\\Code - Insiders.exe"],
    },
    Known {
        id: "windsurf",
        label: "Windsurf",
        line: LineArg::Goto,
        mac: &[("Windsurf.app", "Contents/Resources/app/bin/windsurf")],
        unix: &["windsurf"],
        windows: &["Programs\\Windsurf\\Windsurf.exe"],
    },
    Known {
        id: "sublime",
        label: "Sublime Text",
        line: LineArg::Suffix,
        mac: &[("Sublime Text.app", "Contents/SharedSupport/bin/subl")],
        unix: &["subl", "com.sublimetext.three"],
        windows: &["Sublime Text\\subl.exe", "Sublime Text\\sublime_text.exe"],
    },
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Editor {
    pub id: &'static str,
    pub label: &'static str,
    program: PathBuf,
    /// Arguments before the file (`-a App` for `open`).
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
            let editor = |program: PathBuf, prefix: Vec<OsString>, line| Editor {
                id: known.id,
                label: known.label,
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
                    return Some(if executable(&cli) {
                        editor(cli, vec![], known.line)
                    } else {
                        // The bundle without its launcher: Finder's `open`.
                        editor(
                            "/usr/bin/open".into(),
                            vec!["-a".into(), app.into_os_string()],
                            LineArg::None,
                        )
                    });
                }
            }
            for root in &probe.windows_roots {
                for rel in known.windows {
                    let exe = rel.split('\\').fold(root.clone(), |p, part| p.join(part));
                    if exe.is_file() {
                        return Some(editor(exe, vec![], known.line));
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
                        return Some(editor(bin, vec![], known.line));
                    }
                }
            }
            None
        })
        .collect()
}

/// Detected once per run: a newly installed editor appears after a restart.
pub(crate) fn editors() -> &'static [Editor] {
    static EDITORS: OnceLock<Vec<Editor>> = OnceLock::new();
    EDITORS.get_or_init(|| detect_with(&Probe::current()))
}

/// The last editor used this session comes first.
static LAST_USED: Mutex<Option<&'static str>> = Mutex::new(None);

fn ordered() -> Vec<&'static Editor> {
    let last = *LAST_USED.lock().unwrap();
    let mut list: Vec<_> = editors().iter().collect();
    list.sort_by_key(|e| Some(e.id) != last);
    list
}

/// «Open in ▸» with the installed editors; nothing when none is installed.
pub(crate) fn submenu(
    menu: PopupMenu,
    root: PathBuf,
    rel: String,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let editors = ordered();
    if editors.is_empty() {
        return menu;
    }
    menu.submenu("Open in", window, cx, move |mut menu, _, _| {
        for editor in &editors {
            let (root, rel, editor) = (root.clone(), rel.clone(), *editor);
            menu = menu.item(
                PopupMenuItem::new(editor.label)
                    .on_click(move |_, window, cx| open(editor, &root, &rel, window, cx)),
            );
        }
        menu
    })
}

fn open(editor: &'static Editor, root: &Path, rel: &str, window: &mut Window, cx: &mut App) {
    let result = (|| -> anyhow::Result<()> {
        let path = reader_files::checked_path(root, rel)?;
        // Never hand an editor a stale file: save the open draft first.
        let line = match prepare_open_note(root, rel, cx) {
            Ok(line) => line,
            Err(error) => anyhow::bail!("{error}"),
        };
        let mut child = std::process::Command::new(&editor.program)
            .args(editor.args(&path, line))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        cx.background_executor()
            .spawn(async move {
                let _ = child.wait();
            })
            .detach();
        *LAST_USED.lock().unwrap() = Some(editor.id);
        Ok(())
    })();
    if let Err(error) = result {
        reader_toast::error(
            format!("Cannot open in {}: {error:#}", editor.label),
            window,
            cx,
        );
    }
}

/// The caret line (1-based) of a Reader editing this note, after saving it.
fn prepare_open_note(root: &Path, rel: &str, cx: &mut App) -> anyhow::Result<Option<usize>> {
    for reader in reader_open::readers(cx) {
        let line = reader.update(cx, |this, cx| this.prepare_open_in(root, rel, cx))?;
        if line.is_some() {
            return Ok(line);
        }
    }
    Ok(None)
}

impl Reader {
    /// Save a dirty draft of `rel` and report its caret line; `Ok(None)` when
    /// this Reader is not editing that note.
    fn prepare_open_in(
        &mut self,
        root: &Path,
        rel: &str,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Option<usize>> {
        if self.vault_root != root || self.current_rel != rel {
            return Ok(None);
        }
        let Some(line) = self.editing_caret_line(cx) else {
            return Ok(None);
        };
        anyhow::ensure!(
            self.save_source(cx),
            "the note could not be saved; it was not opened"
        );
        Ok(Some(line))
    }
}

#[cfg(test)]
mod tests {
    use super::{detect_with, Editor, LineArg, Probe};
    use std::path::{Path, PathBuf};

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
    }

    #[test]
    fn line_arguments_match_each_editor() {
        let editor = |line| Editor {
            id: "x",
            label: "X",
            program: "/bin/x".into(),
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
