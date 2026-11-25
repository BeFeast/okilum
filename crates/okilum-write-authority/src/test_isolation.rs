//! Per-process isolation of the account-wide writer state for test binaries.
//!
//! `account_directory()` is one account-wide location by design, so any test
//! that opens an editor or writes a note registers a vault in the *real*
//! per-user state directory. On pool hosts that leaked about 13,000 entries
//! (#3271); every admission scans all of them under one lock, so unrelated
//! timing tests went from milliseconds past their budget.
//!
//! The `test-isolation` feature, enabled only through `[dev-dependencies]`,
//! installs a constructor that runs before `main` of every test binary linking
//! this crate. It gives the process a private root and points the account state
//! variable and the temp directory into it. The root is deleted when the process
//! exits, so no test state and no stray temp directory outlives the run (#3271).
//! Child processes inherit the root and share it instead of making their own.
//! Release builds never enable the feature.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

/// Names the private root; set by the first process, inherited by its children.
pub const ROOT_VARIABLE: &str = "OKILUM_TEST_ISOLATION_ROOT";

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether this process (or the test process that spawned it) is isolated.
pub fn active() -> bool {
    ensure_linked();
    ACTIVE.load(Ordering::SeqCst)
}

/// Called from `account_directory`: it keeps this module, and the constructor in
/// it, in every binary that can reach the account state.
pub(crate) fn ensure_linked() {
    std::hint::black_box(&ACTIVE);
}

/// The environment variable `account_directory` reads on this platform.
fn state_variable() -> &'static str {
    if cfg!(target_os = "macos") {
        "HOME"
    } else if cfg!(windows) {
        "LOCALAPPDATA"
    } else {
        "XDG_STATE_HOME"
    }
}

#[ctor::ctor(unsafe)]
fn isolate_test_process() {
    if std::env::var_os(ROOT_VARIABLE).is_some() {
        ACTIVE.store(true, Ordering::SeqCst);
        return;
    }
    // A short name: sockets created under a temp directory have a path limit.
    let root = std::env::temp_dir().join(format!("ok-t{}", std::process::id()));
    // A stale root can only belong to an earlier process that had this pid.
    let _ = remove_tree(&root);
    create_private(&root)
        .unwrap_or_else(|error| panic!("test isolation root {}: {error}", root.display()));
    let state = root.join("state");
    create_private(&state)
        .unwrap_or_else(|error| panic!("test isolation state {}: {error}", state.display()));
    std::env::set_var(ROOT_VARIABLE, &root);
    std::env::set_var(state_variable(), &state);
    std::env::set_var("TMPDIR", &root);
    if cfg!(windows) {
        std::env::set_var("TEMP", &root);
        std::env::set_var("TMP", &root);
    }
    #[cfg(unix)]
    unix::remove_at_exit(root);
    ACTIVE.store(true, Ordering::SeqCst);
}

fn create_private(path: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// Some tests leave read-only directories behind; make them removable first.
fn remove_tree(root: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    unix::make_writable(root);
    match fs::symlink_metadata(root) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(root),
        Ok(_) => fs::remove_file(root),
        Err(_) => Ok(()),
    }
}

#[cfg(unix)]
mod unix {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        sync::OnceLock,
    };

    static ROOT: OnceLock<(u32, PathBuf)> = OnceLock::new();

    extern "C" {
        fn atexit(callback: extern "C" fn()) -> i32;
    }

    extern "C" fn cleanup() {
        // A forked copy of this process must not delete its parent's root.
        if let Some((pid, root)) = ROOT.get() {
            if *pid == std::process::id() {
                let _ = super::remove_tree(root);
            }
        }
    }

    pub(super) fn remove_at_exit(root: PathBuf) {
        if ROOT.set((std::process::id(), root)).is_ok() {
            // Failure only means the root is left for the pool's own cleanup.
            let _ = unsafe { atexit(cleanup) };
        }
    }

    pub(super) fn make_writable(dir: &Path) {
        let Ok(meta) = fs::symlink_metadata(dir) else {
            return;
        };
        if !meta.is_dir() {
            return;
        }
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                make_writable(&entry.path());
            }
        }
    }
}

/// The private root of this test process, when isolated.
pub fn root() -> Option<PathBuf> {
    std::env::var_os(ROOT_VARIABLE).map(PathBuf::from)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::{account_directory, Authority};

    #[test]
    fn test_process_never_touches_the_real_writer_state() {
        assert!(active(), "the constructor did not run in this test binary");
        let root = root().unwrap();
        let account = account_directory().unwrap();
        assert!(
            account.starts_with(&root),
            "{} is outside the test root {}",
            account.display(),
            root.display()
        );
        assert!(std::env::temp_dir().starts_with(&root));
        // The real directory is what an un-isolated process would have used.
        let real = std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(".local/state/okilum/local-writers-v1"));
        let count = |dir: &Option<PathBuf>| {
            dir.as_ref()
                .and_then(|dir| fs::read_dir(dir).ok())
                .map_or(0, Iterator::count)
        };
        let before = count(&real);
        let vault = tempfile::tempdir().unwrap();
        Authority::for_root(vault.path()).unwrap();
        assert_eq!(count(&real), before, "a test leaked into the real state");
        assert!(account.read_dir().unwrap().count() > 0);
    }

    #[test]
    fn exiting_process_removes_its_root_and_every_temp_directory_in_it() {
        const CHILD: &str = "OKILUM_TEST_ISOLATION_CHILD";
        if std::env::var_os(CHILD).is_some() {
            // A leaked directory, as the shell tests used to leave behind.
            std::mem::forget(tempfile::tempdir().unwrap());
            let leaked = tempfile::tempdir().unwrap().keep();
            println!("ISOLATION-ROOT {}", root().unwrap().display());
            println!("ISOLATION-LEAK {}", leaked.display());
            return;
        }
        let scratch = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "test_isolation::tests::exiting_process_removes_its_root_and_every_temp_directory_in_it",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .env_remove(ROOT_VARIABLE)
            .env("TMPDIR", scratch.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}");
        let path = |tag: &str| {
            // libtest prints its own `test name ... ` prefix on the same line.
            let line = stdout
                .lines()
                .find_map(|line| line.split_once(tag).map(|(_, path)| path));
            PathBuf::from(
                line.unwrap_or_else(|| panic!("no {tag} in {stdout}"))
                    .trim(),
            )
        };
        let (root, leak) = (path("ISOLATION-ROOT "), path("ISOLATION-LEAK "));
        assert!(root.starts_with(scratch.path()), "{}", root.display());
        assert!(leak.starts_with(&root), "{}", leak.display());
        assert!(!root.exists() && !leak.exists());
        assert_eq!(fs::read_dir(scratch.path()).unwrap().count(), 0);
    }

    #[test]
    fn root_is_removed_with_read_only_directories_inside() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("tree");
        let locked = root.join("a/locked");
        fs::create_dir_all(&locked).unwrap();
        fs::write(locked.join("file"), "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();
        }
        remove_tree(&root).unwrap();
        assert!(!root.exists());
    }
}
