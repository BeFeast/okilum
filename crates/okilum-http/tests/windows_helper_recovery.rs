#![cfg(all(windows, feature = "updates"))]

use std::{
    fs,
    io::Write,
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
use velopack::{
    helper_replace::{inspect, lock, native_path_for_test, publish_fault, recover, Fault},
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
fn quarantined(helper: &Path) -> Vec<Vec<u8>> {
    let prefix = format!(
        "{}.vpk-quarantine-",
        helper.file_name().unwrap().to_string_lossy()
    );
    let mut found: Vec<Vec<u8>> = fs::read_dir(helper.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&prefix)
        })
        .map(|path| fs::read(path).unwrap_or_default())
        .collect();
    found.sort();
    found
}
fn assert_clean(helper: &Path) {
    for suffix in [".vpk-stage", ".vpk-backup", ".vpk-txn", ".vpk-lock"] {
        assert!(
            fs::symlink_metadata(sibling(helper, suffix)).is_err(),
            "transaction not cleaned: {suffix}"
        );
    }
}
/// Missing helper, original in the backup, new helper staged, valid record.
fn interrupted_between_renames(root: &Path) -> PathBuf {
    let helper = fixture(root);
    assert!(publish_fault(staged(&helper), &helper, Fault::LeaveAfterBackupMove).is_err());
    assert!(!helper.exists());
    assert_eq!(fs::read(sibling(&helper, ".vpk-backup")).unwrap(), OLD);
    assert_eq!(fs::read(sibling(&helper, ".vpk-stage")).unwrap(), NEW);
    assert!(sibling(&helper, ".vpk-txn").exists());
    helper
}
fn code(result: Result<(), velopack::Error>) -> i32 {
    match result {
        Err(velopack::Error::Io(error)) => error.raw_os_error().unwrap_or(0),
        other => panic!("expected an OS error, got {other:?}"),
    }
}

#[test]
fn documented_replacefile_errors_converge_to_a_verified_helper() {
    for (fault, installed) in [
        (Fault::RemoveReplaced, OLD),
        (Fault::MoveReplacement, OLD),
        (Fault::MoveReplacement2, NEW),
    ] {
        let root = tempfile::tempdir().unwrap();
        let helper = fixture(root.path());
        let result = publish_fault(staged(&helper), &helper, fault);
        // Success is decided by identity: only an installed new helper is Ok.
        assert_eq!(result.is_ok(), installed == NEW, "{fault:?}: {result:?}");
        assert_eq!(fs::read(&helper).unwrap(), installed, "{fault:?}");
        assert_clean(&helper);
        assert!(quarantined(&helper).is_empty(), "{fault:?}");
    }
}

#[test]
fn real_replacefile_partial_failures_report_plain_win32_codes() {
    let mut codes = Vec::new();
    for (hold_destination, hold_stage, occupy_backup) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let root = tempfile::tempdir().unwrap();
        let helper = fixture(root.path());
        let held = hold_destination.then(|| hold(&helper));
        let result = publish_fault(
            staged(&helper),
            &helper,
            Fault::Real {
                hold_stage,
                occupy_backup,
            },
        );
        drop(held);
        let code = code(result);
        println!("REAL_REPLACEFILEW_CODE destination={hold_destination} stage={hold_stage} backup={occupy_backup} code={code}");
        assert!(
            code > 0 && code < 0x1_0000,
            "ReplaceFileW error must be a plain Win32 code, not an HRESULT: {code:#x}"
        );
        assert_eq!(fs::read(&helper).unwrap(), OLD);
        assert_clean(&helper);
        codes.push(code);
    }
    assert!(
        codes.iter().any(|code| [1175, 1176, 1177].contains(code)),
        "no real partial-failure code observed: {codes:?}"
    );
}

