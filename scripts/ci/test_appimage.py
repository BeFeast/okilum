"""Portable artifact identity and native loader checks must fail closed."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('appimage_verify', ROOT / 'scripts/appimage/verify.py')
verify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify)


class AppImageTests(unittest.TestCase):
    def test_loader_accepts_resolved_libraries_and_rejects_absence_or_loader_failure(self):
        verify.require_loaded('libc.so.6 => /lib/libc.so.6 (0x123)', 0)
        for output, code in [('libfoo.so => not found', 0),
                             ("libc.so.6: version `GLIBC_2.39' not found", 1),
                             ('not a dynamic executable', 1)]:
            with self.subTest(output=output), self.assertRaises(ValueError):
                verify.require_loaded(output, code)

    def test_only_matching_payload_and_source_reach_native_loader(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            artifact, extracted = root / 'artifact', root / 'extracted'
            artifact.mkdir()
            extracted.mkdir()
            image = artifact / 'Okilum-x86_64.AppImage'
            image.write_bytes(b'exact artifact')
            provenance = {'artifact': image.name, 'sha256': hashlib.sha256(image.read_bytes()).hexdigest(),
                          'source_sha': 'a' * 40}
            (artifact / 'provenance.json').write_text(json.dumps(provenance))
            source = extracted / 'SOURCE_SHA'
            source.write_text('a' * 40 + '\n')
            for name in ('okilum.desktop', 'okilum.svg', 'AppRun'):
                (extracted / name).touch()
            with patch.object(verify.subprocess, 'run', return_value=SimpleNamespace(
                    stdout='gpgv 2.4; libc.so.6 => /lib/libc.so.6', stderr='', returncode=0)) as loader:
                parent_environment = dict(os.environ)
                verify.verify(artifact, extracted)
                self.assertEqual(loader.call_count, 3)
                self.assertEqual(loader.call_args.kwargs['env']['LD_LIBRARY_PATH'],
                                 str(extracted / 'usr/lib'))
                self.assertEqual(dict(os.environ), parent_environment)
                loader.reset_mock()
                source.write_text('b' * 40)
                with self.assertRaisesRegex(ValueError, 'source identity'):
                    verify.verify(artifact, extracted)
                loader.assert_not_called()
                source.write_text('a' * 40)
                image.write_bytes(b'replaced artifact')
                with self.assertRaisesRegex(ValueError, 'checksum'):
                    verify.verify(artifact, extracted)
                loader.assert_not_called()

    def test_shipped_launch_clears_library_override_and_preserves_arguments_and_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            image = Path(temporary) / 'fake shipped artifact'
            image.write_text('#!/bin/sh\n'
                             'test "${LD_LIBRARY_PATH+set}" != set || exit 91\n'
                             'test "$APPIMAGE_EXTRACT_AND_RUN" = 1 || exit 92\n'
                             'printf "%s\\n" "$@"\n'
                             'exit 23\n')
            image.chmod(0o755)
            environment = dict(os.environ, LD_LIBRARY_PATH='/smoke-only/library/path')
            result = subprocess.run(['bash', str(ROOT / 'scripts/appimage/launch.sh'),
                                     str(image), '--vault', 'vault with spaces'],
                                    env=environment, capture_output=True, text=True)
            self.assertEqual(result.returncode, 23)
            self.assertEqual(result.stdout, '--vault\nvault with spaces\n')
            self.assertEqual(environment['LD_LIBRARY_PATH'], '/smoke-only/library/path')

    def test_smoke_help_and_window_use_the_documented_runtime_option(self):
        smoke = (ROOT / 'scripts/appimage/smoke.sh').read_text()
        launch = '/artifact/Okilum-x86_64.AppImage --appimage-extract-and-run'
        self.assertIn(launch + ' --help', smoke)
        self.assertIn(launch + ' "$@"', smoke)
        self.assertIn('unset APPIMAGE_EXTRACT_AND_RUN LD_LIBRARY_PATH', smoke)
        self.assertNotIn('/source/scripts/appimage/launch.sh', smoke)
        self.assertNotIn('/work/squashfs-root/AppRun', smoke)

    def test_required_workflow_retains_artifact_before_testing_and_aggregates_both_gates(self):
        workflow = (ROOT / '.github/workflows/forgejo-linux.yml').read_text()
        self.assertIn('needs: [linux-checks, appimage]', workflow)
        self.assertIn('test "$CHECKS" = success && test "$APPIMAGE" = success', workflow)
        self.assertLess(workflow.index('name: linux-appimage-${{ github.sha }}'),
                        workflow.index('name: Verify unchanged AppImage'))
        self.assertIn('runs-on: ubuntu-22.04', workflow)
        self.assertIn('for image in ubuntu:24.04 fedora:44;', workflow)
        self.assertEqual(workflow.count('bash scripts/appimage/build.sh'), 1)
