"""Real signing/repo-add tests on Arch; pure validation runs on other CI hosts."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import publish
import catalog


class MemoryR2:
    def __init__(self):
        self.objects = {}

    def call(self, method, key):
        assert method == 'GET'
        return self.objects.get(key)

    def put(self, key, data, *_):
        self.objects[key] = data


class Validation(unittest.TestCase):
    def test_build_identifier_cannot_escape_archive(self):
        for value in [0, -1, '../stable', '1/2', '01']:
            with self.assertRaises(ValueError):
                publish.package_name(value)
        self.assertEqual(publish.package_name(42), 'okilum-0.1.42-1-x86_64.pkg.tar.zst')


@unittest.skipUnless(shutil.which('repo-add'), 'Arch packaging tools required')
class SignedRepository(unittest.TestCase):
    def test_publish_promote_and_reject_tampering(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home = root / 'gpg'
            home.mkdir(mode=0o700)
            subprocess.run(['gpg', '--homedir', str(home), '--batch', '--passphrase', '',
                            '--quick-generate-key', 'Test <test@example.invalid>', 'ed25519', 'sign', '1d'], check=True)
            info = publish.run('gpg', '--homedir', str(home), '--with-colons', '--list-secret-keys').decode()
            fingerprint = next(line.split(':')[9] for line in info.splitlines() if line.startswith('fpr:'))
            secret = publish.run('gpg', '--homedir', str(home), '--armor', '--export-secret-keys').decode()
            package = root / publish.package_name(42)
            info_path = root / '.PKGINFO'
            info_path.write_text('pkgname = okilum\npkgver = 0.1.42-1\narch = x86_64\npkgdesc = test\nsize = 0\n')
            publish.run('bsdtar', '-caf', str(package), '-C', str(root), '.PKGINFO')
            store = MemoryR2()
            with patch.dict(os.environ, ARCH_GPG_PRIVATE_KEY=secret, ARCH_GPG_FINGERPRINT=fingerprint):
                with publish.SigningKey() as key:
                    args = argparse.Namespace(command='publish', build=42, source='a' * 40, package=str(package))
                    for channel in ['beta', 'stable']:
                        tmp = root / channel
                        tmp.mkdir()
                        publish.execute(args, store, key, tmp)
                        prefix = f'okilum/arch/{channel}/x86_64'
                        db = tmp / f'okilum-{channel}.db'
                        db.write_bytes(store.objects[f'{prefix}/{db.name}'])
                        Path(str(db) + '.sig').write_bytes(store.objects[f'{prefix}/{db.name}.sig'])
                        key.verify(db)
                        self.assertIn('okilum-0.1.42-1', publish.run('bsdtar', '-tf', str(db)).decode())
                        args.command = 'promote'
                    for suffix in ['', '.sig']:
                        filename = package.name + suffix
                        self.assertEqual(store.objects[f'okilum/arch/beta/x86_64/{filename}'],
                                         store.objects[f'okilum/arch/stable/x86_64/{filename}'])
                    store.objects[f'okilum/arch/builds/42/{package.name}'] += b'tampered'
                    with self.assertRaisesRegex(ValueError, 'checksum mismatch'):
                        publish.execute(args, store, key, root)
                    manifest = json.loads(store.objects['okilum/arch/stable/x86_64/latest.json'])
                    manifest['build'] = 43
                    store.objects['okilum/arch/stable/x86_64/latest.json'] = publish.encode(manifest)
                    with self.assertRaisesRegex(ValueError, 'backwards'):
                        publish.publish_channel(store, key, root, 'stable', {'build': 42}, b'', b'')
            publish.run('gpgconf', '--homedir', str(home), '--kill', 'all')


    def test_archive_only_records_an_older_build_without_moving_beta(self):
        """#1040: an explicit older commit is archived and promotable; beta stays newer."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home = root / 'gpg'
            home.mkdir(mode=0o700)
            subprocess.run(['gpg', '--homedir', str(home), '--batch', '--passphrase', '',
                            '--quick-generate-key', 'Test <test@example.invalid>', 'ed25519', 'sign', '1d'], check=True)
            info = publish.run('gpg', '--homedir', str(home), '--with-colons', '--list-secret-keys').decode()
            fingerprint = next(line.split(':')[9] for line in info.splitlines() if line.startswith('fpr:'))
            secret = publish.run('gpg', '--homedir', str(home), '--armor', '--export-secret-keys').decode()

            def package(build):
                folder = root / f'pkg{build}'
                folder.mkdir()
                (folder / '.PKGINFO').write_text(f'pkgname = okilum\npkgver = 0.1.{build}-1\narch = x86_64\n'
                                                 'pkgdesc = test\nsize = 0\n')
                path = folder / publish.package_name(build)
                publish.run('bsdtar', '-caf', str(path), '-C', str(folder), '.PKGINFO')
                return str(path)
            store = MemoryR2()
            with patch.dict(os.environ, ARCH_GPG_PRIVATE_KEY=secret, ARCH_GPG_FINGERPRINT=fingerprint):
                with publish.SigningKey() as key:
                    for build, source, archive_only in [(50, 'b' * 40, False), (44, 'a' * 40, True)]:
                        tmp = root / f'run{build}'
                        tmp.mkdir()
                        args = argparse.Namespace(command='publish', build=build, source=source,
                                                  package=package(build), archive_only=archive_only)
                        publish.execute(args, store, key, tmp)
                    beta = json.loads(store.objects['okilum/arch/beta/x86_64/latest.json'])
                    self.assertEqual(beta['build'], 50)
                    self.assertEqual(json.loads(store.objects['okilum/arch/builds/44/manifest.json'])['source'],
                                     'a' * 40)
                    self.assertIn(f'{catalog.PREFIX}/{"a" * 40}/linux.json', store.objects)
                    tmp = root / 'promote'
                    tmp.mkdir()
                    publish.execute(argparse.Namespace(command='promote', build=44), store, key, tmp)
                    self.assertEqual(json.loads(store.objects['okilum/arch/stable/x86_64/latest.json'])['build'], 44)
            publish.run('gpgconf', '--homedir', str(home), '--kill', 'all')


if __name__ == '__main__':
    unittest.main()
