//! First-launch import of Tessera app state into Okilum locations (#967).
//!
//! Copy-first: legacy directories are only read, never modified, and remain a
//! backup. A root whose new location does not exist yet is copied into a
//! sibling staging directory, verified file by file and renamed into place in
//! one step, so an interrupted import leaves either nothing or a complete root.
//! A new root that already holds data is merged without overwriting: an old
//! file that differs is kept under `okilum-import-conflicts/` and reported. A
//! completion marker makes every later run a no-op. Caches are rebuilt, not
//! imported.
//!
//! The import refuses to run while a legacy Tessera instance holds its
//! instance lock, and two imports cannot run at once.
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Written into each new root last; its presence means the root is imported.
pub const MARKER: &str = "okilum-import.json";
/// Old files that differ from an existing new file are kept here, under the new root.
pub const CONFLICTS_DIR: &str = "okilum-import-conflicts";
/// The legacy single-instance lock, held by a running Tessera Reader.
pub const LEGACY_INSTANCE_LOCK: &str = "reader-instance.lock";
const STAGING_PREFIX: &str = ".okilum-import-";

/// Which product a build identifies as. The rename flips this (#970); the
/// import runs only in an Okilum build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Brand {
    Tessera,
    Okilum,
}

pub const APP_BRAND: Brand = Brand::Tessera;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Linux,
    Windows,
}

impl Os {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Linux
        }
    }
}

/// The inputs that decide where app state lives. Captured once, so tests and
/// native checks can describe any machine.
#[derive(Clone, Debug, Default)]
pub struct Environment {
    pub os: Option<Os>,
    pub home: Option<PathBuf>,
    pub xdg_state_home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    /// Windows `%LOCALAPPDATA%`.
    pub local_app_data: Option<PathBuf>,
    /// Windows `%APPDATA%`.
    pub roaming_app_data: Option<PathBuf>,
    /// An explicit `OKILUM_STATE_DIR` / `TESSERA_STATE_DIR` isolated state.
    pub explicit_state_dir: Option<PathBuf>,
}

fn absolute(value: Option<OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|path| path.is_absolute())
}

/// `OKILUM_*` wins; the legacy `TESSERA_*` name is still honoured when the new
/// one is unset, so existing launch scripts keep working after the rename.
pub fn env_var(new: &str, legacy: &str) -> Option<OsString> {
    std::env::var_os(new).or_else(|| std::env::var_os(legacy))
}

impl Environment {
    pub fn current() -> Self {
        Environment {
            os: Some(Os::current()),
            // `HOME` only, as the legacy helpers read it (not USERPROFILE).
            home: absolute(std::env::var_os("HOME")),
            xdg_state_home: absolute(std::env::var_os("XDG_STATE_HOME")),
            xdg_config_home: absolute(std::env::var_os("XDG_CONFIG_HOME")),
            xdg_data_home: absolute(std::env::var_os("XDG_DATA_HOME")),
            local_app_data: absolute(std::env::var_os("LOCALAPPDATA")),
            roaming_app_data: absolute(std::env::var_os("APPDATA")),
            explicit_state_dir: absolute(env_var("OKILUM_STATE_DIR", "TESSERA_STATE_DIR")),
        }
    }
}

/// One legacy directory and the directory that replaces it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    /// Stable label for reports: `state`, `config` or `brain`.
    pub kind: &'static str,
    pub old: PathBuf,
    pub new: PathBuf,
}

