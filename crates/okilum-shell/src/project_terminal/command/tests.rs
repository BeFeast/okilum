use super::*;

/// A fake trusted lookup: installed names live under `/trusted`; absolute
/// names are installed as themselves.
fn fake(installed: &'static [&'static str]) -> impl Fn(&str) -> Option<PathBuf> {
    move |name| {
        installed.contains(&name).then(|| {
            if name.starts_with('/') {
                PathBuf::from(name)
            } else {
                Path::new("/trusted").join(name)
            }
        })
    }
}

fn launcher_of(kind: Kind, program: &str) -> Launcher {
    Launcher {
        program: program.into(),
        kind,
    }
}

fn words(plan: &Plan) -> Vec<String> {
    plan.args
        .iter()
        .map(|a| a.to_str().unwrap().to_owned())
        .collect()
}

/// Absolute on the platform the tests run on.
fn windows_path(path: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!("C:{}", path.replace('/', "\\")))
    } else {
        PathBuf::from(path)
    }
}

fn powershell() -> PathBuf {
    windows_path("/Windows/System32/WindowsPowerShell/v1.0/powershell.exe")
}

#[test]
fn terminal_selection_all_platforms_uses_only_fake_lookup() {
    let program =
        |os, terminal, installed| launcher(os, terminal, fake(installed)).map(|l| l.program);
    assert_eq!(
        program(
            "linux",
            Some("kitty"),
            &["xdg-terminal-exec", "kitty", "x-terminal-emulator"]
        )
        .unwrap(),
        Path::new("/trusted/xdg-terminal-exec")
    );
    assert_eq!(
        launcher(
            "linux",
            Some("kitty"),
            fake(&["kitty", "x-terminal-emulator"])
        )
        .unwrap(),
        launcher_of(Kind::Posix { dash_e: true }, "/trusted/kitty")
    );
    assert_eq!(
        program("linux", Some("missing"), &["x-terminal-emulator"]).unwrap(),
        Path::new("/trusted/x-terminal-emulator")
    );
    assert_eq!(
        program(
            "linux",
            Some("/Applications/My Terminal"),
            &["/Applications/My Terminal"]
        )
        .unwrap(),
        Path::new("/Applications/My Terminal")
    );
    assert_eq!(
        launcher("macos", None, fake(&["/usr/bin/osascript"])).unwrap(),
        launcher_of(Kind::AppleScript, "/usr/bin/osascript")
    );
    assert_eq!(
        launcher("windows", None, fake(&["wt.exe", "powershell.exe"])).unwrap(),
        launcher_of(
            Kind::WindowsTerminal {
                powershell: "/trusted/powershell.exe".into()
            },
            "/trusted/wt.exe"
        )
    );
    assert_eq!(
        launcher("windows", None, fake(&["powershell.exe"])).unwrap(),
        launcher_of(Kind::PowerShell, "/trusted/powershell.exe")
    );
    assert!(
        launcher("windows", None, fake(&["wt.exe", "cmd.exe"])).is_err(),
        "cmd.exe is never a launcher; PowerShell must come from the system directory"
    );
    for os in ["linux", "macos", "windows"] {
        assert!(
            launcher(os, None, fake(&[])).is_err(),
            "{os} reports a missing terminal"
        );
    }
}

#[test]
fn remote_local_host_setting_and_override_derivation() {
    for origin in [
        "http://localhost:3773",
        "http://127.2.3.4:3773",
        "http://[::1]:3773",
    ] {
        assert_eq!(
            initial(Some(origin), true, None, "/checkout", None)
                .unwrap()
                .directory
                .as_deref(),
            Some("/checkout")
        );
    }
    for (origin, host) in [
        ("https://maestro:3773", "maestro"),
        ("https://192.0.2.7", "192.0.2.7"),
        ("https://[2001:db8::7]", "2001:db8::7"),
    ] {
        assert_eq!(
            initial(Some(origin), true, None, "ignored", None)
                .unwrap()
                .argv,
            ["ssh", "--", host]
        );
    }
    assert_eq!(
        initial(None, false, None, "/checkout", None)
            .unwrap()
            .directory
            .as_deref(),
        Some("/checkout")
    );
    assert!(initial(None, true, None, "/checkout", None).is_err());
    assert_eq!(
        initial(Some("https://t3.example"), true, Some("maestro"), "", None)
            .unwrap()
            .argv,
        ["ssh", "--", "maestro"]
    );
    assert_eq!(
        initial(None, true, None, "", Some("ssh 'my-host'"))
            .unwrap()
            .argv,
        ["ssh", "my-host"]
    );
    assert_eq!(
        initial(None, false, None, "/checkout", Some("  "))
            .unwrap()
            .directory
            .as_deref(),
        Some("/checkout")
    );
    assert!(initial(Some("https://user:password@maestro"), true, None, "", None).is_err());
    assert!(initial(Some("file:///checkout"), true, None, "", None).is_err());
}

