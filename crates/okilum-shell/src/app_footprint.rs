//! Everything Okilum creates on this machine outside the user's vaults, and its
//! removal on uninstall (#974). The inventory is docs/uninstall.md; the source
//! gate test below fails when code computes a new base directory that the
//! inventory and `roots()` do not know about.
// Windows runs the purge from the Velopack hook, Linux from
// `okilum --uninstall-data`, macOS from Settings → About → Uninstall Okilum….
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use okilum_shell_kit::macos_cli;

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

/// Relocate ordinary AppImage launches before any instance/update transaction.
/// An in-flight update must acknowledge its original target first; its next
/// ordinary launch performs migration. CLI/QA routes never reach this function.
#[cfg(target_os = "linux")]
pub(crate) fn enter_managed_appimage() {
    use std::os::unix::process::CommandExt;
    #[cfg(feature = "appimage-acceptance-harness")]
    if crate::appimage_acceptance::active() {
        return;
    }
    if std::env::var_os("OKILUM_APPIMAGE_UPDATE_RECEIPT").is_some() {
        return;
    }
    let (Some(home), Some(source)) = (std::env::var_os("HOME"), std::env::var_os("APPIMAGE"))
    else {
        return;
    };
    let source = PathBuf::from(source);
    // A non-numeric (development) build never replaces a known managed build.
    let build = env!("OKILUM_BUILD_VERSION").parse().unwrap_or(0);
    let target = match okilum_appimage::installation::install(Path::new(&home), &source, build) {
        Ok(target) => target,
        Err(error) => {
            diagnostic_error!("AppImage installation: {error:#}");
            return;
        }
    };
    refresh_desktop_database(Path::new(&home));
    if target == source {
        return;
    }
    let error = std::process::Command::new(target)
        .args(std::env::args_os().skip(1))
        .env_remove("APPIMAGE")
        .env_remove("APPDIR")
        .env_remove("LD_LIBRARY_PATH")
        .env("APPIMAGE_EXTRACT_AND_RUN", "1")
        .exec();
    diagnostic_error!("Cannot launch installed AppImage: {error}");
    std::process::exit(1);
}

/// Ordinary Linux AppImage startup only; CLI commands and acceptance fixtures
/// return before reaching this hook. The primary instance owns the background job.
#[cfg(target_os = "linux")]
pub(crate) fn install_appimage_desktop() {
    if crate::linux_sprint::enabled() {
        return; // The Sprint installer owns only okilum-sprint.desktop.
    }
    #[cfg(feature = "appimage-acceptance-harness")]
    if crate::appimage_acceptance::active() {
        return;
    }
    let Some(image) = std::env::var_os("APPIMAGE").map(PathBuf::from) else {
        return;
    };
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("appimage-desktop".into())
        .spawn(move || {
            if let Err(error) = okilum_appimage::desktop::install(&home, &image) {
                diagnostic_error!("AppImage desktop integration: {error:#}");
                return;
            }
            refresh_desktop_database(&home);
        });
}

#[cfg(target_os = "linux")]
fn refresh_desktop_database(home: &Path) {
    let Ok((desktop, icon)) = okilum_appimage::desktop::paths(home) else {
        return;
    };
    // Optional cache refresh only, never change defaults or MIME associations.
    if let Some(directory) = desktop.parent() {
        let _ = std::process::Command::new("update-desktop-database")
            .arg(directory)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .env_remove("LD_LIBRARY_PATH")
            .env_remove("LD_PRELOAD")
            .status();
    }
    if let Some(theme) = icon
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .filter(|theme| theme.is_dir())
    {
        let _ = std::process::Command::new("gtk-update-icon-cache")
            .args(["--force", "--ignore-theme-index"])
            .arg(theme)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .env_remove("LD_LIBRARY_PATH")
            .env_remove("LD_PRELOAD")
            .status();
    }
}

#[cfg(target_os = "linux")]
fn desktop_payload(home: &Path) -> Vec<PathBuf> {
    okilum_appimage::desktop::paths(home)
        .map(|(desktop, icon)| {
            let mut paths = vec![desktop, icon];
            if let Ok((image, command)) = okilum_appimage::installation::paths(home) {
                paths.extend([image, command]);
            }
            paths
        })
        .unwrap_or_default()
        .into_iter()
        .filter(|path| path.symlink_metadata().is_ok())
        .collect()
}

#[cfg(target_os = "linux")]
fn remove_appimage_desktop(home: &Path, report: &mut Report) {
    let before = desktop_payload(home);
    let result = okilum_appimage::desktop::remove(home)
        .and_then(|()| okilum_appimage::installation::remove(home));
    for path in before {
        if path.symlink_metadata().is_err() {
            report.removed.push(path);
        } else if let Err(error) = &result {
            report.failed.push((path, error.to_string()));
        }
    }
    refresh_desktop_database(home);
}

/// What an uninstall removed, kept or could not remove; printed for support.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Report {
    pub removed: Vec<PathBuf>,
    pub exported_drafts: Vec<PathBuf>,
    pub kept_holding_vault: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
    /// Roots emptied around Sync's retained identity instead of removed (#588).
    pub kept_for_sync: Vec<PathBuf>,
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
// Linux `--uninstall-data`; macOS and the Windows hook use `purge_planned`.
#[cfg_attr(all(any(target_os = "macos", windows), not(test)), allow(dead_code))]
pub(crate) fn purge(
    roots: &[PathBuf],
    state: Option<&Path>,
    temp: &Path,
    documents: &Path,
) -> Report {
    purge_planned(&Plan::remove_all(roots), state, temp, documents)
}

/// What the purge does to each root, decided before anything changes (#588).
#[derive(Debug)]
pub(crate) struct Plan {
    roots: Vec<(PathBuf, Step)>,
    keep: Vec<PathBuf>,
}
#[derive(Debug, PartialEq)]
enum Step {
    Remove,
    /// A symlinked root where Sync's identity may live, or a root that is
    /// itself (inside) a kept folder: never followed, enumerated or removed.
    Leave,
    /// Empty this resolved directory around the kept paths.
    EmptyAround(PathBuf),
}
impl Plan {
    #[cfg_attr(all(any(target_os = "macos", windows), not(test)), allow(dead_code))]
    fn remove_all(roots: &[PathBuf]) -> Self {
        Self {
            roots: roots
                .iter()
                .map(|root| (root.clone(), Step::Remove))
                .collect(),
            keep: Vec::new(),
        }
    }
    /// Sync's retained identity survives even when a root contains it: on
    /// macOS `Application Support/okilum` (config) is the same folder as
    /// `Application Support/Okilum`, the parent of `Sync/` (#588), and on
    /// Windows `%LOCALAPPDATA%\okilum` (state) is the same NTFS folder as
    /// `%LOCALAPPDATA%\Okilum` (#3109). Roots and kept paths are compared
    /// canonically (symlinked HOME or XDG folders, NTFS junctions, 8.3 names,
    /// case-insensitive names); any path that cannot be resolved refuses.
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
    pub(crate) fn keeping(roots: &[PathBuf], keep: &[PathBuf]) -> anyhow::Result<Self> {
        use anyhow::Context as _;
        // Each kept path in two forms: the path itself (final component not
        // followed) and, when it is a symlink, the folder it leads to. A kept
        // `Sync` linked into another root keeps that root too.
        let mut resolved_keep = Vec::new();
        for kept in keep {
            resolved_keep.push(
                resolve_final(kept)
                    .with_context(|| format!("cannot resolve {}", kept.display()))?,
            );
            match std::fs::symlink_metadata(kept) {
                Ok(metadata) if metadata.file_type().is_symlink() => resolved_keep.push(
                    std::fs::canonicalize(kept)
                        .with_context(|| format!("cannot resolve {}", kept.display()))?,
                ),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("cannot inspect {}", kept.display()))
                }
            }
        }
        let mut steps = Vec::new();
        for root in roots {
            // Also for a missing root: a dangling symlink among its ancestors
            // (a broken XDG_CONFIG_HOME, say) cannot be resolved and refuses.
            let resolved = resolve_final(root)
                .with_context(|| format!("cannot resolve {}", root.display()))?;
            let metadata = match std::fs::symlink_metadata(root) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    steps.push((root.clone(), Step::Remove));
                    continue;
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("cannot inspect {}", root.display()))
                }
                Ok(metadata) => metadata,
            };
            let within_kept = resolved_keep
                .iter()
                .any(|kept| inside(&resolved, kept, IGNORE_CASE));
            let holds_kept = resolved_keep
                .iter()
                .any(|kept| inside(kept, &resolved, IGNORE_CASE));
            let step = if within_kept {
                Step::Leave
            } else if metadata.file_type().is_symlink() {
                // A link or junction root is never followed by the purge, but
                // its target must resolve: a dangling one refuses (#3109).
                std::fs::canonicalize(root)
                    .with_context(|| format!("cannot resolve the target of {}", root.display()))?;
                // Removing a link never touches its target; keep the link when
                // Sync's identity is reached through it.
                if holds_kept || keep.iter().any(|kept| inside(kept, root, IGNORE_CASE)) {
                    Step::Leave
                } else {
                    Step::Remove
                }
            } else if holds_kept {
                Step::EmptyAround(resolved)
            } else {
                Step::Remove
            };
            steps.push((root.clone(), step));
        }
        Ok(Self {
            roots: steps,
            keep: resolved_keep,
        })
    }
}