/// Legacy and new app-state roots for this machine, in import order. Caches
/// are excluded (they are rebuilt). An explicit state-dir override is an
/// isolated, caller-chosen location and is never migrated.
pub fn roots(env: &Environment) -> Vec<Root> {
    let mut roots = Vec::new();
    let mut push = |kind: &'static str, base: Option<&PathBuf>, old: &str, new: &str| {
        if let Some(base) = base {
            let root = Root {
                kind,
                old: base.join(old),
                new: base.join(new),
            };
            if !roots.iter().any(|r: &Root| r.old == root.old) {
                roots.push(root);
            }
        }
    };
    let home = env.home.as_ref();
    let dot_config = env
        .xdg_config_home
        .clone()
        .or_else(|| home.map(|h| h.join(".config")));
    match env.os.unwrap_or_else(Os::current) {
        Os::MacOs => {
            let support = home.map(|h| h.join("Library/Application Support"));
            if env.explicit_state_dir.is_none() {
                push(
                    "state",
                    support.as_ref(),
                    "uk.oklabs.tessera",
                    "com.befeast.okilum",
                );
            }
            // Reader settings use XDG_CONFIG_HOME when set, else Application Support.
            let config = env.xdg_config_home.clone().or(support);
            push("config", config.as_ref(), "tessera", "okilum");
            // Brain workspace, recovery and outboxes use ~/.config even on macOS.
            push("brain", dot_config.as_ref(), "tessera", "okilum");
        }
        Os::Linux => {
            let state = env
                .xdg_state_home
                .clone()
                .or_else(|| home.map(|h| h.join(".local/state")));
            if env.explicit_state_dir.is_none() {
                push("state", state.as_ref(), "tessera", "okilum");
            }
            push("config", dot_config.as_ref(), "tessera", "okilum");
            let data = env
                .xdg_data_home
                .clone()
                .or_else(|| home.map(|h| h.join(".local/share")));
            push("data", data.as_ref(), "tessera", "okilum");
        }
        Os::Windows => {
            if env.explicit_state_dir.is_none() {
                push("state", env.local_app_data.as_ref(), "tessera", "okilum");
            }
            push("config", env.roaming_app_data.as_ref(), "tessera", "okilum");
            // Brain stores are Unix-only (`cfg(all(unix, feature = "brain"))`).
        }
    }
    roots
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedFile {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    kind: String,
    from: PathBuf,
    files: Vec<ImportedFile>,
    conflicts: Vec<PathBuf>,
    skipped: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The marker shows this root was imported earlier; nothing changed.
    AlreadyImported,
    /// No legacy directory exists for this root.
    NoLegacyData,
    /// The new root did not exist; the whole legacy root was copied.
    Copied { files: usize, bytes: u64 },
    /// The new root already held data; missing files were added, differing
    /// legacy files were kept under [`CONFLICTS_DIR`].
    Merged {
        copied: usize,
        conflicts: Vec<PathBuf>,
    },
}

#[derive(Debug)]
pub struct Report {
    pub roots: Vec<(Root, Outcome)>,
}

#[derive(Debug)]
pub enum ImportError {
    /// A legacy Tessera instance holds this lock; quit it first.
    LegacyRunning(PathBuf),
    /// Another import holds the import lock.
    Busy(PathBuf),
    Io {
        path: PathBuf,
        error: io::Error,
    },
    /// A copied file did not read back with the source contents.
    Verification(PathBuf),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::LegacyRunning(lock) => write!(
                f,
                "Tessera is still running ({}). Quit Tessera, then open Okilum again.",
                lock.display()
            ),
            ImportError::Busy(lock) => {
                write!(f, "Another import is in progress ({})", lock.display())
            }
            ImportError::Io { path, error } => write!(f, "{}: {error}", path.display()),
            ImportError::Verification(path) => {
                write!(f, "{} did not copy exactly", path.display())
            }
        }
    }
}

impl std::error::Error for ImportError {}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> ImportError + '_ {
    move |error| ImportError::Io {
        path: path.to_path_buf(),
        error,
    }
}

/// Test and native-check hooks; production passes `Options::default()`.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Fail after copying this many files, as an interrupted import would.
    pub fail_after: Option<usize>,
}

/// True when a legacy Tessera holds its instance lock in `state_dir`, or a
/// live run still holds its `reader-runs/*.active` marker. Never creates a
/// file: the legacy directory is read-only to the import.
pub fn legacy_instance_running(state_dir: &Path) -> bool {
    let held = |path: &Path| {
        File::open(path)
            .is_ok_and(|file| matches!(file.try_lock(), Err(fs::TryLockError::WouldBlock)))
    };
    held(&state_dir.join(LEGACY_INSTANCE_LOCK))
        || fs::read_dir(state_dir.join("reader-runs")).is_ok_and(|entries| {
            entries.flatten().any(|entry| {
                entry.path().extension().is_some_and(|e| e == "active") && held(&entry.path())
            })
        })
}