#[test]
fn refused_promotion_keeps_every_copy_then_startup_recovers() {
    let root = tempfile::tempdir().unwrap();
    let helper = fixture(root.path());
    assert!(publish_fault(staged(&helper), &helper, Fault::PromotionBlocked).is_err());
    assert!(!helper.exists());
    assert_eq!(fs::read(sibling(&helper, ".vpk-backup")).unwrap(), OLD);
    assert_eq!(fs::read(sibling(&helper, ".vpk-stage")).unwrap(), NEW);
    assert!(sibling(&helper, ".vpk-txn").exists());
    // The app executable lives in the current directory; discovery walks back to the root.
    let locator = auto_locate_app_manifest(LocationContext::FromSpecifiedAppExecutable(
        root.path().join("current").join("okilum.exe"),
    ))
    .unwrap();
    assert_eq!(fs::read(locator.get_update_path()).unwrap(), NEW);
    assert_clean(&helper);
}

#[test]
fn startup_recovers_every_interruption_state_including_real_replace_backup() {
    for (fault, installed) in [
        (Fault::LeaveBeforeSwap, OLD),
        (Fault::LeaveAfterBackupMove, NEW),
        (Fault::LeaveAfterSwap, NEW),
    ] {
        let root = tempfile::tempdir().unwrap();
        let helper = fixture(root.path());
        assert!(publish_fault(staged(&helper), &helper, fault).is_err());
        assert!(sibling(&helper, ".vpk-txn").exists(), "{fault:?}");
        if matches!(fault, Fault::LeaveAfterSwap) {
            // This is the real Windows API, not a fake backup. Passing NULL to
            // ReplaceFileW makes this assertion fail even when replacement works.
            assert_eq!(fs::read(sibling(&helper, ".vpk-backup")).unwrap(), OLD);
        }
        let locator = locate(root.path()).unwrap();
        assert_eq!(
            fs::read(locator.get_update_path()).unwrap(),
            installed,
            "{fault:?}"
        );
        assert_clean(&helper);
    }
}

#[test]
fn planted_or_truncated_copies_are_never_promoted_or_deleted_while_helper_missing() {
    // A truncated backup next to a verified stage: the stage wins and the
    // backup is set aside intact, never promoted and never deleted.
    let root = tempfile::tempdir().unwrap();
    let helper = interrupted_between_renames(root.path());
    fs::write(sibling(&helper, ".vpk-backup"), b"trunc").unwrap();
    recover(&helper).unwrap();
    assert_eq!(fs::read(&helper).unwrap(), NEW);
    assert_eq!(quarantined(&helper), vec![b"trunc".to_vec()]);
    assert_clean(&helper);

    // Neither copy matches the record: nothing is promoted or removed.
    let root = tempfile::tempdir().unwrap();
    let helper = interrupted_between_renames(root.path());
    fs::write(sibling(&helper, ".vpk-backup"), b"planted backup").unwrap();
    fs::write(sibling(&helper, ".vpk-stage"), b"planted stage").unwrap();
    assert!(recover(&helper).is_err());
    assert!(locate(root.path()).is_err());
    assert!(!helper.exists());
    assert_eq!(
        fs::read(sibling(&helper, ".vpk-backup")).unwrap(),
        b"planted backup"
    );
    assert_eq!(
        fs::read(sibling(&helper, ".vpk-stage")).unwrap(),
        b"planted stage"
    );
    assert!(inspect(&helper).unwrap().contains("missing"));

    // A stage without any transaction record is never promoted.
    let root = tempfile::tempdir().unwrap();
    let helper = fixture(root.path());
    fs::remove_file(&helper).unwrap();
    fs::write(sibling(&helper, ".vpk-stage"), NEW).unwrap();
    assert!(recover(&helper).is_err());
    assert!(!helper.exists());
    assert_eq!(fs::read(sibling(&helper, ".vpk-stage")).unwrap(), NEW);
}

