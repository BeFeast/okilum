#!/usr/bin/env python3
"""Native helper regression suites and independently fired revert controls.

Each control reverts exactly one fix in the generated Velopack source, rebuilds,
and requires the named test to report FAILED (a compile error never counts).
The pristine source is restored and checksum-verified before the suites rerun.
"""
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
VENDOR = ROOT / 'vendor/velopack-1.2.161/src'
COMMAND = ['cargo', 'test', '--locked', '-p', 'okilum-http', '--features', 'updates']
RECOVERY = 'windows_helper_recovery'
ZIP = 'zip_compat'

# (name, file, [(before, after), ...], test target, exact test name)
CONTROLS = [
    ('replace-without-backup', 'helper_replace.rs',
     [(b'PCWSTR(backup.as_ptr()),', b'PCWSTR::null(),')],
     RECOVERY, 'startup_recovers_every_interruption_state_including_real_replace_backup'),
    ('promote-by-name', 'helper_replace.rs',
     [(b'if identity(&mut file)? == expected {', b'if true {')],
     RECOVERY, 'planted_or_truncated_copies_are_never_promoted_or_deleted_while_helper_missing'),
    ('set-aside-while-missing', 'helper_replace.rs',
     [(b'if present(&p.stage)? || present(&p.backup)? {\n                return Err(other("update helper is missing and no copy',
       b'retire(p, &p.stage, valid, "x", operations)?; retire(p, &p.backup, valid, "x", operations)?;\n'
       b'            if present(&p.stage)? || present(&p.backup)? {\n                return Err(other("update helper is missing and no copy')],
     RECOVERY, 'planted_or_truncated_copies_are_never_promoted_or_deleted_while_helper_missing'),
    ('delete-unverifiable-leftovers', 'helper_replace.rs',
     [(b'rename_handle(&file, &p.quarantine())', b'delete_handle(file)')],
     RECOVERY, 'planted_or_truncated_copies_are_never_promoted_or_deleted_while_helper_missing'),
    ('accept-hardlinks', 'helper_replace.rs',
     [(b' && info.nNumberOfLinks == 1)', b')')],
     RECOVERY, 'linked_candidates_with_matching_bytes_are_never_promoted'),
    ('follow-reparse-points', 'helper_replace.rs',
     [(b'.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)', b'.custom_flags(0)')],
     RECOVERY, 'linked_candidates_with_matching_bytes_are_never_promoted'),
    ('prefer-original', 'helper_replace.rs',
     [(b'[(&p.stage, Some(record.new), "staged copy"), (&p.backup, record.original, "backup")]',
       b'[(&p.backup, record.original, "backup"), (&p.stage, Some(record.new), "staged copy")]')],
     RECOVERY, 'documented_replacefile_errors_converge_to_a_verified_helper'),
    ('success-by-name', 'helper_replace.rs',
     [(b'if installed == record.new {', b'if true {')],
     RECOVERY, 'documented_replacefile_errors_converge_to_a_verified_helper'),
    ('no-recovery-after-replace-error', 'helper_replace.rs',
     [(b'if let Err(recovery) = recover_locked(&p, operations) {',
       b'if let Err(recovery) = Ok::<(), io::Error>(()) {')],
     RECOVERY, 'documented_replacefile_errors_converge_to_a_verified_helper'),
    ('no-promotion-when-missing', 'helper_replace.rs',
     [(b'if let Some((file, from)) = candidate {', b'if let Some((file, from)) = None::<(fs::File, PathBuf)> {')],
     RECOVERY, 'first_install_and_interrupted_first_install_publish_the_staged_helper'),
    ('refuse-leftovers-next-to-helper', 'helper_replace.rs',
     [(b'        // `held` denies writers and deleters of the admitted helper during cleanup.',
       b'        if present(&p.backup)? && present(&p.stage)? { return Err(other("ambiguous")); }\n'
       b'        // `held` denies writers and deleters of the admitted helper during cleanup.')],
     RECOVERY, 'present_destination_sets_unverifiable_leftovers_aside_without_blocking'),
    ('hresult-codes', 'helper_replace.rs',
     [(b'if code & 0xFFFF_0000 == 0x8007_0000 {', b'if false {')],
     RECOVERY, 'real_replacefile_partial_failures_report_plain_win32_codes'),
    ('raw-paths', 'helper_replace.rs',
     [(b'"\\\\\\\\?\\\\".encode_utf16().chain(units).collect()', b'units')],
     RECOVERY, 'long_path_replacement_and_recovery_do_not_require_host_max_path_opt_in'),
    ('lock-not-exclusive', 'helper_replace.rs',
     [(b'Ok(Self { lock: Some(crate::lockfile::LockFile::try_acquire_lock(p.lock.clone())?), path: p.lock.clone() })',
       b'Ok(Self { lock: None, path: p.lock.clone() })')],
     RECOVERY, 'helper_lock_excludes_publication_from_another_process'),
    ('keep-orphan-temporaries', 'helper_replace.rs',
     [(b'        sweep_temporaries(p);\n', b'')],
     RECOVERY, 'orphaned_temporaries_are_removed_but_open_ones_are_kept'),
    ('lock-file-left-behind', 'helper_replace.rs',
     [(b'let _ = fs::remove_file(&self.path);', b'')],
     RECOVERY, 'startup_recovers_every_interruption_state_including_real_replace_backup'),
    ('locator-recovery-fatal', 'locator.rs',
     [(b'if let Err(error) = crate::helper_replace::recover(update_exe) {',
       b'if let Err(error) = crate::helper_replace::recover(update_exe) { panic!("{error}");')],
     RECOVERY, 'refused_cleanup_never_disables_discovery_of_an_existing_helper'),
    ('locator-without-recovery', 'locator.rs',
     [(b'        recover_helper(&config.UpdateExePath);', b'')],
     RECOVERY, 'killed_process_does_not_lose_recovery_files_and_next_locator_restores_helper'),
    ('delete-then-rename', 'helper_replace.rs',
     [(b'                operations.replace(&p)\n',
       b'                { fs::remove_file(&p.destination)?; rename_new(&p.stage, &p.destination) }\n')],
     ZIP, 'windows::replacement_preserves_explicit_execute_ace_and_nondefault_attributes'),
    ('truncate-before-read', 'bundle.rs',
     [(b'let mut outfile = tempfile::NamedTempFile::new_in(parent)?;',
       b'let mut outfile = misc::retry_io(|| File::create(path))?;'),
      (b'outfile.as_file().sync_all()?;\n        crate::helper_replace::publish(outfile, path)?;',
       b'outfile.sync_all()?;')],
     ZIP, 'failed_crc_after_partial_copy_keeps_original_helper_identical'),
    ('stub-errors-ignored', 'bundle.rs',
     [(b'self.extract_zip_idx_to_path(i, &dest_path)?;', b'let _ = self.extract_zip_idx_to_path(i, &dest_path);')],
     ZIP, 'failed_stub_extraction_returns_error_and_keeps_original'),
    ('package-before-helper', 'manager.rs',
     [(b'self.prepare_update_helper(&partial_file)?;', b''),
      (b'fs::rename(&partial_file, &final_target_file)?;',
       b'fs::rename(&partial_file, &final_target_file)?; let _ = self.prepare_update_helper(&final_target_file);')],
     ZIP, 'windows::helper_failure_never_publishes_pending_package_and_retry_prepares_helper'),
    ('cached-package-not-prepared', 'manager.rs',
     [(b'self.prepare_update_helper(&final_target_file)?;', b'')],
     ZIP, 'windows::cached_package_with_stale_or_corrupted_helper_is_not_pending_and_retry_repairs_it'),
    ('pending-without-helper-check', 'manager.rs',
     [(b'if !self.is_update_helper_prepared(&asset).unwrap_or(false) { return None; }', b'')],
     ZIP, 'windows::cached_package_with_stale_or_corrupted_helper_is_not_pending_and_retry_repairs_it'),
]


