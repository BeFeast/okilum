#![cfg(all(windows, feature = "updates"))]

use std::{
    fs,
    io::Write,
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
use velopack::{
    helper_replace::{native_path_for_test, publish_fault, recover, Fault},
    locator::{auto_locate_app_manifest, LocationContext, VelopackLocator},
};
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
const OLD: &[u8] = b"original executable helper bytes";
const NEW: &[u8] = b"fully verified replacement helper bytes";

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}
fn staged(path: &Path) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap()).unwrap();
    file.write_all(NEW).unwrap();
    file.as_file().sync_all().unwrap();
    file
}
fn fixture(root: &Path) -> PathBuf {
    fs::create_dir_all(root.join("current")).unwrap();
    fs::create_dir_all(root.join("packages")).unwrap();
    fs::write(root.join("current/sq.version"), "<package><metadata><id>HelperRecovery</id><version>1.0.0</version><title>Fixture</title><authors>QA</authors><description>Test</description><mainExe>okilum.exe</mainExe></metadata></package>").unwrap();
    let helper = root.join("Update.exe");
    fs::write(&helper, OLD).unwrap();
    helper
}
fn locate(root: &Path) -> Result<VelopackLocator, velopack::Error> {
    auto_locate_app_manifest(LocationContext::FromSpecifiedRootDir(root.to_owned(), None))
}
/// Another process holding the file without FILE_SHARE_DELETE refuses its rename/removal.
fn hold(path: &Path) -> fs::File {
    fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
        .unwrap()
}
fn assert_clean(helper: &Path) {
    for suffix in [".vpk-stage", ".vpk-backup"] {
        assert!(
            !sibling(helper, suffix).exists(),
            "transaction not cleaned: {suffix}"
        );
    }
}

#[test]
fn documented_replacefile_errors_restore_original_and_return_error() {
    for fault in [
        Fault::RemoveReplaced,
        Fault::MoveReplacement,
        Fault::MoveReplacement2,
    ] {
        let root = tempfile::tempdir().unwrap();
        let helper = fixture(root.path());
        assert!(publish_fault(staged(&helper), &helper, fault).is_err());
        assert_eq!(
            fs::read(&helper).unwrap(),
            OLD,
            "original lost on documented failure"
        );
        assert_clean(&helper);
        recover(&helper).unwrap();
        assert_eq!(fs::read(&helper).unwrap(), OLD);
    }
}

#[test]
fn refused_rollback_keeps_backup_and_verified_stage_then_startup_recovers() {
    let root = tempfile::tempdir().unwrap();
    let helper = fixture(root.path());
    assert!(publish_fault(staged(&helper), &helper, Fault::RollbackBlocked).is_err());
    assert!(!helper.exists());
    assert_eq!(fs::read(sibling(&helper, ".vpk-backup")).unwrap(), OLD);
    assert_eq!(fs::read(sibling(&helper, ".vpk-stage")).unwrap(), NEW);
    // The app executable lives in the current directory; discovery walks back to the root.
    let locator = auto_locate_app_manifest(LocationContext::FromSpecifiedAppExecutable(
        root.path().join("current").join("okilum.exe"),
    ))
    .unwrap();
    assert_eq!(fs::read(locator.get_update_path()).unwrap(), OLD);
    assert_clean(&helper);
}

#[test]
fn startup_recovers_every_interruption_state_including_real_replace_backup() {
    for (fault, installed) in [
        (Fault::LeaveBeforeSwap, OLD),
        (Fault::LeaveAfterBackupMove, OLD),
        (Fault::LeaveAfterSwap, NEW),
    ] {
        let root = tempfile::tempdir().unwrap();
        let helper = fixture(root.path());
        assert!(publish_fault(staged(&helper), &helper, fault).is_err());
        if matches!(fault, Fault::LeaveAfterSwap) {
            // This is the real Windows API, not a fake backup. Passing NULL to
            // ReplaceFileW makes this assertion fail even when replacement works.
            assert_eq!(fs::read(sibling(&helper, ".vpk-backup")).unwrap(), OLD);
        }
        let locator = locate(root.path()).unwrap();
        assert_eq!(fs::read(locator.get_update_path()).unwrap(), installed);
        assert_clean(&helper);
    }
}

#[test]
fn first_install_and_interrupted_first_install_publish_the_staged_helper() {
    let root = tempfile::tempdir().unwrap();
    let helper = fixture(root.path());
    fs::remove_file(&helper).unwrap();
    // No destination: publication is a no-clobber rename; ReplaceFileW is not used.
    publish_fault(staged(&helper), &helper, Fault::RemoveReplaced).unwrap();
    assert_eq!(fs::read(&helper).unwrap(), NEW);
    assert_clean(&helper);

    // The helper vanished after staging (e.g. an interrupted first install):
    // the stage is a complete, flushed extraction and is the only copy.
    fs::write(&helper, OLD).unwrap();
    assert!(publish_fault(staged(&helper), &helper, Fault::LeaveBeforeSwap).is_err());
    fs::remove_file(&helper).unwrap();
    let locator = locate(root.path()).unwrap();
    assert_eq!(fs::read(locator.get_update_path()).unwrap(), NEW);
    assert_clean(&helper);
}

