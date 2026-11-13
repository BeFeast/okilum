#!/usr/bin/env python3
"""Native helper regression suites and independently fired unsafe revert controls."""
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
COMMAND = ['cargo', 'test', '--locked', '-p', 'okilum-http', '--features', 'updates']


def suite(target, *extra):
    subprocess.run(COMMAND + ['--test', target, *extra, '--', '--nocapture'], cwd=ROOT, check=True)


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    suite('zip_compat')
    suite('windows_helper_recovery')
    source = ROOT / 'vendor/velopack-1.2.161/src/helper_replace.rs'
    original = source.read_bytes()
    # Actual API backup creation is asserted after successful ReplaceFileW.
    mutations = [
        (b'PCWSTR(backup.as_ptr()),', b'PCWSTR::null(),',
         'startup_recovers_every_interruption_state_including_real_replace_backup'),
        (b'operations.restore(&p.backup, &p.destination)?;', b'remove_owned(&p.stage)?; operations.restore(&p.backup, &p.destination)?;',
         'refused_rollback_keeps_backup_and_verified_stage_then_startup_recovers'),
    ]
    try:
        for before, after, test in mutations:
            assert original.count(before) == 1, before
            source.write_bytes(original.replace(before, after))
            result = subprocess.run(COMMAND + ['--test', 'windows_helper_recovery', test, '--', '--exact', '--nocapture'],
                                    cwd=ROOT, text=True, encoding="utf-8", stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            print(result.stdout, flush=True)
            assert result.returncode == 101 and f'{test} ... FAILED' in result.stdout, test
            print('HELPER_RECOVERY_REVERT_CONTROL_FIRED ' + test, flush=True)
    finally:
        source.write_bytes(original)
    subprocess.run([sys.executable, 'scripts/velopack-setup.py', '--verify'], cwd=ROOT, check=True)
    suite('windows_helper_recovery')
    suite('zip_compat')


if __name__ == '__main__':
    main()