/// Canonical form for comparison: the nearest existing ancestor is resolved
/// (symlinks and junctions, 8.3 names expanded, case as stored), missing
/// components are appended, and the final component itself is never
/// followed: an existing final directory or file is resolved too, a final
/// symlink or junction is not. Any error other than "missing" fails.
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
fn resolve_final(path: &Path) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};
    let invalid = || {
        Error::new(
            ErrorKind::InvalidInput,
            "not an absolute path with a final name",
        )
    };
    if !path.is_absolute() {
        return Err(invalid());
    }
    let name = path.file_name().ok_or_else(invalid)?;
    match std::fs::symlink_metadata(path) {
        // Not a link: resolving it changes only its spelling (case, 8.3).
        Ok(metadata) if !metadata.file_type().is_symlink() => {
            return std::fs::canonicalize(path);
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut existing = path.parent().ok_or_else(invalid)?;
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                missing.push(existing.file_name().ok_or_else(invalid)?);
                existing = existing.parent().ok_or_else(invalid)?;
            }
            Err(error) => return Err(error),
        }
    }
    let mut resolved = std::fs::canonicalize(existing)?;
    for component in missing.iter().rev() {
        resolved.push(component);
    }
    resolved.push(name);
    Ok(resolved)
}

/// Run a `Plan`: remove roots and stale search sessions in `temp`, keeping
/// vaults and the plan's kept paths.
pub(crate) fn purge_planned(
    plan: &Plan,
    state: Option<&Path>,
    temp: &Path,
    documents: &Path,
) -> Report {
    let mut report = Report::default();
    let vaults = state.map(recorded_vaults).unwrap_or_default();
    if let Some(state) = state {
        export_unsaved_drafts(state, &documents.join(DRAFT_EXPORT_FOLDER), &mut report);
    }
    for (root, step) in &plan.roots {
        if *step == Step::Leave {
            report.kept_for_sync.push(root.clone());
            continue;
        }
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
        if let Step::EmptyAround(resolved) = step {
            remove_except(resolved, &plan.keep, &mut report);
            report.kept_for_sync.push(root.clone());
            continue;
        }
        // Removes a symlinked root's link, never its target.
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

/// macOS (APFS default) and Windows compare names without case.
const IGNORE_CASE: bool = cfg!(any(windows, target_os = "macos"));

/// Remove everything in the resolved directory `dir` except the resolved
/// paths in `keep` and their ancestors. Symlinks are removed as links and
/// never followed; a symlink on the way to a kept path is left alone.
fn remove_except(dir: &Path, keep: &[PathBuf], report: &mut Report) {
    // An unreadable folder fails safe (nothing is removed) but is reported.
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            report.failed.push((dir.to_path_buf(), error.to_string()));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report.failed.push((dir.to_path_buf(), error.to_string()));
                continue;
            }
        };
        let path = entry.path();
        if keep.iter().any(|kept| inside(&path, kept, IGNORE_CASE)) {
            continue;
        }
        let kind = entry.file_type();
        if keep.iter().any(|kept| inside(kept, &path, IGNORE_CASE)) {
            if kind.as_ref().is_ok_and(|kind| kind.is_dir()) {
                remove_except(&path, keep, report);
            }
            continue;
        }
        let removed = match kind {
            Ok(kind) if kind.is_dir() => std::fs::remove_dir_all(&path),
            // A directory symlink or junction on Windows is removed as a
            // directory entry; its target is never touched.
            Ok(kind) if cfg!(windows) && kind.is_symlink() => {
                std::fs::remove_file(&path).or_else(|_| std::fs::remove_dir(&path))
            }
            _ => std::fs::remove_file(&path),
        };
        match removed {
            Ok(()) => report.removed.push(path),
            Err(error) => report.failed.push((path, error.to_string())),
        }
    }
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
    match windows_hook(&windows::Installed) {
        Ok(report) => diagnostic_error!("Okilum uninstall: {report:?}"),
        Err(error) => diagnostic_error!("Okilum uninstall removed nothing: {error:#}"),
    }
}

/// What the Windows hook does to the machine; `windows::Installed` is the
/// real one, tests inject fakes so they drive `windows_hook` itself.
#[cfg(any(windows, test))]
trait WindowsHost {
    fn roots(&self) -> Vec<PathBuf>;
    /// `Okilum\Sync` as `sidecar::windows::bootstrap` resolves it.
    fn sync_state_directory(&self) -> anyhow::Result<PathBuf>;
    /// An isolated Reader profile never touches the account's Sync task.
    fn isolated_profile(&self) -> bool;
    /// The authenticated stop of the account's Sync task, then unregister it.
    fn stop_sync(&self) -> anyhow::Result<()>;
    /// File associations, folder and `okilum:` registrations, the AUMID.
    fn remove_registrations(&self);
    fn state_directory(&self) -> Option<PathBuf>;
    fn temp_directory(&self) -> PathBuf;
    fn documents_directory(&self) -> PathBuf;
    /// Crash dumps and the detached cleanup that waits for Update.exe.
    fn remove_leftovers(&self);
}

/// The Windows hook (#3109). The purge plan that keeps Sync's identity
/// (`Okilum\Sync`, `Okilum\SyncDirect`; `%LOCALAPPDATA%\okilum` is the same
/// NTFS folder as their parent) is decided canonically first; then Sync's task
/// is stopped through the authenticated stop and unregistered; only then are
/// registrations, state, config and dumps removed. When a path cannot be
/// resolved or the stop fails, it removes nothing.
#[cfg(any(windows, test))]
fn windows_hook(host: &impl WindowsHost) -> anyhow::Result<Report> {
    stop_then_remove(
        || {
            use anyhow::Context as _;
            let sync = host.sync_state_directory()?;
            let direct = sync
                .parent()
                .context("Sync state parent unavailable")?
                .join("SyncDirect");
            let plan = Plan::keeping(&host.roots(), &[sync, direct])?;
            if !host.isolated_profile() {
                host.stop_sync()?;
            }
            Ok(plan)
        },
        |plan| {
            host.remove_registrations();
            let state = host.state_directory();
            let report = purge_planned(
                &plan,
                state.as_deref(),
                &host.temp_directory(),
                &host.documents_directory(),
            );
            host.remove_leftovers();
            report
        },
    )
}

/// `windows_uninstall_cleanup.ps1` with its install root and local app data.
// Called by the Windows uninstall hook; tested everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
fn cleanup_script(local: &Path) -> String {
    let quote = |path: &Path| path.display().to_string().replace('\'', "''");
    format!(
        "$Root='{}'\n$Local='{}'\n{}",
        quote(&local.join("BeFeast.Okilum")),
        quote(local),
        include_str!("windows_uninstall_cleanup.ps1")
    )
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
        // Preserve the old connection authority and running daemon. The retired
        // Settings controls cannot safely remove them; never imply they can.
        let sync = state.join("sync/setup.json");
        let active = read_json(&sync).is_some_and(|record| record["retired"] != true);
        if active {
            return Some(
                "Sync is still set up on this computer. Automatic cleanup is unavailable \
                 for this older connection. Your files and saved connection have been kept."
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
            "An existing file sync process ({}) is still installed. Automatic cleanup is \
             unavailable while it is present. Your files and connection settings have been kept.",
            unit.file_name().to_string_lossy()
        )
    })
}

