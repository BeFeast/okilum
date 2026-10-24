//! AppImage-only user desktop payload. The shell owns startup/uninstall timing.
//! These explicit-path APIs never register defaults or touch system directories.
use anyhow::{ensure, Context, Result};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const MARKER: &str = "X-Okilum-AppImage=true\n";
const ICON: &[u8] = include_bytes!("../../okilum-shell/assets/brand/app-icon-light.svg");

/// Exact files owned by this helper, kept separate from distribution launchers.
pub fn paths(home: &Path) -> Result<(PathBuf, PathBuf)> {
    ensure!(
        home.is_absolute(),
        "Desktop integration needs an absolute home"
    );
    let share = home.join(".local/share");
    Ok((
        share.join("applications/okilum-appimage.desktop"),
        share.join("icons/hicolor/scalable/apps/okilum-appimage.svg"),
    ))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    let parent = path.parent().context("Missing desktop payload parent")?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

// Exec quoting has two layers: desktop-entry string escaping and Exec quoting.
fn exec_path(image: &Path) -> Result<String> {
    let value = image.to_str().context("AppImage path is not UTF-8")?;
    ensure!(
        !value.chars().any(char::is_control),
        "AppImage path contains controls"
    );
    let mut escaped = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\\\\\"),
            '"' | '`' | '$' => {
                escaped.push_str("\\\\");
                escaped.push(ch);
            }
            '%' => escaped.push_str("%%"),
            _ => escaped.push(ch),
        }
    }
    escaped.push('"');
    Ok(escaped)
}

/// Silent, idempotent refresh on every AppImage launch, including a moved image.
/// Call off the UI thread; report errors without blocking ordinary startup.
/// Passing an image does not grant update authority or change channel selection.
pub fn install(home: &Path, image: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    ensure!(image.is_absolute(), "AppImage path must be absolute");
    let metadata = fs::symlink_metadata(image)?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o111 != 0,
        "AppImage must be an executable regular file owned by you"
    );
    let (desktop, icon) = paths(home)?;
    if desktop.exists() {
        ensure!(
            fs::read_to_string(&desktop)?
                .lines()
                .any(|l| l == MARKER.trim()),
            "Refusing to overwrite an unowned desktop entry"
        );
    }
    if icon.exists() {
        ensure!(
            fs::read(&icon)? == ICON,
            "Refusing to overwrite an unowned desktop icon"
        );
    }
    let entry = format!("[Desktop Entry]\nType=Application\nName=Okilum\nComment=Read and search your Markdown vault\nExec={} %u\nIcon=okilum-appimage\nTerminal=false\nCategories=Office;Utility;\nMimeType=text/markdown;x-scheme-handler/okilum;\nStartupNotify=true\nStartupWMClass=okilum\n{MARKER}", exec_path(image)?);
    // Install the icon first so a published launcher always has its payload.
    atomic_write(&icon, ICON)?;
    atomic_write(&desktop, entry.as_bytes())
}

