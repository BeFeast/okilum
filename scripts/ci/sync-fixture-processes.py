#!/usr/bin/env python3
"""Fail on surviving pinned fixture daemons, optionally cleaning this job only."""
import argparse
import os
from pathlib import Path
import signal
import time


def fixtures(root):
    root = Path(root).resolve()
    found = []
    for entry in Path('/proc').iterdir():
        if not entry.name.isdecimal():
            continue
        try:
            args = (entry / 'cmdline').read_bytes().split(b'\0')
            executable = os.readlink(entry / 'exe')
            if '/okilum-sync-fixture/' not in executable or Path(executable).name != 'syncthing':
                continue
            home = Path(os.fsdecode(args[args.index(b'--home') + 1])).resolve()
            if home.is_relative_to(root):
                found.append((int(entry.name), str(home)))
        except (FileNotFoundError, ProcessLookupError, PermissionError, ValueError, IndexError):
            continue
    return found


def check(root, cleanup=False):
    survivors = fixtures(root)
    print(f'Fixture guard: root={root} survivors={len(survivors)}', flush=True)
    for pid, home in survivors:
        print(f'ORPHAN fixture pid={pid} home={home}', flush=True)
    if cleanup:
        for pid, home in survivors:
            # Revalidate ownership immediately before signaling. Never kill by
            # binary name, cache path alone, or another job's fixture home.
            if (pid, home) in fixtures(root):
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
        deadline = time.monotonic() + 5
        while fixtures(root) and time.monotonic() < deadline:
            time.sleep(.05)
        remaining = fixtures(root)
        print(f'Fixture cleanup: remaining={len(remaining)}', flush=True)
        return int(bool(survivors or remaining))
    return int(bool(survivors))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', required=True, type=Path)
    parser.add_argument('--cleanup', action='store_true')
    args = parser.parse_args()
    return check(args.root, args.cleanup)


if __name__ == '__main__':
    raise SystemExit(main())
