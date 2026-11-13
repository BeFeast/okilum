//! Pure command derivation and launcher selection. No process is started here.
use base64::{engine::general_purpose::STANDARD, Engine as _};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub struct Initial {
    pub argv: Vec<String>,
    pub directory: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    /// Always absolute: nothing is looked up from the current directory.
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// Windows: start in its own console (PowerShell without Windows Terminal).
    pub new_console: bool,
}

/// Overrides use shell-style word quoting, not shell evaluation. In particular,
/// `$()`, redirects, pipes and semicolons cannot become shell instructions.
pub fn override_argv(value: &str) -> Result<Vec<String>, String> {
    if value.contains(['\0', '\n', '\r']) {
        return Err("Terminal command must be one line".into());
    }
    let argv = shell_words::split(value).map_err(|_| "Terminal command has unmatched quotes")?;
    if argv
        .first()
        .is_none_or(|v| v.trim().is_empty() || v.starts_with('-'))
    {
        return Err("Terminal command needs an executable (for example ssh maestro)".into());
    }
    Ok(argv)
}

fn valid_host(host: &str) -> bool {
    if host.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        })
}

pub fn initial(
    origin: Option<&str>,
    needs_connection: bool,
    host_setting: Option<&str>,
    checkout: &str,
    custom: Option<&str>,
) -> Result<Initial, String> {
    if let Some(custom) = custom.filter(|v| !v.trim().is_empty()) {
        return Ok(Initial {
            argv: override_argv(custom)?,
            directory: None,
        });
    }
    let host = if let Some(host) = host_setting {
        Some(host.to_owned())
    } else if let Some(origin) = origin {
        let url = url::Url::parse(origin).map_err(|_| "T3 address is invalid")?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("T3 address must be an HTTP(S) origin without credentials".into());
        }
        match url.host().ok_or("T3 address has no host")? {
            url::Host::Domain(host) if host.eq_ignore_ascii_case("localhost") => None,
            url::Host::Ipv4(ip) if ip.is_loopback() => None,
            url::Host::Ipv6(ip) if ip.is_loopback() => None,
            host => Some(host.to_string().trim_matches(['[', ']']).to_owned()),
        }
    } else {
        if needs_connection {
            return Err(
                "Connect this project's T3 server or set its terminal command first".into(),
            );
        }
        None
    };
    if let Some(host) = host {
        if !valid_host(&host) {
            return Err("T3 host is not a valid SSH destination".into());
        }
        Ok(Initial {
            argv: vec!["ssh".into(), "--".into(), host],
            directory: None,
        })
    } else {
        if checkout.is_empty() || checkout.contains('\0') {
            return Err("Project checkout is missing or invalid".into());
        }
        Ok(Initial {
            argv: vec![],
            directory: Some(checkout.into()),
        })
    }
}

/// A terminal launcher: always an absolute program, never a bare name that the
/// OS could resolve from the current directory or the vault (#2837 review).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launcher {
    pub program: PathBuf,
    pub kind: Kind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `/usr/bin/osascript` driving Terminal.app (project command).
    AppleScript,
    /// `/usr/bin/open -a <app> <folder>`.
    Open,
    /// Windows Terminal; the System32 PowerShell runs encoded commands in it.
    WindowsTerminal { powershell: PathBuf },
    /// System32 PowerShell in a new console, without cmd.exe.
    PowerShell,
    /// The fixed POSIX wrapper, after `-e` unless the launcher takes argv directly.
    Posix { dash_e: bool },
    /// A known emulator opened in a folder with one `<option><folder>` word.
    Folder {
        flags: &'static [&'static str],
        option: &'static str,
    },
}

/// Finds a launcher in trusted locations only:
/// an absolute name must exist; `powershell.exe` comes from the system
/// directory only; other bare names come from absolute `PATH` entries outside
/// `excluded` (the vault, the selected folder). Relative entries, `.`, empty
/// entries and names with a directory part are never used, so a synced
/// `powershell.exe`, `wt.exe` or `xdg-terminal-exec` next to a note cannot run.
pub fn resolve(
    name: &str,
    path: Option<&OsStr>,
    excluded: &[PathBuf],
    system: Option<&Path>,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let candidate = Path::new(name);
    if candidate.is_absolute() {
        return exists(candidate).then(|| candidate.to_owned());
    }
    if name.is_empty() || candidate.components().count() != 1 || name.contains(['/', '\\']) {
        return None;
    }
    if name.eq_ignore_ascii_case("powershell.exe") {
        let system = system.filter(|s| s.is_absolute())?;
        let program = system
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        return exists(&program).then_some(program);
    }
    let inside = |dir: &Path| excluded.iter().any(|root| dir.starts_with(root));
    std::env::split_paths(path?).find_map(|dir| {
        if !dir.is_absolute() || inside(&dir) {
            return None;
        }
        // A PATH entry may be a link into the vault.
        if std::fs::canonicalize(&dir).is_ok_and(|real| inside(&real)) {
            return None;
        }
        let program = dir.join(name);
        exists(&program).then_some(program)
    })
}