/// Remove only this helper's payload, even when the original AppImage was moved.
/// The shell calls this after its existing uninstall safety/confirmation checks.
pub fn remove(home: &Path) -> Result<()> {
    let (desktop, icon) = paths(home)?;
    if desktop.exists() {
        ensure!(
            fs::read_to_string(&desktop)?
                .lines()
                .any(|l| l == MARKER.trim()),
            "Refusing to remove an unowned desktop entry"
        );
        fs::remove_file(desktop)?;
    }
    if icon.exists() {
        ensure!(
            fs::read(&icon)? == ICON,
            "Refusing to remove a changed desktop icon"
        );
        fs::remove_file(icon)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn install_move_remove_preserves_other_launchers() {
        let home = tempfile::tempdir().unwrap();
        let first = home.path().join("first $image%.AppImage");
        fs::write(&first, b"original image").unwrap();
        fs::set_permissions(&first, fs::Permissions::from_mode(0o755)).unwrap();
        let (desktop, icon) = paths(home.path()).unwrap();
        fs::create_dir_all(desktop.parent().unwrap()).unwrap();
        let peer = desktop.parent().unwrap().join("okilum.desktop");
        fs::write(&peer, b"distribution launcher").unwrap();
        install(home.path(), &first).unwrap();
        let initial = fs::read_to_string(&desktop).unwrap();
        assert!(initial.contains("\\\\$image%%.AppImage\" %u"));
        assert_eq!(fs::read(&icon).unwrap(), ICON);
        install(home.path(), &first).unwrap();
        assert_eq!(initial, fs::read_to_string(&desktop).unwrap());
        let moved = home.path().join("moved image.AppImage");
        fs::rename(&first, &moved).unwrap();
        install(home.path(), &moved).unwrap();
        let refreshed = fs::read_to_string(&desktop).unwrap();
        assert!(refreshed.contains("moved image.AppImage"));
        assert!(!refreshed.contains("first "));
        remove(home.path()).unwrap();
        remove(home.path()).unwrap();
        assert!(!desktop.exists() && !icon.exists());
        assert_eq!(fs::read(peer).unwrap(), b"distribution launcher");
    }

    // Use the real desktop-entry parser and launcher, not our own encoder as
    // the oracle. This runs only a disposable script in the hosted Linux gate.
    #[test]
    fn gio_launch_preserves_literal_executable_path() {
        let home = tempfile::tempdir().unwrap();
        let image = home.path().join("space \\\"`$HOME; & %.AppImage");
        let receipt = home.path().join("launched");
        fs::write(
            &image,
            b"#!/bin/sh\nprintf '%s\\n' \"$0\" \"$#\" > \"$OKILUM_DESKTOP_PROBE\"\n",
        )
        .unwrap();
        fs::set_permissions(&image, fs::Permissions::from_mode(0o755)).unwrap();
        install(home.path(), &image).unwrap();
        let (desktop, _) = paths(home.path()).unwrap();
        let result = std::process::Command::new("python3")
            .args([
                "-c",
                r#"import ctypes, sys, time
from pathlib import Path
gio = ctypes.CDLL('libgio-2.0.so.0')
gio.g_desktop_app_info_new_from_filename.argtypes = [ctypes.c_char_p]
gio.g_desktop_app_info_new_from_filename.restype = ctypes.c_void_p
gio.g_app_info_launch.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p)]
gio.g_app_info_launch.restype = ctypes.c_int
app = gio.g_desktop_app_info_new_from_filename(sys.argv[1].encode())
assert app, 'GLib rejected the generated desktop entry'
error = ctypes.c_void_p()
assert gio.g_app_info_launch(app, None, None, ctypes.byref(error)), 'GIO launch failed'
receipt = Path(sys.argv[2])
deadline = time.monotonic() + 5
while not receipt.exists() and time.monotonic() < deadline:
    time.sleep(0.02)
assert receipt.read_text().splitlines() == [sys.argv[3], '0'], 'Executable path or argument count changed'
"#,
            ])
            .arg(desktop)
            .arg(&receipt)
            .arg(&image)
            .env("OKILUM_DESKTOP_PROBE", &receipt)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "GIO desktop launch: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[test]
    fn refuses_unowned_entry_and_invalid_exec() {
        let home = tempfile::tempdir().unwrap();
        let (desktop, _) = paths(home.path()).unwrap();
        fs::create_dir_all(desktop.parent().unwrap()).unwrap();
        fs::write(&desktop, b"custom launcher").unwrap();
        assert!(remove(home.path()).is_err());
        assert_eq!(fs::read(&desktop).unwrap(), b"custom launcher");
        assert!(exec_path(Path::new("/tmp/bad\nimage")).is_err());
        assert!(paths(Path::new("relative")).is_err());
        assert_eq!(
            exec_path(Path::new("/tmp/a\\b\"`$%.AppImage")).unwrap(),
            "\"/tmp/a\\\\\\\\b\\\\\"\\\\`\\\\$%%.AppImage\""
        );
    }
}
