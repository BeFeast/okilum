#!/usr/bin/env python3
"""Explicit final signed pacman release followed by durable publication freeze."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile
import urllib.error
import urllib.request

import appimage_stable
import linux

MARKER = 'okilum/arch/frozen.json'
NOTICE = b'Okilum Linux is transitioning to the signed AppImage'
IMAGE_URL = b'https://updates.befeast.com/okilum/linux/stable/Okilum-x86_64.AppImage'


class PublicStore:
    """Public GETs only: check-only cannot reach authenticated publication APIs."""
    def call(self, method, key):
        if method != 'GET' or not re.fullmatch(r'okilum/[A-Za-z0-9_./-]+', key) or '..' in key:
            raise ValueError('Public preflight permits only release-object GETs')
        request = urllib.request.Request('https://updates.befeast.com/' + key,
                                         headers={'User-Agent': 'curl'})
        try:
            with urllib.request.urlopen(request, timeout=120) as response:
                return response.read()
        except urllib.error.HTTPError as error:
            if error.code == 404:
                return None
            raise

    def put(self, *_args, **_kwargs):
        raise ValueError('Public preflight cannot publish')


class PublicKey(linux.arch.SigningKey):
    def __enter__(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = Path(self.tmp.name)
        self.home.chmod(0o700)
        self.fingerprint = appimage_stable.publication.FINGERPRINT
        try:
            public = PublicStore().call('GET', 'okilum/arch/okilum-signing-key.asc')
            if public is None:
                raise ValueError('Missing public Linux signing key')
            self.gpg('--import', input=public)
            return self
        except BaseException:
            self.__exit__()
            raise

    def sign(self, _path):
        raise ValueError('Public preflight cannot sign')


def stable_target(store, signing, tmp):
    """Verify the actual migration URL before any final Arch transaction."""
    prefix = 'okilum/linux/stable'
    raw = appimage_stable.verify(store, signing, tmp, prefix, 'latest.json')
    release = json.loads(raw)
    build, source = release.get('build'), release.get('source')
    if (not isinstance(build, int) or build <= 5000
            or not isinstance(source, str) or not re.fullmatch('[0-9a-f]{40}', source)
            or (release.get('schema'), release.get('platform'), release.get('channel'),
                release.get('version'), release.get('filename'), release.get('fingerprint'), release.get('key')) != (
                1, 'linux-appimage', 'stable', f'0.1.{build}', appimage_stable.IMAGE,
                appimage_stable.publication.FINGERPRINT, f'okilum/linux/builds/{build}/{appimage_stable.IMAGE}')):
        raise ValueError('Invalid signed AppImage Stable migration target')
    image = appimage_stable.verify(store, signing, tmp, prefix, appimage_stable.IMAGE)
    provenance_raw = appimage_stable.required(store, f'okilum/linux/builds/{build}/provenance.json')
    provenance = json.loads(provenance_raw)
    if (len(image) != release['size'] or hashlib.sha256(image).hexdigest() != release['sha256']
            or hashlib.sha256(provenance_raw).hexdigest() != release['provenance_sha256']
            or provenance.get('source_sha') != source or provenance.get('build_version') != str(build)
            or provenance.get('release_version') != f'0.1.{build}'
            or provenance.get('sha256') != release['sha256']
            or provenance.get('acceptance_harness', False) is not False):
        raise ValueError('AppImage Stable migration payload/provenance mismatch')
    return {'build': build, 'source': source, 'sha256': release['sha256']}


def preflight(store, signing, tmp, source, build):
    if not re.fullmatch('[0-9a-f]{40}', source) or build < 1:
        raise ValueError('Expected full source SHA and positive final pacman build')
    if signing.fingerprint != appimage_stable.publication.FINGERPRINT:
        raise ValueError('Final pacman requires the pinned Linux signing authority')
    manifest, package, signature = linux.candidate(store, source, build)
    path = tmp / manifest['filename']; path.write_bytes(package)
    Path(str(path) + '.sig').write_bytes(signature); signing.verify(path)
    linux.arch.validate_package(path, build)
    install = subprocess.run(['bsdtar', '-xOf', str(path), '.INSTALL'], capture_output=True, check=True).stdout
    if NOTICE not in install or IMAGE_URL not in install:
        raise ValueError('Final pacman package lacks AppImage migration notice')
    current = store.call('GET', f'{linux.PREFIX}/latest.json')
    if current is not None:
        previous = json.loads(current)
        if previous['build'] > build or (previous['build'] == build and previous != manifest):
            raise ValueError('Refusing final pacman rollback or conflicting build identity')
    return {'pacman': manifest, 'appimage_stable': stable_target(store, signing, tmp),
            'publication_performed': False}


def execute(store, signing, tmp, source, build):
    if not re.fullmatch('[0-9a-f]{40}', source) or build < 1:
        raise ValueError('Expected full source SHA and positive final pacman build')
    frozen = store.call('GET', MARKER)
    identity = {'state': 'frozen', 'source': source, 'build': build}
    if frozen is not None:
        if json.loads(frozen) != identity:
            raise ValueError('Pacman already frozen at another final release')
        return
    preflight(store, signing, tmp, source, build)
    linux.execute(store, signing, tmp, source, build)
    # Freeze only after the signed final Stable transaction completed.
    store.put(MARKER, linux.catalog.encode(identity),
              'application/json', 'no-cache')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', required=True)
    parser.add_argument('--build', type=int, required=True)
    parser.add_argument('--check-only', action='store_true',
                        help='Verify public package, notice and signed AppImage Stable; never sign, publish or freeze')
    args = parser.parse_args(argv)
    if not re.fullmatch('[0-9a-f]{40}', args.source) or args.build < 1:
        parser.error('Expected full source SHA and positive final pacman build')
    subprocess.run(['git', 'merge-base', '--is-ancestor', args.source, 'origin/main'], check=True)
    if args.check_only:
        store = PublicStore()
        if store.call('GET', MARKER) is not None:
            raise ValueError('Pacman is already frozen; no new final release can be selected')
        with tempfile.TemporaryDirectory() as directory, PublicKey() as signing:
            receipt = preflight(store, signing, Path(directory), args.source, args.build)
        print(json.dumps(receipt, sort_keys=True))
        return
    with tempfile.TemporaryDirectory() as directory, linux.arch.SigningKey() as signing:
        execute(linux.arch.R2(), signing, Path(directory), args.source, args.build)


if __name__ == '__main__':
    main()
