#!/usr/bin/env python3
"""Prove the real Ready-verification click regression against pre-fix bridge."""
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[3]
BASELINE = '2ad69383345eee330569070b935d242cad7b6b52'  # main before #2586


def main():
    bridge = ROOT / 'crates/okilum-shell/src/updater/bridge.m'
    original = bridge.read_bytes()
    if subprocess.run(['git', 'cat-file', '-e', BASELINE], cwd=ROOT, stderr=subprocess.DEVNULL).returncode:
        subprocess.run(['git', 'fetch', '--depth=1', 'origin', BASELINE], cwd=ROOT, check=True)
    baseline = subprocess.check_output(['git', 'show', f'{BASELINE}:crates/okilum-shell/src/updater/bridge.m'], cwd=ROOT)
    evidence = Path(os.environ['RUNNER_TEMP']) / 'sparkle-freshness-evidence'
    try:
        bridge.write_bytes(baseline)
        result = subprocess.run([sys.executable, str(Path(__file__).with_name('run.py')), sys.argv[1]],
                                env={**os.environ, 'SPARKLE_SCENARIOS': 'restart-verifying'}, cwd=ROOT)
        assert result.returncode != 0, 'Pre-fix bridge unexpectedly accepted the early Restart click'
        trace = (evidence / 'restart-verifying.trace').read_text()
        # Compilation/setup failure is not a reproduced regression. Require the
        # actual archive, Ready verification and click, followed by ignored intent.
        for marker in ('downloaded 2', 'prepared 2', 'restart clicked during real Ready verification', 'verified-ready 2'):
            assert marker in trace, trace
        assert 'final editor guard boundary' not in trace and 'report 13 ' not in trace, trace
        for path in list(evidence.iterdir()):
            path.rename(path.with_name('negative-control-' + path.name))
        print('PASS negative control: real Sparkle Ready-verification click was silently discarded before #2586', flush=True)
    finally:
        bridge.write_bytes(original)


if __name__ == '__main__':
    main()
