#!/usr/bin/env python3
"""The actual delayed-PATCH regression must fail when local admission is removed."""
import os
from pathlib import Path
import runpy
import subprocess
import tempfile

lifecycle = runpy.run_path(str(Path(__file__).with_name('sync-fixture-lifecycle.py')))
source = Path('crates/okilum-sync-controller/src/admission.rs')
original = source.read_text()
needle = 'Some(okilum_write_authority::operation(root)?)'
assert needle in original
work = Path(tempfile.mkdtemp(prefix='sync-admission-negative-', dir=os.environ['TMPDIR']))
(work / 'state').mkdir(mode=0o700)
(work / 'tmp').mkdir()
lifecycle['subreaper']()
try:
    source.write_text(original.replace(needle, 'None'))
    result = subprocess.run([
        'cargo', 'test', '--locked', '-p', 'okilum-sync-controller', '--test', 'managed_launch',
        'issue2582_configuration_process_drains_and_later_real_effects_are_refused',
        '--', '--exact', '--nocapture'],
        env=dict(os.environ, TMPDIR=str(work / 'tmp'), XDG_STATE_HOME=str(work / 'state')),
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=300)
    (work / 'negative-control.log').write_text(result.stdout)
    print(result.stdout, flush=True)
    assert result.returncode != 0 and 'test result: FAILED. 0 passed; 1 failed;' in result.stdout, result.stdout
    assert 'configuration lease must survive until acknowledgement/readback' in result.stdout, result.stdout
    assert not lifecycle['check'](work), 'surviving fixture process'
    lifecycle['reap']()
    print('MANAGED_ADMISSION_NEGATIVE_CONTROL_PASS: compiled bypass failed the real drain assertion', flush=True)
finally:
    source.write_text(original)
    lifecycle['check'](work, cleanup=True)
    lifecycle['reap']()
