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


PAYLOAD = '''#!/bin/sh
set -eu
printf '%s\\n' "$APPDIR" "$APPIMAGE" "$@"
test "$(id -u)" = "$EXPECTED_UID"
test "$1" = --vault
test "$2" = 'vault with spaces'
'''


def image(runtime, filesystem, destination):
    destination.write_bytes(runtime.read_bytes() + filesystem.read_bytes())
    destination.chmod(0o755)
    return destination


def launch(artifact, temporary, uid, retain=True, flag=True):
    environment = dict(os.environ, TMPDIR=str(temporary), EXPECTED_UID=str(uid))
    for name in ('APPIMAGE', 'APPDIR', 'APPIMAGE_EXTRACT_AND_RUN', 'NO_CLEANUP'):
        environment.pop(name, None)
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runtime', required=True, type=Path)
    parser.add_argument('--baseline', required=True, type=Path)
    args = parser.parse_args()
    assert os.geteuid() == 0, 'two real UID controls require hosted-runner sudo'
    with tempfile.TemporaryDirectory() as name:
        root = Path(name)
        root.chmod(0o755)
        appdir = root / 'AppDir'
        appdir.mkdir()
        (appdir / 'AppRun').write_text(PAYLOAD)
        (appdir / 'AppRun').chmod(0o755)
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
            assert cache == temporary / f'appimage_extracted_{uid}_{digest}', cache
            # Managed re-exec and updater restart: both runtime entry forms.
            require_launch(launch(fixed, temporary, uid, flag=False), fixed, uid)
            copy = root / f'home-{uid}' / 'managed.AppImage'
            copy.parent.mkdir()
            copy.write_bytes(fixed.read_bytes())
            copy.chmod(0o755)
            os.chown(copy.parent, uid, uid)
            os.chown(copy, uid, uid)
            require_launch(launch(copy, temporary, uid), copy, uid)
            # Cleanup must unlink this symlink, never traverse its foreign target.
            (cache / 'foreign-link').symlink_to(legacy, target_is_directory=True)
            clean = launch(copy, temporary, uid, retain=False)
            assert clean.returncode == 0, clean
            assert not cache.exists(), (
                'normal runtime cleanup must remove only this UID cache', clean,
                [(str(p), p.stat().st_uid, oct(p.stat().st_mode))
                 for p in temporary.rglob('*')])
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
        print('APPIMAGE_TWO_USER_START_REEXEC_RESTART_CLEANUP_PASS', flush=True)


if __name__ == '__main__':
    main()