#[test]
fn linked_candidates_with_matching_bytes_are_never_promoted() {
    for kind in ["hardlink", "symlink"] {
        let root = tempfile::tempdir().unwrap();
        let helper = interrupted_between_renames(root.path());
        let stage = sibling(&helper, ".vpk-stage");
        let backup = sibling(&helper, ".vpk-backup");
        // Bytes that match the record, reachable only through a link.
        let elsewhere = root.path().join("elsewhere.bin");
        fs::write(&elsewhere, NEW).unwrap();
        fs::remove_file(&stage).unwrap();
        match kind {
            "hardlink" => fs::hard_link(&elsewhere, &stage).unwrap(),
            _ => std::os::windows::fs::symlink_file(&elsewhere, &stage)
                .expect("symbolic link creation (CI runs elevated)"),
        }
        fs::write(&backup, b"planted backup").unwrap();
        assert!(recover(&helper).is_err(), "{kind} promoted");
        assert!(!helper.exists(), "{kind}");
        assert!(fs::symlink_metadata(&stage).is_ok(), "{kind} link removed");
        assert_eq!(fs::read(&elsewhere).unwrap(), NEW, "{kind} target changed");
    }
}

#[test]
fn first_install_and_interrupted_first_install_publish_the_staged_helper() {
    let root = tempfile::tempdir().unwrap();
    let helper = fixture(root.path());
    fs::remove_file(&helper).unwrap();
    // No destination: publication is a verified rename by handle.
    publish_fault(staged(&helper), &helper, Fault::RemoveReplaced).unwrap();
    assert_eq!(fs::read(&helper).unwrap(), NEW);
    assert_clean(&helper);

    // The helper vanished after staging: the record proves the stage.
    fs::write(&helper, OLD).unwrap();
    assert!(publish_fault(staged(&helper), &helper, Fault::LeaveBeforeSwap).is_err());
    fs::remove_file(&helper).unwrap();
    let locator = locate(root.path()).unwrap();
    assert_eq!(fs::read(locator.get_update_path()).unwrap(), NEW);
    assert_clean(&helper);
}

#[test]
fn present_destination_sets_unverifiable_leftovers_aside_without_blocking() {
    let root = tempfile::tempdir().unwrap();
    let helper = fixture(root.path());
    for (backup, stage) in [(true, false), (false, true), (true, true)] {
        if backup {
            fs::write(sibling(&helper, ".vpk-backup"), b"previous helper").unwrap();
        }
        if stage {
            fs::write(sibling(&helper, ".vpk-stage"), NEW).unwrap();
        }
        recover(&helper).unwrap();
        assert_eq!(fs::read(&helper).unwrap(), OLD, "{backup} {stage}");
        assert_clean(&helper);
    }
    // Without a record nothing is provably obsolete: set aside, never deleted.
    assert_eq!(quarantined(&helper).len(), 4);
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
fn refused_startup_restore_retains_every_copy_and_recovers_in_session() {
    let root = tempfile::tempdir().unwrap();
    let helper = interrupted_between_renames(root.path());
    let held = hold(&sibling(&helper, ".vpk-stage"));
    match locate(root.path()) {
        Err(velopack::Error::NotInstalled(_)) => {}
        Err(error) => panic!("refused recovery must not replace discovery: {error}"),
        Ok(_) => panic!("helper cannot be located while its verified copy is held"),
    }
    assert!(!helper.exists());
    let diagnostic = inspect(&helper).unwrap();
    assert!(diagnostic.contains("missing"), "{diagnostic}");
    assert!(
        !diagnostic.contains(&*root.path().to_string_lossy()),
        "diagnostic must not name paths: {diagnostic}"
    );
    assert_eq!(fs::read(sibling(&helper, ".vpk-backup")).unwrap(), OLD);
    assert_eq!(fs::read(sibling(&helper, ".vpk-stage")).unwrap(), NEW);
    drop(held);
    // Same process, no relaunch: the retry succeeds.
    recover(&helper).unwrap();
    assert_eq!(fs::read(&helper).unwrap(), NEW);
    assert!(inspect(&helper).is_none());
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
    assert!(publish_fault(staged(&helper), &helper, Fault::PromotionBlocked).is_err());
    recover(&helper).unwrap();
    assert_eq!(fs::read(&helper).unwrap(), NEW);
    assert_clean(&helper);
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
    assert_eq!(fs::read(locator.get_update_path()).unwrap(), NEW);
    assert_clean(&helper);
}

#[test]
fn helper_lock_excludes_publication_from_another_process() {
    const CHILD: &str = "OKILUM_HELPER_LOCK_HOLDER_ROOT";
    if let Some(root) = std::env::var_os(CHILD) {
        let root = PathBuf::from(root);
        let _lock = lock(&root.join("Update.exe")).unwrap();
        fs::write(root.join("locked"), b"").unwrap();
        while !root.join("release").exists() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let helper = fixture(root.path());
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "helper_lock_excludes_publication_from_another_process",
            "--exact",
            "--nocapture",
        ])
        .env(CHILD, root.path())
        .spawn()
        .unwrap();
    while !root.path().join("locked").exists() {
        assert!(child.try_wait().unwrap().is_none(), "lock holder exited");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let result = publish_fault(
        staged(&helper),
        &helper,
        Fault::Real {
            hold_stage: false,
            occupy_backup: false,
        },
    );
    assert!(
        result.is_err(),
        "publication ran while another process held the helper lock"
    );
    assert_eq!(fs::read(&helper).unwrap(), OLD);
    fs::write(root.path().join("release"), b"").unwrap();
    assert!(child.wait().unwrap().success());
    publish_fault(
        staged(&helper),
        &helper,
        Fault::Real {
            hold_stage: false,
            occupy_backup: false,
        },
    )
    .unwrap();
    assert_eq!(fs::read(&helper).unwrap(), NEW);
    assert_clean(&helper);
}

fn assert_no_transaction(helper: &Path) {
    for suffix in [".vpk-stage", ".vpk-backup", ".vpk-txn"] {
        assert!(
            fs::symlink_metadata(sibling(helper, suffix)).is_err(),
            "transaction left: {suffix}"
        );
    }
}
fn leftovers(helper: &Path, marker: &str) -> usize {
    let prefix = format!("{}{marker}", helper.file_name().unwrap().to_string_lossy());
    fs::read_dir(helper.parent().unwrap())
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(&prefix)
        })
        .count()
}
const REAL: Fault = Fault::Real {
    hold_stage: false,
    occupy_backup: false,
};

