//! Per-user `okilum:` link registration on Windows (#1049, docs/deep-links.md),
//! owned by the Velopack install lifecycle like the Markdown handler. The
//! portable ZIP never runs these hooks, so it registers nothing (#1037).
#![cfg_attr(not(windows), allow(dead_code))]

const KEY: &str = "HKCU\\Software\\Classes\\okilum";

/// `reg.exe` arguments that register the scheme for `executable`. Pure, so
/// the exact keys are tested on every platform.
fn install_commands(executable: &std::path::Path) -> Vec<Vec<String>> {
    let command = format!("\"{}\" \"%1\"", executable.display());
    let add = |key: String, value: &[&str], data: &str| {
        let mut args = vec!["add".to_owned(), key];
        args.extend(value.iter().map(|s| (*s).to_owned()));
        args.extend(["/t", "REG_SZ", "/d", data, "/f"].map(str::to_owned));
        args
    };
    vec![
        add(KEY.to_owned(), &["/ve"], "URL:Okilum link"),
        add(KEY.to_owned(), &["/v", "URL Protocol"], ""),
        add(format!("{KEY}\\shell\\open\\command"), &["/ve"], &command),
    ]
}

fn uninstall_commands() -> Vec<Vec<String>> {
    vec![vec!["delete".into(), KEY.into(), "/f".into()]]
}

#[cfg(windows)]
fn registry(args: &[String]) {
    match std::process::Command::new("reg.exe").args(args).status() {
        Ok(status) if status.success() => {}
        result => eprintln!("okilum: link registration: {result:?}"),
    }
}

#[cfg(windows)]
pub(crate) fn install() {
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    for args in install_commands(&executable) {
        registry(&args);
    }
}

#[cfg(windows)]
pub(crate) fn uninstall() {
    for args in uninstall_commands() {
        registry(&args);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_and_removes_only_the_okilum_scheme() {
        let exe =
            std::path::Path::new("C:/Users/Dana/AppData/Local/BeFeast.Okilum/current/okilum.exe");
        let commands = install_commands(exe);
        let keys: Vec<&str> = commands.iter().map(|c| c[1].as_str()).collect();
        assert!(keys
            .iter()
            .all(|k| k.starts_with("HKCU\\Software\\Classes\\okilum")));
        assert!(commands
            .iter()
            .any(|c| c.contains(&"URL Protocol".to_owned())));
        let open = commands.last().unwrap();
        assert_eq!(
            open[1],
            "HKCU\\Software\\Classes\\okilum\\shell\\open\\command"
        );
        assert_eq!(
            open[open.len() - 2],
            "\"C:/Users/Dana/AppData/Local/BeFeast.Okilum/current/okilum.exe\" \"%1\"",
            "the link is one quoted argument"
        );
        // Positive control: uninstall removes exactly the key install created.
        assert_eq!(uninstall_commands(), [vec!["delete", KEY, "/f"]]);
    }

    /// Real HKCU on a disposable Windows runner: `windows-native.yml` with
    /// package `okilum-shell` and filter `url_protocol`.
    #[cfg(windows)]
    #[test]
    fn url_protocol_round_trip_in_the_registry() {
        let query = |key: &str| {
            std::process::Command::new("reg.exe")
                .args(["query", key, "/s"])
                .output()
                .unwrap()
        };
        uninstall();
        assert!(
            !query(KEY).status.success(),
            "positive control: no key before install"
        );
        install();
        let listed = String::from_utf8_lossy(&query(KEY).stdout).into_owned();
        let exe = std::env::current_exe().unwrap();
        assert!(listed.contains("URL Protocol"), "{listed}");
        assert!(
            listed.contains(&format!("\"{}\" \"%1\"", exe.display())),
            "{listed}"
        );
        uninstall();
        assert!(!query(KEY).status.success(), "uninstall leaves no key");
    }
}
