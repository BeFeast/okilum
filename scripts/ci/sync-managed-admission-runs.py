#!/usr/bin/env python3
"""Execute named #2582 controls with the #2499 kill/reap fixture owner."""
import os
from pathlib import Path
import runpy
import signal
import subprocess
import sys
import tempfile

lifecycle = runpy.run_path(str(Path(__file__).with_name('sync-fixture-lifecycle.py')))
TESTS = {
    'issue2582_configuration_process_drains_and_later_real_effects_are_refused',
    'issue2582_autonomous_restart_fences_dead_holder_and_other_folder_transfers',
    'issue2582_linux_bootstrap_and_replaced_bindings_fail_closed',
}


def main():
    lifecycle['subreaper']()
    work = Path(tempfile.mkdtemp(prefix='sync-managed-admission-', dir=os.environ['TMPDIR']))
    (work / 'state').mkdir(mode=0o700)
    (work / 'tmp').mkdir()
    log = work / 'acceptance.log'
    env = dict(os.environ, TMPDIR=str(work / 'tmp'), XDG_STATE_HOME=str(work / 'state'), RUST_BACKTRACE='1')
    print(f'Managed admission fixture evidence: {work}', flush=True)
    process = None
    def interrupted(signum, _frame):
        raise SystemExit(128 + signum)
    signal.signal(signal.SIGTERM, interrupted)
    try:
        with log.open('w') as output:
            process = subprocess.Popen(
                [str(Path(sys.argv[1]).resolve()), 'issue2582_', '--include-ignored',
                 '--skip', 'issue2582_child', '--nocapture', '--test-threads=1'],
                env=env, stdout=output, stderr=subprocess.STDOUT,
                preexec_fn=lifecycle['parent_death_signal']())
        result = process.wait(timeout=300)
        output = log.read_text()
        print(output, flush=True)
        for name in TESTS:
            assert f'test {name} ...' in output and name in output, (name, output)
        # Child subprocesses print their own summaries. Require the parent suite
        # to report exactly all THREE selected tests, never a non-empty child.
        assert result == 0 and 'test result: ok. 3 passed; 0 failed;' in output, (result, output)
        assert not lifecycle['check'](work), 'surviving fixture process'
        lifecycle['reap']()
        print('MANAGED_ADMISSION_PASS: 3 selected tests passed; fixture processes=0; children reaped', flush=True)
        return 0
    finally:
        if process is not None:
            if process.poll() is None:
                process.kill()
            process.wait()
        lifecycle['check'](work, cleanup=True)
        lifecycle['reap']()


if __name__ == '__main__':
    raise SystemExit(main())
