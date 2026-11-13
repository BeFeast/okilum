#!/usr/bin/env python3
"""Native Okilum updater guards: positive suite plus compiled revert controls.

Every control is compiled into the same test binary behind one environment
switch, so the large shell crate builds once. With a control selected, one
guard (Ready helper check, launch lock, launch re-verification, repair
classification) is bypassed and its test must fail on its own assertion. The
source is restored and checked against git afterwards.
"""
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / 'crates/okilum-shell/src/updater/windows.rs'
SWITCH = 'OKILUM_UPDATER_GUARD_CONTROL'
PREFIX = 'updater::windows::tests::'
BASE = ['cargo', 'test', '--locked', '-p', 'okilum-shell', '--bin', 'okilum']
HOOK = (b'\n#[allow(dead_code)]\nfn updater_guard_control(name: &str) -> bool {\n'
        b'    std::env::var("' + SWITCH.encode() + b'").as_deref() == Ok(name)\n}\n')
CONTROLS = [
    ('ready', b'manager.is_update_helper_prepared(asset)?,',
     b'updater_guard_control("ready") || manager.is_update_helper_prepared(asset)?,',
     'ready_requires_helper_bytes_for_this_package', 'stale helper must not be Ready'),
    ('arm-lock', b'let _helper = manager.lock_update_helper().map_err(',
     b'let _helper = (!updater_guard_control("arm-lock")).then(|| manager.lock_update_helper()).transpose().map_err(',
     'helper_launch_is_locked_and_reverified_across_processes', 'launch must wait for the helper lock'),
    ('arm-reverify', b'    reverify(manager, asset)?;\n    launch()',
     b'    if !updater_guard_control("arm-reverify") { reverify(manager, asset)?; }\n    launch()',
     'helper_launch_is_locked_and_reverified_across_processes', 'launch must re-verify the helper under the lock'),
    ('repair-label', b'        (Some(true), _) | (None, None) => Install::Portable,\n',
     b'        (Some(true), _) | (None, None) => Install::Portable,\n'
     b'        (None, Some(_)) if updater_guard_control("repair-label") => Install::Portable,\n'
     b'        (Some(false), Some(_)) if updater_guard_control("installed-repair") => Install::Installed,\n',
     'helper_needing_repair_is_not_reported_as_portable', 'must not be called Portable'),
    # Same injected arms as repair-label (one source edit); this switch bypasses
    # only the located-install classification.
    ('installed-repair', None, None,
     'helper_needing_repair_is_not_reported_as_portable', 'must not be called Installed'),
]


def run(*args, control=None):
    env = dict(os.environ)
    env.pop(SWITCH, None)
    if control:
        env[SWITCH] = control
    result = subprocess.run(BASE + list(args), cwd=ROOT, env=env, text=True, encoding='utf-8',
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    print('\n'.join(line[:2000] for line in result.stdout.splitlines()), flush=True)
    return result


def main():
    sys.stdout.reconfigure(encoding='utf-8')
    original = SOURCE.read_bytes()
    mutated = original
    for _, before, after, _, _ in CONTROLS:
        if before is None:
            continue
        assert mutated.count(before) == 1, before
        mutated = mutated.replace(before, after)
    missed = []
    try:
        SOURCE.write_bytes(mutated + HOOK)
        listed = run(PREFIX, '--', '--list')
        names = [line for line in listed.stdout.splitlines() if line.endswith(': test')]
        assert listed.returncode == 0 and len(names) == 3, names
        assert run(PREFIX, '--', '--nocapture', '--test-threads=1').returncode == 0, 'positive'
        for name, _, _, test, assertion in CONTROLS:
            negative = run(PREFIX + test, '--', '--exact', '--nocapture', control=name)
            if (negative.returncode == 101 and f'test {PREFIX}{test} ... FAILED' in negative.stdout
                    and assertion in negative.stdout):
                print(f'UPDATER_GUARD_REVERT_CONTROL_FIRED {name} -> {test}', flush=True)
            else:
                print(f'UPDATER_GUARD_REVERT_CONTROL_NOT_FIRED {name} -> {test}', flush=True)
                missed.append(name)
    finally:
        SOURCE.write_bytes(original)
    subprocess.run(['git', 'diff', '--exit-code', '--', str(SOURCE)], cwd=ROOT, check=True)
    assert not missed, ('unfired controls', missed)
    assert run(PREFIX, '--', '--nocapture', '--test-threads=1').returncode == 0, 'restored positive'
    print(f'UPDATER_GUARD_REVERT_CONTROLS_ALL_FIRED {len(CONTROLS)}', flush=True)
    print('UPDATER_READY_GUARD_RESTORED_SUITE_PASS', flush=True)


if __name__ == '__main__':
    main()
