use super::*;

#[test]
fn terminal_selection_all_platforms_uses_only_fake_lookup() {
    let choose =
        |os, terminal, installed: &[&str]| launcher(os, terminal, |p| installed.contains(&p));
    assert_eq!(
        choose(
            "linux",
            Some("kitty"),
            &["xdg-terminal-exec", "kitty", "x-terminal-emulator"]
        )
        .unwrap(),
        "xdg-terminal-exec"
    );
    assert_eq!(
        choose("linux", Some("kitty"), &["kitty", "x-terminal-emulator"]).unwrap(),
        "kitty"
    );
    assert_eq!(
        choose("linux", Some("missing"), &["x-terminal-emulator"]).unwrap(),
        "x-terminal-emulator"
    );
    assert_eq!(
        choose(
            "linux",
            Some("/Applications/My Terminal"),
            &["/Applications/My Terminal"]
        )
        .unwrap(),
        "/Applications/My Terminal"
    );
    assert_eq!(choose("macos", None, &["osascript"]).unwrap(), "osascript");
    assert_eq!(
        choose("windows", None, &["wt.exe", "cmd.exe"]).unwrap(),
        "wt.exe"
    );
    assert_eq!(choose("windows", None, &["cmd.exe"]).unwrap(), "cmd.exe");
    for os in ["linux", "macos", "windows"] {
        assert!(
            choose(os, None, &[]).is_err(),
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
    let encoded = plan.args.last().unwrap();
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
        for terminal in ["xdg-terminal-exec", "kitty"] {
            let p = plan("linux", &initial, terminal);
            let offset = usize::from(terminal != "xdg-terminal-exec");
            assert_eq!(p.args[offset..], posix_args(&initial));
            assert!(
                !p.args[offset + 2].contains(hostile),
                "fixed shell wrapper contains no data"
            );
            assert_eq!(p.args.last().unwrap(), hostile);
        }
        let mac = plan("macos", &initial, "osascript");
        assert!(
            !mac.args[1].contains(hostile),
            "fixed AppleScript contains no data"
        );
        assert_eq!(
            shell_words::split(mac.args.last().unwrap()).unwrap(),
            posix_args(&initial)
        );
        for terminal in ["wt.exe", "cmd.exe"] {
            let win = plan("windows", &initial, terminal);
            let data = decode_windows(&win);
            assert_eq!(data["argv"], serde_json::json!(initial.argv));
            assert_eq!(data["directory"], serde_json::json!(initial.directory));
            assert!(win.args.contains(&"-NoExit".into()));
            assert!(!win.args.iter().any(|v| v.contains(hostile)));
        }
    }
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
    let p = plan("linux", &run, "xdg-terminal-exec");
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
    std::fs::write(&shell, "#!/bin/sh\npwd -P\n").unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o700)).unwrap();
    let initial = initial(None, false, None, directory.to_str().unwrap(), None).unwrap();
    let p = plan("linux", &initial, "xdg-terminal-exec");
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
    let data = decode_windows(&plan("windows", &initial, "wt.exe"));
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
    let choose = |os, terminal, desktop, installed: &[&str]| {
        folder_launcher(os, terminal, desktop, |p| installed.contains(&p))
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
        "xdg-terminal-exec"
    );
    assert_eq!(
        choose("linux", Some("kitty"), None, &["kitty", "ptyxis"]).unwrap(),
        "kitty"
    );
    assert_eq!(
        choose("linux", None, None, &["x-terminal-emulator", "ptyxis"]).unwrap(),
        "x-terminal-emulator"
    );
    // Without one, the desktop's native terminal before other known ones.
    let installed = ["xterm", "konsole", "gnome-terminal", "ptyxis"];
    assert_eq!(
        choose("linux", None, Some("ubuntu:GNOME"), &installed).unwrap(),
        "ptyxis"
    );
    assert_eq!(
        choose("linux", Some("missing"), Some("KDE"), &installed).unwrap(),
        "konsole"
    );
    assert_eq!(
        choose("linux", None, Some("Hyprland"), &["xterm", "foot"]).unwrap(),
        "foot"
    );
    assert_eq!(
        choose("macos", None, None, &["/usr/bin/open"]).unwrap(),
        "/usr/bin/open"
    );
    assert_eq!(
        choose("windows", None, None, &["wt.exe", "cmd.exe"]).unwrap(),
        "wt.exe"
    );
    assert_eq!(
        choose("windows", None, None, &["cmd.exe"]).unwrap(),
        "cmd.exe"
    );
    for os in ["linux", "macos", "windows"] {
        assert!(
            choose(os, None, Some("GNOME"), &[]).is_err(),
            "{os} reports a missing terminal"
        );
    }
}

