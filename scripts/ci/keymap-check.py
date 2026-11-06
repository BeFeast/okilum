#!/usr/bin/env python3
"""Require compiled regression failures with the ignored user keymap (#2626)."""
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]
COMMAND = ['cargo', 'test', '--locked', '-p', 'okilum-shell', '--bin', 'okilum']
PREFIX = 'reader_keymap::tests::keymap_2626_'


def run(name, *args):
    result = subprocess.run(COMMAND + [name, '--', *args], cwd=ROOT,
                            capture_output=True, text=True, timeout=1200)
    print(result.stdout, end='', flush=True)
    print(result.stderr, end='', flush=True)
    return result


def main():
    path = ROOT / 'crates/okilum-shell/src/reader_keymap.rs'
    original = path.read_bytes()
    try:
        text = original.decode()
        for registration in ('            cx.clear_key_bindings();\n',
                             '            cx.bind_keys(effective);\n'):
            assert text.count(registration) == 1, 'expected one live-keymap update'
            text = text.replace(registration, '')
        path.write_bytes(text.encode())
        for suffix in ('overrides_null_aliases_and_multiple_bindings_preserve_other_contexts',
                       'record_cancel_reassign_dispatch_reset_and_search_use_the_live_map'):
            name = PREFIX + suffix
            inventory = run(name, '--exact', '--list')
            assert inventory.returncode == 0, 'negative control must compile'
            assert f'{name}: test' in inventory.stdout, 'negative control must be discovered'
            result = run(name, '--exact', '--nocapture', '--test-threads=1')
            assert result.returncode != 0, 'ignored user keymap unexpectedly passed'
            assert re.search(r'test result: FAILED\. 0 passed; 1 failed;', result.stdout), (
                'expected one runtime assertion failure, not a compiler/infra failure')
            print(f'KEYMAP_NEGATIVE_PASS {name}', flush=True)
    finally:
        path.write_bytes(original)
        assert path.read_bytes() == original
    inventory = run('keymap_2626_', '--list')
    assert inventory.returncode == 0
    assert len(re.findall(r'^.*keymap_2626_.*: test$', inventory.stdout, re.M)) >= 6
    assert run('keymap_2626_', '--nocapture', '--test-threads=1').returncode == 0
    print('KEYMAP_RESTORED_PASS', flush=True)


if __name__ == '__main__':
    main()
