"""QA packages retain executable mode and detect corrupted payloads."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('linux_package', Path(__file__).with_name('package-linux-binary.py'))
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)


class LinuxBinaryTests(unittest.TestCase):
    def test_archive_roundtrip_and_corruption_positive_control(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / 'built-reader'
            binary.write_text('#!/bin/sh\nprintf "reader-control\\n"\n')
            sha = 'a' * 40
            archive = package.package(binary, root / 'out', sha)
            subprocess.run(['sha256sum', '-c', archive.name + '.sha256'], cwd=archive.parent,
                           check=True, capture_output=True)
            subprocess.run(['tar', '--zstd', '-xf', str(archive), '-C', str(root)], check=True)
            payload = root / f'okilum-linux-x86_64-{sha}'
            self.assertEqual((payload / 'SOURCE_SHA').read_text(), sha + '\n')
            self.assertEqual(subprocess.check_output([str(payload / 'okilum')], text=True), 'reader-control\n')
            subprocess.run(['sha256sum', '-c', 'SHA256SUMS'], cwd=payload, check=True, capture_output=True)
            (payload / 'okilum').write_bytes(b'corrupted')
            result = subprocess.run(['sha256sum', '-c', 'SHA256SUMS'], cwd=payload, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b'okilum: FAILED', result.stdout)

    def test_invalid_source_sha_is_rejected(self):
        with self.assertRaises(ValueError):
            package.package(Path('/nonexistent'), Path('/nonexistent'), '../main')