#[test]
fn folder_plans_pass_hostile_folders_as_one_argument_on_every_platform() {
    for folder in HOSTILE_FOLDERS {
        let folder = *folder;
        // macOS: Terminal.app (or a configured app) opens the folder; no shell text.
        let mac = folder_plan("macos", folder, "/usr/bin/open", "Terminal");
        assert_eq!(mac.program, "/usr/bin/open");
        assert_eq!(mac.args, ["-a", "Terminal", folder]);
        assert_eq!(
            folder_plan("macos", folder, "/usr/bin/open", "iTerm").args[1],
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
            let plan = folder_plan("linux", folder, terminal, "Terminal");
            assert_eq!(plan.program, terminal);
            assert_eq!(plan.args, expected, "{terminal}");
        }
        // The desktop default, TERMINAL and xterm: the fixed wrapper, folder as data.
        let directory = Initial {
            argv: vec![],
            directory: Some(folder.into()),
        };
        for terminal in [
            "xdg-terminal-exec",
            "x-terminal-emulator",
            "xterm",
            "/opt/My Term",
        ] {
            let plan = folder_plan("linux", folder, terminal, "Terminal");
            assert_eq!(plan.args.last().unwrap(), folder);
            assert_eq!(plan, super::plan("linux", &directory, terminal));
            assert!(
                !plan.args[..plan.args.len() - 1]
                    .iter()
                    .any(|a| a.contains(folder)),
                "the wrapper itself carries no data"
            );
        }
        // Windows Terminal takes the folder after -d as one CRT argument.
        let wt = folder_plan("windows", folder, "wt.exe", "Terminal");
        assert_eq!(wt.args, ["-d", folder]);
        // cmd.exe never sees the folder: it is base64 data for PowerShell.
        let cmd = folder_plan("windows", folder, "cmd.exe", "Terminal");
        assert_eq!(cmd.program, "cmd.exe");
        assert!(!cmd.args.iter().any(|a| a.contains(folder)));
        assert_eq!(decode_windows(&cmd)["directory"], folder);
        assert_eq!(decode_windows(&cmd)["argv"], serde_json::json!([]));
    }
    // wt splits its command line at `;`, so such a folder is encoded data too.
    let folder = r"C:\Users\me\a;b @x";
    let wt = folder_plan("windows", folder, "wt.exe", "Terminal");
    assert_eq!(wt.args[0], "new-tab");
    assert!(!wt.args.iter().any(|a| a.contains(folder)));
    assert_eq!(decode_windows(&wt)["directory"], folder);
    // Positive control: an ordinary Windows folder takes the plain route.
    assert_eq!(
        folder_plan("windows", r"C:\Users\me\Notes", "wt.exe", "Terminal").args,
        ["-d", r"C:\Users\me\Notes"]
    );
}

#[cfg(unix)]
#[test]
fn folder_wrapper_enters_a_hostile_folder_literally() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().unwrap();
    let shell = root.path().join("fake-shell");
    std::fs::write(&shell, "#!/bin/sh\npwd -P\n").unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o700)).unwrap();
    for name in [
        "-rf folder",
        "@Assets",
        "space ' \" $(exit 99) & ;",
        "Папка ✓",
    ] {
        let folder = root.path().join(name);
        std::fs::create_dir(&folder).unwrap();
        let plan = folder_plan("linux", folder.to_str().unwrap(), "xdg-terminal-exec", "");
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
        ("LD_LIBRARY_PATH", "/tmp/.mount_OkilumX/usr/lib"),
        ("PATH", "/tmp/.mount_OkilumX/usr/bin:/home/me/bin:/usr/bin"),
        ("XDG_DATA_DIRS", "/tmp/.mount_OkilumXother/share:/usr/share"),
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
    ] {
        assert_eq!(edit(name), Some(None), "{name} is removed");
    }
    assert_eq!(
        edit("LD_LIBRARY_PATH"),
        Some(None),
        "only bundle entries: removed"
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
}
