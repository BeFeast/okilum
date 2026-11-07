#!/usr/bin/env python3
"""Compile the reported #2684 controls with the previous unsupported-begin path."""
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
    # Keep the parser implementation compiled (including warnings-as-errors)
    # while reproducing the old renderer's rejection of every \\begin token.
    before = b'"begin" => self.environment()?,'
    assert original.count(before) == 1
    try:
        path.write_bytes(original.replace(before, b'"begin" if false => self.environment()?,', 1))
        inventory = run('--list')
        assert inventory.returncode == 0, 'negative controls must compile'
        assert inventory.stdout.count(': test\n') == 2, 'both native controls must be discovered'
        result = run('--nocapture', '--test-threads=1')
        assert result.returncode != 0
        assert 'test result: FAILED. 0 passed; 2 failed;' in result.stdout, (
            'require two actual runtime failures, never compile/infra failures')
        print('MATH_ENVIRONMENT_NEGATIVE_PASS', flush=True)
    finally:
        path.write_bytes(original)
    assert run('--nocapture', '--test-threads=1').returncode == 0
    print('MATH_ENVIRONMENT_RESTORED_PASS', flush=True)


if __name__ == '__main__':
    main()
