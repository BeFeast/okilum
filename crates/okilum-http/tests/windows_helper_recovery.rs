#![cfg(all(windows, feature = "updates"))]

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use velopack::{
    helper_replace::{native_path_for_test, publish_fault, recover, Fault},
    locator::{auto_locate_app_manifest, LocationContext},
};
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
fn assert_clean(helper: &Path) {
    for suffix in [".vpk-stage", ".vpk-backup", ".vpk-ready"] {
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
    assert!(sibling(&helper, ".vpk-ready").exists());
    let locator = auto_locate_app_manifest(LocationContext::FromSpecifiedAppExecutable(
        root.path().join("current/okilum.exe"),
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
        let locator = auto_locate_app_manifest(LocationContext::FromSpecifiedRootDir(
            root.path().to_owned(),
            None,
        ))
        .unwrap();
        assert_eq!(fs::read(locator.get_update_path()).unwrap(), installed);
        assert_clean(&helper);
    }
}

#[test]
fn verified_stage_can_recover_missing_helper_but_incomplete_stage_is_retained() {
    let root = tempfile::tempdir().unwrap();
    let helper = fixture(root.path());
    assert!(publish_fault(staged(&helper), &helper, Fault::LeaveBeforeSwap).is_err());
    fs::remove_file(&helper).unwrap();
    recover(&helper).unwrap();
    assert_eq!(fs::read(&helper).unwrap(), NEW);
    assert_clean(&helper);
    fs::remove_file(&helper).unwrap();
    fs::write(sibling(&helper, ".vpk-stage"), b"incomplete").unwrap();
    assert!(recover(&helper).is_err());
    assert_eq!(
        fs::read(sibling(&helper, ".vpk-stage")).unwrap(),
        b"incomplete"
    );
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
    let locator = auto_locate_app_manifest(LocationContext::FromSpecifiedRootDir(
        root.path().to_owned(),
        None,
    ))
    .unwrap();
    assert_eq!(fs::read(locator.get_update_path()).unwrap(), OLD);
    assert_clean(&helper);
}
