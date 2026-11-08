#!/usr/bin/env python3
"""Execute the real static runtime as two distinct Linux UIDs (#2750).

Uses tiny executable SquashFS payloads, not mocks of the extraction algorithm.
Only disposable trees are chowned. Run on the hosted runner as root.
"""
import argparse
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import time


PAYLOAD = '''#!/bin/sh
set -eu
printf '%s\\n' "$APPDIR" "$APPIMAGE" "$@"
test "$(id -u)" = "$EXPECTED_UID"
test "$1" = --vault
test "$2" = 'vault with spaces'
if [ -n "${RUNTIME_LINK:-}" ]; then
    ln -s "$RUNTIME_LINK" "$APPDIR/foreign-link"
fi
if [ -n "${RUNTIME_HOLD:-}" ]; then
    printf '%s\\n' "$APPDIR" > "$RUNTIME_HOLD/ready"
    while [ ! -f "$RUNTIME_HOLD/release" ]; do sleep .05; done
    if [ ! -f "$APPDIR/lifetime-sentinel" ]; then
        echo RUNTIME_RUNNING_PAYLOAD_REMOVED >&2
        exit 43
    fi
fi
'''


def image(runtime, filesystem, destination):
    destination.write_bytes(runtime.read_bytes() + filesystem.read_bytes())
    destination.chmod(0o755)
    return destination


def launch(artifact, temporary, uid, retain=True, flag=True, link_target=None):
    environment = dict(os.environ, TMPDIR=str(temporary), EXPECTED_UID=str(uid))
    for name in ('APPIMAGE', 'APPDIR', 'APPIMAGE_EXTRACT_AND_RUN', 'NO_CLEANUP', 'RUNTIME_HOLD', 'RUNTIME_LINK'):
        environment.pop(name, None)
    if link_target is not None:
        environment['RUNTIME_LINK'] = str(link_target)
    if retain:
        environment['NO_CLEANUP'] = '1'
    if not flag:
        environment['APPIMAGE_EXTRACT_AND_RUN'] = '1'
    return subprocess.run([str(artifact), *(['--appimage-extract-and-run'] if flag else []),
                           '--vault', 'vault with spaces'], env=environment,
                          user=uid, group=uid, extra_groups=[],
                          capture_output=True, text=True, timeout=30)


def require_launch(result, artifact, uid):
    assert result.returncode == 0, (uid, result)
    lines = result.stdout.splitlines()
    assert lines[1:] == [str(artifact), '--vault', 'vault with spaces'], lines
    cache = Path(lines[0])
    assert cache.is_dir() and cache.stat().st_uid == uid, cache
    return cache


