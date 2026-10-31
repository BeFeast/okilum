//! OS launcher argv. Defaults are data; only an explicit override is shell code.
#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub program: String,
    pub args: Vec<String>,
}
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
pub fn initial(host: Option<&str>, checkout: &str, custom: Option<&str>) -> Result<String, String> {
    if let Some(custom) = custom.filter(|v| !v.trim().is_empty()) {
        if custom.contains(['\0', '\n', '\r']) {
            return Err("Terminal command must be one line".into());
        }
        return Ok(custom.to_owned());
    }
    if let Some(host) = host {
        if host.is_empty()
            || host.starts_with('-')
            || !host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".:-_".contains(c))
        {
            return Err("T3 host is not a valid SSH destination".into());
        }
        Ok(format!("ssh {}", quote(host)))
    } else {
        Ok(format!("cd -- {}", quote(checkout)))
    }
}
pub fn plan(os: &str, command: &str, launcher: &str) -> Plan {
    match os {
        "macos" => Plan { program: "osascript".into(), args: vec!["-e".into(),
            "on run argv\ntell application \"Terminal\"\nactivate\ndo script (item 1 of argv)\nend tell\nend run".into(), "--".into(), command.into()] },
        "windows" => {
            let mut args = if launcher == "wt.exe" { vec!["new-tab".into()] } else { vec![] };
            // PowerShell quoting is deliberately separate from POSIX defaults.
            args.extend(["powershell.exe".into(), "-NoExit".into(), "-Command".into(), command.into()]);
            if launcher == "cmd.exe" { args.splice(..0,["/c".into(),"start".into(),"".into()]); }
            Plan { program: launcher.into(), args }
        }
        _ => {
            let mut args = if launcher == "xdg-terminal-exec" { vec![] } else { vec!["-e".into()] };
            args.extend(["/bin/sh".into(),"-lc".into(),format!("{command}; exec \"${{SHELL:-/bin/sh}}\"")]);
            Plan { program: launcher.into(), args }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_and_overrides_preserve_command_boundaries() {
        assert_eq!(
            initial(Some("maestro"), "unused", None).unwrap(),
            "ssh 'maestro'"
        );
        assert_eq!(
            initial(None, "/tmp/a ' $x", None).unwrap(),
            "cd -- '/tmp/a '\\'' $x'"
        );
        assert!(initial(Some("-oProxyCommand=evil"), "", None).is_err());
        assert_eq!(
            initial(None, "", Some("ssh maestro")).unwrap(),
            "ssh maestro"
        );
        assert!(initial(None, "", Some("a\nb")).is_err());
    }
    #[test]
    fn platform_argv_does_not_interpolate_apple_script_or_split_commands() {
        let command = "ssh host; echo \"literal\"";
        let mac = plan("macos", command, "");
        assert_eq!(mac.args.last().unwrap(), command);
        let linux = plan("linux", command, "xdg-terminal-exec");
        assert_eq!(linux.args[0], "/bin/sh");
        assert!(!linux.args.contains(&"-e".to_owned()));
        assert_eq!(plan("linux", command, "alacritty").args[0], "-e");
        assert_eq!(
            plan("windows", command, "wt.exe").args.last().unwrap(),
            command
        );
    }
}
