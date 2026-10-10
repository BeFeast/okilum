//! Everything Okilum creates on this machine outside the user's vaults, and its
//! removal on uninstall (#974). The inventory is docs/uninstall.md; the source
//! gate test below fails when code computes a new base directory that the
//! inventory and `roots()` do not know about.
// Windows runs the purge from the Velopack hook, Linux from
// `okilum --uninstall-data`, macOS from Settings → About → Uninstall Okilum….
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Prefix of the per-run full-text search sessions in the system temp dir.
pub(crate) const SEARCH_SESSION_PREFIX: &str = "okilum-search-session-";
/// Unsaved drafts are exported here before the app's state is removed.
const DRAFT_EXPORT_FOLDER: &str = "Okilum unsaved drafts";

/// Every directory the app owns, from the same functions that decide where it
/// writes. A root that holds a recorded vault is never removed.
pub(crate) fn roots() -> Vec<PathBuf> {
    let mut roots = BTreeSet::new();
    if let Ok(state) = crate::reader_history::state_directory() {
        roots.insert(state);
    }
    if let Some(config) = crate::reader_layout::config_base() {
        roots.insert(config.join("okilum"));
    }
    if let Ok(cache) = crate::reader_open::cache_base() {
        roots.insert(cache.join("okilum"));
    }
    // Brain profiles and outboxes always use XDG config, also on macOS.
    #[cfg(unix)]
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".config"));
        roots.insert(config.join("okilum"));
    }
    roots.into_iter().filter(|p| p.is_absolute()).collect()
}

/// What an uninstall removed, kept or could not remove; printed for support.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Report {
    pub removed: Vec<PathBuf>,
    pub exported_drafts: Vec<PathBuf>,
    pub kept_holding_vault: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
}

/// Vault roots recorded in the state dir. Windows stores verbatim prefixes.
fn recorded_vaults(state: &Path) -> Vec<PathBuf> {
    let mut vaults = Vec::new();
    let mut add = |value: &serde_json::Value| {
        if let Some(path) = value.as_str() {
            let plain = path
                .strip_prefix(r"\\?\UNC\")
                .map(|rest| format!(r"\\{rest}"))
                .unwrap_or_else(|| path.strip_prefix(r"\\?\").unwrap_or(path).to_owned());
            vaults.push(PathBuf::from(plain));
        }
    };
    if let Some(session) = read_json(&state.join("update-session.json")) {
        for key in ["recent_roots", "quick_roots"] {
            for root in session[key].as_array().into_iter().flatten() {
                add(root);
            }
        }
        for root in session["last_documents"].as_object().into_iter().flatten() {
            add(&serde_json::Value::String(root.0.clone()));
        }
    }
    if let Some(ui) = read_json(&state.join("reader-ui.json")) {
        for key in ["vaults", "vault_colors"] {
            for root in ui[key].as_object().into_iter().flatten() {
                let root = root.0.strip_prefix("reader:").unwrap_or(root.0);
                add(&serde_json::Value::String(root.to_owned()));
            }
        }
    }
    vaults
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Export drafts whose text differs from the file on disk (or whose file is
/// gone) so removing the state never loses unsaved work.
fn export_unsaved_drafts(state: &Path, export: &Path, report: &mut Report) {
    let Ok(entries) = std::fs::read_dir(state.join("editor-drafts")) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Some(draft) = read_json(&path) else {
            continue;
        };
        let (Some(note), Some(text)) = (draft["path"].as_str(), draft["text"].as_str()) else {
            continue;
        };
        if std::fs::read(note).is_ok_and(|disk| disk == text.as_bytes()) {
            continue;
        }
        let name = Path::new(note)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "draft.md".into());
        let target = unique(export, &name);
        let written = std::fs::create_dir_all(export).and_then(|()| std::fs::write(&target, text));
        match written {
            Ok(()) => report.exported_drafts.push(target),
            Err(error) => report.failed.push((path, format!("export draft: {error}"))),
        }
    }
}

fn unique(folder: &Path, name: &str) -> PathBuf {
    let candidate = folder.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, extension) = name.rsplit_once('.').unwrap_or((name, "md"));
    (2..)
        .map(|n| folder.join(format!("{stem} ({n}).{extension}")))
        .find(|path| !path.exists())
        .expect("unbounded")
}

/// Remove `roots` and stale search sessions in `temp`, keeping vaults.
pub(crate) fn purge(
    roots: &[PathBuf],
    state: Option<&Path>,
    temp: &Path,
    documents: &Path,
) -> Report {
    let mut report = Report::default();
    let vaults = state.map(recorded_vaults).unwrap_or_default();
    if let Some(state) = state {
        export_unsaved_drafts(state, &documents.join(DRAFT_EXPORT_FOLDER), &mut report);
    }
    for root in roots {
        if !root.exists() {
            continue;
        }
        // Records can be lost (an earlier purge, a reset state dir), so a
        // root that holds notes is kept even when no record names it.
        if vaults
            .iter()
            .any(|vault| inside(vault, root, cfg!(windows)))
            || holds_user_notes(root)
        {
            report.kept_holding_vault.push(root.clone());
            continue;
        }
        match std::fs::remove_dir_all(root) {
            Ok(()) => report.removed.push(root.clone()),
            Err(error) => report.failed.push((root.clone(), error.to_string())),
        }
    }
    for session in search_sessions(temp) {
        match std::fs::remove_dir_all(&session) {
            Ok(()) => report.removed.push(session),
            Err(error) => report.failed.push((session, error.to_string())),
        }
    }
    report
}

/// Whether `path` is `root` or below it. NTFS paths are case-insensitive, so
/// a recorded vault that differs from the app root only in case still counts.
fn inside(path: &Path, root: &Path, ignore_case: bool) -> bool {
    if !ignore_case {
        return path.starts_with(root);
    }
    let lower = |p: &Path| -> Vec<String> {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .collect()
    };
    lower(path).starts_with(&lower(root))
}

/// App directories hold JSON, logs and index files, never notes; Markdown or
/// an Obsidian folder inside one means a user put their vault there.
fn holds_user_notes(root: &Path) -> bool {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if name == ".obsidian" {
                return true;
            }
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(path);
            } else if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
            {
                return true;
            }
        }
    }
    false
}

