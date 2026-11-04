#!/usr/bin/env python3
"""Real pinned-daemon lifetime controls; absence only counts after observing peers."""
import ctypes
from concurrent.futures import ThreadPoolExecutor
import os
from pathlib import Path
import runpy
import signal
import subprocess
import sys
import tempfile
import time

processes = runpy.run_path(str(Path(__file__).with_name('sync-fixture-processes.py')))
fixtures = processes['fixtures']
check = processes['check']
staging = runpy.run_path(str(Path(__file__).with_name('sync-fixture-stage.py')))


def parent_death_signal():
    parent = os.getpid()
    libc = ctypes.CDLL(None, use_errno=True)
    def register():
        if libc.prctl(1, signal.SIGKILL, 0, 0, 0) != 0:  # PR_SET_PDEATHSIG
            os._exit(1)
        if os.getppid() != parent:
            os._exit(1)
    return register


def interrupted(signum, _frame):
    raise SystemExit(128 + signum)


def subreaper():
    # Reap the actual daemons adopted when the test is SIGKILLed, even on runners
    # whose PID 1 does not reap. Fail rather than silently weakening the control.
    if ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(), 'PR_SET_CHILD_SUBREAPER')


def reap():
    try:
        while os.waitpid(-1, os.WNOHANG)[0]:
            pass
    except ChildProcessError:
        pass


def until(predicate, label, seconds=60):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(.05)
    raise AssertionError(f'timed out: {label}')


def control(executable, evidence, mode):
    work = evidence / mode
    work.mkdir()
    ready = work / 'ready'
    env = dict(os.environ, TMPDIR=str(work), OKILUM_SYNC_FIXTURE_READY=str(ready),
               OKILUM_SYNC_FIXTURE_CONTROL=mode)
    log = work / 'test.log'
    with log.open('w') as output:
        process = subprocess.Popen([str(executable), 'source_sharing::fixture_lifecycle_control',
                                    '--exact', '--ignored', '--nocapture'], env=env,
                                   stdout=output, stderr=subprocess.STDOUT,
                                   preexec_fn=parent_death_signal())
    try:
        until(lambda: ready.exists() or process.poll() is not None, f'{mode} ready')
        assert ready.exists(), log.read_text()
        pids = {int(pid) for pid in ready.read_text().split()}
        live = fixtures(work)
        assert len(pids) == 2 and {pid for pid, _ in live} == pids, live
        print(f'Lifecycle {mode}: positive control observed 2 live fixtures {sorted(pids)}', flush=True)
        homes = [home for _, home in live]
        if mode == 'normal':
            binary = Path(os.environ['OKILUM_SYNC_DIRECT_CLIENT'])
            data = staging['verified'](binary.read_bytes(), staging['BINARY_SHA256'])
            def publish_and_exec(_):
                staging['publish'](binary, data, 0o555)
                output = subprocess.check_output([str(binary), '--version'], text=True)
                assert output.startswith('syncthing v2.1.6 '), output
            with ThreadPoolExecutor(max_workers=4) as pool:
                list(pool.map(publish_and_exec, range(4)))
            assert {pid for pid, _ in fixtures(work)} == pids
            assert staging['stage'](binary.parent.parent) == binary
            print('Staging control: 4 concurrent publications + execs passed while 2 pinned daemons stayed live', flush=True)
        if mode == 'killed':
            process.kill()  # SIGKILL deliberately bypasses Rust Drop and unwinding.
        else:
            ready.with_suffix('.release').touch()
        rc = process.wait(timeout=70)
        expected = {'normal': 0, 'panic': 101, 'killed': -signal.SIGKILL}[mode]
        assert rc == expected, (rc, expected, log.read_text())
        if mode == 'panic':
            assert 'forced fixture lifecycle failure (#2498)' in log.read_text(), log.read_text()
        # No fallback cleanup before this assertion: it must test Drop/PDEATHSIG.
        until(lambda: not fixtures(work), f'{mode} fixtures exited', seconds=5)
        def reaped():
            reap()
            return all(not Path(f'/proc/{pid}').exists() for pid in pids)
        until(reaped, f'{mode} children reaped', seconds=5)
        print(f'Lifecycle {mode}: exit={rc} fixture processes=0; both children reaped', flush=True)
        return homes
    finally:
        if process.poll() is None:
            process.kill()
        process.wait()
        check(work, cleanup=True)
        reap()


def orphan_control(evidence, home):
    # Deliberately bypass the Rust lifetime helper: the CI guard must FAIL on a
    # real orphan, then clean it. Reuse the killed test's disposable config only.
    binary = os.environ['OKILUM_SYNC_DIRECT_CLIENT']
    with (evidence / 'orphan.log').open('w') as output:
        process = subprocess.Popen([binary, 'serve', '--no-browser', '--no-restart',
                                    '--no-upgrade', '--home', home],
                                   env=dict(HOME=home, STMONITORED='1', GOMAXPROCS='2'),
                                   stdout=output, stderr=subprocess.STDOUT)
    try:
        until(lambda: any(pid == process.pid for pid, _ in fixtures(evidence)), 'guard orphan observed')
        guard = subprocess.run([sys.executable, str(Path(__file__).with_name('sync-fixture-processes.py')),
                                '--root', str(evidence), '--cleanup'], check=False)
        assert guard.returncode == 1, 'guard accepted a surviving fixture'
        process.wait(timeout=5)
        assert not fixtures(evidence)
        print('Lifecycle guard: orphan positive control exit=1; fixture processes=0', flush=True)
    finally:
        if process.poll() is None:
            process.kill()
        process.wait()


def main():
    signal.signal(signal.SIGTERM, interrupted)
    subreaper()
    evidence = Path(tempfile.mkdtemp(prefix='sync-lifecycle-', dir=os.environ['TMPDIR']))
    print(f'Lifecycle evidence: {evidence}', flush=True)
    executable = Path(sys.argv[1]).resolve()
    try:
        # Own the replacement control's cache; never modify another job's binary.
        binary = evidence / 'okilum-sync-fixture' / staging['BINARY_SHA256'] / 'syncthing'
        data = staging['verified'](Path(os.environ['OKILUM_SYNC_DIRECT_CLIENT']).read_bytes(),
                                   staging['BINARY_SHA256'])
        staging['publish'](binary, data, 0o555)
        os.environ['OKILUM_SYNC_DIRECT_CLIENT'] = str(binary)
        for mode in ('normal', 'panic', 'killed'):
            homes = control(executable, evidence, mode)
        orphan_control(evidence, homes[0])
    finally:
        check(evidence, cleanup=True)
        reap()


if __name__ == '__main__':
    main()
