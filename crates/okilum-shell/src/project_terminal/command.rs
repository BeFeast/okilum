//! Pure command derivation and launcher selection. No process is started here.
use base64::{engine::general_purpose::STANDARD, Engine as _};

#[derive(Debug, PartialEq, Eq)]
pub struct Initial {
    pub argv: Vec<String>,
    pub directory: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub program: String,
    pub args: Vec<String>,
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

/// Executable lookup is injected, including on non-native platforms in tests.
/// TERMINAL names one executable, so a path containing spaces stays one word.
pub fn launcher(
    os: &str,
    terminal: Option<&str>,
    available: impl Fn(&str) -> bool,
) -> Result<String, String> {
    let candidates: Vec<&str> = match os {
        "macos" => vec!["osascript"],
        "windows" => vec!["wt.exe", "cmd.exe"],
        _ => vec![
            "xdg-terminal-exec",
            terminal.unwrap_or(""),
            "x-terminal-emulator",
        ],
    };
    for program in candidates.into_iter().filter(|v| !v.is_empty()) {
        if available(program) {
            return Ok(program.into());
        }
    }
    Err(match os {
        "macos" => "Terminal could not open: osascript is unavailable",
        "windows" => "No system terminal found: install Windows Terminal or restore cmd.exe",
        _ => "No system terminal found: install xdg-terminal-exec or x-terminal-emulator, or set TERMINAL to an executable",
    }.into())
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

// The POSIX shell receives data exclusively as positional arguments. It stays
// interactive after SSH/custom command exit; failed cd never runs in another cwd.
const POSIX: &str = r#"if [ "$1" = cd ]; then
    cd -- "$2" || exit
else
    shift
    "$@"
fi
exec "${SHELL:-/bin/sh}""#;

fn posix_args(initial: &Initial) -> Vec<String> {
    let mut args = vec![
        "/bin/sh".into(),
        "-c".into(),
        POSIX.into(),
        "okilum-terminal".into(),
    ];
    if let Some(directory) = &initial.directory {
        args.extend(["cd".into(), directory.clone()]);
    } else {
        args.push("run".into());
        args.extend(initial.argv.clone());
    }
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

pub fn plan(os: &str, initial: &Initial, launcher: &str) -> Plan {
    match os {
        "macos" => {
            // Terminal.app accepts shell text only. Serialize each argv word,
            // including the fixed wrapper, then pass it as AppleScript argv.
            let command = posix_args(initial)
                .iter()
                .map(|v| quote(v))
                .collect::<Vec<_>>()
                .join(" ");
            Plan { program: launcher.into(), args: vec!["-e".into(),
                "on run argv\ntell application \"Terminal\"\nactivate\ndo script (item 1 of argv)\nend tell\nend run".into(), "--".into(), command] }
        }
        "windows" => {
            // wt parses semicolons itself and cmd reparses /c arguments. Only
            // fixed flags and base64 cross those parsers. PowerShell decodes a
            // JSON data object and creates a process directly, never Invoke-Expression.
            let arguments = initial
                .argv
                .iter()
                .skip(1)
                .map(|v| windows_quote(v))
                .collect::<Vec<_>>()
                .join(" ");
            let data = serde_json::json!({"argv": initial.argv, "directory": initial.directory, "arguments": arguments});
            let data =
                STANDARD.encode(serde_json::to_vec(&data).expect("serializable terminal data"));
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
            let mut args = if launcher == "wt.exe" {
                vec!["new-tab".into()]
            } else {
                vec!["/d".into(), "/c".into(), "start".into(), "".into()]
            };
            args.extend([
                "powershell.exe".into(),
                "-NoLogo".into(),
                "-NoExit".into(),
                "-EncodedCommand".into(),
                encoded,
            ]);
            Plan {
                program: launcher.into(),
                args,
            }
        }
        _ => {
            let mut args = if launcher == "xdg-terminal-exec" {
                vec![]
            } else {
                vec!["-e".into()]
            };
            args.extend(posix_args(initial));
            Plan {
                program: launcher.into(),
                args,
            }
        }
    }
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
        let launch = plan("windows", &initial, "wt.exe");
        assert!(!launch.args.iter().any(|arg| arg.starts_with('@')));
        assert_eq!(windows_quote("@AgentContract.md"), "\"@AgentContract.md\"");
        let script = String::from_utf16(
            &STANDARD
                .decode(launch.args.last().unwrap())
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
