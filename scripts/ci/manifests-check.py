#!/usr/bin/env python3
"""Require compiled runtime failures with the pre-#1859 creation/Board code."""
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]
BASE = '9acc73f7236a41d9b0b500c55ffa0f7c61921cc0'
TESTS = [
    'projects::create::tests::issue1859_new_project_toml_preserves_identity_icon_and_source',
    'board::manage::tests::issue1859_last_yaml_card_can_be_removed_without_converting_or_touching_other_fields',
]


def run(package, name, *args):
    command = ['cargo', 'test', '--locked', '-p', package]
    result = subprocess.run(command + [name, '--', *args], cwd=ROOT,
                            capture_output=True, text=True, timeout=1200)
    print(result.stdout, end='', flush=True)
    print(result.stderr, end='', flush=True)
    return result


def main():
    paths = [ROOT / 'crates/okilum-t3/src' / name for name in ('projects/create.rs', 'board/manage.rs')]
    originals = {path: path.read_bytes() for path in paths}
    try:
        for path in paths:
            path.write_bytes(subprocess.check_output(['git', 'show', f'{BASE}:{path.relative_to(ROOT).as_posix()}'], cwd=ROOT))
        for name in TESTS:
            inventory = run('okilum-t3', name, '--exact', '--list')
            assert inventory.returncode == 0, 'baseline must compile'
            assert f'{name}: test' in inventory.stdout, 'regression must be discovered'
            result = run('okilum-t3', name, '--exact', '--nocapture', '--test-threads=1')
            assert result.returncode != 0, 'pre-fix behavior unexpectedly passed'
            assert re.search(r'test result: FAILED\. 0 passed; 1 failed;', result.stdout), 'runtime assertion required'
            print(f'MANIFESTS_NEGATIVE_PASS {name}', flush=True)
    finally:
        for path, original in originals.items():
            path.write_bytes(original)
            assert path.read_bytes() == original
    assert run('okilum-t3', 'issue1859_', '--nocapture', '--test-threads=1').returncode == 0
    print('MANIFESTS_RESTORED_PASS', flush=True)


if __name__ == '__main__':
    main()