#[test]
fn running_helper_is_replaced_or_kept_and_retry_converges() {
    let root = tempfile::tempdir().unwrap();
    let helper = fixture(root.path());
    let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("PING.EXE");
    fs::copy(&system, &helper).unwrap();
    let original = fs::read(&helper).unwrap();
    let mut running = std::process::Command::new(&helper)
        .args(["-n", "120", "127.0.0.1"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(
        running.try_wait().unwrap().is_none(),
        "helper is not running"
    );
    let result = publish_fault(staged(&helper), &helper, REAL);
    println!(
        "RUNNING_HELPER_PUBLISH {:?}",
        result.as_ref().map_err(|e| e.to_string())
    );
    match &result {
        Ok(()) => assert_eq!(fs::read(&helper).unwrap(), NEW),
        Err(_) => assert_eq!(fs::read(&helper).unwrap(), original),
    }
    assert_no_transaction(&helper);
    running.kill().unwrap();
    running.wait().unwrap();
    publish_fault(staged(&helper), &helper, REAL).unwrap();
    assert_eq!(fs::read(&helper).unwrap(), NEW);
    assert_clean(&helper);
    assert_eq!(
        leftovers(&helper, ".vpk-obsolete-"),
        0,
        "obsolete copy kept"
    );
}

#[test]
fn read_only_helper_fails_safely_or_keeps_its_attribute_and_retry_converges() {
    let root = tempfile::tempdir().unwrap();
    let helper = fixture(root.path());
    let mut permissions = fs::metadata(&helper).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&helper, permissions).unwrap();
    let result = publish_fault(staged(&helper), &helper, REAL);
    println!(
        "READ_ONLY_HELPER_PUBLISH {:?}",
        result.as_ref().map_err(|e| e.to_string())
    );
    match &result {
        Ok(()) => {
            assert_eq!(fs::read(&helper).unwrap(), NEW);
            assert!(fs::metadata(&helper).unwrap().permissions().readonly());
        }
        Err(_) => assert_eq!(fs::read(&helper).unwrap(), OLD),
    }
    assert_no_transaction(&helper);
    let mut permissions = fs::metadata(&helper).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(&helper, permissions).unwrap();
    publish_fault(staged(&helper), &helper, REAL).unwrap();
    assert_eq!(fs::read(&helper).unwrap(), NEW);
    assert_clean(&helper);
}

const POWER_CUT_ROOT: &str = "OKILUM_POWER_CUT_ROOT";
const POWER_CUT_PHASE: &str = "OKILUM_POWER_CUT_PHASE";
/// Large, distinct helpers widen every on-disk window for a power cut.
fn power_cut_bytes(seed: u8) -> Vec<u8> {
    (0..8 * 1024 * 1024u32)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed) ^ (i >> 13) as u8)
        .collect()
}
fn staged_bytes(path: &Path, bytes: &[u8]) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap()).unwrap();
    file.write_all(bytes).unwrap();
    file.as_file().sync_all().unwrap();
    file
}