#[test]
fn invalid_hosts_and_override_syntax_fail_closed() {
    for host in [
        "",
        "-oProxyCommand=evil",
        "a;evil",
        "a\nevil",
        "user@host",
        "a/host",
        "a b",
        "a$()",
        "a:22",
        "..",
        "-host",
        "host-",
    ] {
        assert!(
            initial(None, true, Some(host), "", None).is_err(),
            "{host:?}"
        );
    }
    for value in ["\0", "a\nb", "a\rb", "'unclosed", "''", "-option"] {
        assert!(override_argv(value).is_err(), "{value:?}");
    }
    assert_eq!(
        override_argv(r#"ssh maestro ';' '$(touch danger)' 'a"b'"#).unwrap(),
        ["ssh", "maestro", ";", "$(touch danger)", "a\"b"]
    );
}

fn decode_windows(plan: &Plan) -> serde_json::Value {
    let encoded = plan.args.last().unwrap().to_str().unwrap();
    assert!(encoded
        .bytes()
        .all(|v| v.is_ascii_alphanumeric() || b"+/=".contains(&v)));
    let bytes = STANDARD.decode(encoded).unwrap();
    let utf16: Vec<_> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|v| u16::from_le_bytes(*v))
        .collect();
    let script = String::from_utf16(&utf16).unwrap();
    assert!(!script.contains("Invoke-Expression"));
    let payload = script
        .split("FromBase64String('")
        .nth(1)
        .unwrap()
        .split("')")
        .next()
        .unwrap();
    serde_json::from_slice(&STANDARD.decode(payload).unwrap()).unwrap()
}

fn windows_launchers() -> [Launcher; 2] {
    [
        launcher_of(
            Kind::WindowsTerminal {
                powershell: powershell(),
            },
            windows_path("/Users/me/AppData/Local/Microsoft/WindowsApps/wt.exe")
                .to_str()
                .unwrap(),
        ),
        launcher_of(Kind::PowerShell, powershell().to_str().unwrap()),
    ]
}

#[test]
fn hostile_arguments_survive_every_platform_boundary_as_data() {
    let hostile = "space ' \" ; & | > < %PATH% !x! $() ` \\ 🦀\n";
    let run = Initial {
        argv: vec!["printf".into(), hostile.into()],
        directory: None,
    };
    for initial in [
        run,
        Initial {
            argv: vec![],
            directory: Some(hostile.into()),
        },
    ] {
        for (dash_e, program) in [
            (false, "/usr/bin/xdg-terminal-exec"),
            (true, "/usr/bin/kitty"),
        ] {
            let p = plan(&initial, &launcher_of(Kind::Posix { dash_e }, program));
            let offset = usize::from(dash_e);
            assert_eq!(words(&p)[offset..], posix_args(&initial));
            assert!(
                !words(&p)[offset + 2].contains(hostile),
                "fixed shell wrapper contains no data"
            );
            assert_eq!(p.args.last().unwrap(), hostile);
        }
        let mac = plan(
            &initial,
            &launcher_of(Kind::AppleScript, "/usr/bin/osascript"),
        );
        assert!(
            !words(&mac)[1].contains(hostile),
            "fixed AppleScript contains no data"
        );
        assert_eq!(
            shell_words::split(mac.args.last().unwrap().to_str().unwrap()).unwrap(),
            posix_args(&initial)
        );
        for launcher in windows_launchers() {
            let win = plan(&initial, &launcher);
            let data = decode_windows(&win);
            assert_eq!(data["argv"], serde_json::json!(initial.argv));
            assert_eq!(data["directory"], serde_json::json!(initial.directory));
            assert!(win.args.contains(&"-NoExit".into()));
            assert!(!words(&win).iter().any(|v| v.contains(hostile)));
        }
    }
}

