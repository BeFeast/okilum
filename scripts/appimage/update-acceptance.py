#!/usr/bin/env python3
"""Native actual N->N+1 acceptance. Consumes two already-built source artifacts.

Run only on hosted Ubuntu/Fedora, as an unprivileged user. No builds, downloads,
production signatures, installed-app writes or feed mutations occur here.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

IMAGE = 'Okilum-x86_64.AppImage'


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def run(*args, **kwargs):
    result = subprocess.run([str(a) for a in args], text=True, capture_output=True,
                            timeout=180, **kwargs)
    print(result.stdout, end='')
    print(result.stderr, end='')
    return result


def original(directory):
    provenance = json.loads((directory / 'provenance.json').read_bytes())
    assert provenance['acceptance_harness'] is True
    assert provenance['sha256'] == digest(directory / IMAGE)
    assert provenance['release_version'] == '0.1.' + provenance['build_version']
    assert len(provenance['source_sha']) == 40
    result = run(directory / IMAGE, '--appimage-extract-and-run', '--build-info')
    assert result.returncode == 0
    assert f"build: {provenance['build_version']}\n" in result.stdout
    assert f"version: {provenance['release_version']}\n" in result.stdout
    assert 'gpui-test-support: no' in result.stdout
    return provenance


def main(old, new, evidence):
    assert os.geteuid() != 0, 'Native update must prove no root is required'
    old_p, new_p = original(old), original(new)
    assert int(new_p['build_version']) > int(old_p['build_version'])
    assert new_p['source_sha'] != old_p['source_sha']
    evidence.mkdir(parents=True, exist_ok=False)
    environment = dict(os.environ)
    for key in ['APPIMAGE', 'APPDIR', 'LD_LIBRARY_PATH', 'APPIMAGE_EXTRACT_AND_RUN']:
        environment.pop(key, None)
    with tempfile.TemporaryDirectory(prefix='okilum-native-update-') as temporary:
        work = Path(temporary)
        home = work / 'signer'; home.mkdir(mode=0o700)
        try:
            assert run('gpg', '--homedir', home, '--batch', '--pinentry-mode', 'loopback',
                       '--passphrase', '', '--quick-generate-key',
                       'Disposable AppImage update acceptance', 'ed25519', 'sign', '1d').returncode == 0
            info = run('gpg', '--homedir', home, '--with-colons', '--list-keys')
            fingerprint = next(line.split(':')[9] for line in info.stdout.splitlines() if line.startswith('fpr:'))
            fixture = work / 'fixture'; fixture.mkdir()
            exported = run('gpg', '--homedir', home, '--armor', '--export', fingerprint)
            assert exported.returncode == 0
            (fixture / 'public.asc').write_text(exported.stdout)
            build = int(new_p['build_version'])
            key = f'okilum/linux/builds/{build}/{IMAGE}'
            payload = fixture / key; payload.parent.mkdir(parents=True)
            shutil.copyfile(new / IMAGE, payload)
            shutil.copyfile(new / 'provenance.json', payload.parent / 'provenance.json')
            descriptor = {
                'schema': 1, 'platform': 'linux-appimage', 'channel': 'beta',
                'build': build, 'version': new_p['release_version'], 'source': new_p['source_sha'],
                'filename': IMAGE, 'key': key, 'sha256': digest(payload), 'size': payload.stat().st_size,
                'fingerprint': fingerprint, 'provenance_sha256': digest(payload.parent / 'provenance.json'),
            }
            latest = fixture / 'release.json'
            latest.write_text(json.dumps(descriptor, sort_keys=True) + '\n')
            for signed in [latest, payload]:
                assert run('gpg', '--homedir', home, '--batch', '--detach-sign', signed).returncode == 0

            controls = []

            def launch(name, receipt=None):
                case = work / name; case.mkdir()
                target = case / 'Original with spaces.AppImage'
                shutil.copyfile(old / IMAGE, target); target.chmod(0o755)
                receipt = receipt or case / 'receipt.json'
                result = run(target, '--appimage-extract-and-run', '--appimage-update-acceptance',
                             fixture, fingerprint, receipt, env=environment)
                (evidence / f'{name}.log').write_text(result.stdout + result.stderr)
                return case, target, receipt, result

            # A real valid signature is the positive control for each rejection.
            saved_descriptor = latest.read_bytes()
            latest.write_bytes(saved_descriptor + b'tampered')
            case, target, _, result = launch('tampered-descriptor')
            assert result.returncode != 0 and digest(target) == old_p['sha256']
            controls.append('tampered descriptor rejected; original unchanged')
            latest.write_bytes(saved_descriptor)

            with payload.open('ab') as stream:
                stream.write(b'tampered')
            case, target, _, result = launch('tampered-payload')
            assert result.returncode != 0 and digest(target) == old_p['sha256']
            controls.append('tampered payload rejected; original unchanged')
            with payload.open('r+b') as stream:
                stream.truncate(new_p['size'] if 'size' in new_p else (new / IMAGE).stat().st_size)
            assert digest(payload) == descriptor['sha256']

            # The real newer executable starts but cannot create its confirmation
            # receipt; this is an honest launch-failure/rollback control with no
            # changed executable, source, build or signed artifact identity.
            case, target, _, result = launch('restart-failure', work / 'absent' / 'receipt.json')
            assert result.returncode == 0 and 'APPIMAGE_NATIVE_ARMED' in result.stdout
            deadline = time.monotonic() + 150
            while time.monotonic() < deadline:
                logs = list(case.glob('.okilum-update-*/apply.log'))
                if logs and 'restoring previous AppImage' in logs[0].read_text() and digest(target) == old_p['sha256']:
                    break
                time.sleep(0.25)
            else:
                raise AssertionError('real executable restart failure did not roll back')
            (evidence / 'restart-failure-apply.log').write_bytes(logs[0].read_bytes())
            controls.append('actual newer executable receipt failure rolled back to actual older bytes')

            case, target, receipt, result = launch('actual-n-to-n-plus-1')
            assert result.returncode == 0 and 'APPIMAGE_NATIVE_ARMED' in result.stdout
            assert f"old_build={old_p['build_version']} old_source={old_p['source_sha']}" in result.stdout
            deadline = time.monotonic() + 150
            while time.monotonic() < deadline and not receipt.exists():
                time.sleep(0.25)
            assert receipt.exists(), 'new executable never confirmed its build/source after restart'
            confirmed = json.loads(receipt.read_bytes())
            assert confirmed['build'] == new_p['build_version'] and confirmed['source'] == new_p['source_sha']
            assert confirmed['sha256'] == digest(target) == new_p['sha256']
            assert Path(confirmed['path']) == target
            assert target.stat().st_mode & 0o777 == 0o755
            backups = list(case.glob('.okilum-update-*/previous.AppImage'))
            assert len(backups) == 1 and digest(backups[0]) == old_p['sha256']
            (evidence / 'successful-apply.log').write_bytes(backups[0].with_name('apply.log').read_bytes())
            report = {'old': old_p, 'new': new_p, 'restart': confirmed, 'controls': controls,
                      'authority': 'disposable fixture, public-only updater verification',
                      'production_feed': 'untouched', 'result': 'APPIMAGE_NATIVE_N_TO_N_PLUS_1_PASS'}
            (evidence / 'receipt.json').write_text(json.dumps(report, indent=2) + '\n')
            print(report['result'])
        finally:
            run('gpgconf', '--homedir', home, '--kill', 'all')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--old', type=Path, required=True)
    parser.add_argument('--new', type=Path, required=True)
    parser.add_argument('--evidence', type=Path, required=True)
    args = parser.parse_args()
    main(args.old.resolve(), args.new.resolve(), args.evidence.resolve())