/// VM driver (never in CI): publish with the production state machine and
/// either block at a named phase (marker `power-cut-ready`, flushed) or loop.
#[test]
#[ignore = "Windows QA VM power-cut writer, driven externally"]
fn power_cut_writer() {
    let root = PathBuf::from(std::env::var_os(POWER_CUT_ROOT).expect(POWER_CUT_ROOT));
    let phase = std::env::var(POWER_CUT_PHASE).expect(POWER_CUT_PHASE);
    let helper = fixture(&root);
    let mut file = fs::File::create(&helper).unwrap();
    file.write_all(&power_cut_bytes(1)).unwrap();
    file.sync_all().unwrap();
    drop(file);
    if phase == "loop" {
        for round in 0u64.. {
            let bytes = power_cut_bytes(if round % 2 == 0 { 2 } else { 1 });
            publish_fault(staged_bytes(&helper, &bytes), &helper, REAL).unwrap();
            fs::write(root.join("power-cut-progress"), round.to_string()).unwrap();
        }
    }
    let phase: &'static str = Box::leak(phase.into_boxed_str());
    let _ = publish_fault(
        staged_bytes(&helper, &power_cut_bytes(2)),
        &helper,
        Fault::Pause(phase),
    );
    panic!("phase {phase} was never reached");
}

/// VM verifier after the hard reset: list what survived, run production
/// discovery/recovery, and require exactly one helper that is one of the two.
#[test]
#[ignore = "Windows QA VM power-cut verifier, driven externally"]
fn power_cut_verify() {
    use sha2::{Digest, Sha256};
    let root = PathBuf::from(std::env::var_os(POWER_CUT_ROOT).expect(POWER_CUT_ROOT));
    let helper = root.join("Update.exe");
    let digest = |bytes: &[u8]| format!("{:x}", Sha256::digest(bytes));
    let old = digest(&power_cut_bytes(1));
    let new = digest(&power_cut_bytes(2));
    let mut names: Vec<_> = fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().unwrap().is_file())
        .map(|e| {
            let bytes = fs::read(e.path()).unwrap_or_default();
            let hash = digest(&bytes);
            let kind = if hash == old {
                "old"
            } else if hash == new {
                "new"
            } else {
                "other"
            };
            format!("{}:{}:{kind}", e.file_name().to_string_lossy(), bytes.len())
        })
        .collect();
    names.sort();
    println!("POWER_CUT_SURVIVED {}", names.join(" "));
    let located = locate(&root);
    let installed = fs::read(&helper).map(|b| digest(&b));
    println!(
        "POWER_CUT_RECOVERED locate_ok={} helper={}",
        located.is_ok(),
        match &installed {
            Ok(hash) if *hash == old => "old",
            Ok(hash) if *hash == new => "new",
            Ok(_) => "OTHER",
            Err(_) => "MISSING",
        }
    );
    located.unwrap();
    let installed = installed.unwrap();
    assert!(installed == old || installed == new, "helper is neither");
    assert_clean(&helper);
    assert_eq!(
        quarantined(&helper).len(),
        0,
        "unexpected unverifiable copy"
    );
}
