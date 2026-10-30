//! Explorer folder verbs owned exclusively by the installed application's hooks.
#![cfg_attr(not(windows), allow(dead_code))]

const KEYS: [&str; 2] = [
    "HKCU\\Software\\Classes\\Directory\\shell\\BeFeast.Okilum.OpenVault",
    "HKCU\\Software\\Classes\\Directory\\Background\\shell\\BeFeast.Okilum.OpenVault",
];

fn install_commands(executable: &std::path::Path) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    for (key, selection) in KEYS.into_iter().zip(["%1", "%V"]) {
        let mut add = |path: String, value: &[&str], data: String| {
            let mut args = vec!["add".into(), path];
            args.extend(value.iter().map(|s| (*s).to_owned()));
            args.extend(["/t".into(), "REG_SZ".into(), "/d".into(), data, "/f".into()]);
            commands.push(args);
        };
        add(key.into(), &["/ve"], "Open in Okilum as Vault".into());
        add(
            key.into(),
            &["/v", "Icon"],
            format!("\"{}\",0", executable.display()),
        );
        add(key.into(), &["/v", "MultiSelectModel"], "Single".into());
        // Append a path component, not a trailing backslash. This also quotes
        // drive/UNC roots correctly under Windows command-line parsing.
        add(
            format!("{key}\\command"),
            &["/ve"],
            format!("\"{}\" --vault \"{selection}\\.\"", executable.display()),
        );
    }
    commands
}

fn uninstall_commands() -> Vec<Vec<String>> {
    KEYS.into_iter()
        .map(|key| vec!["delete".into(), key.into(), "/f".into()])
        .collect()
}

#[cfg(windows)]
fn registry(args: &[String]) {
    match std::process::Command::new("reg.exe").args(args).status() {
        Ok(status) if status.success() => {}
        result => eprintln!("Folder verb registration: {result:?}"),
    }
}

/// Installed builds only: portable startup never calls this function.
#[cfg(windows)]
pub fn install(executable: &std::path::Path) {
    for args in install_commands(executable) {
        registry(&args);
    }
}

/// Preserve all shared Explorer keys and every unrelated verb.
#[cfg(windows)]
pub fn uninstall() {
    for args in uninstall_commands() {
        registry(&args);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_quote_the_executable_and_both_folder_placeholders() {
        let exe = std::path::Path::new("C:\\Users\\QA User\\Okilum\\current\\okilum.exe");
        let commands = install_commands(exe);
        assert_eq!(commands.len(), 8);
        for (group, placeholder) in commands.chunks_exact(4).zip(["%1", "%V"]) {
            assert_eq!(group[0][group[0].len() - 2], "Open in Okilum as Vault");
            assert_eq!(group[2][group[2].len() - 2], "Single");
            assert_eq!(
                group[3][group[3].len() - 2],
                format!("\"{}\" --vault \"{placeholder}\\.\"", exe.display())
            );
        }
    }

    #[test]
    fn uninstall_removes_exactly_owned_subtrees_not_shared_roots() {
        let commands = install_commands(std::path::Path::new("okilum.exe"));
        for (key, removal) in KEYS.into_iter().zip(uninstall_commands()) {
            assert!(commands.iter().any(|command| command[1] == key));
            assert_eq!(removal, ["delete", key, "/f"]);
            assert!(key.ends_with("\\BeFeast.Okilum.OpenVault"));
            assert!(key.starts_with("HKCU\\Software\\Classes\\Directory\\"));
        }
    }

    #[cfg(windows)]
    #[test]
    fn real_registry_round_trip_preserves_neighbor_verbs() {
        let exe = std::path::Path::new("C:\\Okilum QA\\okilum.exe");
        let query = |key: &str| {
            std::process::Command::new("reg.exe")
                .args(["query", key, "/s"])
                .output()
                .unwrap()
        };
        let neighbors: Vec<_> = KEYS
            .iter()
            .map(|key| format!("{}\\QaNeighbor1914", key.rsplit_once('\\').unwrap().0))
            .collect();
        for key in &neighbors {
            assert!(std::process::Command::new("reg.exe")
                .args(["add", key, "/ve", "/d", "QA neighbor", "/f"])
                .status()
                .unwrap()
                .success());
        }
        uninstall();
        for key in KEYS {
            assert!(!query(key).status.success());
        }
        install(exe);
        install(exe);
        for key in KEYS {
            let result = query(key);
            assert!(result.status.success());
            assert!(String::from_utf8_lossy(&result.stdout).contains("Open in Okilum as Vault"));
        }
        uninstall();
        for key in KEYS {
            assert!(!query(key).status.success());
        }
        for key in neighbors {
            assert!(
                query(&key).status.success(),
                "uninstall removed a neighboring verb"
            );
            assert!(std::process::Command::new("reg.exe")
                .args(["delete", &key, "/f"])
                .status()
                .unwrap()
                .success());
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_parser_preserves_roots_spaces_and_unicode() {
        use std::os::windows::ffi::OsStringExt;
        for placeholder in ["%1", "%V"] {
            for folder in [
                r"C:\",
                r"C:\Notes with spaces",
                r"C:\Ноты",
                r"\\server\share\",
            ] {
                let commands =
                    install_commands(std::path::Path::new(r"C:\Program Files\Okilum\okilum.exe"));
                let template = &commands[if placeholder == "%1" { 3 } else { 7 }];
                let text = template[template.len() - 2].replace(placeholder, folder);
                let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
                let mut count = 0;
                unsafe {
                    let args = windows_sys::Win32::UI::Shell::CommandLineToArgvW(
                        wide.as_ptr(),
                        &mut count,
                    );
                    assert!(!args.is_null());
                    assert_eq!(count, 3);
                    let arg = *args.add(2);
                    let mut length = 0;
                    while *arg.add(length) != 0 {
                        length += 1;
                    }
                    let value =
                        std::ffi::OsString::from_wide(std::slice::from_raw_parts(arg, length));
                    windows_sys::Win32::Foundation::LocalFree(args.cast());
                    assert_eq!(value, std::ffi::OsString::from(format!(r"{folder}\.")));
                }
            }
        }
    }
}