fn windows_launcher(resolve: &impl Fn(&str) -> Option<PathBuf>) -> Result<Launcher, String> {
    let powershell = resolve("powershell.exe").ok_or(
        "Terminal could not open: Windows PowerShell is missing from the system directory",
    )?;
    Ok(match resolve("wt.exe") {
        Some(program) => Launcher {
            program,
            kind: Kind::WindowsTerminal { powershell },
        },
        None => Launcher {
            program: powershell,
            kind: Kind::PowerShell,
        },
    })
}

/// The project terminal's launcher. `resolve` maps a name to a trusted
/// absolute program (see [`resolve`]); tests inject a fake on every platform.
/// TERMINAL names one executable, so a path containing spaces stays one word.
pub fn launcher(
    os: &str,
    terminal: Option<&str>,
    resolve: impl Fn(&str) -> Option<PathBuf>,
) -> Result<Launcher, String> {
    match os {
        "macos" => resolve("/usr/bin/osascript")
            .map(|program| Launcher {
                program,
                kind: Kind::AppleScript,
            })
            .ok_or_else(|| "Terminal could not open: osascript is unavailable".into()),
        "windows" => windows_launcher(&resolve),
        _ => [
            ("xdg-terminal-exec", false),
            (terminal.unwrap_or(""), true),
            ("x-terminal-emulator", true),
        ]
        .into_iter()
        .filter(|(name, _)| !name.is_empty())
        .find_map(|(name, dash_e)| {
            resolve(name).map(|program| Launcher {
                program,
                kind: Kind::Posix { dash_e },
            })
        })
        .ok_or_else(|| {
            "No system terminal found: install xdg-terminal-exec or x-terminal-emulator, or set TERMINAL to an executable".into()
        }),
    }
}

/// Terminal emulators tried after the desktop's own choice, with the option
/// that starts them in a folder. `None` takes the POSIX wrapper after `-e`.
const KNOWN_FOLDER_TERMINALS: &[(&str, &[&str], Option<&str>)] = &[
    ("ptyxis", &["--new-window"], Some("--working-directory=")),
    ("kgx", &[], Some("--working-directory=")),
    ("gnome-terminal", &[], Some("--working-directory=")),
    ("konsole", &[], Some("--workdir=")),
    ("xfce4-terminal", &[], Some("--working-directory=")),
    ("tilix", &[], Some("--working-directory=")),
    ("terminator", &[], Some("--working-directory=")),
    ("alacritty", &[], Some("--working-directory=")),
    ("kitty", &[], Some("--directory=")),
    ("foot", &[], Some("--working-directory=")),
    ("wezterm", &["start"], Some("--cwd=")),
    ("xterm", &[], None),
];

/// The terminals a desktop ships, tried first among the known emulators.
fn native_to(desktop: &str, program: &str) -> bool {
    let desktop = desktop.to_ascii_uppercase();
    let names: &[&str] = if desktop.contains("GNOME") || desktop.contains("UNITY") {
        &["ptyxis", "kgx", "gnome-terminal"]
    } else if desktop.contains("KDE") {
        &["konsole"]
    } else if desktop.contains("XFCE") {
        &["xfce4-terminal"]
    } else {
        &[]
    };
    names.contains(&program)
}

