//! FSEvents replay cursors are operational state, separate from disposable indexes.
use std::path::Path;

/// Directory events require the metadata walk that startup already performs.
/// They do not prove that every descendant note's bytes changed. Retain exact
/// file invalidations and let revision checks detect unreported child edits.
#[cfg(any(target_os = "macos", test))]
fn record_path(
    relative: &Path,
    directory: bool,
    dirty: &mut Vec<String>,
    directories: &mut Vec<String>,
) {
    let path = tessera_core::vault::note_path(relative);
    if directory || path.is_empty() {
        directories.push(path);
    } else {
        dirty.push(path);
    }
}

#[cfg(target_os = "macos")]
#[path = "reader_replay_macos.rs"]
mod native;
#[cfg(target_os = "macos")]
pub use native::prepare;

#[cfg(not(target_os = "macos"))]
#[derive(Default)]
pub struct Replay {
    pub force_all: bool,
    pub dirty: Vec<String>,
    pub directories: Vec<String>,
}
#[cfg(not(target_os = "macos"))]
pub fn prepare(_: &Path, _: Option<&Path>, _: Option<&str>) -> Replay {
    Replay::default()
}
#[cfg(not(target_os = "macos"))]
impl Replay {
    pub fn save(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_and_known_directories_preserve_file_invalidations() {
        let mut dirty = Vec::new();
        let mut directories = Vec::new();
        record_path(Path::new(""), false, &mut dirty, &mut directories);
        record_path(Path::new("Folder"), true, &mut dirty, &mut directories);
        record_path(
            Path::new("Folder/Changed.md"),
            false,
            &mut dirty,
            &mut directories,
        );
        // Missing type flags must remain conservative for a non-root path.
        record_path(Path::new("Unknown"), false, &mut dirty, &mut directories);
        assert_eq!(directories, ["", "Folder"]);
        assert_eq!(dirty, ["Folder/Changed.md", "Unknown"]);
    }

    #[cfg(unix)]
    #[test]
    fn directory_replay_discovers_topology_and_revision_changes_without_reading_unchanged_notes() {
        use std::os::unix::fs::MetadataExt;
        use tessera_core::vault::warm::{self, SourceRevision};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join("Folder")).unwrap();
        let changed = root.join("Folder/Changed.md");
        std::fs::write(&changed, "[[Old]]").unwrap();
        std::fs::write(root.join("Old.md"), "old target").unwrap();
        std::fs::write(root.join("New.md"), "new target").unwrap();
        let before = std::fs::metadata(&changed).unwrap();
        let before_revision = SourceRevision::read(&changed).unwrap();
        assert!(
            before_revision.is_precise(),
            "precise filesystem positive control"
        );
        let (_, previous, _) = warm::reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        let mut dirty = Vec::new();
        let mut directories = Vec::new();
        record_path(Path::new(""), true, &mut dirty, &mut directories);
        record_path(Path::new("Folder"), true, &mut dirty, &mut directories);
        let mut previous = previous;
        previous.invalidate_paths(&dirty);
        let (_, _, unchanged) = warm::reconcile_with_reader(
            &root,
            Some(&previous),
            false,
            &mut |_, _| Ok(()),
            &mut |_| panic!("directory event must not reopen unchanged notes"),
        )
        .unwrap();
        assert_eq!((unchanged.read, unchanged.reused), (0, 3));
        assert!(unchanged.graph_reused);

        // A directory event can cover a child edit without an exact file event.
        // Exact revisions still detect same-size writes with restored mtime.
        std::fs::write(&changed, "[[New]]").unwrap();
        let modified = before.modified().unwrap();
        std::fs::File::options()
            .write(true)
            .open(&changed)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let after = std::fs::metadata(&changed).unwrap();
        assert_eq!(
            (before.len(), before.modified().unwrap(), before.ino()),
            (after.len(), after.modified().unwrap(), after.ino())
        );
        assert_ne!(
            (before.ctime(), before.ctime_nsec()),
            (after.ctime(), after.ctime_nsec()),
            "ctime edit positive control"
        );
        std::fs::write(root.join("Folder/Added.md"), "new inventory entry").unwrap();
        let mut reads = Vec::new();
        let (vault, current, stats) = warm::reconcile_with_reader(
            &root,
            Some(&previous),
            false,
            &mut |_, _| Ok(()),
            &mut |path| {
                reads.push(path.strip_prefix(&root).unwrap().to_path_buf());
                std::fs::read(path)
            },
        )
        .unwrap();
        reads.sort();
        assert_eq!(
            reads,
            [Path::new("Folder/Added.md"), Path::new("Folder/Changed.md")]
        );
        assert_eq!((stats.read, stats.reused), (2, 2));
        assert!(vault.backlinks("Old.md").is_empty());
        assert_eq!(vault.backlinks("New.md")[0].path, "Folder/Changed.md");
        assert_eq!(
            current.source("Folder/Added.md").as_deref(),
            Some("new inventory entry")
        );

        // Explicit file events remain forced even when the current stamp matches.
        let mut current = current;
        record_path(Path::new("Old.md"), false, &mut dirty, &mut directories);
        current.invalidate_paths(&dirty);
        let (_, _, stats) =
            warm::reconcile(&root, Some(&current), false, &mut |_, _| Ok(())).unwrap();
        assert_eq!((stats.read, stats.reused), (1, 3));
        assert_eq!(stats.reuse.invalidated_revision, 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "manual 5k-note directory replay profile with actual filesystem latency"]
    fn directory_replay_cloud_profile() {
        use tessera_core::vault::warm;
        #[link(name = "dl")]
        unsafe extern "C" {
            fn dlsym(
                handle: *mut std::ffi::c_void,
                name: *const std::ffi::c_char,
            ) -> *mut std::ffi::c_void;
        }
        let set: unsafe extern "C" fn(*const std::ffi::c_char) = unsafe {
            let symbol = dlsym(std::ptr::null_mut(), c"tessera_slow_fs_phase".as_ptr());
            assert!(!symbol.is_null(), "run with the filesystem preload");
            std::mem::transmute(symbol)
        };
        let phase = |name: &std::ffi::CStr| unsafe { set(name.as_ptr()) };
        let temp = tempfile::Builder::new()
            .prefix("tessera-replay-directory-")
            .tempdir()
            .unwrap();
        let root = temp.path().join("vault");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&root).unwrap();
        for index in 0..5001 {
            let path = root.join(format!("Note {index}.md"));
            std::fs::write(
                &path,
                format!(
                    "# Note {index}\n\n[[Note {}]]\n{}",
                    (index + 1) % 5001,
                    "Canonical paragraph. ".repeat(100)
                ),
            )
            .unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(
                    std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000),
                ))
                .unwrap();
        }
        let (vault, previous, _) = warm::reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        warm::save_provisional(&previous, &vault, &cache, Some("Note 0.md")).unwrap();
        let previous = warm::Snapshot::load_checked(&cache, &root).unwrap();
        let source_ms: u64 = std::env::var("TESSERA_SLOW_FS_MS")
            .unwrap()
            .parse()
            .unwrap();
        let metadata_us: u64 = std::env::var("TESSERA_SLOW_FS_METADATA_US")
            .unwrap()
            .parse()
            .unwrap();
        assert!(source_ms > 0 && metadata_us > 0);
        phase(c"positive_control");
        let start = std::time::Instant::now();
        std::fs::read(root.join("Note 0.md")).unwrap();
        assert!(start.elapsed() >= std::time::Duration::from_millis(source_ms));
        let start = std::time::Instant::now();
        std::fs::symlink_metadata(root.join("Note 0.md")).unwrap();
        assert!(start.elapsed() >= std::time::Duration::from_micros(metadata_us));
        phase(c"setup");
        for sample in 0..2 {
            for legacy in [true, false] {
                let mut dirty = Vec::new();
                let mut directories = Vec::new();
                if legacy {
                    dirty.push(String::new()); // The previous callback's root-path behavior.
                } else {
                    record_path(Path::new(""), true, &mut dirty, &mut directories);
                    assert_eq!(directories, [""]);
                }
                let mut saved = previous.clone();
                saved.invalidate_paths(&dirty);
                phase(c"warm_reconcile");
                let start = std::time::Instant::now();
                let (vault, _, stats) = warm::reconcile_with_reader(
                    &root,
                    Some(&saved),
                    false,
                    &mut |_, _| Ok(()),
                    &mut |path| {
                        assert!(
                            legacy,
                            "directory replay must not open any unchanged source"
                        );
                        std::fs::read(path)
                    },
                )
                .unwrap();
                let milliseconds = start.elapsed().as_secs_f64() * 1000.;
                phase(c"setup");
                assert_eq!(
                    (stats.read, stats.reused),
                    if legacy { (5001, 0) } else { (0, 5001) }
                );
                assert_eq!(stats.graph_reused, !legacy);
                assert_eq!(vault.backlinks("Note 0.md")[0].path, "Note 5000.md");
                eprintln!("DIRECTORY_REPLAY_PROFILE sample={sample} legacy_root_invalidation={legacy} read={} reused={} graph_reused={} reconcile_ms={milliseconds:.2}; source I/O {source_ms}ms, metadata {metadata_us}us, same fixture/session; cache load/persist/search and native FileProvider excluded", stats.read, stats.reused, stats.graph_reused);
            }
        }
    }
}
