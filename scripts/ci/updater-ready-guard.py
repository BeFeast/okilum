#!/usr/bin/env python3
"""Native Okilum Ready guard: positive suite plus a compiled revert control.

The control is compiled into the same test binary behind an environment switch,
so the large shell crate builds once. With the switch set, Okilum's own
helper/package byte check is bypassed and the test must fail on its stale-helper
assertion. The source is restored and checked against git afterwards.
"""
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / 'crates/okilum-shell/src/updater/windows.rs'
SWITCH = 'OKILUM_UPDATER_READY_GUARD_CONTROL'
TEST = 'updater::windows::tests::ready_requires_helper_bytes_for_this_package'
BASE = ['cargo', 'test', '--locked', '-p', 'okilum-shell', '--bin', 'okilum', TEST]
BEFORE = b'manager.is_update_helper_prepared(asset)?,'
AFTER = b'std::env::var_os("' + SWITCH.encode() + b'").is_some() || manager.is_update_helper_prepared(asset)?,'


def run(*args, control=False):
    env = dict(os.environ)
    env.pop(SWITCH, None)
    if control:
        env[SWITCH] = '1'
    result = subprocess.run(BASE + list(args), cwd=ROOT, env=env, text=True, encoding='utf-8',
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    print(result.stdout, flush=True)
    return result


def main():
    sys.stdout.reconfigure(encoding='utf-8')
    original = SOURCE.read_bytes()
    assert original.count(BEFORE) == 1
    try:
        SOURCE.write_bytes(original.replace(BEFORE, AFTER))
        listed = run('--', '--list')
        assert listed.returncode == 0 and f'{TEST}: test' in listed.stdout, 'test inventory'
        assert run('--', '--exact', '--nocapture').returncode == 0, 'positive'
        negative = run('--', '--exact', '--nocapture', control=True)
        assert negative.returncode == 101 and f'test {TEST} ... FAILED' in negative.stdout, 'control'
        assert 'stale helper must not be Ready' in negative.stdout, 'control assertion'
        print('UPDATER_READY_GUARD_REVERT_CONTROL_FIRED', flush=True)
    finally:
        SOURCE.write_bytes(original)
    subprocess.run(['git', 'diff', '--exit-code', '--', str(SOURCE)], cwd=ROOT, check=True)
    assert run('--', '--exact', '--nocapture').returncode == 0, 'restored positive'
    print('UPDATER_READY_GUARD_RESTORED_SUITE_PASS', flush=True)


if __name__ == '__main__':
    main()
