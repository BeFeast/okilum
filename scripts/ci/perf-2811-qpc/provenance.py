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
import sys
import tarfile

from capsule import CLOCK, HARNESS, ROOT, SOURCES, git


def sha(data):
    return hashlib.sha256(data).hexdigest()


def native_environment(environment):
    # Native harness dispatch precedes --build-info parsing. Never inherit a
    # different fixture, style control or diagnostic mode into either path.
    return {key: value for key, value in environment.items() if not key.startswith('OKILUM_')}


def build_environment(environment):
    selected = {key: value for key, value in environment.items()
                if key.startswith('CARGO_PROFILE_RELEASE_') or key.endswith('RUSTFLAGS') or
                key in ['CARGO_INCREMENTAL', 'CARGO_BUILD_JOBS', 'CARGO_TARGET_DIR',
                        'OKILUM_SOURCE_COMMIT', 'OKILUM_WINDOWS_ICON']}
    invalid = [key for key, value in selected.items() if value.strip() and
               (key.startswith('CARGO_PROFILE_RELEASE_') or key.endswith('RUSTFLAGS'))]
    return {'environment': selected, 'unexpected_profile_or_rustflags': sorted(invalid)}


def validate_build_environment(environment):
    report = build_environment(environment)
    assert not report['unexpected_profile_or_rustflags'], (
        'unexpected release profile/target Rustflags tuning', report['unexpected_profile_or_rustflags'])
    return report['environment']


def pe_stack(raw):
    offset = struct.unpack_from('<I', raw, 0x3c)[0] + 24
    assert struct.unpack_from('<H', raw, offset)[0] == 0x20b, 'PE32+ required'
    return struct.unpack_from('<QQ', raw, offset + 72)


def binary_metadata(raw):
    if raw[:4] != b'\xcf\xfa\xed\xfe':
        reserve, commit = pe_stack(raw)
        assert (reserve, commit) == (1048576, 4096), 'PE stack reserve/commit must remain unchanged'
        return {'format': 'PE32+', 'stack_reserve': reserve, 'stack_commit': commit}
    magic, cpu, subtype, filetype, count, command_bytes, flags, reserved = struct.unpack_from('<8I', raw)
    assert cpu == 0x0100000c and filetype == 2, 'native thin arm64 Mach-O executable required'
    offset, stacks = 32, []
    assert offset + command_bytes <= len(raw), 'truncated Mach-O load commands'
    for _ in range(count):
        command, length = struct.unpack_from('<II', raw, offset)
        assert length >= 8 and offset + length <= 32 + command_bytes, 'invalid Mach-O load command'
        if command == 0x80000028:  # LC_MAIN: entryoff + stacksize
            assert length == 24
            entry, stack = struct.unpack_from('<QQ', raw, offset + 8)
            stacks.append(stack)
        offset += length
    assert offset == 32 + command_bytes and stacks == [0], 'Mach-O must retain default LC_MAIN stack size'
    return {'format': 'Mach-O arm64', 'lc_main_stacksize': 0, 'stack_policy': 'OS default, no override'}


def native_binary(folder):
    paths = [p for p in [folder / 'okilum.exe', folder / 'okilum'] if p.is_file()]
    assert len(paths) == 1, 'exactly one native binary required'
    return paths[0].resolve()


def runtime_environment(environment, binary):
    result = native_environment(environment)
    if binary.name == 'okilum':
        framework = binary.parent / 'Sparkle.framework/Sparkle'
        assert framework.is_file(), 'isolated Sparkle framework must be available'
        result['DYLD_FRAMEWORK_PATH'] = str(binary.parent)
    return result


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
    mac = sys.platform == 'darwin'
    name = 'okilum' if mac else 'okilum.exe'
    binary = Path(os.environ['CARGO_TARGET_DIR']) / 'release' / name
    raw = binary.read_bytes()
    metadata = binary_metadata(raw)
    shutil.copy2(binary, output / name)
    environment = native_environment(os.environ)
    if mac:
        framework = destination / 'vendor/sparkle/Sparkle.framework'
        environment['DYLD_FRAMEWORK_PATH'] = str(framework.parent)
        archive = output / 'Sparkle.framework.tar'
        with tarfile.open(archive, 'w') as stream:
            stream.add(framework, arcname='Sparkle.framework')
        manifest['sparkle_archive_sha256'] = sha(archive.read_bytes())
        manifest['sparkle_binary_sha256'] = sha((framework / 'Sparkle').read_bytes())
        sdk = Path(subprocess.check_output(['xcrun', '--show-sdk-path'], text=True).strip())
        headers = [sdk / 'usr/include' / p for p in ['time.h', '_time.h']]
        headers = [p for p in headers if p.is_file()]
        text = '\n'.join(p.read_text() for p in headers)
        assert all(value in text for value in ['CLOCK_THREAD_CPUTIME_ID', 'clock_gettime', 'clock_getres'])
        manifest['darwin_clock_sdk'] = {'sdk': str(sdk),
            'version': subprocess.check_output(['xcrun', '--show-sdk-version'], text=True).strip(),
            'headers_sha256': {str(p): sha(p.read_bytes()) for p in headers}}
    info = subprocess.check_output([str(binary), '--build-info'], text=True, env=environment)
    assert manifest['diagnostic_commit'] in info and 'gpui-test-support: no' in info
    (output / 'build-info.txt').write_text(info)
    (output / 'backend-manifest.json').write_bytes((destination / 'scripts/gpui-pre-windows/manifest.json').read_bytes())
    manifest.update({'binary_sha256': sha(raw), 'binary_bytes': len(raw),
                     'binary_metadata': metadata,
                     'rustc': subprocess.check_output(['rustc', '-Vv'], cwd=destination, text=True),
                     'cargo': subprocess.check_output(['cargo', '-V'], cwd=destination, text=True),
                     'build_environment': validate_build_environment(os.environ)})
    assert manifest['rustc'].startswith('rustc 1.100.0 ')
    assert os.environ['OKILUM_SOURCE_COMMIT'] == manifest['diagnostic_commit']
    (output / 'build-manifest.json').write_text(json.dumps(manifest, indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['source', 'verify', 'environment', 'prepare', 'finish'])
    parser.add_argument('--kind', choices=SOURCES)
    parser.add_argument('--source')
    parser.add_argument('--destination', type=Path)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    if args.command == 'source':
        print(reference(args.kind, json.loads((ROOT / 'scripts/ci/perf-2811-qpc/references.json').read_text())))
    elif args.command == 'verify':
        print(validate_source(args.kind, args.source)[1])
    elif args.command == 'environment':
        (args.output / 'build-environment-preflight.json').write_text(json.dumps(build_environment(os.environ), indent=2))
        validate_build_environment(os.environ)
    elif args.command == 'prepare':
        prepare(args.kind, args.source, args.destination.resolve(), args.output.resolve())
    else:
        finish(args.destination.resolve(), args.output.resolve())
