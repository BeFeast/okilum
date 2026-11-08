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

// Desktop-entry string escaping is applied before the Exec argument grammar.
// Do not execute a desktop file to discover its target.
fn entry_string(value: &str) -> Option<String> {
    let mut result = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            result.push(ch);
            continue;
        }
        result.push(match chars.next()? {
            's' => ' ',
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            '\\' => '\\',
            _ => return None,
        });
    }
    Some(result)
}

fn executable(entry: &str) -> Option<PathBuf> {
    let line = entry_string(entry)?;
    let words = shell_words::split(&line).ok()?;
    let mut arguments = words.iter();
    let mut program = arguments.next()?;
    if program == "env" || program == "/usr/bin/env" {
        program = loop {
            let arg = arguments.next()?;
            if arg == "--" || (!arg.starts_with('-') && arg.contains('=')) {
                continue;
            }
            break arg;
        };
    }
    // Only %% is a literal percent. Field codes are not executable path bytes.
    let mut path = String::new();
    let mut chars = program.chars();
    while let Some(ch) = chars.next() {
        if ch == '%' && chars.next()? != '%' {
            return None;
        }
        path.push(ch);
    }
    let path = PathBuf::from(path);
    path.is_absolute().then_some(path)
}

/// An existing visible launcher for these exact bytes owns desktop integration.
/// Gear Lever and AppImageLauncher choose their own names and directories; a
/// folder name or an environment flag alone never establishes that ownership.
pub fn external_launcher(home: &Path, image: &Path) -> Result<Option<PathBuf>> {
    use std::os::unix::fs::MetadataExt;
    let (desktop, _) = paths(home)?;
    let directory = desktop.parent().context("Missing applications directory")?;
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let path = entry?.path();
        if path.extension() != Some(std::ffi::OsStr::new("desktop")) {
            continue;
        }
        // Another installer may atomically replace/remove an unrelated entry
        // while we scan. That race must not prevent our own first launch.
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.len() > 1_048_576
        {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let mut main = false;
        let mut fields = std::collections::BTreeMap::new();
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                main = line == "[Desktop Entry]";
            } else if main && !line.starts_with('#') {
                if let Some((key, value)) = line.split_once('=') {
                    fields.insert(key.trim(), value.trim());
                }
            }
        }
        if fields.get("Type") != Some(&"Application")
            || fields.get("Hidden") == Some(&"true")
            || fields.get("NoDisplay") == Some(&"true")
            || fields.get("X-Okilum-AppImage") == Some(&"true")
        {
            continue;
        }
        if fields
            .get("Exec")
            .and_then(|value| executable(value))
            .as_deref()
            == Some(image)
        {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// Evidence that the exact legacy image belongs to our previous installation.
/// A distribution or user-authored launcher never authorizes deleting its target.
pub(crate) fn points_to(home: &Path, image: &Path) -> Result<bool> {
    let (desktop, _) = paths(home)?;
    let Ok(entry) = fs::read_to_string(desktop) else {
        return Ok(false);
    };
    let exec = format!("Exec=/usr/bin/env -- {} %u", exec_path(image)?);
    Ok(entry.lines().any(|line| line == MARKER.trim()) && entry.lines().any(|line| line == exec))
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
    if external_launcher(home, image)?.is_some() {
        // Retire our previous duplicate only. Never rewrite or remove the
        // manager's launcher, icon, executable or update configuration.
        let (desktop, _) = paths(home)?;
        if fs::read_to_string(desktop)
            .is_ok_and(|text| text.lines().any(|line| line == MARKER.trim()))
        {
            remove(home)?;
        }
        return Ok(());
    }
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
    // GNOME caches theme roots, not every nested apps directory. Installing a
    // new SVG beneath an existing hicolor tree can leave a valid old cache
    // without this icon. A file icon is immediately usable even before the
    // desktop's theme cache refreshes (or without gtk-update-icon-cache).
    let icon_value = icon.to_str().context("Desktop icon path is not UTF-8")?;
    ensure!(
        !icon_value.chars().any(char::is_control),
        "Desktop icon path contains controls"
    );
    let icon_value = icon_value.replace('\\', "\\\\");
    // GLib validates argv[0] before expanding desktop field codes. A literal
    // percent in the image path is encoded as %% and fails that early lookup.
    // env is the fixed executable; the image stays one quoted argument and is
    // exec'd directly after field expansion, without a shell or PATH lookup.
    let entry = format!("[Desktop Entry]\nType=Application\nName=Okilum\nComment=Read and search your Markdown vault\nExec=/usr/bin/env -- {} %u\nIcon={icon_value}\nTerminal=false\nCategories=Office;Utility;\nMimeType=text/markdown;x-scheme-handler/okilum;inode/directory;\nActions=OpenVault;\nStartupNotify=true\nStartupWMClass=okilum\n{MARKER}\n[Desktop Action OpenVault]\nName=Open in Okilum as Vault\nExec=/usr/bin/env -- {} --vault %f\n", exec_path(image)?, exec_path(image)?);
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
    fn gio_icon_is_the_installed_file_without_theme_lookup() {
        let sandbox = tempfile::tempdir().unwrap();
        let home = sandbox.path().join("home space $percent% back\\slash");
        fs::create_dir(&home).unwrap();
        let image = home.join("Okilum.AppImage");
        fs::write(&image, b"not executed").unwrap();
        fs::set_permissions(&image, fs::Permissions::from_mode(0o755)).unwrap();
        install(&home, &image).unwrap();
        let (desktop, icon) = paths(&home).unwrap();
        let result = std::process::Command::new("python3")
            .args(["-c", r#"import ctypes, sys
gio = ctypes.CDLL('libgio-2.0.so.0')
obj = ctypes.CDLL('libgobject-2.0.so.0')
gio.g_desktop_app_info_new_from_filename.argtypes = [ctypes.c_char_p]
gio.g_desktop_app_info_new_from_filename.restype = ctypes.c_void_p
gio.g_app_info_get_icon.argtypes = [ctypes.c_void_p]
gio.g_app_info_get_icon.restype = ctypes.c_void_p
gio.g_file_icon_get_type.restype = ctypes.c_size_t
obj.g_type_check_instance_is_a.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
obj.g_type_check_instance_is_a.restype = ctypes.c_int
gio.g_file_icon_get_file.argtypes = [ctypes.c_void_p]
gio.g_file_icon_get_file.restype = ctypes.c_void_p
gio.g_file_get_path.argtypes = [ctypes.c_void_p]
gio.g_file_get_path.restype = ctypes.c_char_p
app = gio.g_desktop_app_info_new_from_filename(sys.argv[1].encode())
assert app, 'GLib rejected the desktop entry'
icon = gio.g_app_info_get_icon(app)
assert obj.g_type_check_instance_is_a(icon, gio.g_file_icon_get_type()), 'Icon still needs theme-cache lookup'
assert gio.g_file_get_path(gio.g_file_icon_get_file(icon)) == sys.argv[2].encode(), 'Icon path changed'
"#])
            .arg(desktop)
            .arg(icon)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[test]
    fn manager_detection_requires_the_visible_main_entry_and_exact_executable() {
        let home = tempfile::tempdir().unwrap();
        let image = home.path().join("Programs/Okilum % image.AppImage");
        fs::create_dir_all(image.parent().unwrap()).unwrap();
        fs::write(&image, b"image").unwrap();
        fs::set_permissions(&image, fs::Permissions::from_mode(0o755)).unwrap();
        let (desktop, _) = paths(home.path()).unwrap();
        fs::create_dir_all(desktop.parent().unwrap()).unwrap();
        let manager = desktop.with_file_name("gearlever_okilum.desktop");
        let command = format!(
            "/usr/bin/env DESKTOPINTEGRATION=1 -- {} %u",
            exec_path(&image).unwrap()
        );
        for extra in [
            "",
            "Hidden=true\n",
            "NoDisplay=true\n",
            "X-Okilum-AppImage=true\n",
        ] {
            fs::write(&manager, format!("[Desktop Entry]\nType=Application\nName=Okilum\nExec={command}\n{extra}[Desktop Action Other]\nExec=/unrelated\n")).unwrap();
            assert_eq!(
                external_launcher(home.path(), &image).unwrap().is_some(),
                extra.is_empty()
            );
        }
        for entry in [
            "[Desktop Entry]\nType=Application\nName=Okilum\nExec=/unrelated %u\n".to_string(),
            format!("[Desktop Entry]\nType=Application\nName=Okilum\nTryExec={}\nExec=/unrelated\n", image.display()),
            format!("[Desktop Entry]\nType=Application\nName=Other\nExec=/unrelated\n[Desktop Action Okilum]\nExec={command}\n"),
        ] {
            fs::write(&manager, entry).unwrap();
            assert!(external_launcher(home.path(), &image).unwrap().is_none());
        }
        // A pathname convention does not establish manager ownership.
        install(home.path(), &image).unwrap();
        assert!(desktop.exists());
    }

    #[test]
    fn adopting_external_integration_retires_only_our_old_duplicate() {
        let home = tempfile::tempdir().unwrap();
        let image = home.path().join("Okilum.AppImage");
        fs::write(&image, b"image").unwrap();
        fs::set_permissions(&image, fs::Permissions::from_mode(0o755)).unwrap();
        install(home.path(), &image).unwrap();
        let (desktop, icon) = paths(home.path()).unwrap();
        let manager = desktop.with_file_name("gearlever_okilum.desktop");
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName=Okilum\nExec={} %u\nIcon=/manager/icon.svg\n",
            exec_path(&image).unwrap()
        );
        fs::write(&manager, &entry).unwrap();
        install(home.path(), &image).unwrap();
        assert!(!desktop.exists() && !icon.exists());
        assert_eq!(fs::read_to_string(manager).unwrap(), entry);
        assert_eq!(fs::read(image).unwrap(), b"image");
    }

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

    /// GLib parses the shipped action and expands its selected-file argument.
    /// The disposable executable records argv, so shell injection/path splitting
    /// cannot masquerade as a successful open.
    #[test]
    fn gio_folder_open_and_action_preserve_selected_path() {
        let home = tempfile::tempdir().unwrap();
        let image = home.path().join("Okilum $ percent%.AppImage");
        let folder = home.path().join("Vault space Заметки % #");
        fs::create_dir(&folder).unwrap();
        fs::write(
            &image,
            b"#!/bin/sh\nprintf '%s\\n' \"$#\" \"$@\" > \"$OKILUM_DESKTOP_PROBE\"\n",
        )
        .unwrap();
        fs::set_permissions(&image, fs::Permissions::from_mode(0o755)).unwrap();
        install(home.path(), &image).unwrap();
        let (desktop, icon) = paths(home.path()).unwrap();
        let receipt = home.path().join("launched");
        let result = std::process::Command::new("python3")
            .args(["-c", r#"import configparser, ctypes, sys, time
from pathlib import Path
from urllib.parse import unquote, urlparse
entry, receipt, folder = map(Path, sys.argv[1:])
gio = ctypes.CDLL('libgio-2.0.so.0')
glib = ctypes.CDLL('libglib-2.0.so.0')
gio.g_desktop_app_info_new_from_filename.argtypes = [ctypes.c_char_p]
gio.g_desktop_app_info_new_from_filename.restype = ctypes.c_void_p
gio.g_file_new_for_path.argtypes = [ctypes.c_char_p]
gio.g_file_new_for_path.restype = ctypes.c_void_p
glib.g_list_append.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
glib.g_list_append.restype = ctypes.c_void_p
gio.g_app_info_launch.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p)]
gio.g_app_info_launch.restype = ctypes.c_int
config = configparser.ConfigParser(interpolation=None)
config.optionxform = str
config.read(entry)
assert 'inode/directory;' in config['Desktop Entry']['MimeType']
assert config['Desktop Entry']['Actions'] == 'OpenVault;'
assert config['Desktop Action OpenVault']['Name'] == 'Open in Okilum as Vault'
selected = glib.g_list_append(None, gio.g_file_new_for_path(str(folder).encode()))
for action in (False, True):
    launcher = entry
    if action:
        config['Desktop Entry']['Exec'] = config['Desktop Action OpenVault']['Exec']
        launcher = entry.with_name('action-test.desktop')
        with launcher.open('w') as stream: config.write(stream, space_around_delimiters=False)
    app = gio.g_desktop_app_info_new_from_filename(str(launcher).encode())
    assert app, 'GLib rejected desktop entry'
    error = ctypes.c_void_p()
    assert gio.g_app_info_launch(app, selected, None, ctypes.byref(error)), 'GLib launch failed'
    deadline = time.monotonic() + 5
    while (not receipt.exists() or len(receipt.read_text().splitlines()) < (3 if action else 2)) and time.monotonic() < deadline:
        time.sleep(.02)
    args = receipt.read_text().splitlines()
    if action:
        assert args == ['2', '--vault', str(folder)], args
    else:
        assert args[0] == '1', args
        path = unquote(urlparse(args[1]).path) if args[1].startswith('file:') else args[1]
        assert path == str(folder), args
    receipt.unlink()
"#])
            .arg(&desktop).arg(&receipt).arg(&folder)
            .env("OKILUM_DESKTOP_PROBE", &receipt).output().unwrap();
        assert!(
            result.status.success(),
            "GIO folder/action: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        remove(home.path()).unwrap();
        assert!(!desktop.exists() && !icon.exists());
        assert!(folder.is_dir());
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
