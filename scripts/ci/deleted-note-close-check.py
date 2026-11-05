#!/usr/bin/env python3
"""Native regression controls for the deleted-note Close/Quit wiring (#2651).

Run only on a compiler runner. Mutate only the old bypasses, require compiled
assertion failures, restore the exact source bytes, then rerun all close tests.
"""
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]
PREFIX = 'reader_deleted_note::close_tests::'
COMMAND = ['cargo', 'test', '--locked', '-p', 'okilum-shell', '--bin', 'okilum']


def run(name, *args):
    result = subprocess.run(COMMAND + [name, '--', *args], cwd=ROOT,
                            capture_output=True, text=True, timeout=1200)
    print(result.stdout, end='', flush=True)
    print(result.stderr, end='', flush=True)
    return result


def main():
    paths = [ROOT / 'crates/okilum-shell/src' / name for name in
             ('reader_open.rs', 'reader_app_menu.rs', 'brain/app_quit.rs')]
    original = {path: path.read_bytes() for path in paths}
    try:
        text = original[paths[0]].decode()
        needle = 'reader.prepare_window_close(window, cx)'
        assert text.count(needle) == 1, 'native close hook not found exactly once'
        paths[0].write_text(text.replace(needle, 'reader.save_source_for_close(cx)'))
        for path, callback in zip(paths[1:], ('request_quit', 'request')):
            text = original[path].decode()
            needle = (f'    if !crate::reader_editor::confirm_deleted_for_quit({callback}, cx) {{\n'
                      '        return;\n    }\n')
            assert text.count(needle) == 1, f'Quit hook not found exactly once: {path.name}'
            path.write_text(text.replace(needle, ''))
        for suffix in ('read_close_offers_choices_and_only_explicit_discard_closes',
                       'source_keyboard_quit_offers_choices'):
            name = PREFIX + suffix
            inventory = run(name, '--exact', '--list')
            assert inventory.returncode == 0, 'negative control must compile'
            assert f'{name}: test' in inventory.stdout, 'negative control must be discovered'
            result = run(name, '--exact', '--nocapture', '--test-threads=1')
            assert result.returncode != 0, 'old Close/Quit bypass unexpectedly passed'
            assert re.search(r'test result: FAILED\. 0 passed; 1 failed;', result.stdout), (
                'expected one runtime assertion failure, not a compiler/infra failure')
            assert name in result.stdout, 'wrong negative-control test'
            print(f'DELETED_CLOSE_NEGATIVE_PASS {name}', flush=True)
    finally:
        for path, data in original.items():
            path.write_bytes(data)
        assert all(path.read_bytes() == data for path, data in original.items())
    inventory = run(PREFIX, '--list')
    assert inventory.returncode == 0, 'restored production must compile'
    assert len(re.findall(r'^' + re.escape(PREFIX) + r'.*: test$', inventory.stdout, re.M)) >= 9
    result = run(PREFIX, '--nocapture', '--test-threads=1')
    assert result.returncode == 0, 'restored production controls must pass'
    print('DELETED_CLOSE_RESTORED_PASS', flush=True)


if __name__ == '__main__':
    main()