fn search_sessions(temp: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(temp) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(SEARCH_SESSION_PREFIX)
        })
        .map(|e| e.path())
        .collect()
}

/// A crashed or killed run leaves its search session behind (~100 MB each).
/// Sweep the ones older than a day at startup; a live session is recent.
pub(crate) fn sweep_stale_search_sessions() {
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(24 * 60 * 60);
    for session in search_sessions(&std::env::temp_dir()) {
        let stale = std::fs::metadata(&session)
            .and_then(|m| m.modified())
            .is_ok_and(|modified| modified < cutoff);
        if stale {
            let _ = std::fs::remove_dir_all(&session);
        }
    }
}

/// Velopack's before-uninstall hook: everything Okilum created for this user.
#[cfg(windows)]
pub(crate) fn uninstall() {
    crate::markdown_handler::uninstall();
    crate::url_protocol::uninstall();
    windows::remove_registrations();
    let state = crate::reader_history::state_directory().ok();
    let documents = dirs::document_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir);
    let report = purge(
        &roots(),
        state.as_deref(),
        &std::env::temp_dir(),
        &documents,
    );
    windows::remove_crash_dumps();
    windows::remove_velopack_log_after_exit();
    eprintln!("Okilum uninstall: {report:?}");
}

/// PowerShell `-EncodedCommand`: base64 of the UTF-16LE script.
// Called by the Windows uninstall hook; tested everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
fn encoded_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64_encode(&bytes)
}

// Called by the Windows uninstall hook; tested everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                TABLE[(n >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

