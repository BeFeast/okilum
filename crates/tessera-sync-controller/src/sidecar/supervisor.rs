//! Fixed Syncthing launch protocol. Payload/signature and private-directory
//! verification belong to preparation; these primitives are not installer hooks.
use anyhow::{ensure, Result};

pub mod ipc;
#[cfg(unix)]
pub mod process_group;
pub mod runtime;

#[derive(Clone, Debug)]
pub struct Launch {
    pub executable: String,
    pub config: String,
    pub data: String,
}
impl Launch {
    pub fn windows_command_line(&self) -> Result<Vec<u16>> {
        for value in [&self.executable, &self.config, &self.data] {
            super::windows::path(value)?;
        }
        ensure!(
            self.config != self.data,
            "separate config and data directories required"
        );
        let arguments = [
            self.executable.as_str(),
            "serve",
            "--no-browser",
            "--no-restart",
            "--no-upgrade",
            "--config",
            &self.config,
            "--data",
            &self.data,
        ];
        let text = arguments
            .into_iter()
            .map(quote)
            .collect::<Vec<_>>()
            .join(" ");
        let mut wide: Vec<_> = text.encode_utf16().collect();
        ensure!(wide.len() < 32767, "Windows command line too long");
        wide.push(0);
        Ok(wide)
    }
}
// Windows CommandLineToArgvW/CRT backslash-before-quote convention. Use an
// explicit lpApplicationName too; never let Windows guess an executable prefix.
fn quote(value: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for ch in value.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        if ch == '"' {
            out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
        } else {
            out.extend(std::iter::repeat_n('\\', slashes));
        }
        slashes = 0;
        out.push(ch);
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_has_fixed_isolation_flags_and_handles_unicode_spaces_and_trailing_slash() {
        let launch = Launch {
            executable: r"C:\Program Files\Tessera\syncthing.exe".into(),
            config: "C:\\Users\\Олег\\Sync Config\\".into(),
            data: r"C:\Users\Олег\Sync Data".into(),
        };
        let wide = launch.windows_command_line().unwrap();
        assert_eq!(wide.last(), Some(&0));
        let line = String::from_utf16(&wide[..wide.len() - 1]).unwrap();
        assert_eq!(line, "\"C:\\Program Files\\Tessera\\syncthing.exe\" \"serve\" \"--no-browser\" \"--no-restart\" \"--no-upgrade\" \"--config\" \"C:\\Users\\Олег\\Sync Config\\\\\" \"--data\" \"C:\\Users\\Олег\\Sync Data\"");
        assert!(!line.contains("apikey") && !line.contains("password"));
    }
    #[test]
    fn launch_rejects_relative_paths_and_control_characters_before_creation() {
        let mut launch = Launch {
            executable: r"C:\runtime\syncthing.exe".into(),
            config: r"C:\state\config".into(),
            data: r"C:\state\data".into(),
        };
        for bad in [
            "syncthing.exe",
            "C:\\runtime\\a\0.exe",
            "C:\\runtime\\..\\other.exe",
        ] {
            launch.executable = bad.into();
            assert!(launch.windows_command_line().is_err());
        }
    }
}