/// `okilum --uninstall-data [--yes]`: remove this user's Okilum data after
/// `pacman -Rns okilum` (or before), keeping vaults. Returns the exit code.
#[cfg(target_os = "linux")]
pub(crate) fn uninstall_data(assume_yes: bool) -> i32 {
    if crate::linux_sprint::enabled() {
        diagnostic_error!("Remove Linux Sprint through its dedicated installer; ordinary Okilum cleanup is disabled.");
        return 2;
    }
    use std::io::{BufRead, Write};
    let state = crate::reader_history::state_directory().ok();
    let units = crate::reader_settings_sync::managed_service_directory().ok();
    if let Some(reason) = blocked(state.as_deref(), units.as_deref()) {
        diagnostic_error!("{reason}\nNothing was removed.");
        return 2;
    }
    let temp = std::env::temp_dir();
    let desktop_home = std::env::var_os("HOME").map(PathBuf::from);
    let mut targets: Vec<PathBuf> = roots().into_iter().filter(|p| p.exists()).collect();
    targets.extend(search_sessions(&temp));
    if let Some(home) = &desktop_home {
        targets.extend(desktop_payload(home));
    }
    let enrollment = enrollment_lock();
    targets.extend(enrollment.iter().filter(|p| p.exists()).cloned());
    let vaults: BTreeSet<PathBuf> = state
        .as_deref()
        .map(recorded_vaults)
        .unwrap_or_default()
        .into_iter()
        .collect();
    if targets.is_empty() {
        diagnostic_println!("No Okilum data was found for this user. Nothing to remove.");
        return 0;
    }
    diagnostic_println!(
        "This removes Okilum's settings, history, caches and search index for this user:"
    );
    for target in &targets {
        diagnostic_println!("  {}", target.display());
    }
    diagnostic_println!(
        "Your notes stay where they are. Unsaved drafts are saved to Documents first."
    );
    if !vaults.is_empty() {
        diagnostic_println!("Vaults Okilum knew about (not touched):");
        for vault in &vaults {
            diagnostic_println!("  {}", vault.display());
        }
    }
    if !assume_yes {
        print!("Remove? [y/N] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().lock().read_line(&mut answer);
        if !matches!(answer.trim(), "y" | "Y" | "yes" | "Yes") {
            diagnostic_println!("Nothing was removed.");
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
        diagnostic_error!("{reason}\nNothing was removed.");
        return 2;
    }
    let mut report = purge(&roots(), state.as_deref(), &temp, &documents);
    if let Some(home) = &desktop_home {
        remove_appimage_desktop(home, &mut report);
    }
    for lock in enrollment.into_iter().filter(|p| p.exists()) {
        match std::fs::remove_file(&lock) {
            Ok(()) => report.removed.push(lock),
            Err(error) => report.failed.push((lock, error.to_string())),
        }
    }
    for path in &report.removed {
        diagnostic_println!("Removed {}", path.display());
    }
    for path in &report.exported_drafts {
        diagnostic_println!("Saved unsaved draft to {}", path.display());
    }
    for path in &report.kept_holding_vault {
        diagnostic_println!("Kept {} because it contains notes", path.display());
    }
    for (path, error) in &report.failed {
        diagnostic_error!("Could not remove {}: {error}", path.display());
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
/// writes Saved Application State on quit, and Okilum logs its own quit to
/// the state folder after the purge (#1284). Arguments: bundle path, pid,
/// state folder.
#[cfg(any(target_os = "macos", test))]
const MACOS_CLEANUP: &str = r#"
app=$1; pid=$2; state=$3
while kill -0 "$pid" 2>/dev/null; do sleep 0.5; done
sleep 1
# Only a real Okilum bundle names what to clean: no bundle, no guess.
case "$app" in *.app) ;; *) exit 0 ;; esac
id=$(defaults read "$app/Contents/Info" CFBundleIdentifier 2>/dev/null) || exit 0
case "$id" in com.befeast.okilum|com.befeast.okilum.*) ;; *) exit 0 ;; esac
defaults delete "$id" >/dev/null 2>&1
# The quit line written after the purge: only the log, and the folder only if
# nothing else is in it.
if [ -n "$state" ]; then
    rm -f "$state/reader-diagnostic.log" "$state/reader-diagnostic.log.previous"
    rmdir "$state" 2>/dev/null
fi
lib="$HOME/Library"
rm -rf "$lib/Preferences/$id.plist" "$lib/Saved Application State/$id.savedState"     "$lib/HTTPStorages/$id" "$lib/HTTPStorages/$id.binarycookies"     "$lib/Caches/$id" "$lib/WebKit/$id"
find "$lib/Logs/DiagnosticReports" -maxdepth 1 -iname 'okilum*' -exec rm -f {} + 2>/dev/null
case "$app" in
    *.app) mkdir -p "$HOME/.Trash" && mv "$app" "$HOME/.Trash/Okilum $(date +%Y-%m-%d\ %H.%M.%S).app" ;;
esac
"#;

/// Shown when uninstall stops before removing anything (#588).
#[cfg(target_os = "macos")]
const SYNC_STOP_FAILED: &str = "Sync on this Mac could not be stopped, so nothing was removed. \
     Open Settings → Sync, press Fix if Sync needs attention, then try again.";

/// The account's Sync helper and its login agent belong to this app bundle
/// (#588). Stop and unregister them before uninstall removes anything; Sync's
/// identity and journal stay. An isolated Reader profile never touches them.
/// Returns the purge plan that keeps Sync's retained identity folders: the
/// config root `Application Support/okilum` is the same folder as their
/// parent `Application Support/Okilum` on a case-insensitive volume.
#[cfg(target_os = "macos")]
fn stop_sync_for_uninstall() -> anyhow::Result<Plan> {
    use anyhow::Context as _;
    let sync = okilum_sync_controller::sidecar::macos::account_state_directory()?;
    let direct = sync
        .parent()
        .context("Sync state parent unavailable")?
        .join("SyncDirect");
    // Decided before the helper stops: an unresolvable path changes nothing.
    let plan = Plan::keeping(&roots(), &[sync, direct])?;
    if crate::reader_history::isolated_state_directory().is_none() {
        // Checks the login agent even when Sync's state folder is gone; the
        // signing policy is needed only when a journal exists.
        okilum_sync_controller::sidecar::macos::bootstrap::disable_current_app(
            crate::reader_settings_sync::native_signing_policy,
        )?;
    }
    Ok(plan)
}

