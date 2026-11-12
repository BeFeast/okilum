#!/usr/bin/env python3
"""Bound a Linux test group, including daemonized descendants (#2503).

A plain timeout around setsid cannot signal the new session. Become a Linux
subreaper before starting a session, so double-forked daemons remain ours too.
Only descendants of this supervisor are ever signalled; no host-wide pkill.
"""
import argparse
import ctypes
import math
import os
from pathlib import Path
import signal
import shutil
import subprocess
import tempfile
import time

GRACE = 30


def descendants():
    """Return pid -> (parent, start ticks), tolerating concurrent process exit."""
    processes = {}
    for entry in Path('/proc').iterdir():
        if not entry.name.isdigit():
            continue
        try:
            fields = (entry / 'stat').read_text().rsplit(')', 1)[1].split()
            processes[int(entry.name)] = (int(fields[1]), fields[19])
        except (FileNotFoundError, ProcessLookupError):
            pass
    owned = {os.getpid()}
    while True:
        added = {pid for pid, (parent, _) in processes.items() if parent in owned} - owned
        if not added:
            break
        owned.update(added)
    return {pid: processes[pid] for pid in owned if pid != os.getpid()}


def send(pid, sig):
    try:
        os.kill(pid, sig)
    except ProcessLookupError:
        pass
    except PermissionError:
        # sync-network-policy uses sudo/unshare/runuser. Signal only the exact
        # privileged descendant discovered above, never unrelated runner work.
        subprocess.run(['sudo', '-n', 'kill', '-s', str(int(sig)), '--', str(pid)],
                       check=True, timeout=2, stdout=subprocess.DEVNULL)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--kill-after', type=float, default=GRACE,
                        help='TERM grace in seconds, at most 30 (default: 30)')
    parser.add_argument('--isolated-writer-account', action='store_true',
                        help='private inherited XDG state until every descendant is reaped')
    parser.add_argument('group')
    parser.add_argument('limit', type=float, help='positive wall-clock seconds')
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if not math.isfinite(args.limit) or args.limit <= 0 or not 0 < args.kill_after <= GRACE or not args.command:
        parser.error('require positive limit, grace in (0, 30], and a command')
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(), 'PR_SET_CHILD_SUBREAPER')
    cancelled = []
    for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(sig, lambda number, frame: cancelled.append(number))
    started = time.monotonic()
    print(f'TEST_GROUP_START group={args.group} limit={args.limit:g}s', flush=True)
    state = tempfile.mkdtemp(prefix='okilum-writer-account-') if args.isolated_writer_account else None
    options = dict(start_new_session=True)
    if state:
        options['env'] = dict(os.environ, XDG_STATE_HOME=state)
    process = subprocess.Popen(args.command, **options)
    stopping = None
    timed_out = False
    terminated = set()
    cleanup_kill_reported = False
    # Leave time to reap and return inside the limit + grace wall-clock budget.
    reap_reserve = min(.5, args.kill_after / 10)
    while True:
        result = process.poll()
        owned = descendants()
        # Reap adopted grandchildren, but let Popen collect its direct child.
        for pid in owned:
            if pid != process.pid:
                try:
                    os.waitpid(pid, os.WNOHANG)
                except ChildProcessError:
                    pass
        now = time.monotonic()
        if stopping is None and now - started >= args.limit:
            timed_out = True
            stopping = started + args.limit
            print(f'TEST_GROUP_TIMEOUT group={args.group} limit={args.limit:g}s kill_after={args.kill_after:g}s',
                  flush=True)
        if stopping is None and (cancelled or result is not None):
            stopping = now
        if stopping is not None:
            sig = signal.SIGKILL if now >= stopping + args.kill_after - reap_reserve else signal.SIGTERM
            if sig == signal.SIGKILL and owned and not timed_out and not cleanup_kill_reported:
                print(f'TEST_GROUP_KILL group={args.group} limit={args.limit:g}s reason=descendant-cleanup',
                      flush=True)
                cleanup_kill_reported = True
            for pid, (_, ticks) in owned.items():
                identity = (pid, ticks)
                if sig == signal.SIGKILL or identity not in terminated:
                    send(pid, sig)
                    terminated.add(identity)
        if result is not None and not owned:
            break
        time.sleep(.05)
    if state:
        bindings = len(list((Path(state) / 'okilum/local-writers-v1').glob('*.json')))
        # Only this newly allocated CI account is removed, after the same
        # subreaper has collected every child that could still use it.
        shutil.rmtree(state)
        print(f'TEST_GROUP_WRITER_ACCOUNT group={args.group} bindings={bindings} cleanup=after-reap',
              flush=True)
    status = 124 if timed_out else 128 + cancelled[0] if cancelled else result
    if status < 0:
        status = 128 - status
    print(f'TEST_GROUP_END group={args.group} elapsed={time.monotonic()-started:.3f}s status={status}',
          flush=True)
    return status


if __name__ == '__main__':
    raise SystemExit(main())
