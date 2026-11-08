#!/usr/bin/env python3
"""Crash in the actual capture gap; removing gap recovery must fail there."""
import os
from pathlib import Path
import runpy
import subprocess
import tempfile

lifecycle = runpy.run_path(str(Path(__file__).with_name('sync-fixture-lifecycle.py')))
source = Path('crates/okilum-sync-controller/src/direct/inventory/merge/storage.rs')
original = source.read_text()
needle = 'if recovering {'
assert original.count(needle) == 1
work = Path(tempfile.mkdtemp(prefix='sync-first-merge-negative-', dir=os.environ['TMPDIR']))
(work / 'state').mkdir(mode=0o700)
(work / 'tmp').mkdir()
lifecycle['subreaper']()
try:
    source.write_text(original.replace(needle, 'if false && recovering {'))
    result = subprocess.run([
        'cargo', 'test', '--locked', '-p', 'okilum-sync-controller',
        '--features', 'first-sync-fixture', '--test', 'managed_launch',
        'issue2043_crash_recovery_retains_versions_and_rejects_third_version',
        '--', '--exact', '--ignored', '--nocapture'],
        env=dict(os.environ, TMPDIR=str(work / 'tmp'), XDG_STATE_HOME=str(work / 'state'),
                 OKILUM_FIRST_SYNC_CRASH_CASE='source-captured'),
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=300)
    (work / 'negative-control.log').write_text(result.stdout)
    print(result.stdout, flush=True)
    assert result.returncode != 0 and 'test result: FAILED. 0 passed; 1 failed;' in result.stdout, result.stdout
    assert 'recover checkpoint source-captured' in result.stdout, result.stdout
    assert 'files changed; refresh first Sync preview' in result.stdout, result.stdout
    assert not lifecycle['check'](work), 'surviving fixture process'
    lifecycle['reap']()
    print('FIRST_SYNC_NEGATIVE_CONTROL_PASS: compiled bypass failed actual capture-gap recovery', flush=True)
finally:
    source.write_text(original)
    lifecycle['check'](work, cleanup=True)
    lifecycle['reap']()
