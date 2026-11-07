"""Require the actual multi-window regression to reject both former policies.

Run after the complete positive updater suite on the same native runner. Change
only announcement ownership/admission, keep the real GPUI test, and restore the
exact reviewed source even on failure. Compilation/setup/zero tests cannot count
as a negative-control verdict.
"""
import json
import os
from pathlib import Path
import re
import subprocess

SOURCE = Path('crates/okilum-shell/src/updater/mod.rs')
CASE = 'updater::tests::ready_announcements_belong_to_each_window'
FAILURE = 'each restored window must show its ready notice'


def variants(source):
    shared = '    let announced = announcements.windows.entry(window_id).or_default();'
    inactive = 'package.is_some_and(|package| announced.claim(package))'
    if source.count(shared) != 1 or source.count(inactive) != 1:
        raise ValueError('The two reviewed announcement-control anchors changed')
    return {
        'shared-claim': source.replace(shared, '''    let shared_window_id = announcements.windows.keys().next().copied().unwrap_or(window_id);
    let announced = announcements.windows.entry(shared_window_id).or_default();'''),
        'active-only': source.replace(inactive, 'window.is_window_active() && ' + inactive),
    }


def main():
    original = SOURCE.read_bytes()
    evidence = Path(os.environ['RUNNER_TEMP']) / 'updater-ready-controls'
    evidence.mkdir(exist_ok=True)
    verdicts = []
    try:
        for name, source in variants(original.decode()).items():
            SOURCE.write_text(source)
            result = subprocess.run([
                'cargo', 'test', '--locked', '-p', 'okilum-shell', '--bin', 'okilum',
                CASE, '--', '--exact', '--nocapture', '--test-threads=1',
            ], capture_output=True, text=True, timeout=600)
            output = result.stdout + result.stderr
            (evidence / f'{name}.log').write_text(output)
            plain = re.sub(r'\x1b\[[0-9;]*m', '', output)
            if (result.returncode != 101 or FAILURE not in plain
                    or f'test {CASE} ... FAILED' not in plain
                    or 'test result: FAILED. 0 passed; 1 failed;' not in plain
                    or 'error[E' in plain):
                print(output[-16000:])
                raise ValueError(f'{name}: missing the required GPUI assertion failure')
            verdicts.append({'control': name, 'runtime_failure': FAILURE,
                             'exit': result.returncode})
            print(f'UPDATER_READY_NEGATIVE_CONTROL {name}: expected runtime assertion failed')
    finally:
        SOURCE.write_bytes(original)
        if SOURCE.read_bytes() != original:
            raise ValueError('Could not restore the exact reviewed source')
    (evidence / 'receipt.json').write_text(json.dumps({
        'source': os.environ['OKILUM_SOURCE_COMMIT'], 'controls': verdicts,
        'reviewed_source_restored': True,
    }, indent=2) + '\n')


if __name__ == '__main__':
    main()
