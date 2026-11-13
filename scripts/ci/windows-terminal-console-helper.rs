//! Drives the production Windows console spawner of «Open in terminal» (#2837).
//! `console` is the shipped fallback; `redirected` and `bare` reproduce the
//! #2858 head (std handles with a new console; `cmd /c start` from the folder).
#[path = "../../crates/okilum-shell/src/project_terminal/windows_console.rs"]
mod windows_console;

use std::ffi::OsString;
use std::os::windows::process::CommandExt as _;
use std::process::{Command, Stdio};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let mode = args.next().expect("mode").into_string().expect("mode text");
    let encoded = args.next().expect("encoded command");
    let system = windows_console::system_directory().expect("system directory");
    let powershell = system
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    assert!(powershell.is_file(), "{}", powershell.display());
    let words: Vec<OsString> = ["-NoLogo", "-NoExit", "-EncodedCommand"]
        .into_iter()
        .map(OsString::from)
        .chain([encoded])
        .collect();
    let pid = match mode.as_str() {
        "console" => windows_console::spawn(&powershell, &words).expect("console spawn"),
        "redirected" => Command::new(&powershell)
            .args(&words)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0000_0010)
            .spawn()
            .expect("redirected spawn")
            .id(),
        "bare" => Command::new(system.join("cmd.exe"))
            .args(["/d", "/c", "start", "", "powershell.exe"])
            .args(&words)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("cmd spawn")
            .id(),
        other => panic!("unknown mode {other}"),
    };
    println!("PID {pid}");
}