/// Why the per-user purge must not run now; nothing is removed.
#[cfg(target_os = "linux")]
fn blocked(state: Option<&Path>, units: Option<&Path>) -> Option<String> {
    if let Some(state) = state {
        // Another Okilum holds the instance lock while it runs.
        if let Ok(file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(state.join("reader-instance.lock"))
        {
            if file.try_lock().is_err() {
                return Some("Quit Okilum first, then run this again.".into());
            }
        }
        // Sync must be removed through the controller so the hub forgets this
        // computer and the user service is stopped, never by deleting files.
        let sync = state.join("sync/setup.json");
        let active = read_json(&sync).is_some_and(|record| record["retired"] != true);
        if active {
            return Some(
                "Sync is still set up on this computer. Open Okilum → Settings → Sync → \
                 Remove this computer first, then run this again."
                    .into(),
            );
        }
    }
    let unit = units.and_then(|dir| {
        std::fs::read_dir(dir).ok()?.flatten().find(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with("okilum-syncthing-") && name.ends_with(".service")
        })
    });
    unit.map(|unit| {
        format!(
            "The sync service {} is still installed. Open Okilum → Settings → Sync → \
             Remove this computer first, then run this again.",
            unit.file_name().to_string_lossy()
        )
    })
}

/// `okilum --uninstall-data [--yes]`: remove this user's Okilum data after
/// `pacman -Rns okilum` (or before), keeping vaults. Returns the exit code.
#[cfg(target_os = "linux")]
pub(crate) fn uninstall_data(assume_yes: bool) -> i32 {
    use std::io::{BufRead, Write};
    let state = crate::reader_history::state_directory().ok();
    let units = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|config| config.join("systemd/user"));
    if let Some(reason) = blocked(state.as_deref(), units.as_deref()) {
        eprintln!("{reason}\nNothing was removed.");
        return 2;
    }
    let temp = std::env::temp_dir();
    let mut targets: Vec<PathBuf> = roots().into_iter().filter(|p| p.exists()).collect();
    targets.extend(search_sessions(&temp));
    let enrollment = enrollment_lock();
    targets.extend(enrollment.iter().filter(|p| p.exists()).cloned());
    let vaults: BTreeSet<PathBuf> = state
        .as_deref()
        .map(recorded_vaults)
        .unwrap_or_default()
        .into_iter()
        .collect();
    if targets.is_empty() {
        println!("No Okilum data was found for this user. Nothing to remove.");
        return 0;
    }
    println!("This removes Okilum's settings, history, caches and search index for this user:");
    for target in &targets {
        println!("  {}", target.display());
    }
    println!("Your notes stay where they are. Unsaved drafts are saved to Documents first.");
    if !vaults.is_empty() {
        println!("Vaults Okilum knew about (not touched):");
        for vault in &vaults {
            println!("  {}", vault.display());
        }
    }
    if !assume_yes {
        print!("Remove? [y/N] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().lock().read_line(&mut answer);
        if !matches!(answer.trim(), "y" | "Y" | "yes" | "Yes") {
            println!("Nothing was removed.");
            return 1;
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| temp.clone());
    let documents = std::env::var_os("XDG_DOCUMENTS_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join("Documents"));
    // Okilum or Sync may have started while the question was open.
    if let Some(reason) = blocked(state.as_deref(), units.as_deref()) {
        eprintln!("{reason}\nNothing was removed.");
        return 2;
    }
    let mut report = purge(&roots(), state.as_deref(), &temp, &documents);
    for lock in enrollment.into_iter().filter(|p| p.exists()) {
        match std::fs::remove_file(&lock) {
            Ok(()) => report.removed.push(lock),
            Err(error) => report.failed.push((lock, error.to_string())),
        }
    }
    for path in &report.removed {
        println!("Removed {}", path.display());
    }
    for path in &report.exported_drafts {
        println!("Saved unsaved draft to {}", path.display());
    }
    for path in &report.kept_holding_vault {
        println!("Kept {} because it contains notes", path.display());
    }
    for (path, error) in &report.failed {
        eprintln!("Could not remove {}: {error}", path.display());
    }
    if report.failed.is_empty() {
        0
    } else {
        1
    }
}

/// Sync enrollment serialises through a lock in `/tmp` (see the sync controller).
#[cfg(target_os = "linux")]
fn enrollment_lock() -> Option<PathBuf> {
    let uid = rustix::process::geteuid().as_raw();
    Some(PathBuf::from(format!("/tmp/okilum-sync-enrollment-{uid}")))
}

/// What macOS keeps outside the app roots, removed after the app has quit:
/// preferences go through `defaults` (cfprefsd caches them), and AppKit
/// writes Saved Application State on quit. Arguments: bundle path, pid.
#[cfg(any(target_os = "macos", test))]
const MACOS_CLEANUP: &str = r#"
app=$1; pid=$2
while kill -0 "$pid" 2>/dev/null; do sleep 0.5; done
sleep 1
# Only a real Okilum bundle names what to clean: no bundle, no guess.
case "$app" in *.app) ;; *) exit 0 ;; esac
id=$(defaults read "$app/Contents/Info" CFBundleIdentifier 2>/dev/null) || exit 0
case "$id" in com.befeast.okilum|com.befeast.okilum.*) ;; *) exit 0 ;; esac
defaults delete "$id" >/dev/null 2>&1
lib="$HOME/Library"
rm -rf "$lib/Preferences/$id.plist" "$lib/Saved Application State/$id.savedState"     "$lib/HTTPStorages/$id" "$lib/HTTPStorages/$id.binarycookies"     "$lib/Caches/$id" "$lib/WebKit/$id"
find "$lib/Logs/DiagnosticReports" -maxdepth 1 -iname 'okilum*' -exec rm -f {} + 2>/dev/null
case "$app" in
    *.app) mkdir -p "$HOME/.Trash" && mv "$app" "$HOME/.Trash/Okilum $(date +%Y-%m-%d\ %H.%M.%S).app" ;;
