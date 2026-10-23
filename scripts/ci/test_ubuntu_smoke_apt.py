"""Offline mirror/retry contract; executed only by the normal hosted Linux gate."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).parent
FAKE = r'''
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
source = next(x.split('=', 1)[1] for x in args if x.startswith('Dir::Etc::sourcelist='))
text = Path(source).read_text()
with open(os.environ['CALLS'], 'a') as log:
    log.write(json.dumps({'args': args, 'sources': text}) + '\n')
mode = os.environ['MODE']
if mode == 'both_fail' or (mode == 'fallback' and 'https://archive.' in text and '--download-only' in args):
    sys.exit(124)
'''


class UbuntuSmokeAptTests(unittest.TestCase):
    def invoke(self, mode):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            # A fixture OS file lets this offline test run on any hosted Linux.
            release = root / 'os-release'
            release.write_text('ID=ubuntu\nVERSION_ID=24.04\n')
            script = root / 'ubuntu-smoke-apt.sh'
            script.write_text((ROOT / script.name).read_text().replace('/etc/os-release', str(release)))
            shutil.copy(ROOT / 'apt-bounded.sh', root)
            apt = root / 'apt-get'
            apt.write_text('#!/usr/bin/env python3\n' + FAKE)
            apt.chmod(0o755)
            dpkg = root / 'dpkg'
            dpkg.write_text('#!/bin/sh\necho amd64\n')
            dpkg.chmod(0o755)
            ca = root / 'ca.crt'
            ca.write_text('test fixture')
            calls = root / 'calls'
            env = dict(os.environ, PATH=f'{root}:' + os.environ['PATH'],
                       CALLS=str(calls), MODE=mode, OKILUM_APT_CA_FILE=str(ca))
            result = subprocess.run(['bash', str(script), '--bounded', 'python3', 'file'],
                                    env=env, capture_output=True, text=True, timeout=10)
            records = [json.loads(line) for line in calls.read_text().splitlines()]
            self.assertTrue(records, 'fake apt never started')
            for record in records:
                self.assertIn('signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg', record['sources'])
                self.assertIn('noble-security main universe', record['sources'])
                self.assertIn('Acquire::https::CaInfo=' + str(ca), record['args'])
                self.assertIn('Dir::Etc::sourceparts=-', record['args'])
                self.assertFalse(any('AllowUnauthenticated' in arg or 'Verify-Peer=false' in arg for arg in record['args']))
            return result, records

    def test_primary_positive_control(self):
        result, calls = self.invoke('success')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(calls), 3)
        self.assertIn('--download-only', calls[1]['args'])
        self.assertIn('--no-download', calls[2]['args'])
        self.assertTrue(all('https://archive.ubuntu.com/' in c['sources'] for c in calls))

    def test_failed_download_retries_once_with_other_official_mirror(self):
        result, calls = self.invoke('fallback')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(calls), 5)
        self.assertIn('https://security.ubuntu.com/', calls[2]['sources'])
        self.assertIn('--no-download', calls[-1]['args'])
        self.assertIn('status 124', result.stderr)

    def test_both_updates_fail_closed_without_install(self):
        result, calls = self.invoke('both_fail')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(calls), 2)
        self.assertTrue(all(c['args'][-1] == 'update' for c in calls))


if __name__ == '__main__':
    unittest.main()
