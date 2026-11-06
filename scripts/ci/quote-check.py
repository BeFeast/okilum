#!/usr/bin/env python3
"""Require compiled regression failures with the inactive Quote command (#2625)."""
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]
COMMAND = ['cargo', 'test', '--locked', '-p', 'okilum-shell', '--bin', 'okilum']
PREFIX = 'reader_editor::format::key_tests::quote_2625_'


def run(name, *args):
    result = subprocess.run(COMMAND + [name, '--', *args], cwd=ROOT,
                            capture_output=True, text=True, timeout=1200)
    print(result.stdout, end='', flush=True)
    print(result.stderr, end='', flush=True)
    return result


def main():
    path = ROOT / 'crates/okilum-shell/src/reader_editor/format.rs'
    original = path.read_bytes()
    try:
        text = original.decode()
        command = '                this.format_command(quote, window, cx)'
        assert text.count(command) == 1, 'expected one Quote handler'
        path.write_bytes(text.replace(command, '                let _ = (this, window, cx);').encode())
        for suffix in ('keyboard_source_live_toggle_and_undo',
                       'menu_source_live_toggle_and_undo'):
            name = PREFIX + suffix
            inventory = run(name, '--exact', '--list')
            assert inventory.returncode == 0, 'negative control must compile'
            assert f'{name}: test' in inventory.stdout, 'negative control must be discovered'
            result = run(name, '--exact', '--nocapture', '--test-threads=1')
            assert result.returncode != 0, 'inactive Quote unexpectedly passed'
            assert re.search(r'test result: FAILED\. 0 passed; 1 failed;', result.stdout), (
                'expected one runtime assertion failure, not a compiler/infra failure')
            print(f'QUOTE_NEGATIVE_PASS {name}', flush=True)
    finally:
        path.write_bytes(original)
        assert path.read_bytes() == original
    inventory = run('quote_2625_', '--list')
    assert inventory.returncode == 0
    assert len(re.findall(r'^.*quote_2625_.*: test$', inventory.stdout, re.M)) >= 5
    assert run('quote_2625_', '--nocapture', '--test-threads=1').returncode == 0
    print('QUOTE_RESTORED_PASS', flush=True)


if __name__ == '__main__':
    main()
