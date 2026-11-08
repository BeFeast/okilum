#!/usr/bin/env python3
"""Requiring the old inspection capability must fail a real expired-review merge."""
import os
from pathlib import Path
import runpy
import subprocess
import tempfile

lifecycle = runpy.run_path(str(Path(__file__).with_name('sync-fixture-lifecycle.py')))
source = Path('crates/okilum-sync-controller/src/direct/inventory.rs')
original = source.read_text()
needle = '    fn renew_first_sync_preview(&self, preview: &mut Preview) -> Result<()> {'
assert original.count(needle) == 1
work = Path(tempfile.mkdtemp(prefix='sync-first-expiry-negative-', dir=os.environ['TMPDIR']))
(work / 'state').mkdir(mode=0o700)
(work / 'tmp').mkdir()
lifecycle['subreaper']()
try:
    # Restore the old requirement before the new explicit reopening. The test
    # expires the real source cache and proves ordinary validation refuses it.
    source.write_text(original.replace(needle, needle + '\n        self.validate_remote_preview(preview)?;'))
    result = subprocess.run([
        'cargo', 'test', '--locked', '-p', 'okilum-sync-controller',
        '--features', 'first-sync-fixture', '--test', 'managed_launch',
        'issue2043_two_managed_instances_preview_union_preservation_and_bidirectional_handoff',
        '--', '--exact', '--ignored', '--nocapture'],
        env=dict(os.environ, TMPDIR=str(work / 'tmp'), XDG_STATE_HOME=str(work / 'state')),
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=300)
    (work / 'negative-control.log').write_text(result.stdout)
    print(result.stdout, flush=True)
    assert result.returncode != 0 and 'test result: FAILED. 0 passed; 1 failed;' in result.stdout, result.stdout
    assert 'first merge: Stale' in result.stdout, result.stdout
    assert 'FIRST_SYNC_EXPIRED_REVIEW_PASS' not in result.stdout, result.stdout
    assert not lifecycle['check'](work), 'surviving expired-review fixture process'
    lifecycle['reap']()
    print('FIRST_SYNC_EXPIRY_NEGATIVE_CONTROL_PASS: old capability requirement failed the actual expired-review merge', flush=True)
finally:
    source.write_text(original)
    lifecycle['check'](work, cleanup=True)
    lifecycle['reap']()