/// Import every root. `lock` serialises imports; it lives next to, not inside,
/// the new roots. Stops at the first error; completed roots stay completed.
pub fn import(roots: &[Root], lock: &Path, options: &Options) -> Result<Report, ImportError> {
    for root in roots.iter().filter(|r| r.kind == "state") {
        if legacy_instance_running(&root.old) {
            return Err(ImportError::LegacyRunning(
                root.old.join(LEGACY_INSTANCE_LOCK),
            ));
        }
    }
    if let Some(parent) = lock.parent() {
        fs::create_dir_all(parent).map_err(io_at(parent))?;
    }
    let guard = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock)
        .map_err(io_at(lock))?;
    match guard.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => return Err(ImportError::Busy(lock.to_path_buf())),
        Err(fs::TryLockError::Error(error)) => {
            return Err(ImportError::Io {
                path: lock.to_path_buf(),
                error,
            })
        }
    }
    let mut budget = options.fail_after;
    let mut report = Report { roots: Vec::new() };
    for root in roots {
        let outcome = import_root(root, &mut budget)?;
        report.roots.push((root.clone(), outcome));
    }
    Ok(report)
}

/// The import lock lives beside the new state root (or the first root when
/// state is isolated), never inside a root that may be renamed into place.
pub fn lock_path(roots: &[Root]) -> Option<PathBuf> {
    let anchor = roots.iter().find(|r| r.kind == "state").or(roots.first())?;
    Some(anchor.new.parent()?.join("okilum-import.lock"))
}

/// What an Okilum build runs once at startup, before it takes its own
/// instance lock: import every legacy root for this machine.
pub fn first_launch(env: &Environment) -> Result<Report, ImportError> {
    let roots = roots(env);
    let Some(lock) = lock_path(&roots) else {
        return Ok(Report { roots: Vec::new() });
    };
    import(&roots, &lock, &Options::default())
}

fn import_root(root: &Root, budget: &mut Option<usize>) -> Result<Outcome, ImportError> {
    let marker = root.new.join(MARKER);
    if marker.is_file() {
        return Ok(Outcome::AlreadyImported);
    }
    if !root.old.is_dir() {
        return Ok(Outcome::NoLegacyData);
    }
    let parent = root.new.parent().ok_or_else(|| ImportError::Io {
        path: root.new.clone(),
        error: io::Error::other("new root has no parent"),
    })?;
    fs::create_dir_all(parent).map_err(io_at(parent))?;
    discard_stale_staging(root)?;
    if !root.new.exists() {
        return copy_fresh(root, parent, budget);
    }
    merge(root, budget)
}

fn staging_name(root: &Root) -> String {
    let name = root
        .new
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!("{STAGING_PREFIX}{name}-")
}

/// An earlier run that stopped before its final rename left only staging.
fn discard_stale_staging(root: &Root) -> Result<(), ImportError> {
    let Some(parent) = root.new.parent() else {
        return Ok(());
    };
    let prefix = staging_name(root);
    for entry in fs::read_dir(parent).map_err(io_at(parent))? {
        let entry = entry.map_err(io_at(parent))?;
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            fs::remove_dir_all(entry.path()).map_err(io_at(&entry.path()))?;
        }
    }
    Ok(())
}

fn copy_fresh(
    root: &Root,
    parent: &Path,
    budget: &mut Option<usize>,
) -> Result<Outcome, ImportError> {
    let staging = parent.join(format!("{}{}", staging_name(root), uuid::Uuid::new_v4()));
    fs::create_dir(&staging).map_err(io_at(&staging))?;
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    for (relative, source) in legacy_files(&root.old, &mut skipped)? {
        spend(budget, &source)?;
        files.push(copy_verified(&source, &staging.join(&relative), &relative)?);
    }
    let bytes = files.iter().map(|f| f.bytes).sum();
    let count = files.len();
    write_marker(&staging, root, files, Vec::new(), skipped)?;
    fs::rename(&staging, &root.new).map_err(io_at(&root.new))?;
    Ok(Outcome::Copied {
        files: count,
        bytes,
    })
}

fn merge(root: &Root, budget: &mut Option<usize>) -> Result<Outcome, ImportError> {
    let mut files = Vec::new();
    let mut conflicts = Vec::new();
    let mut skipped = Vec::new();
    for (relative, source) in legacy_files(&root.old, &mut skipped)? {
        let target = root.new.join(&relative);
        let source_hash = sha256_of(&source)?;
        if target.is_file() {
            if sha256_of(&target)? == source_hash {
                continue;
            }
            let kept = root.new.join(CONFLICTS_DIR).join(&relative);
            if !(kept.is_file() && sha256_of(&kept)? == source_hash) {
                spend(budget, &source)?;
                copy_verified(&source, &kept, &relative)?;
            }
            conflicts.push(relative);
            continue;
        }
        if target.exists() {
            // A directory or special file where the legacy root has a file.
            conflicts.push(relative);
            continue;
        }
        spend(budget, &source)?;
        files.push(copy_verified(&source, &target, &relative)?);
    }
    let copied = files.len();
    write_marker(&root.new, root, files, conflicts.clone(), skipped)?;
    Ok(Outcome::Merged { copied, conflicts })
}