/// Uninstall order (#588, #3109): Sync is stopped first, and when that fails
/// nothing is purged, exported or moved to the Trash.
#[cfg(any(target_os = "macos", windows, test))]
fn stop_then_remove<K, T>(
    stop: impl FnOnce() -> anyhow::Result<K>,
    remove: impl FnOnce(K) -> T,
) -> anyhow::Result<T> {
    Ok(remove(stop()?))
}

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
    if let Some(command) = macos_cli::owned_command() {
        items.push(command.display().to_string());
    }
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
    let asking = window.window_handle();
    cx.spawn(async move |cx| {
        if answer.recv().await != Ok(true) {
            return;
        }
        // No Sync action of this app (Settings, Fix, forwarded `okilum sync`
        // commands, background serving) may run from the helper's stop to the
        // quit: closed admission refuses even actions already queued.
        crate::reader_settings_sync::ADMISSION.close();
        cx.update(crate::reader_settings_sync::suspend);
        // The Sync helper runs from inside the bundle that goes to the Trash.
        // Stop it and unregister its agent first; otherwise remove nothing.
        let removed = cx
            .background_executor()
            .spawn(async move {
                // Waits for an in-flight Sync action and blocks new ones.
                let _serial = crate::reader_settings_sync::serial_actions();
                stop_then_remove(stop_sync_for_uninstall, |plan| {
                    let home = std::env::var_os("HOME")
                        .map(PathBuf::from)
                        .unwrap_or_default();
                    let report = purge_planned(
                        &plan,
                        state.as_deref(),
                        &std::env::temp_dir(),
                        &home.join("Documents"),
                    );
                    if let Err(error) = macos_cli::remove() {
                        diagnostic_error!("Cannot remove the Okilum terminal command: {error}");
                    }
                    diagnostic_error!("Okilum uninstall: {report:?}");
                    let app = std::env::current_exe()
                        .ok()
                        .and_then(|exe| exe.ancestors().nth(3).map(Path::to_path_buf))
                        .unwrap_or_default();
                    use std::os::unix::process::CommandExt;
                    let _ = std::process::Command::new("/bin/sh")
                        .args(["-c", MACOS_CLEANUP, "okilum-uninstall"])
                        .arg(&app)
                        .arg(std::process::id().to_string())
                        .arg(state.as_deref().unwrap_or(Path::new("")))
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .process_group(0)
                        .spawn();
                })
            })
            .await;
        if let Err(error) = removed {
            diagnostic_error!("Okilum uninstall cancelled, Sync could not be stopped: {error:#}");
            let shown = asking.update(cx, |_, window, cx| {
                window.prompt(
                    gpui::PromptLevel::Critical,
                    "Okilum was not uninstalled",
                    Some(SYNC_STOP_FAILED),
                    &["OK"],
                    cx,
                )
            });
            // Okilum stays: admit Sync actions and serve paired computers again.
            crate::reader_settings_sync::ADMISSION.reopen();
            cx.update(crate::reader_settings_sync::serve_paired);
            if let Ok(answer) = shown {
                let _ = answer.await;
            }
            return;
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
}

#[cfg(windows)]
mod windows {
    use std::process::Command;

    /// Notifications register this AUMID (gpui system_notifications).
    const AUMID_KEY: &str = r"HKCU\Software\Classes\AppUserModelId\com.befeast.okilum";

    /// The installed app's effects for `windows_hook`.
    pub(super) struct Installed;
    impl super::WindowsHost for Installed {
        fn roots(&self) -> Vec<std::path::PathBuf> {
            super::roots()
        }
        fn sync_state_directory(&self) -> anyhow::Result<std::path::PathBuf> {
            okilum_sync_controller::sidecar::windows::bootstrap::active_state_directory()
        }
        fn isolated_profile(&self) -> bool {
            crate::reader_history::isolated_state_directory().is_some()
        }
        fn stop_sync(&self) -> anyhow::Result<()> {
            // Returns at once when Sync was never set up on this account;
            // identity, configuration and journal stay, the hub is not told.
            okilum_sync_controller::sidecar::windows::bootstrap::disable_current_app()
        }
        fn remove_registrations(&self) {
            crate::markdown_handler::uninstall();
            okilum_core::folder_registration::uninstall();
            crate::url_protocol::uninstall();
            remove_registrations();
        }
        fn state_directory(&self) -> Option<std::path::PathBuf> {
            crate::reader_history::state_directory().ok()
        }
        fn temp_directory(&self) -> std::path::PathBuf {
            std::env::temp_dir()
        }
        fn documents_directory(&self) -> std::path::PathBuf {
            dirs::document_dir()
                .or_else(dirs::home_dir)
                .unwrap_or_else(std::env::temp_dir)
        }
        fn remove_leftovers(&self) {
            remove_crash_dumps();
            remove_leftovers_after_exit();
        }
    }

    pub(super) fn remove_registrations() {
        let _ = Command::new("reg.exe")
            .args(["delete", AUMID_KEY, "/f"])
            .status();
    }

    /// The rest of the uninstall (`windows_uninstall_cleanup.ps1`) has to wait
    /// until Velopack's Update.exe exits: Velopack appends to its per-app log
    /// after this hook returns, and Explorer records the shortcuts and
    /// executables while they are removed. Velopack runs the hook in a job that
    /// ends with it, which kills ordinary child processes, so the waiting
    /// cleanup is created through WMI (`Win32_Process.Create`), outside that job.
    pub(super) fn remove_leftovers_after_exit() {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let Some(local) = dirs::data_local_dir() else {
            return;
        };
        let launch = format!(
            "Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments \
             @{{CommandLine='powershell.exe -NoProfile -NonInteractive -WindowStyle Hidden \
             -EncodedCommand {}'}} | Out-Null",
            super::encoded_command(&super::cleanup_script(&local))
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
    fn windows_cleanup_script_quotes_its_paths_and_covers_shell_records() {
        let local = Path::new(r"C:\Users\o'brien\AppData\Local");
        let script = cleanup_script(local);
        let root = local.join("BeFeast.Okilum").display().to_string();
        let mut lines = script.lines();
        assert_eq!(
            lines.next(),
            Some(format!("$Root='{}'", root.replace('\'', "''")).as_str())
        );
        assert_eq!(
            lines.next(),
            Some(r"$Local='C:\Users\o''brien\AppData\Local'")
        );
        for wanted in [
            "velopack_BeFeast.Okilum.log",
            r"Shell\MuiCache",
            r"Compatibility Assistant\Store",
            "ApplicationAssociationToasts",
            r"Search\JumplistData",
            r"Start\TileProperties\W~$id",
            "UserAssist",
        ] {
            assert!(script.contains(wanted), "cleanup misses {wanted}");
        }
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
    fn uninstall_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
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
        (fixture, state, units)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn uninstall_data_refuses_while_running() {
        let (_fixture, state, units) = uninstall_fixture();
        // A running Okilum holds the instance lock.
        let lock = std::fs::File::create(state.join("reader-instance.lock")).unwrap();
        lock.lock().unwrap();
        let reason = blocked(Some(&state), Some(&units));
        assert!(
            reason.as_deref().unwrap().contains("Quit Okilum"),
            "{reason:?}"
        );
        lock.unlock().unwrap();
        assert_eq!(blocked(Some(&state), Some(&units)), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn uninstall_data_refuses_while_sync_is_set_up() {
        let (_fixture, state, units) = uninstall_fixture();
        write(&state.join("sync/setup.json"), r#"{"retired":false}"#);
        let reason = blocked(Some(&state), Some(&units));
        assert!(
            reason.as_deref().unwrap().contains("Sync is still set up"),
            "{reason:?}"
        );
        assert!(!reason.as_deref().unwrap().contains("Settings"));
        assert!(reason.as_deref().unwrap().contains("have been kept"));
        write(&state.join("sync/setup.json"), r#"{"retired":true}"#);
        assert_eq!(
            blocked(Some(&state), Some(&units)),
            None,
            "a removed sync no longer blocks"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn uninstall_data_refuses_while_sync_service_is_installed() {
        // Use a separate state root from the instance-lock case. A concurrent
        // fork can inherit a probe's flock until exec/exit, even with CLOEXEC,
        // and otherwise mask this service refusal with "Quit Okilum first".
        let (_fixture, state, units) = uninstall_fixture();
        write(&state.join("sync/setup.json"), r#"{"retired":true}"#);
        assert_eq!(blocked(Some(&state), Some(&units)), None);
        write(&units.join("okilum-syncthing-abc.service"), "[Unit]");
        let reason = blocked(Some(&state), Some(&units));
        assert!(
            reason
                .as_deref()
                .unwrap()
                .contains("okilum-syncthing-abc.service"),
            "{reason:?}"
        );
        assert!(!reason.as_deref().unwrap().contains("Settings"));
        assert!(reason.as_deref().unwrap().contains("have been kept"));
    }

    /// Records what `windows_hook` asks of the machine, in order.
    struct FakeHost {
        roots: Vec<PathBuf>,
        sync: PathBuf,
        state: PathBuf,
        documents: PathBuf,
        stop: Result<(), &'static str>,
        isolated: bool,
        events: std::cell::RefCell<Vec<&'static str>>,
    }
    impl FakeHost {
        /// `%LOCALAPPDATA%\okilum` with Sync's identity inside it (Sync's own
        /// spelling `Okilum`) and `%APPDATA%\okilum` with config.
        fn new(fixture: &Path) -> Self {
            let local = fixture.join("Local");
            let state = local.join("okilum");
            write(&state.join("settings.json"), "{}");
            write(&state.join("reader/index.bin"), "index");
            write(&state.join("Sync/config/cert.pem"), "certificate");
            write(&state.join("Sync/sidecar.json"), "{}");
            write(&state.join("SyncDirect/direct.json"), "{}");
            let config = fixture.join("Roaming").join("okilum");
            write(&config.join("reader-layout.json"), "{}");
            let spelled = if IGNORE_CASE { "Okilum" } else { "okilum" };
            Self {
                roots: vec![state.clone(), config],
                sync: local.join(spelled).join("Sync"),
                state,
                documents: fixture.join("Documents"),
                stop: Ok(()),
                isolated: false,
                events: Default::default(),
            }
        }
        fn event(&self, name: &'static str) {
            self.events.borrow_mut().push(name);
        }
        fn events(&self) -> Vec<&'static str> {
            self.events.borrow().clone()
        }
        /// Nothing of the fixture was removed.
        fn untouched(&self) -> bool {
            self.state.join("settings.json").exists()
                && self.state.join("reader/index.bin").exists()
                && self.roots[1].join("reader-layout.json").exists()
                && self.state.join("Sync/config/cert.pem").exists()
        }
    }
    impl WindowsHost for FakeHost {
        fn roots(&self) -> Vec<PathBuf> {
            self.event("roots");
            self.roots.clone()
        }
        fn sync_state_directory(&self) -> anyhow::Result<PathBuf> {
            Ok(self.sync.clone())
        }
        fn isolated_profile(&self) -> bool {
            self.isolated
        }
        fn stop_sync(&self) -> anyhow::Result<()> {
            self.event("stop");
            assert!(
                self.untouched(),
                "Sync is stopped before anything is removed"
            );
            self.stop.map_err(anyhow::Error::msg)
        }
        fn remove_registrations(&self) {
            self.event("registrations");
        }
        fn state_directory(&self) -> Option<PathBuf> {
            self.event("purge");
            Some(self.state.clone())
        }
        fn temp_directory(&self) -> PathBuf {
            self.documents.clone()
        }
        fn documents_directory(&self) -> PathBuf {
            self.documents.clone()
        }
        fn remove_leftovers(&self) {
            self.event("leftovers");
        }
    }

    /// #3109, through the production hook `windows_hook`: Sync's task is
    /// stopped (authenticated stop + unregister) before registrations and
    /// files go, and the state root `%LOCALAPPDATA%\okilum` keeps
    /// `Okilum\Sync` and `Okilum\SyncDirect`. The pre-#3109 hook (no stop,
    /// whole roots removed) fails both the event order and the identity checks.
    #[test]
    fn windows_hook_stops_sync_first_and_keeps_its_identity_in_the_state_root() {
        let fixture = tempfile::tempdir().unwrap();
        let host = FakeHost::new(fixture.path());
        let report = windows_hook(&host).unwrap();
        assert_eq!(
            host.events(),
            ["roots", "stop", "registrations", "purge", "leftovers"]
        );
        assert!(!host.state.join("settings.json").exists(), "{report:?}");
        assert!(!host.state.join("reader").exists(), "{report:?}");
        assert!(!host.roots[1].exists(), "{report:?}");
        assert_eq!(
            std::fs::read_to_string(host.sync.join("config/cert.pem")).unwrap(),
            "certificate"
        );
        assert!(host.sync.join("sidecar.json").exists());
        assert!(host.state.join("SyncDirect/direct.json").exists());
        assert_eq!(report.kept_for_sync, vec![host.state.clone()]);
        assert!(report.removed.contains(&host.roots[1]), "{report:?}");
        assert!(report.failed.is_empty(), "{report:?}");
    }

    /// #3109: a failed authenticated stop removes nothing, not even
    /// registrations.
    #[test]
    fn windows_hook_removes_nothing_when_sync_cannot_be_stopped() {
        let fixture = tempfile::tempdir().unwrap();
        let mut host = FakeHost::new(fixture.path());
        host.stop = Err("Sync helper is not answering");
        let error = windows_hook(&host).unwrap_err();
        assert!(format!("{error:#}").contains("not answering"), "{error:#}");
        assert_eq!(host.events(), ["roots", "stop"]);
        assert!(host.untouched());
    }

    /// #3109: an unresolvable kept path or root refuses before Sync is
    /// touched; nothing is stopped or removed.
    #[test]
    fn windows_hook_removes_nothing_when_a_path_cannot_be_resolved() {
        let fixture = tempfile::tempdir().unwrap();
        let mut host = FakeHost::new(fixture.path());
        host.sync = PathBuf::from("Okilum").join("Sync");
        assert!(windows_hook(&host).is_err());
        assert_eq!(host.events(), ["roots"]);
        assert!(host.untouched());
        // A dangling link as the final component of a root (#3112 review).
        #[cfg(unix)]
        {
            let host = FakeHost::new(fixture.path());
            let config = host.roots[1].clone();
            std::fs::remove_dir_all(&config).unwrap();
            std::os::unix::fs::symlink(fixture.path().join("gone"), &config).unwrap();
            assert!(windows_hook(&host).is_err());
            assert_eq!(host.events(), ["roots"]);
            assert!(host.state.join("settings.json").exists());
            assert!(std::fs::symlink_metadata(&config).is_ok(), "link kept");
        }
    }

    /// An isolated Reader profile never touches the account's Sync task, and
    /// still keeps its identity.
    #[test]
    fn windows_hook_in_an_isolated_profile_never_stops_sync() {
        let fixture = tempfile::tempdir().unwrap();
        let mut host = FakeHost::new(fixture.path());
        host.isolated = true;
        windows_hook(&host).unwrap();
        assert_eq!(
            host.events(),
            ["roots", "registrations", "purge", "leftovers"]
        );
        assert!(host.sync.join("config/cert.pem").exists());
        assert!(!host.state.join("settings.json").exists());
    }

    /// `mklink /J`: junctions need no privilege, unlike directory symlinks.
    #[cfg(windows)]
    fn junction(link: &Path, target: &Path) {
        let status = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "mklink /J {}", link.display());
        assert!(std::fs::symlink_metadata(link)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(windows)]
    fn is_link(path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
    }

    /// #3109 FIX FIRST: `%LOCALAPPDATA%` reached through a junction. The root
    /// and Sync's kept folders name the same NTFS folder through different
    /// spellings; the purge empties it around Sync either way.
    #[cfg(windows)]
    #[test]
    fn windows_junctioned_local_app_data_keeps_sync_identity() {
        let fixture = tempfile::tempdir().unwrap();
        let real = fixture.path().join("Local");
        let alias = fixture.path().join("LocalLink");
        let state = real.join("okilum");
        write(&state.join("Sync/config/cert.pem"), "certificate");
        write(&state.join("SyncDirect/direct.json"), "{}");
        junction(&alias, &real);
        let documents = fixture.path().join("Documents");
        for (root_base, keep_base) in [(&alias, &real), (&real, &alias)] {
            write(&state.join("settings.json"), "{}");
            write(&state.join("reader/index.bin"), "index");
            let root = root_base.join("okilum");
            let keep = vec![
                keep_base.join("Okilum").join("Sync"),
                keep_base.join("Okilum").join("SyncDirect"),
            ];
            let plan = Plan::keeping(std::slice::from_ref(&root), &keep).unwrap();
            let report = purge_planned(&plan, None, &documents, &documents);
            assert!(report.failed.is_empty(), "{report:?}");
            assert_eq!(report.kept_for_sync, vec![root.clone()], "{report:?}");
            assert!(!state.join("settings.json").exists(), "{report:?}");
            assert!(!state.join("reader").exists(), "{report:?}");
            assert_eq!(
                std::fs::read_to_string(state.join("Sync/config/cert.pem")).unwrap(),
                "certificate"
            );
            assert!(state.join("SyncDirect/direct.json").exists());
            assert!(is_link(&alias), "the junction itself stays");
        }
    }

    /// #3109 FIX FIRST: a state root that is itself a junction is never
    /// followed or emptied; an unrelated junctioned root loses only its link,
    /// and a junction inside an emptied root is removed as a link.
    #[cfg(windows)]
    #[test]
    fn windows_junctioned_roots_are_never_followed() {
        let fixture = tempfile::tempdir().unwrap();
        let outside = fixture.path().join("Elsewhere");
        write(&outside.join("sentinel.pdf"), "not notes");
        let local = fixture.path().join("Local");
        std::fs::create_dir_all(&local).unwrap();
        let documents = fixture.path().join("Documents");
        let state = local.join("okilum");
        junction(&state, &outside);
        let keep = vec![
            local.join("Okilum").join("Sync"),
            local.join("Okilum").join("SyncDirect"),
        ];
        let plan = Plan::keeping(std::slice::from_ref(&state), &keep).unwrap();
        let report = purge_planned(&plan, None, &documents, &documents);
        assert_eq!(report.kept_for_sync, vec![state.clone()]);
        assert!(report.removed.is_empty(), "{report:?}");
        assert!(outside.join("sentinel.pdf").exists(), "target untouched");
        assert!(is_link(&state));

        // Positive control: an unrelated junctioned root loses only its link.
        let cache = local.join("cache-okilum");
        junction(&cache, &outside);
        let plan = Plan::keeping(std::slice::from_ref(&cache), &keep).unwrap();
        let report = purge_planned(&plan, None, &documents, &documents);
        assert_eq!(report.removed, vec![cache.clone()], "{report:?}");
        assert!(std::fs::symlink_metadata(&cache).is_err());
        assert!(
            outside.join("sentinel.pdf").exists(),
            "link removed, target kept"
        );

        // A junction inside a root emptied around Sync goes as a link.
        let real = fixture.path().join("Local2").join("okilum");
        write(&real.join("Sync/journal.json"), "{}");
        junction(&real.join("link"), &outside);
        let keep = vec![real.join("Sync")];
        let plan = Plan::keeping(std::slice::from_ref(&real), &keep).unwrap();
        let report = purge_planned(&plan, None, &documents, &documents);
        assert!(report.failed.is_empty(), "{report:?}");
        assert!(std::fs::symlink_metadata(real.join("link")).is_err());
        assert!(outside.join("sentinel.pdf").exists(), "target untouched");
        assert!(real.join("Sync/journal.json").exists());
    }

    /// #3109 FIX FIRST: a dangling junction on a root's way or on a kept path
    /// cannot be resolved, so the plan refuses and nothing is removed.
    #[cfg(windows)]
    #[test]
    fn windows_dangling_junction_refuses_the_plan() {
        let fixture = tempfile::tempdir().unwrap();
        let local = fixture.path().join("Local");
        let state = local.join("okilum");
        write(&state.join("settings.json"), "{}");
        let gone = fixture.path().join("gone");
        std::fs::create_dir_all(&gone).unwrap();
        let broken = fixture.path().join("Broken");
        junction(&broken, &gone);
        std::fs::remove_dir(&gone).unwrap();
        let keep = vec![local.join("Okilum").join("Sync")];
        let roots = vec![state.clone(), broken.join("okilum")];
        assert!(Plan::keeping(&roots, &keep).is_err());
        std::fs::create_dir_all(&gone).unwrap();
        junction(&local.join("Okilum").join("Sync"), &gone);
        std::fs::remove_dir(&gone).unwrap();
        assert!(Plan::keeping(std::slice::from_ref(&state), &keep).is_err());
        assert!(
            state.join("settings.json").exists(),
            "planning changes nothing"
        );
    }

    /// #3112 review: a dangling junction as the final component of a root
    /// (`%APPDATA%\okilum`) refuses in the production hook before Sync is
    /// stopped; nothing is removed and the junction stays.
    #[cfg(windows)]
    #[test]
    fn windows_hook_refuses_a_dangling_junction_root() {
        let fixture = tempfile::tempdir().unwrap();
        let host = FakeHost::new(fixture.path());
        let config = host.roots[1].clone();
        std::fs::remove_dir_all(&config).unwrap();
        let gone = fixture.path().join("gone");
        std::fs::create_dir_all(&gone).unwrap();
        junction(&config, &gone);
        std::fs::remove_dir(&gone).unwrap();
        let error = windows_hook(&host).unwrap_err();
        assert!(format!("{error:#}").contains("cannot resolve"), "{error:#}");
        assert_eq!(host.events(), ["roots"]);
        assert!(host.state.join("settings.json").exists());
        assert!(host.state.join("Sync/config/cert.pem").exists());
        assert!(is_link(&config), "the junction stays");
    }

    /// #3109 FIX FIRST: 8.3 short names (CI temp is often `RUNNER~1`) and
    /// long names spell the same folder.
    #[cfg(windows)]
    #[test]
    fn windows_short_names_match_their_long_spelling() {
        use std::os::windows::process::CommandExt as _;
        let fixture = tempfile::tempdir().unwrap();
        let long = std::fs::canonicalize(fixture.path())
            .unwrap()
            .join("LongLocalAppData");
        let state = long.join("okilum");
        write(&state.join("Sync/config/cert.pem"), "certificate");
        let output = std::process::Command::new("cmd")
            .raw_arg(format!(
                "/C for %I in (\"{}\") do @echo %~sI",
                long.display().to_string().trim_start_matches(r"\\?\")
            ))
            .output()
            .unwrap();
        let short = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        // Where 8.3 names are disabled `short` is the long spelling.
        assert!(short.is_absolute() && short.exists(), "{output:?}");
        let documents = fixture.path().join("Documents");
        for (root_base, keep_base) in [(&short, &long), (&long, &short)] {
            write(&state.join("settings.json"), "{}");
            let root = root_base.join("okilum");
            let keep = vec![keep_base.join("Okilum").join("Sync")];
            let plan = Plan::keeping(std::slice::from_ref(&root), &keep).unwrap();
            let report = purge_planned(&plan, None, &documents, &documents);
            assert!(report.failed.is_empty(), "{report:?}");
            assert!(!state.join("settings.json").exists(), "{report:?}");
            assert_eq!(
                std::fs::read_to_string(state.join("Sync/config/cert.pem")).unwrap(),
                "certificate"
            );
        }
    }

    #[test]
    fn a_refused_sync_stop_removes_nothing() {
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state/okilum");
        write(&state.join("settings.json"), "{}");
        let fixture_roots = vec![state.clone()];
        let documents = state.parent().unwrap().join("Documents");
        let mut stopped = false;
        let outcome = stop_then_remove(
            || -> anyhow::Result<()> {
                anyhow::bail!("recovery pending: Sync helper is not answering")
            },
            |()| purge(&fixture_roots, Some(&state), &documents, &documents),
        );
        assert!(outcome.is_err());
        assert!(state.join("settings.json").exists(), "nothing was purged");
        // Positive control: once Sync is stopped the same purge runs.
        let report = stop_then_remove(
            || {
                stopped = true;
                Ok(())
            },
            |()| purge(&fixture_roots, Some(&state), &documents, &documents),
        )
        .unwrap();
        assert!(stopped);
        assert_eq!(report.removed, vec![state.clone()]);
        assert!(!state.exists());
    }

    #[test]
    fn purge_keeps_sync_identity_inside_a_config_root_that_shares_its_folder() {
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path().join("Application Support");
        // The config root as `roots()` names it on macOS.
        let config = base.join("okilum");
        write(&config.join("reader-layout.json"), "{}");
        write(&config.join("profiles/brain.json"), "{}");
        write(&config.join("Sync/journal.json"), "{}");
        write(&config.join("Sync/config/cert.pem"), "certificate");
        write(&config.join("SyncDirect/journal.json"), "{}");
        let documents = fixture.path().join("Documents");
        // Sync's own spelling; on a case-insensitive volume the same folder.
        let spelled = if IGNORE_CASE { "Okilum" } else { "okilum" };
        let keep = vec![
            base.join(spelled).join("Sync"),
            base.join(spelled).join("SyncDirect"),
        ];
        let plan = Plan::keeping(std::slice::from_ref(&config), &keep).unwrap();
        let report = purge_planned(&plan, None, &documents, &documents);
        assert!(!config.join("reader-layout.json").exists());
        assert!(!config.join("profiles").exists());
        assert_eq!(
            std::fs::read_to_string(config.join("Sync/config/cert.pem")).unwrap(),
            "certificate"
        );
        assert!(config.join("Sync/journal.json").exists());
        assert!(config.join("SyncDirect/journal.json").exists());
        assert_eq!(report.kept_for_sync, vec![config.clone()]);
        assert!(report.failed.is_empty(), "{report:?}");
        // Positive control: without a kept path the root goes as before.
        let report = purge(std::slice::from_ref(&config), None, &documents, &documents);
        assert_eq!(report.removed, vec![config.clone()]);
        assert!(!config.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_config_root_is_never_followed_or_emptied() {
        let fixture = tempfile::tempdir().unwrap();
        let outside = fixture.path().join("Elsewhere");
        write(&outside.join("sentinel.pdf"), "not notes");
        let base = fixture.path().join("Application Support");
        std::fs::create_dir_all(&base).unwrap();
        let config = base.join("okilum");
        std::os::unix::fs::symlink(&outside, &config).unwrap();
        // Sync never set up: the kept paths do not exist.
        let keep = vec![config.join("Sync"), config.join("SyncDirect")];
        let documents = fixture.path().join("Documents");
        let plan = Plan::keeping(std::slice::from_ref(&config), &keep).unwrap();
        let report = purge_planned(&plan, None, &documents, &documents);
        assert_eq!(report.kept_for_sync, vec![config.clone()]);
        assert!(report.removed.is_empty(), "{report:?}");
        assert!(outside.join("sentinel.pdf").exists(), "target untouched");
        assert!(std::fs::symlink_metadata(&config)
            .unwrap()
            .file_type()
            .is_symlink());
        // Positive control: an unrelated symlinked root loses only its link.
        let cache = base.join("cache-okilum");
        std::os::unix::fs::symlink(&outside, &cache).unwrap();
        let plan = Plan::keeping(std::slice::from_ref(&cache), &keep).unwrap();
        let report = purge_planned(&plan, None, &documents, &documents);
        assert_eq!(report.removed, vec![cache.clone()]);
        assert!(std::fs::symlink_metadata(&cache).is_err());
        assert!(
            outside.join("sentinel.pdf").exists(),
            "link removed, target kept"
        );
    }

    #[cfg(unix)]
    #[test]
    fn kept_paths_match_a_config_root_reached_through_a_symlinked_home() {
        let fixture = tempfile::tempdir().unwrap();
        let real = fixture.path().join("real-home");
        let alias = fixture.path().join("alias-home");
        let config = real.join("Application Support/okilum");
        write(&config.join("reader-layout.json"), "{}");
        write(&config.join("Sync/config/key.pem"), "key");
        write(&config.join("SyncDirect/journal.json"), "{}");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let documents = fixture.path().join("Documents");
        // Root through the alias, kept paths through the real home, and back.
        for (root_home, keep_home) in [(&alias, &real), (&real, &alias)] {
            let root = root_home.join("Application Support/okilum");
            write(&root.join("reader-layout.json"), "{}");
            let keep = vec![
                keep_home.join("Application Support/okilum/Sync"),
                keep_home.join("Application Support/okilum/SyncDirect"),
            ];
            let plan = Plan::keeping(std::slice::from_ref(&root), &keep).unwrap();
            let report = purge_planned(&plan, None, &documents, &documents);
            assert!(report.failed.is_empty(), "{report:?}");
            assert!(!config.join("reader-layout.json").exists());
            assert_eq!(
                std::fs::read_to_string(config.join("Sync/config/key.pem")).unwrap(),
                "key"
            );
            assert!(config.join("SyncDirect/journal.json").exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_kept_symlink_into_another_root_keeps_that_root() {
        let fixture = tempfile::tempdir().unwrap();
        let config = fixture.path().join("Application Support/okilum");
        write(&config.join("reader-layout.json"), "{}");
        // XDG_CONFIG_HOME's okilum is another root; Sync links into it.
        let xdg = fixture.path().join("xdg/okilum");
        write(&xdg.join("config/key.pem"), "key");
        std::os::unix::fs::symlink(&xdg, config.join("Sync")).unwrap();
        let keep = vec![config.join("Sync"), config.join("SyncDirect")];
        let roots = vec![config.clone(), xdg.clone()];
        let documents = fixture.path().join("Documents");
        let plan = Plan::keeping(&roots, &keep).unwrap();
        let report = purge_planned(&plan, None, &documents, &documents);
        assert!(report.failed.is_empty(), "{report:?}");
        assert!(!config.join("reader-layout.json").exists());
        assert!(std::fs::symlink_metadata(config.join("Sync"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_to_string(xdg.join("config/key.pem")).unwrap(),
            "key"
        );
        assert!(report.kept_for_sync.contains(&xdg), "{report:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_on_a_root_or_kept_path_refuses_the_plan() {
        let fixture = tempfile::tempdir().unwrap();
        let config = fixture.path().join("Application Support/okilum");
        write(&config.join("reader-layout.json"), "{}");
        let keep = vec![config.join("Sync")];
        // XDG_CONFIG_HOME is a dangling symlink: its okilum root is missing
        // but cannot be resolved either.
        let xdg = fixture.path().join("xdg");
        std::os::unix::fs::symlink(fixture.path().join("gone"), &xdg).unwrap();
        let roots = vec![config.clone(), xdg.join("okilum")];
        assert!(Plan::keeping(&roots, &keep).is_err());
        // A kept path that is a dangling symlink cannot be resolved either.
        std::os::unix::fs::symlink(fixture.path().join("gone"), config.join("Sync")).unwrap();
        assert!(Plan::keeping(std::slice::from_ref(&config), &keep).is_err());
        // Control: an ordinary missing root is fine.
        std::fs::remove_file(config.join("Sync")).unwrap();
        let missing = vec![config.clone(), fixture.path().join("absent/okilum")];
        assert!(Plan::keeping(&missing, &keep).is_ok());
        assert!(
            config.join("reader-layout.json").exists(),
            "planning changes nothing"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_config_root_is_reported_not_silently_kept() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let config = fixture.path().join("Application Support/okilum");
        write(&config.join("reader-layout.json"), "{}");
        write(&config.join("Sync/journal.json"), "{}");
        let keep = vec![config.join("Sync")];
        let documents = fixture.path().join("Documents");
        let plan = Plan::keeping(std::slice::from_ref(&config), &keep).unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = std::fs::read_dir(&config).is_err();
        let report = purge_planned(&plan, None, &documents, &documents);
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o700)).unwrap();
        // A privileged runner can read anything; the report needs a real denial.
        if unreadable {
            assert!(
                report
                    .failed
                    .iter()
                    .any(|(path, _)| path.ends_with("okilum")),
                "{report:?}"
            );
            assert!(config.join("reader-layout.json").exists(), "fails safe");
        }
        assert!(config.join("Sync/journal.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn an_unresolvable_kept_path_refuses_the_plan() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let locked = fixture.path().join("locked");
        std::fs::create_dir_all(locked.join("inner")).unwrap();
        let config = fixture.path().join("okilum");
        write(&config.join("reader-layout.json"), "{}");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = std::fs::read_dir(locked.join("inner")).is_err();
        let planned = Plan::keeping(
            std::slice::from_ref(&config),
            &[locked.join("inner/Okilum/Sync")],
        );
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        // A privileged runner can read anything; the refusal needs a real denial.
        if unreadable {
            assert!(planned.is_err(), "{planned:?}");
        }
        assert!(Plan::keeping(
            std::slice::from_ref(&config),
            &[PathBuf::from("relative/Sync")]
        )
        .is_err());
        assert!(
            config.join("reader-layout.json").exists(),
            "planning changes nothing"
        );
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
        // #1284: the quit line written after the purge.
        let state = lib.join("Application Support/com.befeast.okilum");
        write(
            &state.join("reader-diagnostic.log"),
            "{\"phase\":\"app_quit\"}\n",
        );
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", MACOS_CLEANUP, "okilum-uninstall"])
            .arg(&app)
            .arg(pid.to_string())
            .arg(&state)
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
        assert!(!state.exists(), "the quit log and its now empty folder");
        assert!(!app.exists(), "the app moved to the Trash");
        let trash: Vec<_> = std::fs::read_dir(home.join(".Trash"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(trash.len(), 1);
        assert!(trash[0].path().join("Contents/Info.plist").exists());

        // A state folder holding anything besides the log keeps it (a vault
        // there is never touched); only the log goes.
        let app = fixture.path().join("Applications/Okilum.app");
        write(&app.join("Contents/Info.plist"), "plist");
        write(&state.join("reader-diagnostic.log"), "x");
        write(&state.join("Notes/note.md"), "keep");
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", MACOS_CLEANUP, "okilum-uninstall"])
            .arg(&app)
            .arg(pid.to_string())
            .arg(&state)
            .env("HOME", &home)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .status()
            .unwrap();
        assert!(status.success());
        assert!(!state.join("reader-diagnostic.log").exists());
        assert_eq!(
            std::fs::read_to_string(state.join("Notes/note.md")).unwrap(),
            "keep"
        );

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
        ("crates/okilum-shell/src/linux_sprint.rs", 5),
        // Explicit macOS Sync QA validates HOME inside its caller-owned profile;
        // it adds no account root. See the isolated QA inventory in uninstall.md.
        ("crates/okilum-shell/src/main.rs", 1),
        // Roots, crash dumps, the Velopack log folder, the Linux sync units,
        // the drafts export and the macOS home (all in the inventory).
        ("crates/okilum-shell/src/app_footprint.rs", 11),
        // The macOS terminal command is removed separately as an owned file.
        ("crates/okilum-shell-kit/src/macos_cli/mod.rs", 1),
        // Suggests ~/Downloads in the export dialog: the user picks the target.
        ("crates/okilum-shell/src/brain/context_ui.rs", 1),
        // Brain outboxes and profile: all under the `~/.config/okilum` root.
        ("crates/okilum-shell/src/brain/discussion_send_outbox.rs", 2),
        ("crates/okilum-shell/src/brain/editor_recovery.rs", 2),
        ("crates/okilum-shell/src/brain/native_outbox.rs", 2),
        ("crates/okilum-shell/src/brain/t3_route_outbox.rs", 2),
        ("crates/okilum-shell/src/brain.rs", 2),
        // Looks for installed editors in ~/Applications; reads only.
        ("crates/okilum-shell-platform/src/open_in.rs", 1),
        // Linux editor icons (#1400): reads desktop entries, icon themes and
        // the GTK/KDE icon-theme setting; writes nothing.
        (
            "crates/okilum-shell-platform/src/open_in/icons/desktop.rs",
            3,
        ),
        ("crates/okilum-shell-kit/src/reader_history.rs", 4),
        ("crates/okilum-shell-kit/src/reader_layout.rs", 3),
        // Two more: Create a new vault suggests ~/Documents in the save
        // dialog (#1357); the user picks the folder, it is theirs.
        ("crates/okilum-shell/src/reader_open.rs", 6),
        // Sync lives under the state root; its systemd unit is removed by
        // the sync controller (Linux part of #974).
        // Managed Sync units share a systemd base. Uninstall checks the same
        // producer path and refuses until the controller has removed its unit.
        (
            "crates/okilum-shell/src/reader_settings_sync/linux_runtime.rs",
            2,
        ),
        // Records the managed AppImage build beside
        // ~/.local/share/okilum/Okilum.AppImage, removed by installation::remove.
        ("crates/okilum-shell/src/updater/appimage.rs", 1),
        // Moves notes to the system trash; not an app-owned location.
        ("crates/okilum-shell-platform/src/reader_trash_fs.rs", 2),
        // Reads the home folder only to suggest ~/Documents when a new untitled
        // file is saved (the user picks the folder) and to shorten paths to «~»
        // in Recent (#1528); drafts live under the state root.
        ("crates/okilum-shell/src/reader_untitled.rs", 2),
        ("crates/okilum-shell/src/workspace.rs", 3),
    ];

    /// Production code only: drop each `#[cfg(test)] mod … { … }` block, which
    /// may sit anywhere in a file. Braces inside string, raw string and char
    /// literals are skipped so `format!("{x}")` or `r#"{"a":1}"#` does not
    /// unbalance the match.
    fn strip_test_modules(text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = String::new();
        let mut start = 0;
        while let Some(found) = text
            .get(start..)
            .and_then(|rest| rest.find("#[cfg(test)]\nmod "))
        {
            let module = start + found;
            out.push_str(&text[start..module]);
            // `#[cfg(test)] mod name;` loads a file and has no body here:
            // skip the declaration alone, not code up to the next `{`.
            let declaration = text[module..].find(';').map(|i| module + i);
            let open = text[module..].find('{').map(|i| module + i);
            if let Some(end) = declaration.filter(|&end| open.is_none_or(|open| end < open)) {
                start = end + 1;
                continue;
            }
            let Some(open) = open else {
                return out;
            };
            let (mut depth, mut at) = (0usize, open);
            while at < bytes.len() {
                match bytes[at] {
                    // r"…", r#"…"#: no escapes; ends at a quote and as many #.
                    b'r' if at == 0
                        || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_') =>
                    {
                        let hashes = bytes[at + 1..].iter().take_while(|&&b| b == b'#').count();
                        if bytes.get(at + 1 + hashes) == Some(&b'"') {
                            let close: Vec<u8> = std::iter::once(b'"')
                                .chain(std::iter::repeat_n(b'#', hashes))
                                .collect();
                            let body = at + 2 + hashes;
                            at = bytes[body..]
                                .windows(close.len())
                                .position(|w| w == close.as_slice())
                                .map_or(bytes.len(), |i| body + i + close.len() - 1);
                        }
                    }
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
    fn strip_test_modules_skips_braces_and_quotes_in_raw_strings() {
        let text = "a dirs::x\n#[cfg(test)]\nmod tests {\n const J: &str = r#\"{\"z\":1,\"a\":{}}\"#;\n const R: &str = r\"}\";\n fn f() { dirs::y }\n}\nb dirs::z\n";
        let production = strip_test_modules(text);
        assert_eq!(production.matches("dirs::").count(), 2, "{production}");
        assert!(production.contains("b dirs::z"), "{production}");
        // An unterminated module at the end must not slice out of bounds.
        let open = "x\n#[cfg(test)]\nmod t { r#\"";
        assert_eq!(strip_test_modules(open), "x\n");
    }

    #[test]
    fn strip_test_modules_skips_only_an_out_of_line_test_module_declaration() {
        // main.rs declares `#[cfg(test)] mod x_tests;` (file module) before
        // production code; that code must still be scanned.
        let text = "#[cfg(test)]\nmod conformance_tests;\nmod a;\nfn f() { dirs::x }\n#[cfg(test)]\nmod t {\n fn g() { dirs::y }\n}\nfn h() { dirs::z }\n";
        let production = strip_test_modules(text);
        assert_eq!(production.matches("dirs::").count(), 2, "{production}");
        assert!(!production.contains("conformance_tests"), "{production}");
        assert!(production.contains("dirs::x") && production.contains("dirs::z"));
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

    #[cfg(target_os = "linux")]
    #[test]
    fn managed_appimage_uninstall_removes_installation_and_update_artifacts() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let download = home.path().join("download.AppImage");
        std::fs::write(&download, b"not executed").unwrap();
        std::fs::set_permissions(&download, std::fs::Permissions::from_mode(0o755)).unwrap();
        let image = okilum_appimage::installation::install(home.path(), &download, 7547).unwrap();
        let (_, command) = okilum_appimage::installation::paths(home.path()).unwrap();
        let (desktop, icon) = okilum_appimage::desktop::paths(home.path()).unwrap();
        let stage = image.parent().unwrap().join(".okilum-update-completed");
        std::fs::create_dir(&stage).unwrap();
        std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(stage.join("previous.AppImage"), b"rollback").unwrap();
        assert_eq!(
            desktop_payload(home.path()),
            vec![
                desktop.clone(),
                icon.clone(),
                image.clone(),
                command.clone()
            ]
        );
        let mut report = Report::default();
        remove_appimage_desktop(home.path(), &mut report);
        assert_eq!(report.removed, vec![desktop, icon, image.clone(), command]);
        assert!(report.failed.is_empty());
        assert!(!image.parent().unwrap().exists());
        assert!(download.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn appimage_desktop_lifecycle_reports_exact_owned_files_and_keeps_foreign_payloads() {
        use std::os::unix::fs::PermissionsExt;
        let sandbox = tempfile::tempdir().unwrap();
        let home = sandbox.path().join("isolated-home");
        let image = sandbox.path().join("first image.AppImage");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(&image, b"not executed").unwrap();
        std::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (desktop, icon) = okilum_appimage::desktop::paths(&home).unwrap();
        let peer = desktop.parent().unwrap().join("okilum.desktop");
        std::fs::create_dir_all(peer.parent().unwrap()).unwrap();
        std::fs::write(&peer, b"distribution launcher").unwrap();
        okilum_appimage::desktop::install(&home, &image).unwrap();
        let initial = std::fs::read(&desktop).unwrap();
        okilum_appimage::desktop::install(&home, &image).unwrap();
        assert_eq!(std::fs::read(&desktop).unwrap(), initial);
        assert_eq!(desktop_payload(&home), vec![desktop.clone(), icon.clone()]);
        let moved = sandbox.path().join("updated image.AppImage");
        std::fs::rename(&image, &moved).unwrap();
        okilum_appimage::desktop::install(&home, &moved).unwrap();
        let entry = std::fs::read_to_string(&desktop).unwrap();
        assert!(entry.contains("updated image.AppImage"));
        assert!(!entry.contains("first image.AppImage"));
        let mut report = Report::default();
        remove_appimage_desktop(&home, &mut report);
        assert_eq!(report.removed, vec![desktop.clone(), icon.clone()]);
        assert!(report.failed.is_empty());
        assert_eq!(std::fs::read(&peer).unwrap(), b"distribution launcher");
        let mut repeated = Report::default();
        remove_appimage_desktop(&home, &mut repeated);
        assert_eq!(repeated, Report::default());
        std::fs::write(&desktop, b"foreign launcher").unwrap();
        std::fs::write(&icon, b"foreign icon").unwrap();
        assert!(okilum_appimage::desktop::install(&home, &moved).is_err());
        let mut refused = Report::default();
        remove_appimage_desktop(&home, &mut refused);
        assert!(refused.removed.is_empty());
        assert_eq!(refused.failed.len(), 2);
        assert_eq!(std::fs::read(&desktop).unwrap(), b"foreign launcher");
        assert_eq!(std::fs::read(&icon).unwrap(), b"foreign icon");
    }

    #[test]
    fn every_base_directory_in_production_code_is_in_the_uninstall_inventory() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut found = Vec::new();
        // okilum-shell is being split into okilum-shell-* crates (#3241): scan
        // every one of them, so moving a module cannot hide a base directory.
        let mut crate_dirs = vec![root.join("crates/okilum-core/src")];
        for entry in std::fs::read_dir(root.join("crates")).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "okilum-shell" || name.starts_with("okilum-shell-") {
                crate_dirs.push(entry.path().join("src"));
            }
        }
        assert!(
            crate_dirs.len() >= 3,
            "okilum-shell and okilum-shell-kit are scanned"
        );
        for crate_dir in crate_dirs {
            let mut stack = vec![crate_dir];
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
