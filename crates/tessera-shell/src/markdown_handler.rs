//! Per-user Open With registration, owned by the Velopack install lifecycle.
//! Never set .md's default value or modify Explorer UserChoice.
use std::process::Command;
const PROG_ID: &str = "BeFeast.Tessera.Markdown";

fn registry(args: &[&str]) {
    match Command::new("reg.exe").args(args).status() {
        Ok(status) if status.success() => {}
        result => eprintln!("Markdown handler registration: {result:?}"),
    }
}

pub(crate) fn install() {
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let key = format!("HKCU\\Software\\Classes\\{PROG_ID}");
    registry(&[
        "add",
        &key,
        "/ve",
        "/t",
        "REG_SZ",
        "/d",
        "Markdown document",
        "/f",
    ]);
    let command = format!("\"{}\" \"%1\"", executable.display());
    registry(&[
        "add",
        &format!("{key}\\shell\\open\\command"),
        "/ve",
        "/t",
        "REG_SZ",
        "/d",
        &command,
        "/f",
    ]);
    registry(&[
        "add",
        "HKCU\\Software\\Classes\\.md\\OpenWithProgids",
        "/v",
        PROG_ID,
        "/t",
        "REG_SZ",
        "/d",
        "",
        "/f",
    ]);
}

pub(crate) fn uninstall() {
    registry(&[
        "delete",
        "HKCU\\Software\\Classes\\.md\\OpenWithProgids",
        "/v",
        PROG_ID,
        "/f",
    ]);
    registry(&[
        "delete",
        &format!("HKCU\\Software\\Classes\\{PROG_ID}"),
        "/f",
    ]);
}
