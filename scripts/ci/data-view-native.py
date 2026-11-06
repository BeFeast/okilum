#!/usr/bin/env python3
"""Require native data-view inventory, runtime regressions and a fired control."""
import pathlib
import subprocess


def run(*args):
    result = subprocess.run(args, text=True, encoding="utf-8", stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    print(result.stdout, flush=True)
    return result


base = ['cargo', 'test', '--locked', '-p', 'okilum-shell', '--bin', 'okilum']
listing = run(*base, 'data_view_2669_', '--', '--list')
assert listing.returncode == 0
names = [line for line in listing.stdout.splitlines() if line.endswith(': test')]
assert len(names) == 8, names
assert run(*base, 'data_view_2669_', '--', '--nocapture', '--test-threads=1').returncode == 0
source = pathlib.Path('crates/okilum-shell/src/reader_code_file.rs')
positive = source.read_bytes()
assert positive.count(b'.folding(true)') == 1
try:
    source.write_bytes(positive.replace(b'.folding(true)', b'.folding(false)'))
    # A compile failure or a missing test must never count as the control firing.
    negative = run(*base, 'data_view_2669_native_fold_copy_find_and_readonly_source', '--', '--nocapture', '--test-threads=1')
    assert negative.returncode != 0
    assert 'test result: FAILED' in negative.stdout and 'native XML grammar supplies folds' in negative.stdout
    print('DATA_VIEW_2669_COMPILED_DISABLED_FOLD_CONTROL_FIRED', flush=True)
finally:
    source.write_bytes(positive)
assert run(*base, 'data_view_2669_', '--', '--nocapture', '--test-threads=1').returncode == 0
print('DATA_VIEW_2669_RESTORED_NATIVE_SUITE_PASS', flush=True)
