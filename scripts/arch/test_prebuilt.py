"""Packaging/publication boundaries with fake bytes, stores and tools; no native app."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[2]
# The real entrypoints mutate sys.path for sibling imports. Do not let their
# fixture loads redirect unittest discovery to scripts/releases/test_publish.py.
fixture_spec = importlib.util.spec_from_file_location('native_fixture', ROOT / 'scripts/ci/test_linux_binary.py')
f = importlib.util.module_from_spec(fixture_spec)
with patch.object(sys, 'path', sys.path.copy()):
    fixture_spec.loader.exec_module(f)
spec = importlib.util.spec_from_file_location('arch_production', ROOT / 'scripts/arch/publish.py')
publish = importlib.util.module_from_spec(spec)
with patch.object(sys, 'path', sys.path.copy()):
    spec.loader.exec_module(publish)
spec = importlib.util.spec_from_file_location('release_production', ROOT / 'scripts/releases/publication.py')
publication = importlib.util.module_from_spec(spec)
with patch.object(sys, 'path', sys.path.copy()):
    spec.loader.exec_module(publication)


class Store:
    def __init__(self):
        self.objects = {}
    def call(self, method, key):
        assert method == 'GET'
        return self.objects.get(key)
    def put(self, key, data, *_):
        self.objects[key] = data


def package(path, binary=None, receipt=None):
    path.write_bytes(f.tar_data([('usr/bin/okilum', binary if binary is not None else f.binary_bytes(), tarfile.REGTYPE),
        ('usr/share/okilum/production-receipt.json', f.package.encode(receipt or f.receipt()), tarfile.REGTYPE)]))


class Prebuilt(unittest.TestCase):
    def setUp(self):
        # Synthetic receipts isolate package structure from external authority.
        # Real immutable ZIP/API forgery controls live in test_linux_artifact_reuse.
        for module in (publication.native_payload, publish.native_payload):
            self.enterContext(patch.object(module, 'authenticate_receipt'))
        self.enterContext(patch.object(publication.archive_authority.consumer, 'public_read', return_value=None))

    def test_real_pkgbuild_functions_install_identical_bytes_without_cargo(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            prebuilt = root / 'prebuilt'
            (prebuilt / 'payload').mkdir(parents=True)
            (prebuilt / 'payload/okilum').write_bytes(f.binary_bytes())
            (prebuilt / 'receipt.json').write_bytes(f.package.encode(f.receipt()))
            tools = root / 'tools'
            tools.mkdir()
            cargo = tools / 'cargo'
            cargo.write_text('#!/bin/sh\necho invoked >> "$COMPILER_COUNTER"\nexit 97\n')
            cargo.chmod(0o755)
            raster = tools / 'rsvg-convert'
            raster.write_text('#!' + sys.executable + '\nimport pathlib,sys\npathlib.Path(sys.argv[sys.argv.index("--output")+1]).write_bytes(b"fake icon")\n')
            raster.chmod(0o755)
            python = tools / 'python3'
            python.write_text('#!/bin/sh\n# Notices generator is outside this byte-preservation test.\nexit 0\n')
            python.chmod(0o755)
            env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ['PATH'],
                OKILUM_SOURCE=str(ROOT), OKILUM_PREBUILT_DIR=str(prebuilt), pkgdir=str(root / 'pkg'),
                COMPILER_COUNTER=str(root / 'compiler-counter'))
            subprocess.run(['bash', '-c', 'set -euo pipefail; source scripts/arch/PKGBUILD; build; package'],
                           cwd=ROOT, env=env, check=True, capture_output=True)
            self.assertEqual((root / 'pkg/usr/bin/okilum').read_bytes(), f.binary_bytes())
            self.assertEqual((root / 'pkg/usr/share/okilum/production-receipt.json').read_bytes(), (prebuilt / 'receipt.json').read_bytes())
            self.assertFalse((root / 'compiler-counter').exists())
            control = subprocess.run(['cargo', 'build'], env=env, capture_output=True)
            self.assertEqual(control.returncode, 97)
            self.assertEqual((root / 'compiler-counter').read_text(), 'invoked\n')
        self.assertIn("'!strip'", (ROOT / 'scripts/arch/PKGBUILD').read_text())
        build = (ROOT / 'scripts/arch/build.sh').read_text()
        for command in ('cargo build', 'cargo test', 'rustup', 'release-cache.sh'):
            self.assertNotIn(command, build)

    def test_publication_validates_real_receipt_source_build_and_binary_before_signing(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'arch').mkdir()
            path = root / 'arch/okilum-0.1.14108-1-x86_64.pkg.tar.zst'
            package(path)
            (root / 'arch/production-receipt.json').write_bytes(f.package.encode(f.receipt()))
            self.assertEqual(publication.production_build(root, f.SHA), 14108)
            with self.assertRaises(ValueError):
                publication.production_build(root, 'b' * 40)
            altered = f.receipt()
            altered['verified_at'] = '2026-10-18T02:00:00Z'
            (root / 'arch/production-receipt.json').write_bytes(f.package.encode(altered))
            with self.assertRaisesRegex(ValueError, 'differs'):
                publication.production_build(root, f.SHA)
            key = Mock()
            store = Store()
            args = argparse.Namespace(command='publish', build=14108, source=f.SHA, package=str(path), archive_only=True)
            for change in ('source', 'build', 'binary', 'receipt'):
                package(path)
                args.source, args.build = f.SHA, 14108
                if change == 'source':
                    args.source = 'b' * 40
                elif change == 'build':
                    args.build = 14109
                elif change == 'binary':
                    package(path, binary=b'stripped binary')
                elif change == 'receipt':
                    bad = f.receipt()
                    bad['producer']['workflow'] = '.github/workflows/forgejo-linux-binary.yml'
                    package(path, receipt=bad)
                with self.subTest(change=change), self.assertRaises(ValueError):
                    publish.execute(args, store, key, root)
                key.sign.assert_not_called()
                self.assertEqual(store.objects, {})

    def test_archive_stores_original_receipt_and_legacy_promotion_still_works(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            path = root / 'okilum-0.1.14108-1-x86_64.pkg.tar.zst'
            package(path)
            key = Mock()
            key.public.return_value = b'fixture key'
            key.sign.side_effect = lambda p: Path(str(p) + '.sig').write_bytes(b'fixture signature')
            args = argparse.Namespace(command='publish', build=14108, source=f.SHA, package=str(path), archive_only=True)
            store = Store()
            with patch.object(publish, 'validate_package'):
                publish.execute(args, store, key, root)
            saved = json.loads(store.objects['okilum/arch/builds/14108/manifest.json'])
            self.assertEqual(saved['build'], 14108)
            self.assertEqual(saved['production']['producer']['forgejo_run_number'], 9108)
            self.assertEqual(saved['binary_sha256'], f.package.digest(f.binary_bytes()))
            # A pre-schema signed candidate is promoted via its existing receipt;
            # it does not acquire invented full-source/build-info fields.
            del saved['production']
            del saved['binary_sha256']
            store.objects['okilum/arch/builds/14108/manifest.json'] = publish.encode(saved)
            args.command = 'promote'
            args.archive_only = False
            with patch.object(publish, 'publish_channel') as channel, \
                    patch.object(publish.native_payload, 'package_receipt') as revalidate:
                publish.execute(args, store, key, root)
                revalidate.assert_not_called()
                channel.assert_called_once()
                self.assertEqual(channel.call_args.args[4], saved)


if __name__ == '__main__':
    unittest.main()
