#!/usr/bin/env python3
"""Pin capture-only work to four immutable binaries from the reviewed producer."""
import argparse
import hashlib
import json
from pathlib import Path
import sys

from capsule import ROOT, git
from provenance import binary_metadata, native_binary


def verify(inputs, out, platform):
    config = json.loads((ROOT / 'scripts/ci/perf-2811-qpc/capture-inputs.json').read_text())
    out.mkdir(parents=True, exist_ok=True)
    receipt = {'producer_run': config['producer_run'], 'producer_controller': config['producer_controller'],
               'capture_controller': git('rev-parse', 'HEAD').decode().strip(),
               'capture_attempt': config['capture_attempt'], 'setup_attempt': config['setup_attempt'],
               'failed_setup': config['failed_setup'], 'platform': platform, 'inputs_verified': False,
               'binary_sha256': {}, 'no_rebuild': True}
    path = out / 'capture-provenance.json'
    path.write_text(json.dumps(receipt, indent=2))
    try:
        if platform == 'macOS':
            from runner import inherited_process_state
            receipt['inherited_parent'] = inherited_process_state()
            receipt['guard_host_platform'] = sys.platform
            path.write_text(json.dumps(receipt, indent=2))
        assert receipt['producer_controller'] != receipt['capture_controller']
        assert receipt['capture_attempt'] == 1 and receipt['setup_attempt'] == 2
        for kind, expected in config['artifacts'][platform].items():
            folder = inputs / kind
            manifest = json.loads((folder / 'build-manifest.json').read_text())
            assert manifest['controller_commit'] == config['producer_controller']
            assert manifest['diagnostic_commit'] == expected['diagnostic_commit']
            assert manifest['base_source'] == expected['base_source']
            binary = native_binary(folder)
            digest = hashlib.sha256(binary.read_bytes()).hexdigest()
            assert digest == manifest['binary_sha256'] == expected['binary_sha256']
            binary_metadata(binary.read_bytes())  # unchanged PE/Mach-O stack policy
            assert hashlib.sha256((folder / 'observer.diff').read_bytes()).hexdigest() == config['observer_patch_sha256']
            receipt['binary_sha256'][kind] = digest
        receipt['inputs_verified'] = True
    except Exception as error:
        receipt['preflight_error'] = repr(error)
        raise
    finally:
        path.write_text(json.dumps(receipt, indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--inputs', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--platform', choices=['Windows', 'macOS'], required=True)
    args = parser.parse_args()
    verify(args.inputs.resolve(), args.out.resolve(), args.platform)
