#!/usr/bin/env python3
"""Run every #2582/#2043 control under one unchanged 600-second deadline.

Independent crash checkpoints have separate parent processes and private state
roots. At most two run together; each remains a subreaper of its own helpers.
The owning Python process checks and reaps the complete fixture tree on exit.
"""
import os
from pathlib import Path
import runpy
import signal
import subprocess
import sys
import tempfile
import time

lifecycle = runpy.run_path(str(Path(__file__).with_name('sync-fixture-lifecycle.py')))
CRASH_TEST = 'issue2043_crash_recovery_retains_versions_and_rejects_third_version'
TESTS = {
    'issue2582_configuration_process_drains_and_later_real_effects_are_refused',
    'issue2582_autonomous_restart_fences_dead_holder_and_other_folder_transfers',
    'issue2582_linux_bootstrap_and_replaced_bindings_fail_closed',
    'issue2043_scoped_hold_blocks_real_transfer_and_recovers_across_restart',
    CRASH_TEST,
    'issue2043_two_managed_instances_preview_union_preservation_and_bidirectional_handoff',
}
CHECKPOINTS = (
    'destination-prepared', 'source-staged', 'source-partial-blob', 'source-prepared',
    'source-frozen', 'source-captured', 'source-note-captured', 'source-published',
    'destination-published', 'source-complete', 'destination-released',
    'handoff-folder-created', 'third-version',
)


def main():
    lifecycle['subreaper']()
    work = Path(tempfile.mkdtemp(prefix='sync-managed-admission-', dir=os.environ['TMPDIR']))
    executable = str(Path(sys.argv[1]).resolve())
    # Discover the exact compiled parent inventory before selecting any shard.
    listing = subprocess.run([executable, '--list'], check=True, text=True, stdout=subprocess.PIPE).stdout
    discovered = {line.removesuffix(': test') for line in listing.splitlines() if line.endswith(': test')}
    assert discovered == TESTS | {'issue2582_child'}, discovered
    groups = [
        ('crash-even', CHECKPOINTS[::2]),
        ('crash-odd', CHECKPOINTS[1::2]),
        ('remaining', ()),
    ]
    print(f'Managed admission fixture evidence: {work}', flush=True)
    active, all_runs = [], []
    deadline = time.monotonic() + 600

    def interrupted(signum, _frame):
        raise SystemExit(128 + signum)
    signal.signal(signal.SIGTERM, interrupted)

    def start(name, checkpoints):
        root = work / name
        root.mkdir(mode=0o700)
        (root / 'state').mkdir(mode=0o700)
        (root / 'tmp').mkdir()
        env = dict(os.environ, TMPDIR=str(root / 'tmp'), XDG_STATE_HOME=str(root / 'state'), RUST_BACKTRACE='1')
        # Never inherit a caller's narrowed selection into the complete gate.
        env.pop('OKILUM_FIRST_SYNC_CRASH_CASE', None)
        command = [executable, '--include-ignored', '--nocapture', '--test-threads=1']
        if checkpoints:
            command += [CRASH_TEST, '--exact']
            env['OKILUM_FIRST_SYNC_CRASH_CASE'] = ','.join(checkpoints)
        else:
            command += ['--skip', 'issue2582_child', '--skip', CRASH_TEST]
        log = root / 'acceptance.log'
        with log.open('w') as output:
            process = subprocess.Popen(command, env=env, stdout=output, stderr=subprocess.STDOUT,
                                       preexec_fn=lifecycle['parent_death_signal']())
        run = (name, checkpoints, process, log)
        active.append(run)
        all_runs.append(run)

    try:
        while groups or active:
            while groups and len(active) < 2:
                start(*groups.pop(0))
            assert time.monotonic() < deadline, 'managed admission exceeded the unchanged 600s deadline'
            for run in active[:]:
                name, checkpoints, process, log = run
                if process.poll() is None:
                    continue
                process.wait()
                output = log.read_text()
                print(f'=== Managed admission {name} ===\n{output}', flush=True)
                expected = {CRASH_TEST} if checkpoints else TESTS - {CRASH_TEST}
                for test in expected:
                    assert f'test {test} ...' in output, (name, test, output)
                assert process.returncode == 0 and f'test result: ok. {len(expected)} passed; 0 failed;' in output, (name, process.returncode, output)
                observed = [line.split('checkpoint=', 1)[1].split()[0]
                            for line in output.splitlines() if line.startswith('FIRST_SYNC_CRASH_RECOVERY_PASS ')]
                assert observed == list(checkpoints), (name, observed, checkpoints)
                assert not lifecycle['check'](log.parent), f'surviving {name} fixture process'
                active.remove(run)
            time.sleep(.05)
        assert not lifecycle['check'](work), 'surviving fixture process'
        lifecycle['reap']()
        print(f'MANAGED_ADMISSION_PASS: {len(TESTS)} selected tests and {len(CHECKPOINTS)} crash checkpoints passed; fixture processes=0; children reaped', flush=True)
        return 0
    finally:
        for name, _, process, log in all_runs:
            if process.poll() is None:
                process.kill()
            process.wait()
            if any(run[0] == name for run in active):
                print(f'=== Incomplete managed admission {name} ===\n{log.read_text()}', flush=True)
        lifecycle['check'](work, cleanup=True)
        lifecycle['reap']()


if __name__ == '__main__':
    raise SystemExit(main())