/// #2837 review: no Windows plan goes through cmd.exe or `start`, and the
/// PowerShell it starts is the absolute System32 one, never a bare name that
/// cmd or wt would look up in the selected folder first.
#[test]
fn windows_plans_start_only_absolute_programs_without_cmd() {
    let folder = Path::new("/Vault/Notes;x");
    let project = Initial {
        argv: vec!["ssh".into(), "maestro".into()],
        directory: None,
    };
    for launcher in windows_launchers() {
        for plan in [
            plan(&project, &launcher),
            folder_plan(folder, &launcher, "Terminal").unwrap(),
            folder_plan(Path::new("/Vault/Notes"), &launcher, "Terminal").unwrap(),
        ] {
            assert!(plan.program.is_absolute(), "{plan:?}");
            let args = words(&plan);
            for bare in ["cmd.exe", "start", "/c", "powershell.exe", "pwsh.exe"] {
                assert!(
                    !args.iter().any(|a| a.eq_ignore_ascii_case(bare)),
                    "{bare} in {args:?}"
                );
            }
            if let Some(position) = args.iter().position(|a| a.ends_with("powershell.exe")) {
                assert_eq!(args[position], powershell().to_str().unwrap());
            }
        }
    }
    let console = plan(&project, &windows_launchers()[1]);
    assert_eq!(console.program, powershell());
    assert!(
        console.new_console,
        "PowerShell without wt gets its own console"
    );
    assert!(!plan(&project, &windows_launchers()[0]).new_console);
}

#[cfg(unix)]
#[test]
fn fixed_shell_wrapper_does_not_evaluate_argv_and_enters_the_literal_checkout() {
    // Execute the wrapper with printf and a harmless exit shell; no terminal.
    let value = "$(exit 99); 'quote' \"double\" `exit 99` & end";
    let run = Initial {
        argv: vec!["printf".into(), "%s".into(), value.into()],
        directory: None,
    };
    let xdg = launcher_of(Kind::Posix { dash_e: false }, "/usr/bin/xdg-terminal-exec");
    let p = plan(&run, &xdg);
    let output = std::process::Command::new(&p.args[0])
        .args(&p.args[1..])
        .env("SHELL", "/bin/true")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), value);
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("space ' $(exit 99)");
    std::fs::create_dir(&directory).unwrap();
    let shell = root.path().join("fake-shell");
    executable_file(&shell, "#!/bin/sh\npwd -P\n");
    let initial = initial(None, false, None, directory.to_str().unwrap(), None).unwrap();
    let p = plan(&initial, &xdg);
    let output = std::process::Command::new(&p.args[0])
        .args(&p.args[1..])
        .env("SHELL", shell)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        directory.canonicalize().unwrap().to_str().unwrap()
    );
}