/// The launcher for «Open in terminal» (#2837). Linux honours the desktop's
/// default first (xdg-terminal-exec, TERMINAL, x-terminal-emulator), then the
/// known emulators with the desktop's own first. macOS uses `/usr/bin/open`;
/// Windows prefers Windows Terminal, then PowerShell in a new console.
pub fn folder_launcher(
    os: &str,
    terminal: Option<&str>,
    desktop: Option<&str>,
    resolve: impl Fn(&str) -> Option<PathBuf>,
) -> Result<Launcher, String> {
    match os {
        "macos" => resolve("/usr/bin/open")
            .map(|program| Launcher {
                program,
                kind: Kind::Open,
            })
            .ok_or_else(|| "Terminal could not open: /usr/bin/open is unavailable".into()),
        "windows" => windows_launcher(&resolve),
        _ => launcher(os, terminal, &resolve).or_else(|error| {
            let mut known: Vec<_> = KNOWN_FOLDER_TERMINALS.iter().collect();
            known.sort_by_key(|(name, _, _)| !native_to(desktop.unwrap_or(""), name));
            known
                .into_iter()
                .find_map(|&(name, flags, option)| {
                    resolve(name).map(|program| Launcher {
                        program,
                        kind: match option {
                            Some(option) => Kind::Folder { flags, option },
                            None => Kind::Posix { dash_e: true },
                        },
                    })
                })
                .ok_or(error)
        }),
    }
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

// The POSIX shell receives data exclusively as positional arguments. It stays
// interactive after SSH/custom command exit; failed cd never runs in another cwd.
// A terminal started by the session manager (xdg-terminal-exec through uwsm or
// systemd-run) may lack SHELL; the account's login shell is used then, looked
// up before entering the folder. A non-interactive account shell (nologin,
// false) is replaced by /bin/sh (#2837).
const POSIX: &str = r#"login=${SHELL:-$(getent passwd "$(id -u)" 2>/dev/null | cut -d: -f7)}
case ${login##*/} in
    '' | nologin | false | true) login=/bin/sh ;;
esac
if [ "$1" = cd ]; then
    cd -- "$2" || exit
else
    shift
    "$@"
fi
exec "$login""#;

fn wrapper() -> Vec<OsString> {
    ["/bin/sh", "-c", POSIX, "okilum-terminal"]
        .into_iter()
        .map(OsString::from)
        .collect()
}

fn posix_args(initial: &Initial) -> Vec<String> {
    let mut args: Vec<String> = wrapper()
        .into_iter()
        .map(|v| v.into_string().expect("fixed wrapper is UTF-8"))
        .collect();
    if let Some(directory) = &initial.directory {
        args.extend(["cd".into(), directory.clone()]);
    } else {
        args.push("run".into());
        args.extend(initial.argv.clone());
    }
    args
}

/// The wrapper entering `directory`; the folder stays raw bytes on Unix.
fn posix_folder_args(directory: &Path) -> Vec<OsString> {
    let mut args = wrapper();
    args.extend([OsString::from("cd"), directory.as_os_str().to_owned()]);
    args
}

// Windows PowerShell 5's native `&` invocation can strip embedded quotes.
// ProcessStartInfo uses this explicit CRT argv encoding without another shell.
fn windows_quote(value: &str) -> String {
    let mut quoted = String::from("\"");
    let mut slashes = 0;
    for ch in value.chars() {
        if ch == '\\' {
            slashes += 1;
        } else {
            quoted.extend(std::iter::repeat_n(
                '\\',
                if ch == '\"' { slashes * 2 + 1 } else { slashes },
            ));
            quoted.push(ch);
            slashes = 0;
        }
    }
    quoted.extend(std::iter::repeat_n('\\', slashes * 2));
    quoted.push('\"');
    quoted
}

/// PowerShell's `-EncodedCommand` program: JSON data as base64, then
/// Set-Location -LiteralPath or ProcessStartInfo, never Invoke-Expression.
fn powershell_args(initial: &Initial) -> Vec<OsString> {
    let arguments = initial
        .argv
        .iter()
        .skip(1)
        .map(|v| windows_quote(v))
        .collect::<Vec<_>>()
        .join(" ");
    let data = serde_json::json!({"argv": initial.argv, "directory": initial.directory, "arguments": arguments});
    let data = STANDARD.encode(serde_json::to_vec(&data).expect("serializable terminal data"));
    let script = format!(
        r#"$v = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{data}')) | ConvertFrom-Json
if ($null -ne $v.directory) {{ Set-Location -LiteralPath $v.directory -ErrorAction Stop }}
else {{
    $p = New-Object System.Diagnostics.ProcessStartInfo
    $p.FileName = [string]$v.argv[0]
    $p.Arguments = [string]$v.arguments
    $p.UseShellExecute = $false
    $child = [System.Diagnostics.Process]::Start($p)
    $child.WaitForExit()
}}"#
    );
    let encoded = STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    ["-NoLogo", "-NoExit", "-EncodedCommand"]
        .into_iter()
        .map(OsString::from)
        .chain([OsString::from(encoded)])
        .collect()
}

fn posix_plan(program: &Path, dash_e: bool, words: Vec<OsString>) -> Plan {
    let mut args = if dash_e { vec!["-e".into()] } else { vec![] };
    args.extend(words);
    Plan {
        program: program.to_owned(),
        args,
        new_console: false,
    }
}

/// The project terminal: a command (SSH, override) or the local checkout.
pub fn plan(initial: &Initial, launcher: &Launcher) -> Plan {
    let program = launcher.program.clone();
    match &launcher.kind {
        Kind::AppleScript | Kind::Open => {
            // Terminal.app accepts shell text only. Serialize each argv word,
            // including the fixed wrapper, then pass it as AppleScript argv.
            let command = posix_args(initial)
                .iter()
                .map(|v| quote(v))
                .collect::<Vec<_>>()
                .join(" ");
            Plan {
                program,
                args: vec![
                    "-e".into(),
                    "on run argv\ntell application \"Terminal\"\nactivate\ndo script (item 1 of argv)\nend tell\nend run".into(),
                    "--".into(),
                    command.into(),
                ],
                new_console: false,
            }
        }
        // wt parses semicolons itself. Only fixed flags, the absolute System32
        // PowerShell and base64 cross its parser; no cmd.exe is involved.
        Kind::WindowsTerminal { powershell } => {
            let mut args = vec!["new-tab".into(), powershell.clone().into_os_string()];
            args.extend(powershell_args(initial));
            Plan {
                program,
                args,
                new_console: false,
            }
        }
        Kind::PowerShell => Plan {
            program,
            args: powershell_args(initial),
            new_console: true,
        },
        Kind::Posix { dash_e } => posix_plan(
            &program,
            *dash_e,
            posix_args(initial)
                .into_iter()
                .map(OsString::from)
                .collect(),
        ),
        // Only folder launches select these emulators; a command still runs.
        Kind::Folder { .. } => posix_plan(
            &program,
            true,
            posix_args(initial)
                .into_iter()
                .map(OsString::from)
                .collect(),
        ),
    }
}

/// Opens an interactive terminal in `directory`; no command runs in it. The
/// directory is absolute (never an option) and always one argv element, kept
/// as raw OS bytes except where PowerShell needs it as JSON text.
pub fn folder_plan(directory: &Path, launcher: &Launcher, mac_app: &str) -> Result<Plan, String> {
    // Canonical vault paths are absolute; a rooted path never starts with `-`.
    if !directory.has_root() {
        return Err("The folder path is not absolute".into());
    }
    let program = launcher.program.clone();
    let text = || {
        directory
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| "The folder path is not valid Unicode".to_owned())
    };
    let encoded = |text: String| Initial {
        argv: vec![],
        directory: Some(text),
    };
    Ok(match &launcher.kind {
        Kind::Open => Plan {
            program,
            args: vec![
                "-a".into(),
                mac_app.into(),
                directory.as_os_str().to_owned(),
            ],
            new_console: false,
        },
        Kind::AppleScript => plan(&encoded(text()?), launcher),
        // wt splits its command line at `;`; such a folder takes the encoded
        // PowerShell route, which carries the folder as base64 data.
        Kind::WindowsTerminal { .. } if !text()?.contains(';') => Plan {
            program,
            args: vec!["-d".into(), directory.as_os_str().to_owned()],
            new_console: false,
        },
        Kind::WindowsTerminal { .. } | Kind::PowerShell => plan(&encoded(text()?), launcher),
        Kind::Folder { flags, option } => {
            let mut args: Vec<OsString> = flags.iter().map(OsString::from).collect();
            let mut word = OsString::from(option);
            word.push(directory.as_os_str());
            args.push(word);
            Plan {
                program,
                args,
                new_console: false,
            }
        }
        Kind::Posix { dash_e } => posix_plan(&program, *dash_e, posix_folder_args(directory)),
    })
}