fn spend(budget: &mut Option<usize>, source: &Path) -> Result<(), ImportError> {
    if let Some(left) = budget {
        if *left == 0 {
            return Err(ImportError::Io {
                path: source.to_path_buf(),
                error: io::Error::other("interrupted (fault injection)"),
            });
        }
        *left -= 1;
    }
    Ok(())
}

/// Files that a running or crashed app owns transiently: never imported.
fn transient(name: &str) -> bool {
    name == "reader-instance.json"
        || name.ends_with(".lock")
        || name.ends_with(".pending")
        || name.ends_with(".sock")
        || name.ends_with(".tmp")
        || name.starts_with(".ledger-")
        || name.starts_with(STAGING_PREFIX)
        || name == MARKER
}

/// Regular files under `old`, relative paths sorted. Symlinks, sockets and
/// transient files are skipped and listed in `skipped`.
fn legacy_files(
    old: &Path,
    skipped: &mut Vec<PathBuf>,
) -> Result<Vec<(PathBuf, PathBuf)>, ImportError> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(old)
        .follow_links(false)
        .sort_by_file_name()
    {
        let entry = entry.map_err(|error| ImportError::Io {
            path: old.to_path_buf(),
            error: error.into(),
        })?;
        if entry.depth() == 0 {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(old)
            .unwrap_or(entry.path())
            .to_path_buf();
        let file_type = entry.file_type();
        if file_type.is_dir() {
            if entry.depth() == 1 && entry.file_name() == CONFLICTS_DIR {
                skipped.push(relative);
            }
            continue;
        }
        if !file_type.is_file() || transient(&entry.file_name().to_string_lossy()) {
            skipped.push(relative);
            continue;
        }
        files.push((relative, entry.into_path()));
    }
    Ok(files)
}

fn sha256_of(path: &Path) -> Result<String, ImportError> {
    let mut file = File::open(path).map_err(io_at(path))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(io_at(path))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Copy through a temporary sibling, fsync, verify, then rename into place, so
/// a target is either absent or complete. Keeps the source's modified time.
fn copy_verified(
    source: &Path,
    target: &Path,
    relative: &Path,
) -> Result<ImportedFile, ImportError> {
    let parent = target.parent().ok_or_else(|| ImportError::Io {
        path: target.to_path_buf(),
        error: io::Error::other("target has no parent"),
    })?;
    fs::create_dir_all(parent).map_err(io_at(parent))?;
    let temporary = parent.join(format!(".okilum-copy-{}.tmp", uuid::Uuid::new_v4()));
    let bytes = fs::copy(source, &temporary).map_err(io_at(source))?;
    let sync = File::options()
        .write(true)
        .open(&temporary)
        .map_err(io_at(&temporary))?;
    if let Ok(modified) = fs::metadata(source).and_then(|m| m.modified()) {
        let _ = sync.set_modified(modified);
    }
    sync.sync_all().map_err(io_at(&temporary))?;
    drop(sync);
    let expected = sha256_of(source)?;
    if sha256_of(&temporary)? != expected {
        let _ = fs::remove_file(&temporary);
        return Err(ImportError::Verification(source.to_path_buf()));
    }
    fs::rename(&temporary, target).map_err(io_at(target))?;
    Ok(ImportedFile {
        path: relative.to_path_buf(),
        bytes,
        sha256: expected,
    })
}

fn write_marker(
    directory: &Path,
    root: &Root,
    files: Vec<ImportedFile>,
    conflicts: Vec<PathBuf>,
    skipped: Vec<PathBuf>,
) -> Result<(), ImportError> {
    let manifest = Manifest {
        version: 1,
        kind: root.kind.to_owned(),
        from: root.old.clone(),
        files,
        conflicts,
        skipped,
    };
    let path = directory.join(MARKER);
    let temporary = directory.join(format!(".okilum-marker-{}.tmp", uuid::Uuid::new_v4()));
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| ImportError::Io {
        path: path.clone(),
        error: error.into(),
    })?;
    fs::write(&temporary, bytes).map_err(io_at(&temporary))?;
    File::open(&temporary)
        .and_then(|f| f.sync_all())
        .map_err(io_at(&temporary))?;
    fs::rename(&temporary, &path).map_err(io_at(&path))?;
    Ok(())
}

#[cfg(test)]
mod tests;