esac
"#;

/// Settings → About → Uninstall Okilum…: one confirmation, then remove
/// everything Okilum created for this user, move the app to the Trash and quit.
#[cfg(target_os = "macos")]
pub(crate) fn uninstall_from_settings(window: &mut gpui::Window, cx: &mut gpui::App) {
    let state = crate::reader_history::state_directory().ok();
    let mut items: Vec<String> = roots()
        .into_iter()
        .filter(|p| p.exists())
        .map(|p| p.display().to_string())
        .collect();
    items.push("Okilum's preferences, caches and saved window state".into());
    items.push("Okilum.app (moved to the Trash)".into());
    let vaults: BTreeSet<PathBuf> = state
        .as_deref()
        .map(recorded_vaults)
        .unwrap_or_default()
        .into_iter()
        .collect();
    let mut outro =
        vec!["Your notes stay where they are. Unsaved drafts are saved to Documents first.".into()];
    if !vaults.is_empty() {
        outro.push(format!(
            "Vaults that stay: {}",
            vaults
                .iter()
                .map(|v| v.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let answer = crate::reader_confirm::confirm(
        window,
        cx,
        "Uninstall Okilum?",
        crate::reader_confirm::Body {
            intro: vec!["This removes Okilum from this Mac:".into()],
            items,
            outro,
        },
        "Uninstall",
    );
    cx.spawn(async move |cx| {
        if answer.recv().await != Ok(true) {
            return;
        }
        let _ = cx.update(|cx| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            let report = purge(
                &roots(),
                state.as_deref(),
                &std::env::temp_dir(),
                &home.join("Documents"),
            );
            eprintln!("Okilum uninstall: {report:?}");
            let app = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.ancestors().nth(3).map(Path::to_path_buf))
                .unwrap_or_default();
            use std::os::unix::process::CommandExt;
            let _ = std::process::Command::new("/bin/sh")
                .args(["-c", MACOS_CLEANUP, "okilum-uninstall"])
                .arg(&app)
                .arg(std::process::id().to_string())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .process_group(0)
                .spawn();
            cx.quit();
        });
    })
    .detach();
}

#[cfg(windows)]
mod windows {
    use std::process::Command;

    /// Notifications register this AUMID (gpui system_notifications).
    const AUMID_KEY: &str = r"HKCU\Software\Classes\AppUserModelId\com.befeast.okilum";

    pub(super) fn remove_registrations() {
        let _ = Command::new("reg.exe")
            .args(["delete", AUMID_KEY, "/f"])
            .status();
    }

    /// Velopack appends to its per-app log after this hook returns, so the
    /// log can only go once its Update.exe has exited. Velopack runs the hook
    /// in a job that ends with it, which kills ordinary child processes, so
    /// the waiting cleanup is created through WMI (`Win32_Process.Create`),
    /// outside that job. It removes the log, then the shared velopack folder
    /// only when nothing else is left in it.
    pub(super) fn remove_velopack_log_after_exit() {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let Some(local) = dirs::data_local_dir() else {
            return;
        };
        let quote = |path: &std::path::Path| path.display().to_string().replace('\'', "''");
        let root = quote(&local.join("BeFeast.Okilum"));
        let folder = local.join("velopack");
        let log = quote(&folder.join("velopack_BeFeast.Okilum.log"));
        let folder = quote(&folder);
        // An interactive uninstall waits on its dialog; give it an hour.
        let cleanup = format!(
            "for($i=0;$i -lt 3600;$i++){{ if(-not (Get-Process Update -EA SilentlyContinue | \
             Where-Object {{ $_.Path -like '{root}\\*' }})){{ break }}; Start-Sleep 1 }}; \
             Start-Sleep 2; Remove-Item -LiteralPath '{log}' -Force -EA SilentlyContinue; \
             if(-not (Get-ChildItem -LiteralPath '{folder}' -Force -EA SilentlyContinue)){{ \
             Remove-Item -LiteralPath '{folder}' -Force -EA SilentlyContinue }}"
        );
        let launch = format!(
            "Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments \
             @{{CommandLine='powershell.exe -NoProfile -NonInteractive -WindowStyle Hidden \
             -EncodedCommand {}'}} | Out-Null",
            super::encoded_command(&cleanup)
        );
        let _ = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                &launch,
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }

    /// Windows Error Reporting keeps local dumps named after the executable.
    pub(super) fn remove_crash_dumps() {
        let Some(folder) = dirs::data_local_dir().map(|d| d.join("CrashDumps")) else {
            return;
        };
        for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .starts_with("okilum.exe.")
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn encoded_command_is_powershell_utf16le_base64() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        // Checked against Python: base64("ls 'a'".encode("utf-16-le")).
        assert_eq!(encoded_command("ls 'a'"), "bABzACAAJwBhACcA");
    }

    #[test]
    fn purge_removes_owned_dirs_and_sessions_but_never_a_vault_or_unsaved_text() {
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path();
        let state = base.join("state/okilum");
        let config = base.join("config/okilum");
        let temp = base.join("tmp");
        let documents = base.join("Documents");
        let vault = base.join("Notes");
        write(&vault.join("saved.md"), "same text");
        write(&vault.join("edited.md"), "old text");
        // A vault that lives inside a root keeps that whole root.
        let inner_vault = base.join("cache/okilum/inner-vault");
        write(&inner_vault.join("note.md"), "inner");
        write(
            &state.join("update-session.json"),
            &serde_json::json!({
                "recent_roots": [vault, inner_vault],
                "last_documents": {},
            })
            .to_string(),
        );
        write(&state.join("reader-ui.json"), "{}");
        for (key, note, text) in [
            ("a", vault.join("saved.md"), "same text"),
            ("b", vault.join("edited.md"), "unsaved שלום text"),
            ("c", vault.join("deleted.md"), "orphan draft"),
        ] {
            write(
                &state.join(format!("editor-drafts/{key}.json")),
                &serde_json::json!({"path": note, "base": "", "text": text}).to_string(),
            );
        }
        write(&config.join("reader-layout.json"), "{}");
        write(
            &temp.join(format!("{SEARCH_SESSION_PREFIX}x/meta.json")),
            "{}",
        );
        write(&temp.join("someone-else/keep.txt"), "not ours");
        let roots = vec![state.clone(), config.clone(), base.join("cache/okilum")];
        let report = purge(&roots, Some(&state), &temp, &documents);

        assert!(!state.exists() && !config.exists(), "{report:?}");
        assert_eq!(report.kept_holding_vault, vec![base.join("cache/okilum")]);
        assert!(
            inner_vault.join("note.md").exists(),
            "a root holding a vault stays"
        );
        assert_eq!(
            std::fs::read_to_string(vault.join("edited.md")).unwrap(),
            "old text"
        );
        assert!(
            vault.join("saved.md").exists(),
            "vault files are never touched"
        );
        assert!(!temp.join(format!("{SEARCH_SESSION_PREFIX}x")).exists());
        assert!(
            temp.join("someone-else/keep.txt").exists(),
            "positive control"
        );
        let exported: BTreeSet<String> = report
            .exported_drafts
            .iter()
            .map(|p| std::fs::read_to_string(p).unwrap())
            .collect();
        assert_eq!(
            exported,
            BTreeSet::from(["unsaved שלום text".to_owned(), "orphan draft".to_owned()]),
            "only drafts that differ from disk are exported"
        );
        assert!(report.failed.is_empty(), "{report:?}");
        // A second run has lost the vault records with the state dir, yet
        // still keeps the root holding notes and changes nothing else.
        let again = purge(&roots, Some(&state), &temp, &documents);
        assert!(
            again.removed.is_empty() && again.exported_drafts.is_empty(),
            "{again:?}"
        );
        assert_eq!(again.kept_holding_vault, vec![base.join("cache/okilum")]);
        assert!(inner_vault.join("note.md").exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn uninstall_data_refuses_while_running_or_while_sync_is_set_up() {
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let units = fixture.path().join("systemd/user");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&units).unwrap();
        assert_eq!(
            blocked(Some(&state), Some(&units)),
            None,
            "positive control: idle and no sync"
        );
        // A running Okilum holds the instance lock.
        let lock = std::fs::File::create(state.join("reader-instance.lock")).unwrap();
        lock.lock().unwrap();
        assert!(blocked(Some(&state), Some(&units))
            .unwrap()
            .contains("Quit Okilum"));
        lock.unlock().unwrap();
        assert_eq!(blocked(Some(&state), Some(&units)), None);
        write(&state.join("sync/setup.json"), r#"{"retired":false}"#);
        assert!(blocked(Some(&state), Some(&units))
            .unwrap()
            .contains("Sync is still set up"));
        write(&state.join("sync/setup.json"), r#"{"retired":true}"#);
        assert_eq!(
            blocked(Some(&state), Some(&units)),
            None,
            "a removed sync no longer blocks"
        );
        write(&units.join("okilum-syncthing-abc.service"), "[Unit]");
        assert!(blocked(Some(&state), Some(&units))
            .unwrap()
            .contains("okilum-syncthing-abc.service"));
    }

    #[cfg(unix)]
    #[test]
    fn macos_cleanup_waits_for_exit_and_removes_only_okilum() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let home = fixture.path().join("home");
        let lib = home.join("Library");
        let bin = fixture.path().join("bin");
        let calls = fixture.path().join("defaults.log");
        // A fake `defaults` records calls and answers the bundle id.
        write(
            &bin.join("defaults"),
            &format!(
                "#!/bin/sh\necho \"$@\" >> '{}'\n[ \"$1\" = read ] && echo com.befeast.okilum\nexit 0\n",
                calls.display()
            ),
        );
        std::fs::set_permissions(bin.join("defaults"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let app = fixture.path().join("Applications/Okilum.app");
        write(&app.join("Contents/Info.plist"), "plist");
        for path in [
            "Preferences/com.befeast.okilum.plist",
            "Saved Application State/com.befeast.okilum.savedState/data.data",
            "HTTPStorages/com.befeast.okilum/x",
            "Caches/com.befeast.okilum/org.sparkle-project.Sparkle/x",
            "Logs/DiagnosticReports/okilum-2026-10-10.ips",
            "Logs/DiagnosticReports/Other-2026.ips",
            "Preferences/com.other.app.plist",
        ] {
            write(&lib.join(path), "x");
        }
        // A pid that has already exited, so the wait loop returns at once.
        let exited = std::process::Command::new("true").spawn().unwrap();
        let pid = exited.id();
        let mut exited = exited;
        exited.wait().unwrap();
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", MACOS_CLEANUP, "okilum-uninstall"])
            .arg(&app)
            .arg(pid.to_string())
            .env("HOME", &home)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .status()
            .unwrap();
        assert!(status.success());
        assert!(std::fs::read_to_string(&calls)
            .unwrap()
            .contains("delete com.befeast.okilum"));
        for gone in [
            "Preferences/com.befeast.okilum.plist",
            "Saved Application State/com.befeast.okilum.savedState",
            "HTTPStorages/com.befeast.okilum",
            "Caches/com.befeast.okilum",
            "Logs/DiagnosticReports/okilum-2026-10-10.ips",
        ] {
            assert!(!lib.join(gone).exists(), "{gone}");
        }
        assert!(
            lib.join("Logs/DiagnosticReports/Other-2026.ips").exists(),
            "positive control"
        );
        assert!(
            lib.join("Preferences/com.other.app.plist").exists(),
            "positive control"
        );
        assert!(!app.exists(), "the app moved to the Trash");
        let trash: Vec<_> = std::fs::read_dir(home.join(".Trash"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(trash.len(), 1);
        assert!(trash[0].path().join("Contents/Info.plist").exists());

        // An executable outside an app bundle (a development build) must not
        // fall back to the production id: nothing is cleaned.
        write(&lib.join("Preferences/com.befeast.okilum.plist"), "x");
        let loose = fixture.path().join("target/debug");
        std::fs::create_dir_all(&loose).unwrap();
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", MACOS_CLEANUP, "okilum-uninstall"])
            .arg(&loose)
            .arg(pid.to_string())
            .env("HOME", &home)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .status()
            .unwrap();
        assert!(status.success());
        assert!(lib.join("Preferences/com.befeast.okilum.plist").exists());
        assert!(loose.exists());
    }

    #[test]
    fn vault_containment_ignores_case_where_the_file_system_does() {
        let root = Path::new("/Users/u/AppData/Local/okilum");
        let vault = Path::new("/users/U/appdata/local/OKILUM/notes");
        assert!(inside(vault, root, true), "NTFS-style comparison");
        assert!(
            !inside(vault, root, false),
            "positive control: exact comparison differs"
        );
        assert!(!inside(
            Path::new("/Users/u/AppData/Local/okilum2/x"),
            root,
            true
        ));
        assert!(inside(root, root, true));
    }

    #[test]
    fn windows_verbatim_vault_roots_are_recognised() {
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path();
        write(
            &state.join("update-session.json"),
            r#"{"recent_roots":["\\\\?\\C:\\Users\\u\\Notes","\\\\?\\UNC\\nas\\share\\vault"]}"#,
        );
        write(
            &state.join("reader-ui.json"),
            r#"{"vaults":{"reader:/home/u/notes":{}}}"#,
        );
        assert_eq!(
            recorded_vaults(state),
            vec![
                PathBuf::from(r"C:\Users\u\Notes"),
                PathBuf::from(r"\\nas\share\vault"),
                PathBuf::from("/home/u/notes"),
            ]
        );
    }

    /// Production code that computes a base directory, per source file. A new
    /// site means the app may write somewhere the uninstaller does not know:
    /// add it to docs/uninstall.md and `roots()` (or the OS-specific removal),
    /// then update this list.
    const BASE_DIRECTORY_SITES: &[(&str, usize)] = &[
        // Roots, crash dumps, the Velopack log folder, the Linux sync units,
        // the drafts export and the macOS home (all in the inventory).
        ("crates/okilum-shell/src/app_footprint.rs", 10),
        // Suggests ~/Downloads in the export dialog: the user picks the target.
        ("crates/okilum-shell/src/brain/context_ui.rs", 1),
        // Brain outboxes and profile: all under the `~/.config/okilum` root.
        ("crates/okilum-shell/src/brain/discussion_send_outbox.rs", 2),
        ("crates/okilum-shell/src/brain/editor_recovery.rs", 2),
        ("crates/okilum-shell/src/brain/native_outbox.rs", 2),
        ("crates/okilum-shell/src/brain/t3_route_outbox.rs", 2),
        ("crates/okilum-shell/src/brain.rs", 2),
        // Looks for installed editors in ~/Applications; reads only.
        ("crates/okilum-shell/src/open_in.rs", 1),
        ("crates/okilum-shell/src/reader_history.rs", 4),
        ("crates/okilum-shell/src/reader_layout.rs", 3),
        ("crates/okilum-shell/src/reader_open.rs", 4),
        // Sync lives under the state root; its systemd unit is removed by
        // the sync controller (Linux part of #974).
        ("crates/okilum-shell/src/reader_settings_sync.rs", 3),
        // Moves notes to the system trash; not an app-owned location.
        ("crates/okilum-shell/src/reader_trash_fs.rs", 2),
        ("crates/okilum-shell/src/workspace.rs", 3),
    ];

    /// Production code only: drop each `#[cfg(test)] mod … { … }` block, which
    /// may sit anywhere in a file. Braces inside string and char literals are
    /// skipped so `format!("{x}")` does not unbalance the match.
    fn strip_test_modules(text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = String::new();
        let mut start = 0;
        while let Some(found) = text[start..].find("#[cfg(test)]\nmod ") {
            let module = start + found;
            out.push_str(&text[start..module]);
            let Some(open) = text[module..].find('{').map(|i| module + i) else {
                return out;
            };
            let (mut depth, mut at) = (0usize, open);
            while at < bytes.len() {
                match bytes[at] {
                    b'"' => {
                        at += 1;
                        while at < bytes.len() && bytes[at] != b'"' {
                            at += if bytes[at] == b'\\' { 2 } else { 1 };
                        }
                    }
                    b'\'' if bytes.get(at + 2) == Some(&b'\'') => at += 2,
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                at += 1;
            }
            start = at + 1;
        }
        out.push_str(&text[start.min(text.len())..]);
        out
    }

    #[test]
    fn strip_test_modules_keeps_code_after_a_mid_file_test_module() {
        let text = "a dirs::x\n#[cfg(test)]\nmod tests {\n fn f() { let _ = format!(\"{}\", '{'); dirs::y }\n}\nb dirs::z\n";
        let production = strip_test_modules(text);
        assert_eq!(production.matches("dirs::").count(), 2, "{production}");
        assert!(
            production.contains("b dirs::z"),
            "positive control: code after the module stays"
        );
    }

    #[test]
    fn every_base_directory_in_production_code_is_in_the_uninstall_inventory() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut found = Vec::new();
        for crate_dir in ["crates/okilum-shell/src", "crates/okilum-core/src"] {
            let mut stack = vec![root.join(crate_dir)];
            while let Some(dir) = stack.pop() {
                for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                        continue;
                    }
                    let name = path.to_string_lossy();
                    if !name.ends_with(".rs")
                        || name.contains("/tests")
                        || name.ends_with("tests.rs")
                    {
                        continue;
                    }
                    let text = std::fs::read_to_string(&path).unwrap();
                    let production = strip_test_modules(&text);
                    let count = [
                        "dirs::",
                        "\"XDG_STATE_HOME\"",
                        "\"XDG_CONFIG_HOME\"",
                        "\"XDG_CACHE_HOME\"",
                        "\"XDG_DATA_HOME\"",
                        "var_os(\"HOME\")",
                    ]
                    .iter()
                    .map(|needle| production.matches(needle).count())
                    .sum::<usize>();
                    if count > 0 {
                        let relative = path
                            .strip_prefix(&root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/");
                        found.push((relative, count));
                    }
                }
            }
        }
        found.sort();
        let mut expected: Vec<(String, usize)> = BASE_DIRECTORY_SITES
            .iter()
            .map(|(path, count)| ((*path).to_owned(), *count))
            .collect();
        expected.sort();
        assert_eq!(
            found, expected,
            "update docs/uninstall.md, roots() and this list"
        );
    }
}
