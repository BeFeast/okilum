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
    ('discard-stage-before-rollback', 'helper_replace.rs',
     [(b'operations.restore(&p.backup, &p.destination)?;',
       b'remove_owned(&p.stage)?; operations.restore(&p.backup, &p.destination)?;')],
     RECOVERY, 'refused_rollback_keeps_backup_and_verified_stage_then_startup_recovers'),
    ('prefer-stage-over-original', 'helper_replace.rs',
     [(b'if p.backup.try_exists()? {', b'if false {')],
     RECOVERY, 'documented_replacefile_errors_restore_original_and_return_error'),
    ('no-recovery-after-replace-error', 'helper_replace.rs',
     [(b'if let Err(recovery) = recover_locked(&p, operations) {',
       b'if let Err(recovery) = Ok::<(), io::Error>(()) {')],
     RECOVERY, 'documented_replacefile_errors_restore_original_and_return_error'),
    ('no-stage-only-recovery', 'helper_replace.rs',
     [(b'} else if p.stage.try_exists()? {', b'} else if false {')],
     RECOVERY, 'first_install_and_interrupted_first_install_publish_the_staged_helper'),
    ('refuse-ambiguous-leftovers', 'helper_replace.rs',
     [(b'use std::os::windows::fs::OpenOptionsExt;',
       b'if p.backup.try_exists()? && p.stage.try_exists()? { return Err(io::Error::other("ambiguous")); }\n'
       b'        use std::os::windows::fs::OpenOptionsExt;')],
     RECOVERY, 'present_destination_resolves_every_leftover_combination_without_blocking'),
    ('raw-paths', 'helper_replace.rs',
     [(b'"\\\\\\\\?\\\\".encode_utf16().chain(units).collect()', b'units')],
     RECOVERY, 'long_path_replacement_and_recovery_do_not_require_host_max_path_opt_in'),
    ('locator-recovery-fatal', 'locator.rs',
     [(b'if let Err(error) = crate::helper_replace::recover(update_exe) {',
       b'if let Err(error) = crate::helper_replace::recover(update_exe) { panic!("{error}");')],
     RECOVERY, 'refused_cleanup_never_disables_discovery_of_an_existing_helper'),
    ('locator-without-recovery', 'locator.rs',
     [(b'        recover_helper(&config.UpdateExePath);', b'')],
     RECOVERY, 'killed_process_does_not_lose_recovery_files_and_next_locator_restores_helper'),
    ('attributes-not-copied', 'helper_replace.rs',
     [(b'FILE_FLAGS_AND_ATTRIBUTES(attributes)', b'FILE_ATTRIBUTE_NORMAL')],
     ZIP, 'windows::replacement_preserves_explicit_execute_ace_and_nondefault_attributes'),
    ('delete-then-rename', 'helper_replace.rs',
     [(b'                operations.replace(&p)\n',
       b'                { fs::remove_file(&p.destination)?; operations.restore(&p.stage, &p.destination) }\n')],
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
    result = subprocess.run(COMMAND + list(args), cwd=ROOT, text=True, encoding='utf-8',
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    print(result.stdout, flush=True)
    return result


def suite(target):
    assert run('--test', target, '--', '--nocapture').returncode == 0, target


def main():
    sys.stdout.reconfigure(encoding='utf-8')
    suite(ZIP)
    suite(RECOVERY)
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
