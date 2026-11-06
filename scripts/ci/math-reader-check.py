#!/usr/bin/env python3
"""Require compiled regression failures with the old Read math fallback (#2596)."""
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]
COMMAND = ['cargo', 'test', '--locked', '-p', 'okilum-shell', '--bin', 'okilum']
PREFIX = 'reader_obsidian::tests::math_2596_read_'


def run(name, *args):
    result = subprocess.run(COMMAND + [name, '--', *args], cwd=ROOT,
                            capture_output=True, text=True, timeout=1200)
    print(result.stdout, end='', flush=True)
    print(result.stderr, end='', flush=True)
    return result


def main():
    path = ROOT / 'crates/okilum-shell/src/main.rs'
    original = path.read_bytes()
    try:
        text = original.decode()
        for registration in (
            '    .markdown_inline_parser(reader_obsidian::parse_inline_math)\n',
            '    .markdown_block_renderer("math", reader_obsidian::render_math)\n',
        ):
            assert text.count(registration) == 1, 'expected one renderer registration'
            text = text.replace(registration, '')
        path.write_bytes(text.encode())
        for suffix in ('inline_uses_the_live_renderer_and_keeps_source',
                       'display_uses_two_dimensional_layout_and_reports_errors'):
            name = PREFIX + suffix
            inventory = run(name, '--exact', '--list')
            assert inventory.returncode == 0, 'negative control must compile'
            assert f'{name}: test' in inventory.stdout, 'negative control must be discovered'
            result = run(name, '--exact', '--nocapture', '--test-threads=1')
            assert result.returncode != 0, 'old raw-TeX fallback unexpectedly passed'
            assert re.search(r'test result: FAILED\. 0 passed; 1 failed;', result.stdout), (
                'expected one runtime assertion failure, not a compiler/infra failure')
            print(f'MATH_READER_NEGATIVE_PASS {name}', flush=True)
    finally:
        path.write_bytes(original)
        assert path.read_bytes() == original
    inventory = run('math_2596_', '--list')
    assert inventory.returncode == 0
    assert len(re.findall(r'^.*math_2596_.*: test$', inventory.stdout, re.M)) >= 4
    assert run('math_2596_', '--nocapture', '--test-threads=1').returncode == 0
    print('MATH_READER_RESTORED_PASS', flush=True)


if __name__ == '__main__':
    main()
