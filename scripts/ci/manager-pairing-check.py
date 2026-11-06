#!/usr/bin/env python3
"""Require an actual clipboard assertion failure with Copy command disabled."""
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]
TEST = 'reader_t3_settings::pairing_help::tests::manager_2523_copy_button_copies_the_displayed_servers_command'


def run(package, name, *args):
    command = ['cargo', 'test', '--locked', '-p', package]
    if package == 'okilum-shell':
        command += ['--bin', 'okilum']
    result = subprocess.run(command + [name, '--', *args], cwd=ROOT,
                            capture_output=True, text=True, timeout=1200)
    print(result.stdout, end='', flush=True)
    print(result.stderr, end='', flush=True)
    return result


def main():
    path = ROOT / 'crates/okilum-shell/src/reader_t3_settings/pairing_help.rs'
    original = path.read_bytes()
    try:
        source = original.decode()
        write = 'cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))'
        assert source.count(write) == 1
        path.write_bytes(source.replace(write, 'let _ = (&copy, cx);').encode())
        inventory = run('okilum-shell', TEST, '--exact', '--list')
        assert inventory.returncode == 0 and f'{TEST}: test' in inventory.stdout
        result = run('okilum-shell', TEST, '--exact', '--nocapture', '--test-threads=1')
        assert result.returncode != 0
        assert re.search(r'test result: FAILED\. 0 passed; 1 failed;', result.stdout), 'compiled assertion failure required'
        print('PAIRING_COMMAND_NEGATIVE_PASS', flush=True)
    finally:
        path.write_bytes(original)
        assert path.read_bytes() == original
    assert run('okilum-shell', 'manager_2523_', '--nocapture', '--test-threads=1').returncode == 0
    print('PAIRING_COMMAND_RESTORED_PASS', flush=True)


if __name__ == '__main__':
    main()
