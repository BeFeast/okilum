#!/usr/bin/env python3
"""Require the rendered Settings label regression to reject the old fixed width."""
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / 'crates/okilum-shell/src/reader_settings.rs'
TEST = ('reader_settings::tests::'
        'issue2798_settings_navigation_fits_every_label_at_default_and_minimum_width')


def run(*args):
    result = subprocess.run(
        ['cargo', 'test', '--locked', '-p', 'okilum-shell', '--bin', 'okilum',
         TEST, '--', '--exact', *args],
        cwd=ROOT, capture_output=True, text=True, encoding='utf-8', timeout=1200)
    print(result.stdout, end='', flush=True)
    print(result.stderr, end='', flush=True)
    return result


def main():
    inventory = run('--list')
    assert inventory.returncode == 0 and f'{TEST}: test' in inventory.stdout
    original = SOURCE.read_bytes()
    fixed = b'.w(navigation_width(window))'
    assert original.count(fixed) == 1
    try:
        SOURCE.write_bytes(original.replace(fixed, b'.w(px(176.))', 1))
        negative = run('--nocapture', '--test-threads=1')
        assert negative.returncode != 0, 'old fixed sidebar width unexpectedly passed'
        assert 'test result: FAILED. 0 passed; 1 failed;' in negative.stdout
        assert 'must fit inside the button clip' in negative.stdout + negative.stderr
        print('SETTINGS_2798_OLD_WIDTH_RUNTIME_FAILURE_CONFIRMED', flush=True)
    finally:
        SOURCE.write_bytes(original)
        assert SOURCE.read_bytes() == original
    assert run('--nocapture', '--test-threads=1').returncode == 0
    print('SETTINGS_2798_RESTORED_LAYOUT_PASS', flush=True)


if __name__ == '__main__':
    main()