/// Variables an AppImage runtime adds for Okilum itself. A terminal started
/// from it must see the user's session, not Okilum's bundle.
const APPIMAGE_ONLY: &[&str] = &[
    "APPIMAGE",
    "APPDIR",
    "ARGV0",
    "OWD",
    "APPIMAGE_EXTRACT_AND_RUN",
    APPRUN_PRELOAD,
];
/// Set by `scripts/appimage/apprun.sh` to the host ALSA loader it prepends to
/// LD_PRELOAD; that one entry is removed again for the child.
const APPRUN_PRELOAD: &str = "OKILUM_APPRUN_PRELOAD";
/// Search paths and module locations a bundle (AppRun, linuxdeploy plugins,
/// GTK/Qt/GStreamer/Python hooks) may point into APPDIR. Entries inside APPDIR
/// are dropped; a value the user set outside the bundle is kept.
const APPIMAGE_PATH_LISTS: &[&str] = &[
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "PATH",
    "XDG_DATA_DIRS",
    "XDG_CONFIG_DIRS",
    "GSETTINGS_SCHEMA_DIR",
    "GDK_PIXBUF_MODULE_FILE",
    "GDK_PIXBUF_MODULEDIR",
    "GIO_MODULE_DIR",
    "GIO_EXTRA_MODULES",
    "GTK_PATH",
    "GTK_EXE_PREFIX",
    "GTK_DATA_PREFIX",
    "GTK_IM_MODULE_FILE",
    "GST_PLUGIN_PATH",
    "GST_PLUGIN_PATH_1_0",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_SYSTEM_PATH_1_0",
    "GST_PLUGIN_SCANNER",
    "GST_PLUGIN_SCANNER_1_0",
    "QT_PLUGIN_PATH",
    "QT_QPA_PLATFORM_PLUGIN_PATH",
    "QML2_IMPORT_PATH",
    "PYTHONHOME",
    "PYTHONPATH",
    "PERLLIB",
    "PERL5LIB",
    "FONTCONFIG_FILE",
    "FONTCONFIG_PATH",
    "LIBGL_DRIVERS_PATH",
    "LIBVA_DRIVERS_PATH",
    "__EGL_VENDOR_LIBRARY_DIRS",
    "VK_ICD_FILENAMES",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

/// Environment edits for a child of an AppImage launch: `Some` sets, `None`
/// removes. Outside an AppImage (no APPDIR) nothing changes.
pub fn appimage_env(get: impl Fn(&str) -> Option<String>) -> Vec<(&'static str, Option<String>)> {
    let Some(appdir) = get("APPDIR").filter(|d| !d.is_empty()) else {
        return vec![];
    };
    let appdir = appdir.trim_end_matches('/').to_owned();
    let inside = |entry: &str| entry == appdir || entry.starts_with(&format!("{appdir}/"));
    let apprun = get(APPRUN_PRELOAD).filter(|v| !v.is_empty());
    let mut edits: Vec<(&'static str, Option<String>)> =
        APPIMAGE_ONLY.iter().map(|name| (*name, None)).collect();
    for name in APPIMAGE_PATH_LISTS {
        let Some(value) = get(name) else { continue };
        let separator = if *name == "LD_PRELOAD" && !value.contains(':') {
            ' '
        } else {
            ':'
        };
        let entries: Vec<&str> = value.split(separator).filter(|e| !e.is_empty()).collect();
        let mut kept: Vec<&str> = entries.iter().copied().filter(|e| !inside(e)).collect();
        if *name == "LD_PRELOAD" {
            // AppRun prepended exactly this host loader; the user's own stay.
            if let Some(position) = apprun
                .as_deref()
                .and_then(|added| kept.iter().position(|e| *e == added))
            {
                kept.remove(position);
            }
        }
        if kept.len() != entries.len() {
            edits.push((
                name,
                (!kept.is_empty()).then(|| kept.join(&separator.to_string())),
            ));
        }
    }
    edits
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod at_argument_tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn powershell_at_arguments_are_encoded_data_not_splatting() {
        let initial = Initial {
            argv: vec![
                "tool.exe".into(),
                "@AgentContract.md".into(),
                "@Assets/Files/a b.pdf".into(),
            ],
            directory: None,
        };
        let launch = plan(
            &initial,
            &Launcher {
                program: "/trusted/wt.exe".into(),
                kind: Kind::WindowsTerminal {
                    powershell: "/system/powershell.exe".into(),
                },
            },
        );
        assert!(!launch
            .args
            .iter()
            .any(|arg| arg.to_str().unwrap().starts_with('@')));
        assert_eq!(windows_quote("@AgentContract.md"), "\"@AgentContract.md\"");
        let script = String::from_utf16(
            &STANDARD
                .decode(launch.args.last().unwrap().to_str().unwrap())
                .unwrap()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let data = script
            .split("FromBase64String('")
            .nth(1)
            .unwrap()
            .split("')")
            .next()
            .unwrap();
        let payload: serde_json::Value =
            serde_json::from_slice(&STANDARD.decode(data).unwrap()).unwrap();
        assert_eq!(payload["argv"], serde_json::json!(initial.argv));
        assert_eq!(
            payload["arguments"],
            serde_json::json!("\"@AgentContract.md\" \"@Assets/Files/a b.pdf\"")
        );
        assert!(script.contains("FromBase64String"));
        assert!(
            !script.contains("@AgentContract.md"),
            "user arguments are JSON inside base64 data"
        );
    }
}
