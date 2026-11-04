#!/usr/bin/env python3
"""Repeat the actual shell reveal test, with an independent process deadline."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
TEST = 'platform::reveal::tests::show_items_selects_the_item_and_fails_without_a_file_manager'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runs', type=int, default=20)
    args = parser.parse_args()
    assert args.runs >= 20
    build = subprocess.run(['cargo', 'test', '--offline', '--locked', '-p', 'okilum-shell',
                            '--bin', 'okilum', '--no-run', '--message-format=json'],
                           check=True, stdout=subprocess.PIPE, text=True)
    binaries = [message['executable'] for line in build.stdout.splitlines()
                if (message := json.loads(line)).get('reason') == 'compiler-artifact'
                and message.get('executable') and message['profile']['test']]
    assert len(binaries) == 1, binaries
    timings = []
    for index in range(args.runs):
        started = time.monotonic()
        result = subprocess.run([sys.executable, str(ROOT / 'scripts/ci/test-group.py'),
                                 f'reveal-{index + 1}', '20', binaries[0], TEST, '--exact', '--nocapture'],
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        print(result.stdout, end='', flush=True)
        result.check_returncode()
        assert '1 passed; 0 failed' in result.stdout, 'reveal test did not execute'
        assert 'reveal: receiving positive control' in result.stdout, 'fake manager did not answer'
        assert 'reveal: private dbus-daemon reaped' in result.stdout, 'bus cleanup did not execute'
        timings.append(time.monotonic() - started)
    print(f'REVEAL_STRESS runs={len(timings)} hangs=0 max={max(timings):.3f}s', flush=True)


if __name__ == '__main__':
    main()