#[test]
fn windows_crt_argv_encoding_preserves_quotes_spaces_and_trailing_backslashes() {
    assert_eq!(windows_quote(""), r#""""#);
    assert_eq!(windows_quote("a b"), r#""a b""#);
    assert_eq!(windows_quote("a\"b"), r#""a\"b""#);
    assert_eq!(windows_quote("C:\\a b\\"), r#""C:\a b\\""#);
    assert_eq!(windows_quote("a\\\"b"), r#""a\\\"b""#);
    let initial = Initial {
        argv: vec![
            "tool.exe".into(),
            "a\"b".into(),
            "".into(),
            "C:\\a b\\".into(),
        ],
        directory: None,
    };
    let data = decode_windows(&plan(&initial, &windows_launchers()[0]));
    assert_eq!(data["arguments"], r#""a\"b" "" "C:\a b\\""#);
}

/// #2837: names a shell, option parser or PowerShell would misread.
const HOSTILE_FOLDERS: &[&str] = &[
    "/v/-rf folder",
    "/v/@Assets/Files",
    "/v/space ' quote \" double $(exit 99) `x` & | > %PATH% !x!",
    "/v/Папка ✓ 🦀",
];

#[test]
fn folder_terminal_selection_honours_the_desktop_default_then_known_emulators() {
    let choose = |os, terminal, desktop, installed| {
        folder_launcher(os, terminal, desktop, fake(installed)).map(|l| l.program)
    };
    // The desktop's own choice comes before any emulator Okilum knows.
    assert_eq!(
        choose(
            "linux",
            Some("kitty"),
            Some("ubuntu:GNOME"),
            &[
                "xdg-terminal-exec",
                "kitty",
                "x-terminal-emulator",
                "ptyxis"
            ]
        )
        .unwrap(),
        Path::new("/trusted/xdg-terminal-exec")
    );
    assert_eq!(
        choose("linux", Some("kitty"), None, &["kitty", "ptyxis"]).unwrap(),
        Path::new("/trusted/kitty")
    );
    assert_eq!(
        choose("linux", None, None, &["x-terminal-emulator", "ptyxis"]).unwrap(),
        Path::new("/trusted/x-terminal-emulator")
    );
    // Without one, the desktop's native terminal before other known ones.
    let installed: &'static [&'static str] = &["xterm", "konsole", "gnome-terminal", "ptyxis"];
    assert_eq!(
        folder_launcher("linux", None, Some("ubuntu:GNOME"), fake(installed)).unwrap(),
        launcher_of(
            Kind::Folder {
                flags: &["--new-window"],
                option: "--working-directory="
            },
            "/trusted/ptyxis"
        )
    );
    assert_eq!(
        choose("linux", Some("missing"), Some("KDE"), installed).unwrap(),
        Path::new("/trusted/konsole")
    );
    assert_eq!(
        choose("linux", None, Some("Hyprland"), &["xterm", "foot"]).unwrap(),
        Path::new("/trusted/foot")
    );
    assert_eq!(
        folder_launcher("linux", None, None, fake(&["xterm"]))
            .unwrap()
            .kind,
        Kind::Posix { dash_e: true }
    );
    assert_eq!(
        folder_launcher("macos", None, None, fake(&["/usr/bin/open"])).unwrap(),
        launcher_of(Kind::Open, "/usr/bin/open")
    );
    assert_eq!(
        choose("windows", None, None, &["wt.exe", "powershell.exe"]).unwrap(),
        Path::new("/trusted/wt.exe")
    );
    assert_eq!(
        choose("windows", None, None, &["powershell.exe"]).unwrap(),
        Path::new("/trusted/powershell.exe")
    );
    for os in ["linux", "macos", "windows"] {
        assert!(
            choose(os, None, Some("GNOME"), &[]).is_err(),
            "{os} reports a missing terminal"
        );
    }
}

fn known(name: &str) -> Launcher {
    let &(_, flags, option) = KNOWN_FOLDER_TERMINALS.iter().find(|k| k.0 == name).unwrap();
    launcher_of(
        Kind::Folder {
            flags,
            option: option.unwrap(),
        },
        &format!("/usr/bin/{name}"),
    )
}

#[test]
fn folder_plans_pass_hostile_folders_as_one_argument_on_every_platform() {
    for folder in HOSTILE_FOLDERS {
        let folder = *folder;
        let path = Path::new(folder);
        // macOS: Terminal.app (or a configured app) opens the folder; no shell text.
        let open = launcher_of(Kind::Open, "/usr/bin/open");
        let mac = folder_plan(path, &open, "Terminal").unwrap();
        assert_eq!(mac.program, Path::new("/usr/bin/open"));
        assert_eq!(words(&mac), ["-a", "Terminal", folder]);
        assert_eq!(
            words(&folder_plan(path, &open, "iTerm").unwrap())[1],
            "iTerm"
        );
        // Linux known emulators: one `--option=folder` word, never `-e sh -c`.
        for (terminal, expected) in [
            (
                "ptyxis",
                vec![
                    "--new-window".to_owned(),
                    format!("--working-directory={folder}"),
                ],
            ),
            ("kgx", vec![format!("--working-directory={folder}")]),
            (
                "gnome-terminal",
                vec![format!("--working-directory={folder}")],
            ),
            ("konsole", vec![format!("--workdir={folder}")]),
            ("kitty", vec![format!("--directory={folder}")]),
            ("foot", vec![format!("--working-directory={folder}")]),
            ("wezterm", vec!["start".into(), format!("--cwd={folder}")]),
        ] {
            let plan = folder_plan(path, &known(terminal), "Terminal").unwrap();
            assert_eq!(plan.program, Path::new("/usr/bin").join(terminal));
            assert_eq!(words(&plan), expected, "{terminal}");
        }
        // The desktop default, TERMINAL and xterm: the fixed wrapper, folder as data.
        let directory = Initial {
            argv: vec![],
            directory: Some(folder.into()),
        };
        for (dash_e, terminal) in [
            (false, "/usr/bin/xdg-terminal-exec"),
            (true, "/usr/bin/x-terminal-emulator"),
            (true, "/opt/My Term"),
        ] {
            let launcher = launcher_of(Kind::Posix { dash_e }, terminal);
            let plan = folder_plan(path, &launcher, "Terminal").unwrap();
            assert_eq!(plan.args.last().unwrap(), folder);
            assert_eq!(plan, super::plan(&directory, &launcher));
            assert!(
                !words(&plan)[..plan.args.len() - 1]
                    .iter()
                    .any(|a| a.contains(folder)),
                "the wrapper itself carries no data"
            );
        }
        let [wt, console] = windows_launchers();
        // Windows Terminal takes the folder after -d as one CRT argument.
        assert_eq!(
            words(&folder_plan(path, &wt, "Terminal").unwrap()),
            ["-d", folder]
        );
        // Without wt the folder is base64 data for the System32 PowerShell.
        let ps = folder_plan(path, &console, "Terminal").unwrap();
        assert_eq!(ps.program, powershell());
        assert!(!words(&ps).iter().any(|a| a.contains(folder)));
        assert_eq!(decode_windows(&ps)["directory"], folder);
        assert_eq!(decode_windows(&ps)["argv"], serde_json::json!([]));
    }
    // wt splits its command line at `;`, so such a folder is encoded data too.
    let folder = "/Users/me/a;b @x";
    let [wt, _] = windows_launchers();
    let semicolon = folder_plan(Path::new(folder), &wt, "Terminal").unwrap();
    assert_eq!(
        words(&semicolon)[..2],
        ["new-tab", powershell().to_str().unwrap()]
    );
    assert!(!words(&semicolon).iter().any(|a| a.contains(folder)));
    assert_eq!(decode_windows(&semicolon)["directory"], folder);
    // Positive control: an ordinary folder takes the plain route.
    assert_eq!(
        words(&folder_plan(Path::new("/Users/me/Notes"), &wt, "Terminal").unwrap()),
        ["-d", "/Users/me/Notes"]
    );
    assert!(folder_plan(Path::new("relative"), &wt, "Terminal").is_err());
}

/// Writes an executable script from a child shell. Writing it in this
/// process could leak the write descriptor into a concurrent test's fork, and
/// executing the script then fails with ETXTBSY.
#[cfg(unix)]
fn executable_file(path: &Path, body: &str) {
    use std::io::Write as _;
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", r#"cat > "$1" && chmod 700 "$1""#, "sh"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
}

#[cfg(unix)]
#[test]
fn folder_wrapper_enters_a_hostile_folder_literally() {
    let root = tempfile::tempdir().unwrap();
    let shell = root.path().join("fake-shell");
    executable_file(&shell, "#!/bin/sh\npwd -P\n");
    let xdg = launcher_of(Kind::Posix { dash_e: false }, "/usr/bin/xdg-terminal-exec");
    for name in [
        "-rf folder",
        "@Assets",
        "space ' \" $(exit 99) & ;",
        "Папка ✓",
    ] {
        let folder = root.path().join(name);
        std::fs::create_dir(&folder).unwrap();
        let plan = folder_plan(&folder, &xdg, "").unwrap();
        let output = std::process::Command::new(&plan.args[0])
            .args(&plan.args[1..])
            .env("SHELL", &shell)
            .output()
            .unwrap();
        assert!(output.status.success(), "{name}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            folder.canonicalize().unwrap().to_str().unwrap(),
            "{name}"
        );
    }
}

/// #2837 review: a vault path that is not UTF-8 still opens on Linux; the
/// folder stays raw bytes through every Unix plan.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_folders_stay_raw_bytes() {
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join(OsStr::from_bytes(b"caf\xe9 -x"));
    std::fs::create_dir(&folder).unwrap();
    assert!(folder.to_str().is_none(), "the control folder is not UTF-8");
    let shell = root.path().join("fake-shell");
    executable_file(&shell, "#!/bin/sh\npwd -P\n");
    let xdg = launcher_of(Kind::Posix { dash_e: false }, "/usr/bin/xdg-terminal-exec");
    let plan = folder_plan(&folder, &xdg, "").unwrap();
    assert_eq!(plan.args.last().unwrap(), folder.as_os_str());
    let output = std::process::Command::new(&plan.args[0])
        .args(&plan.args[1..])
        .env("SHELL", &shell)
        .output()
        .unwrap();
    assert!(output.status.success());
    let mut printed = output.stdout;
    assert_eq!(printed.pop(), Some(b'\n'));
    assert_eq!(
        std::ffi::OsString::from_vec(printed),
        folder.canonicalize().unwrap().into_os_string()
    );
    let mut word = OsString::from("--working-directory=");
    word.push(&folder);
    assert_eq!(
        folder_plan(&folder, &known("foot"), "").unwrap().args,
        [word]
    );
    assert_eq!(
        folder_plan(
            &folder,
            &launcher_of(Kind::Open, "/usr/bin/open"),
            "Terminal"
        )
        .unwrap()
        .args[2]
            .as_os_str(),
        folder.as_os_str()
    );
    // Only PowerShell needs text: it reports the folder instead of mangling it.
    assert!(folder_plan(&folder, &windows_launchers()[1], "").is_err());
}

/// #2837 review: a launcher with the same name next to the note, in a
/// relative/empty PATH entry or anywhere in the vault is never chosen.
#[test]
fn decoy_launchers_in_the_vault_or_current_directory_are_never_chosen() {
    let root = tempfile::tempdir().unwrap();
    let vault = root.path().join("vault");
    let folder = vault.join("Notes");
    let trusted = root.path().join("bin");
    let system = root.path().join("System32");
    let ps_dir = system.join("WindowsPowerShell").join("v1.0");
    for dir in [&folder, &trusted, &ps_dir] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let names = [
        "powershell.exe",
        "pwsh.exe",
        "wt.exe",
        "cmd.exe",
        "xdg-terminal-exec",
        "foot",
    ];
    for name in names {
        std::fs::write(folder.join(name), b"decoy").unwrap();
        std::fs::write(vault.join(name), b"decoy").unwrap();
    }
    std::fs::write(trusted.join("wt.exe"), b"trusted").unwrap();
    std::fs::write(trusted.join("xdg-terminal-exec"), b"trusted").unwrap();
    std::fs::write(ps_dir.join("powershell.exe"), b"trusted").unwrap();
    let exists = |path: &Path| path.is_file();
    let excluded = [
        vault.clone(),
        folder.clone(),
        vault.canonicalize().unwrap(),
        folder.canonicalize().unwrap(),
    ];
    // The selected folder first in PATH, plus relative and empty entries.
    let path = std::env::join_paths([
        PathBuf::from("."),
        PathBuf::from("Notes"),
        folder.clone(),
        vault.clone(),
        trusted.clone(),
    ])
    .unwrap();
    let find = |name: &str| resolve(name, Some(&path), &excluded, Some(&system), exists);
    assert_eq!(find("wt.exe"), Some(trusted.join("wt.exe")));
    assert_eq!(
        find("xdg-terminal-exec"),
        Some(trusted.join("xdg-terminal-exec"))
    );
    assert_eq!(
        find("powershell.exe"),
        Some(ps_dir.join("powershell.exe")),
        "PowerShell only from the system directory"
    );
    assert_eq!(find("foot"), None, "only decoys exist: nothing is chosen");
    assert_eq!(find("cmd.exe"), None);
    // A name with a directory part, or a relative TERMINAL, is refused.
    for name in ["./wt.exe", "Notes/foot", r".\wt.exe", ""] {
        assert_eq!(find(name), None, "{name}");
    }
    // Without a system directory there is no PowerShell, even on PATH.
    assert_eq!(
        resolve("powershell.exe", Some(&path), &excluded, None, exists),
        None
    );
    // Positive control: without exclusions the decoy in the folder would win.
    assert_eq!(
        resolve("foot", Some(&path), &[], Some(&system), exists),
        Some(folder.join("foot"))
    );
    // A PATH entry that links into the vault is excluded as well.
    #[cfg(unix)]
    {
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&folder, &link).unwrap();
        let path = std::env::join_paths([link]).unwrap();
        assert_eq!(resolve("foot", Some(&path), &excluded, None, exists), None);
    }
    // Every launcher the selection returns is absolute.
    let real = |name: &str| resolve(name, Some(&path), &excluded, Some(&system), exists);
    for os in ["windows", "linux"] {
        if let Ok(launcher) = folder_launcher(os, Some("foot"), None, real) {
            assert!(
                launcher.program.starts_with(&trusted) || launcher.program.starts_with(&system)
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn without_shell_the_wrapper_starts_the_accounts_login_shell() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let account_shell = |shell: &Path| {
        executable_file(
            &bin.join("getent"),
            &format!(
                "#!/bin/sh\necho \"me:x:1000:1000::/home/me:{}\"\n",
                shell.display()
            ),
        )
    };
    let folder = root.path().join("-rf folder");
    std::fs::create_dir(&folder).unwrap();
    let xdg = launcher_of(Kind::Posix { dash_e: false }, "/usr/bin/xdg-terminal-exec");
    let plan = folder_plan(&folder, &xdg, "").unwrap();
    let run = |shell: Option<&Path>| {
        let mut command = std::process::Command::new(&plan.args[0]);
        command
            .args(&plan.args[1..])
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env_remove("SHELL")
            .stdin(std::process::Stdio::null());
        if let Some(shell) = shell {
            command.env("SHELL", shell);
        }
        let output = command.output().unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    };
    // bash as /bin/sh assigns SHELL from passwd itself; dash (Debian/Ubuntu)
    // does not, which is when the wrapper's own lookup matters.
    let sh_assigns_shell = std::process::Command::new("/bin/sh")
        .args(["-c", "echo ${SHELL+set}"])
        .env_remove("SHELL")
        .output()
        .unwrap()
        .stdout
        .starts_with(b"set");
    let login = root.path().join("login-shell");
    executable_file(&login, "#!/bin/sh\necho login-shell\n");
    account_shell(&login);
    if !sh_assigns_shell {
        assert_eq!(run(None).trim(), "login-shell", "SHELL unset: login shell");
    }
    // Positive control: a set SHELL still wins over the account entry.
    let set = root.path().join("set-shell");
    executable_file(&set, "#!/bin/sh\npwd -P\n");
    assert_eq!(
        run(Some(&set)).trim(),
        folder.canonicalize().unwrap().to_str().unwrap()
    );
    // A non-interactive account shell is not executed; /bin/sh reads the
    // (empty) input instead.
    for name in ["nologin", "false"] {
        let dir = root.path().join(format!("sbin-{name}"));
        std::fs::create_dir(&dir).unwrap();
        let blocked = dir.join(name);
        executable_file(&blocked, "#!/bin/sh\necho RAN-ACCOUNT-SHELL\n");
        account_shell(&blocked);
        if !sh_assigns_shell {
            assert_eq!(run(None), "", "{name} is replaced by /bin/sh");
        }
        assert_eq!(run(Some(&blocked)), "", "SHELL={name} is replaced too");
    }
}

#[test]
fn appimage_children_lose_only_the_bundles_environment() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    };
    // Outside an AppImage nothing changes, even with a library path set.
    assert!(appimage_env(env(&[("LD_LIBRARY_PATH", "/opt/lib")])).is_empty());
    let edits = appimage_env(env(&[
        ("APPDIR", "/tmp/.mount_OkilumX/"),
        ("APPIMAGE", "/home/me/Okilum.AppImage"),
        ("OKILUM_APPRUN_PRELOAD", "/usr/lib/libasound.so.2"),
        ("LD_LIBRARY_PATH", "/tmp/.mount_OkilumX/usr/lib"),
        ("LD_PRELOAD", "/usr/lib/libasound.so.2:/home/me/libmine.so"),
        ("PATH", "/tmp/.mount_OkilumX/usr/bin:/home/me/bin:/usr/bin"),
        ("XDG_DATA_DIRS", "/tmp/.mount_OkilumXother/share:/usr/share"),
        ("PYTHONHOME", "/tmp/.mount_OkilumX/usr"),
        (
            "GIO_EXTRA_MODULES",
            "/tmp/.mount_OkilumX/usr/lib/gio/modules",
        ),
        (
            "QT_QPA_PLATFORM_PLUGIN_PATH",
            "/tmp/.mount_OkilumX/usr/plugins/platforms",
        ),
        ("GST_PLUGIN_SYSTEM_PATH", "/tmp/.mount_OkilumX/usr/lib/gst"),
        ("QT_PLUGIN_PATH", "/home/me/qt-plugins"),
    ]));
    let edit = |name: &str| {
        edits
            .iter()
            .find(|(key, _)| *key == name)
            .map(|e| e.1.clone())
    };
    for name in [
        "APPIMAGE",
        "APPDIR",
        "ARGV0",
        "OWD",
        "APPIMAGE_EXTRACT_AND_RUN",
        "OKILUM_APPRUN_PRELOAD",
    ] {
        assert_eq!(edit(name), Some(None), "{name} is removed");
    }
    for name in [
        "LD_LIBRARY_PATH",
        "PYTHONHOME",
        "GIO_EXTRA_MODULES",
        "QT_QPA_PLATFORM_PLUGIN_PATH",
        "GST_PLUGIN_SYSTEM_PATH",
    ] {
        assert_eq!(
            edit(name),
            Some(None),
            "{name}: only bundle entries, removed"
        );
    }
    assert_eq!(
        edit("LD_PRELOAD"),
        Some(Some("/home/me/libmine.so".into())),
        "AppRun's host loader goes; the user's preload stays"
    );
    assert_eq!(
        edit("PATH"),
        Some(Some("/home/me/bin:/usr/bin".into())),
        "user entries kept in order"
    );
    assert_eq!(
        edit("XDG_DATA_DIRS"),
        None,
        "a sibling prefix is not inside APPDIR; unchanged variables are left alone"
    );
    assert_eq!(
        edit("QT_PLUGIN_PATH"),
        None,
        "a user value outside APPDIR stays"
    );
    // A mixed colon/space list: the bundled library goes whichever separator
    // precedes it; AppRun's `:`-prepended loader goes; the user's stays.
    let mixed = appimage_env(env(&[
        ("APPDIR", "/tmp/.mount_OkilumX"),
        ("OKILUM_APPRUN_PRELOAD", "/usr/lib/a.so"),
        (
            "LD_PRELOAD",
            "/usr/lib/a.so:/user/b.so /tmp/.mount_OkilumX/lib/c.so\t/user/d.so",
        ),
    ]));
    assert_eq!(
        mixed
            .iter()
            .find(|(key, _)| *key == "LD_PRELOAD")
            .map(|e| e.1.clone()),
        Some(Some("/user/b.so:/user/d.so".into()))
    );
    // Without AppRun's marker the same host loader is the user's own.
    let unmarked = appimage_env(env(&[
        ("APPDIR", "/tmp/.mount_OkilumX"),
        ("LD_PRELOAD", "/usr/lib/libasound.so.2"),
    ]));
    assert!(!unmarked.iter().any(|(key, _)| *key == "LD_PRELOAD"));
}

/// AppRun names the loader it prepends, which `appimage_env` removes.
#[cfg(unix)]
#[test]
fn apprun_marks_the_preload_it_adds() {
    let apprun = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/appimage/apprun.sh");
    let source = std::fs::read_to_string(apprun).unwrap();
    assert!(source.contains("OKILUM_APPRUN_PRELOAD=$native_alsa"));
    assert!(source.contains("export LD_PRELOAD OKILUM_APPRUN_PRELOAD"));
    assert!(APPIMAGE_ONLY.contains(&APPRUN_PRELOAD));
}
