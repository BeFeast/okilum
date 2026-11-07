#!/usr/bin/env python3
"""Require native runtime failures for both #2684 and #2698 old parser paths."""
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
COMMAND = ['cargo', 'test', '--locked', '-p', 'okilum-shell', '--bin', 'okilum',
           'math_2684_', '--']


def run(*args):
    result = subprocess.run(COMMAND + list(args), cwd=ROOT, capture_output=True,
                            text=True, timeout=1200)
    print(result.stdout, end='', flush=True)
    print(result.stderr, end='', flush=True)
    return result


def main():
    path = ROOT / 'crates/okilum-core/src/tex.rs'
    original = path.read_bytes()
    controls = [
        ('ENVIRONMENT', b'"begin" => self.environment()?,',
         b'"begin" if false => self.environment()?, '),
        ('CONTROL_SPACE', rb'" " | "\n" | "\r" | "\t" => 6,', b'" " => 6,'),
    ]
    for name, before, old in controls:
        assert original.count(before) == 1
        try:
            path.write_bytes(original.replace(before, old, 1))
            inventory = run('--list')
            assert inventory.returncode == 0, 'negative controls must compile'
            assert inventory.stdout.count(': test\n') == 2, 'both native controls must be discovered'
            result = run('--nocapture', '--test-threads=1')
            assert result.returncode != 0
            assert 'test result: FAILED. 0 passed; 2 failed;' in result.stdout, (
                'require two actual runtime failures, never compile/infra failures')
            print(f'MATH_{name}_NEGATIVE_PASS', flush=True)
        finally:
            path.write_bytes(original)
        assert run('--nocapture', '--test-threads=1').returncode == 0
        print(f'MATH_{name}_RESTORED_PASS', flush=True)


if __name__ == '__main__':
    main()