def run(*args):
    try:
        result = subprocess.run(COMMAND + list(args), cwd=ROOT, text=True, encoding='utf-8',
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=900)
    except subprocess.TimeoutExpired as expired:
        # A hung control is reported as not fired; it never eats the job.
        output = expired.stdout or ''
        if isinstance(output, bytes):
            output = output.decode('utf-8', 'replace')
        result = subprocess.CompletedProcess(expired.cmd, -1, output + '\nCONTROL_TIMEOUT\n')
    # Cap each line: a single huge assertion line can swallow the rest of the CI log.
    print('\n'.join(line[:2000] for line in result.stdout.splitlines()), flush=True)
    return result


def suite(target):
    assert run('--test', target, '--', '--nocapture').returncode == 0, target


def main():
    sys.stdout.reconfigure(encoding='utf-8')
    suite(ZIP)
    suite(RECOVERY)
    # Elevated runner only: a full NTFS volume (mounted VHD), then retry.
    assert run('--test', ZIP, 'windows::disk_full_extraction_fails_safely_and_retry_succeeds',
               '--', '--exact', '--ignored', '--nocapture').returncode == 0, 'disk full'
    print('HELPER_DISK_FULL_RETRY_PASS', flush=True)
    originals = {path: path.read_bytes() for path in VENDOR.iterdir() if path.suffix == '.rs'}
    fired, missed = [], []
    try:
        for name, file, edits, target, test in CONTROLS:
            source = VENDOR / file
            mutated = originals[source]
            for before, after in edits:
                assert mutated.count(before) == 1, (name, before)
                mutated = mutated.replace(before, after)
            source.write_bytes(mutated)
            try:
                result = run('--test', target, test, '--', '--exact', '--nocapture')
            finally:
                source.write_bytes(originals[source])
            if (result.returncode == 101 and 'running 1 test' in result.stdout
                    and f'test {test} ... FAILED' in result.stdout):
                print(f'HELPER_REVERT_CONTROL_FIRED {name} -> {test}', flush=True)
                fired.append(name)
            else:
                print(f'HELPER_REVERT_CONTROL_NOT_FIRED {name} -> {test} (exit {result.returncode})', flush=True)
                missed.append(name)
    finally:
        for path, data in originals.items():
            path.write_bytes(data)
    assert not missed and len(fired) == len(CONTROLS), ('unfired controls', missed)
    subprocess.run([sys.executable, 'scripts/velopack-setup.py', '--verify'], cwd=ROOT, check=True)
    suite(RECOVERY)
    suite(ZIP)
    print(f'HELPER_REVERT_CONTROLS_ALL_FIRED {len(fired)}', flush=True)


if __name__ == '__main__':
    main()
