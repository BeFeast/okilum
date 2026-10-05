#!/usr/bin/env python3
"""Signed Arch repositories. Promotion copies the original beta package and signature."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'updater'))
from release import R2  # noqa: E402
import catalog

PREFIX = 'tessera/arch'


def run(*args, **kwargs):
    return subprocess.run(args, check=True, stdout=subprocess.PIPE, **kwargs).stdout


def encode(value):
    return json.dumps(value, sort_keys=True).encode()


def package_name(build):
    if not re.fullmatch(r'[1-9][0-9]*', str(build)):
        raise ValueError('Build must be a positive integer')
    return f'tessera-0.1.{build}-1-x86_64.pkg.tar.zst'


class SigningKey:
    def __enter__(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = Path(self.tmp.name)
        self.home.chmod(0o700)
        self.fingerprint = os.environ['ARCH_GPG_FINGERPRINT']
        if not re.fullmatch(r'[A-F0-9]{40}', self.fingerprint):
            raise ValueError('Expected full GPG fingerprint')
        self.gpg('--import', input=os.environ['ARCH_GPG_PRIVATE_KEY'].encode())
        keys = self.gpg('--with-colons', '--list-secret-keys').decode()
        if f'fpr:::::::::{self.fingerprint}:' not in keys:
            raise ValueError('Signing key does not match configured fingerprint')
        return self

    def gpg(self, *args, **kwargs):
        return run('gpg', '--homedir', str(self.home), '--batch', *args, **kwargs)

    def sign(self, path):
        self.gpg('--yes', '--local-user', self.fingerprint, '--detach-sign', str(path))
        self.verify(path)

    def verify(self, path):
        self.gpg('--verify', str(path) + '.sig', str(path))

    def public(self):
        return self.gpg('--armor', '--export', self.fingerprint)

    def __exit__(self, *_):
        run('gpgconf', '--homedir', str(self.home), '--kill', 'all')
        self.tmp.cleanup()


def validate_package(path, build):
    info = run('bsdtar', '-xOf', str(path), '.PKGINFO').decode()
    fields = dict(line.split(' = ', 1) for line in info.splitlines() if ' = ' in line)
    for key, expected in [('pkgname', 'tessera'), ('pkgver', f'0.1.{build}-1'), ('arch', 'x86_64')]:
        if fields.get(key) != expected:
            raise ValueError(f'Unexpected package {key}: {fields.get(key)}')


def publish_channel(r2, key, tmp, channel, manifest, package, signature):
    prefix = f'{PREFIX}/{channel}/x86_64'
    current = r2.call('GET', f'{prefix}/latest.json')
    if current is not None and json.loads(current)['build'] > manifest['build']:
        raise ValueError('Refusing to move a channel backwards')
    path = tmp / manifest['filename']
    path.write_bytes(package)
    path.with_suffix(path.suffix + '.sig').write_bytes(signature)
    key.verify(path)
    validate_package(path, manifest['build'])
    db = tmp / f'tessera-{channel}.db.tar.gz'
    run('repo-add', '--sign', '--key', key.fingerprint, str(db), str(path),
        env={**os.environ, 'GNUPGHOME': str(key.home)})
    key.verify(db)
    # Upload immutable payloads before the signed DB that references them.
    for name, data in [(path.name, package), (path.name + '.sig', signature)]:
        r2.put(f'{prefix}/{name}', data, 'application/octet-stream', 'public, max-age=31536000, immutable')
    # R2 cannot atomically replace DB + detached signature. Serialized publication
    # keeps the window short; pacman fails closed if a client crosses that window.
    r2.put(f'{prefix}/tessera-{channel}.db.sig', Path(str(db) + '.sig').read_bytes(), 'application/octet-stream', 'no-cache')
    r2.put(f'{prefix}/tessera-{channel}.db', db.read_bytes(), 'application/octet-stream', 'no-cache')
    r2.put(f'{prefix}/latest.json', encode(manifest), 'application/json', 'no-cache')


def execute(args, r2, key, tmp):
    name = package_name(args.build)
    archive = f'{PREFIX}/builds/{args.build}'
    if args.command == 'publish':
        package = Path(args.package).read_bytes()
        manifest = {'build': args.build, 'filename': name, 'sha256': hashlib.sha256(package).hexdigest(), 'source': args.source}
        existing = r2.call('GET', f'{archive}/manifest.json')
        if existing is not None and json.loads(existing) != manifest:
            raise ValueError('Build already exists with different contents')
        path = tmp / name
        path.write_bytes(package)
        validate_package(path, args.build)
        key.sign(path)
        signature = Path(str(path) + '.sig').read_bytes()
        if existing is not None:
            signature = r2.call('GET', f'{archive}/{name}.sig')
        else:
            r2.put(f'{archive}/{name}', package, 'application/octet-stream')
            r2.put(f'{archive}/{name}.sig', signature, 'application/octet-stream')
            r2.put(f'{archive}/manifest.json', encode(manifest), 'application/json')
        channel = 'beta'
    else:
        data = r2.call('GET', f'{archive}/manifest.json')
        if data is None:
            raise ValueError('Unknown beta build')
        manifest = json.loads(data)
        if manifest['build'] != args.build or manifest['filename'] != name:
            raise ValueError('Invalid archived manifest')
        package = r2.call('GET', f'{archive}/{name}')
        signature = r2.call('GET', f'{archive}/{name}.sig')
        if hashlib.sha256(package).hexdigest() != manifest['sha256']:
            raise ValueError('Archived package checksum mismatch')
        channel = 'stable'
    r2.put(f'{PREFIX}/tessera-signing-key.asc', key.public(), 'application/pgp-keys', 'no-cache')
    publish_channel(r2, key, tmp, channel, manifest, package, signature)
    if args.command == 'publish':
        catalog.record(r2, 'linux', args.build, args.source, [
            catalog.asset(f'{archive}/{name}', name, package),
            catalog.asset(f'{archive}/{name}.sig', name + '.sig', signature)])
    print(f'Published {name} to {channel}; source {manifest["source"]}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    for command in ['publish', 'promote']:
        p = sub.add_parser(command)
        p.add_argument('--build', type=int, required=True)
        if command == 'publish':
            p.add_argument('--source', required=True)
            p.add_argument('--package', required=True)
    args = parser.parse_args()
    package_name(args.build)
    with SigningKey() as key, tempfile.TemporaryDirectory() as tmp:
        execute(args, R2(), key, Path(tmp))


if __name__ == '__main__':
    main()
