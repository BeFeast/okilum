#!/usr/bin/env python3
"""Validate committed observer-only refs and record exact native build inputs."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess

from capsule import CLOCK, HARNESS, ROOT, SOURCES, git


def sha(data):
    return hashlib.sha256(data).hexdigest()


def pe_stack(raw):
    offset = struct.unpack_from('<I', raw, 0x3c)[0] + 24
    assert struct.unpack_from('<H', raw, offset)[0] == 0x20b, 'PE32+ required'
    return struct.unpack_from('<QQ', raw, offset + 72)


def reference(kind, pins):
    assert pins['status'] == 'READY_FOR_CAPTURE_REVIEW', 'diagnostic source pin is not ready'
    assert pins[kind]['base_source'] == SOURCES[kind], 'base source pin mismatch'
    source = pins[kind]['diagnostic_commit']
    assert isinstance(source, str) and re.fullmatch('[0-9a-f]{40}', source), 'missing diagnostic source commit'
    return source


def validate_source(kind, source):
    base = SOURCES[kind]
    source = git('rev-parse', source).decode().strip()
    assert git('show', '-s', '--format=%P', source).decode().strip() == base
    paths = git('diff', '--name-only', base, source).decode().splitlines()
    assert sorted(paths) == sorted([HARNESS, CLOCK]), paths
    for path in paths:
        assert git('show', source + ':' + path) == (ROOT / path).read_bytes(), path
    return base, source, paths


def prepare(kind, source, destination, output):
    base, source, paths = validate_source(kind, source)
    subprocess.run(['git', 'worktree', 'add', '--detach', str(destination), source],
                   cwd=ROOT, check=True)
    output.mkdir(parents=True, exist_ok=False)
    inputs = {}
    for path in ['Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml',
                 'scripts/gpui-pre-windows/manifest.json',
                 'scripts/patches/gpui-pre-windows/0001-directx-atlas-upload.diff',
                 HARNESS, CLOCK, 'scripts/ci/code-toolbar-native-app.py']:
        raw = (destination / path).read_bytes()
        inputs[path] = sha(raw)
    assert 'channel = "1.100.0"' in (destination / 'rust-toolchain.toml').read_text()
    manifest = {'kind': kind, 'base_source': base, 'diagnostic_commit': source,
                'controller_commit': git('rev-parse', 'HEAD').decode().strip(),
                'observer_paths': paths, 'input_sha256': inputs,
                'build_command': 'cargo build --locked --release -p okilum-shell --bin okilum --features data-view-native-harness',
                'thresholds': {'open_wall_ms': 100, 'frame_wall_ms': 16}}
    (output / 'build-manifest.json').write_text(json.dumps(manifest, indent=2))
    (output / 'observer.diff').write_bytes(git('diff', '--binary', base, source))


def finish(destination, output):
    manifest = json.loads((output / 'build-manifest.json').read_text())
    assert git('rev-parse', 'HEAD', cwd=destination).decode().strip() == manifest['diagnostic_commit']
    assert not git('status', '--porcelain', '--untracked-files=no', cwd=destination).strip()
    binary = Path(os.environ['CARGO_TARGET_DIR']) / 'release/okilum.exe'
    raw = binary.read_bytes()
    assert pe_stack(raw) == (1048576, 4096), 'stack reserve/commit must remain unchanged'
    shutil.copy2(binary, output / 'okilum.exe')
    info = subprocess.check_output([str(binary), '--build-info'], text=True)
    assert manifest['diagnostic_commit'] in info and 'gpui-test-support: no' in info
    (output / 'build-info.txt').write_text(info)
    (output / 'backend-manifest.json').write_bytes((destination / 'scripts/gpui-pre-windows/manifest.json').read_bytes())
    manifest.update({'binary_sha256': sha(raw), 'binary_bytes': len(raw),
                     'stack_reserve': 1048576, 'stack_commit': 4096,
                     'rustc': subprocess.check_output(['rustc', '-Vv'], cwd=destination, text=True),
                     'cargo': subprocess.check_output(['cargo', '-V'], cwd=destination, text=True),
                     'build_environment': {k: os.environ[k] for k in [
                         'CARGO_INCREMENTAL', 'CARGO_BUILD_JOBS', 'CARGO_TARGET_DIR',
                         'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_PROFILE_RELEASE_DEBUG',
                         'CARGO_PROFILE_RELEASE_CODEGEN_UNITS', 'OKILUM_SOURCE_COMMIT',
                         'OKILUM_WINDOWS_ICON'] if k in os.environ}})
    assert manifest['rustc'].startswith('rustc 1.100.0 ')
    assert os.environ['OKILUM_SOURCE_COMMIT'] == manifest['diagnostic_commit']
    (output / 'build-manifest.json').write_text(json.dumps(manifest, indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['source', 'verify', 'prepare', 'finish'])
    parser.add_argument('--kind', choices=SOURCES)
    parser.add_argument('--source')
    parser.add_argument('--destination', type=Path)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    if args.command == 'source':
        print(reference(args.kind, json.loads((ROOT / 'scripts/ci/perf-2811-qpc/references.json').read_text())))
    elif args.command == 'verify':
        print(validate_source(args.kind, args.source)[1])
    elif args.command == 'prepare':
        prepare(args.kind, args.source, args.destination.resolve(), args.output.resolve())
    else:
        finish(args.destination.resolve(), args.output.resolve())