#[test]
fn present_destination_resolves_every_leftover_combination_without_blocking() {
    let root = tempfile::tempdir().unwrap();
    let helper = fixture(root.path());
    for (backup, stage) in [(true, false), (false, true), (true, true)] {
        if backup {
            fs::write(sibling(&helper, ".vpk-backup"), b"previous helper").unwrap();
        }
        if stage {
            fs::write(sibling(&helper, ".vpk-stage"), NEW).unwrap();
        }
        // The destination only receives complete files by rename, so it wins.
        recover(&helper).unwrap();
        assert_eq!(fs::read(&helper).unwrap(), OLD, "{backup} {stage}");
        assert_clean(&helper);
    }
}

#[test]
fn refused_cleanup_never_disables_discovery_of_an_existing_helper() {
    let root = tempfile::tempdir().unwrap();
    let helper = fixture(root.path());
    assert!(publish_fault(staged(&helper), &helper, Fault::LeaveBeforeSwap).is_err());
    let stage = sibling(&helper, ".vpk-stage");
    let held = hold(&stage);
    // Cleanup is refused, but Update.exe exists and must still be located.
    let locator = locate(root.path()).unwrap();
    assert_eq!(fs::read(locator.get_update_path()).unwrap(), OLD);
    assert!(stage.exists(), "held stage cannot have been removed");
    drop(held);
    locate(root.path()).unwrap();
    assert_clean(&helper);
}

#[test]
fn refused_startup_restore_retains_every_copy_and_the_next_launch_recovers() {
    let root = tempfile::tempdir().unwrap();
    let helper = fixture(root.path());
    assert!(publish_fault(staged(&helper), &helper, Fault::LeaveAfterBackupMove).is_err());
    let backup = sibling(&helper, ".vpk-backup");
    let held = hold(&backup);
    match locate(root.path()) {
        Err(velopack::Error::NotInstalled(_)) => {}
        Err(error) => panic!("refused recovery must not replace discovery: {error}"),
        Ok(_) => panic!("helper cannot be located while its only original is held"),
    }
    assert!(!helper.exists());
    assert_eq!(fs::read(&backup).unwrap(), OLD);
    assert_eq!(fs::read(sibling(&helper, ".vpk-stage")).unwrap(), NEW);
    drop(held);
    let locator = locate(root.path()).unwrap();
    assert_eq!(fs::read(locator.get_update_path()).unwrap(), OLD);
    assert_clean(&helper);
}

#[test]
fn long_path_replacement_and_recovery_do_not_require_host_max_path_opt_in() {
    let owned = tempfile::tempdir().unwrap();
    let mut root = owned.path().to_owned();
    for _ in 0..18 {
        root.push("helper-recovery-long-path");
    }
    let helper = fixture(&root);
    assert!(helper.as_os_str().len() > 400);
    assert!(native_path_for_test(&helper)
        .unwrap()
        .starts_with(&[92, 92, 63, 92]));
    assert!(publish_fault(staged(&helper), &helper, Fault::LeaveAfterSwap).is_err());
    assert_eq!(fs::read(sibling(&helper, ".vpk-backup")).unwrap(), OLD);
    recover(&helper).unwrap();
    assert_eq!(fs::read(&helper).unwrap(), NEW);
    assert_clean(&helper);
    assert!(publish_fault(staged(&helper), &helper, Fault::RollbackBlocked).is_err());
    recover(&helper).unwrap();
    assert_eq!(fs::read(&helper).unwrap(), NEW);
}

#[test]
fn killed_process_does_not_lose_recovery_files_and_next_locator_restores_helper() {
    const CHILD: &str = "OKILUM_HELPER_RECOVERY_ABORT_ROOT";
    if let Some(root) = std::env::var_os(CHILD) {
        let helper = PathBuf::from(root).join("Update.exe");
        assert!(publish_fault(staged(&helper), &helper, Fault::LeaveAfterBackupMove).is_err());
        assert!(!helper.exists());
        std::process::abort();
    }
    let root = tempfile::tempdir().unwrap();
    let helper = fixture(root.path());
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "killed_process_does_not_lose_recovery_files_and_next_locator_restores_helper",
            "--exact",
            "--nocapture",
        ])
        .env(CHILD, root.path())
        .status()
        .unwrap();
    assert!(!status.success(), "child abort control did not fire");
    assert!(!helper.exists());
    assert_eq!(fs::read(sibling(&helper, ".vpk-backup")).unwrap(), OLD);
    assert_eq!(fs::read(sibling(&helper, ".vpk-stage")).unwrap(), NEW);
    let locator = locate(root.path()).unwrap();
    assert_eq!(fs::read(locator.get_update_path()).unwrap(), OLD);
    assert_clean(&helper);
}
