#!/usr/bin/env python3
"""Apply the same diagnostic-only native observer to two exact pinned sources."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[3]
SOURCES = {
    'main': '718d23d8b799bd50569ba0b8ff40e784afb4ed0c',
    'toolbar': '680c230dbcca8ada498cd612ba326f94119540ae',
}
REFS = {kind: f'diagnostic/2811-native-{kind}-{base[:8]}' for kind, base in SOURCES.items()}
HARNESS = 'crates/okilum-shell/src/reader_code_file/toolbar_native.rs'
CLOCK = 'crates/okilum-shell/src/reader_code_file/toolbar_native/qpc_clock.rs'
ORIGINAL_HARNESS_SHA = '265473b112ec73a1cd68eeda723f5d8792b047f40491f5015c7a3a4f80f90a49'


def git(*args, cwd=ROOT):
    return subprocess.check_output(['git', *args], cwd=cwd)


def prepare(kind, destination, manifest_path):
    destination = destination.resolve()
    assert not destination.exists(), 'use a new disposable capsule path'
    assert not git('status', '--porcelain').strip(), 'commit the reviewed controller first'
    pins = json.loads((ROOT / 'scripts/ci/perf-2811-qpc/references.json').read_text())
    assert pins['status'] == 'BASES_PINNED', 'wait for manager final source pin before creating capsules'
    assert pins[kind]['base_source'] == SOURCES[kind], 'controller source pin mismatch'
    source = SOURCES[kind]
    original = git('show', source + ':' + HARNESS)
    assert hashlib.sha256(original).hexdigest() == ORIGINAL_HARNESS_SHA
    subprocess.run(['git', 'worktree', 'add', '-b', REFS[kind], str(destination), source],
                   cwd=ROOT, check=True)
    changes = []
    for relative in [HARNESS, CLOCK]:
        path = destination / relative
        before = path.read_bytes() if path.exists() else b''
        after = (ROOT / relative).read_bytes()
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(after)
        changes.append({'path': relative,
                        'before_sha256': hashlib.sha256(before).hexdigest(),
                        'after_sha256': hashlib.sha256(after).hexdigest()})
    manifest = {
        'base_source': source,
        'diagnostic_ref': REFS[kind],
        'diagnostic_commit': None,
        'requires_local_observer_commit_before_build': True,
        'controller_commit': git('rev-parse', 'HEAD').decode().strip(),
        'controller_dirty': bool(git('status', '--porcelain').strip()),
        'kind': kind,
        'changed_files': changes,
        'required_toolchain': '1.100.0',
        'build_command': 'cargo build --locked --release -p okilum-shell --bin okilum --features data-view-native-harness',
        'source_commit_environment': 'actual diagnostic git rev-parse HEAD after observer commit; NEVER base_source',
        'thresholds': {'open_wall_ms': 100, 'frame_wall_ms': 16},
        'rules': 'diagnostic observer only; original assertions, priority, stack, backend, editor and shaping unchanged',
    }
    # Keep preparation evidence outside the capsule, so a source-only observer
    # commit can be clean. The build controller must add its actual commit SHA
    # to this evidence and verify the two committed diff paths before building.
    manifest_path.write_text(json.dumps(manifest, indent=2))
    print(json.dumps(manifest, indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('kind', choices=SOURCES)
    parser.add_argument('destination', type=Path)
    parser.add_argument('--manifest', type=Path, required=True)
    args = parser.parse_args()
    prepare(args.kind, args.destination, args.manifest)