def overlap(artifact, root, temporary, uid):
    """A second same-UID launch must not remove a still-running payload."""
    control = root / ('hold-' + artifact.name + '-' + str(uid))
    control.mkdir(mode=0o700)
    os.chown(control, uid, uid)
    environment = dict(os.environ, TMPDIR=str(temporary), EXPECTED_UID=str(uid),
                       RUNTIME_HOLD=str(control), APPIMAGE_EXTRACT_AND_RUN='1')
    for name in ('APPIMAGE', 'APPDIR', 'NO_CLEANUP', 'RUNTIME_LINK'):
        environment.pop(name, None)
    held = subprocess.Popen([str(artifact), '--vault', 'vault with spaces'],
                            env=environment, user=uid, group=uid, extra_groups=[],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        deadline = time.monotonic() + 15
        while not (control / 'ready').exists():
            assert held.poll() is None, 'held payload exited before readiness'
            assert time.monotonic() < deadline, 'held payload readiness timeout'
            time.sleep(.05)
        active = Path((control / 'ready').read_text().strip())
        assert active.is_dir() and active.stat().st_uid == uid
        sibling = launch(artifact, temporary, uid, retain=False)
        assert sibling.returncode == 0, sibling
        sibling_cache = Path(sibling.stdout.splitlines()[0])
        assert not sibling_cache.exists(), 'sibling cleanup did not execute'
        (control / 'release').touch()
        stdout, stderr = held.communicate(timeout=15)
        assert stdout.splitlines()[0] == str(active)
        return held.returncode, stderr, active, sibling_cache
    finally:
        if held.poll() is None:
            held.kill()
            held.communicate(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runtime', required=True, type=Path)
    parser.add_argument('--baseline', required=True, type=Path)
    parser.add_argument('--shared-runtime', required=True, type=Path)
    args = parser.parse_args()
    assert os.geteuid() == 0, 'two real UID controls require hosted-runner sudo'
    with tempfile.TemporaryDirectory() as name:
        root = Path(name)
        root.chmod(0o755)
        appdir = root / 'AppDir'
        appdir.mkdir()
        (appdir / 'AppRun').write_text(PAYLOAD)
        (appdir / 'AppRun').chmod(0o755)
        (appdir / 'lifetime-sentinel').write_text('running payload')
        filesystem = root / 'payload.squashfs'
        subprocess.run(['mksquashfs', str(appdir), str(filesystem), '-noappend',
                        '-all-root', '-no-xattrs', '-quiet'], check=True)
        temporary = root / 'shared temporary directory'
        temporary.mkdir(mode=0o777)
        temporary.chmod(0o1777)
        old = image(args.baseline, filesystem, root / 'old.AppImage')
        # Real pre-fix failure: A's extraction is left behind, B cannot use it.
        foreign = require_launch(launch(old, temporary, 19001, retain=False), old, 19001)
        foreign.chmod(0o700)
        denied = launch(old, temporary, 19002)
        assert denied.returncode != 0 and 'Failed to extract AppImage' in denied.stderr, denied
        print('APPIMAGE_TWO_USER_BASELINE_RUNTIME_FAILURE', flush=True)
        shared = image(args.shared_runtime, filesystem, root / 'shared.AppImage')
        broken, stderr, _, _ = overlap(shared, root, temporary, 19001)
        assert broken == 43 and 'RUNTIME_RUNNING_PAYLOAD_REMOVED' in stderr, (broken, stderr)
        print('APPIMAGE_SHARED_LIFETIME_BASELINE_RUNTIME_FAILURE', flush=True)
        fixed = image(args.runtime, filesystem, root / 'fixed.AppImage')
        digest = hashlib.md5(fixed.read_bytes()).hexdigest()
        # The legacy content-only cache of this exact image must also survive.
        legacy = temporary / ('appimage_extracted_' + digest)
        legacy.mkdir(mode=0o700)
        (legacy / 'foreign-sentinel').write_bytes(b'keep original user cache')
        os.chown(legacy, 19001, 19001)
        os.chown(legacy / 'foreign-sentinel', 19001, 19001)
        before = (legacy.stat(), (legacy / 'foreign-sentinel').stat())
        for uid in (19001, 19002):
            cache = require_launch(launch(fixed, temporary, uid), fixed, uid)
            assert cache.parent == temporary, cache
            assert cache.name.startswith(f'appimage_extracted_{uid}_{digest}_'), cache
            assert cache.stat().st_mode & 0o777 == 0o700, cache
            # Managed re-exec and updater restart: both runtime entry forms.
            restarted = require_launch(launch(fixed, temporary, uid, flag=False), fixed, uid)
            assert restarted != cache and cache.is_dir()
            assert restarted.stat().st_mode & 0o777 == 0o700
            copy = root / f'home-{uid}' / 'managed.AppImage'
            copy.parent.mkdir()
            copy.write_bytes(fixed.read_bytes())
            copy.chmod(0o755)
            os.chown(copy.parent, uid, uid)
            os.chown(copy, uid, uid)
            managed = require_launch(launch(copy, temporary, uid), copy, uid)
            assert managed not in (cache, restarted) and cache.is_dir() and restarted.is_dir()
            # Cleanup must unlink this symlink, never traverse its foreign target.
            (cache / 'foreign-link').symlink_to(legacy, target_is_directory=True)
            retained_before = (cache.stat(), restarted.stat(), managed.stat())
            clean = launch(copy, temporary, uid, retain=False, link_target=legacy)
            assert clean.returncode == 0, clean
            clean_cache = Path(clean.stdout.splitlines()[0])
            assert not clean_cache.exists(), (
                'normal runtime cleanup must remove only this UID cache', clean,
                [(str(p), p.stat().st_uid, oct(p.stat().st_mode))
                 for p in temporary.rglob('*')])
            assert (cache.stat(), restarted.stat(), managed.stat()) == retained_before
            assert (cache / 'foreign-link').is_symlink()
            code, stderr, active, sibling_cache = overlap(fixed, root, temporary, uid)
            assert code == 0 and not stderr, (code, stderr)
            assert active != sibling_cache and not active.exists()
            assert (cache.stat(), restarted.stat(), managed.stat()) == retained_before
        assert (legacy.stat(), (legacy / 'foreign-sentinel').stat()) == before
        assert (legacy / 'foreign-sentinel').read_bytes() == b'keep original user cache'
        assert foreign.is_dir() and foreign.stat().st_uid == 19001
        # FUSE mode is unchanged by the patch; runtime metadata stays usable.
        for option, expected in (
                ('--appimage-help', b'AppImage options:'),
                ('--appimage-offset', str(args.runtime.stat().st_size).encode()),
                ('--appimage-version', b'okilum-per-user')):
            result = subprocess.run([str(fixed), option], capture_output=True, timeout=10)
            assert result.returncode == 0 and expected in result.stdout + result.stderr, (option, result)
        print('APPIMAGE_TWO_USER_START_REEXEC_RESTART_CLEANUP_PASS')
        print('APPIMAGE_SAME_USER_CONCURRENT_LIFETIMES_PASS', flush=True)


if __name__ == '__main__':
    main()
