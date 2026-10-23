"""Offline fake-apt regression contracts; run only in the normal hosted gate."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).parent
FAKE = r'''
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
def option(name):
    return next(x.split('=', 1)[1] for x in args if x.startswith(name + '='))
main = Path(option('Dir::Etc::sourcelist')).read_text()
parts = Path(option('Dir::Etc::sourceparts'))
sources = main + ''.join(p.read_text() for p in sorted(parts.iterdir()))
import re
mirrors = [Path(p).read_text() for p in re.findall(r'mirror\+file:([^\s]+)', sources)]
cache = Path(option('Dir::Cache::archives'))
record = {'args': args, 'sources': sources, 'mirrors': mirrors,
          'cache': str(cache), 'cached': (cache / 'verified.deb').exists()}
with open(os.environ['CALLS'], 'a') as log:
    log.write(json.dumps(record) + '\n')
primary = 'https://azure.archive.ubuntu.com' in sources + ''.join(mirrors)
if '--download-only' in args:
    (cache / 'verified.deb').write_text('fixture standing in for a verified download')
if os.environ['MODE'] == 'both_fail' or (
    os.environ['MODE'] == 'fallback' and primary and '--download-only' in args):
    sys.exit(124)
'''


class HostedUbuntuAptTests(unittest.TestCase):
    def invoke(self, mode):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / 'apt'
            parts = source / 'sources.list.d'
            parts.mkdir(parents=True)
            mirror = root / 'ubuntu.list'
            mirror.write_text('# GitHub mirror selection\nhttp://azure.archive.ubuntu.com/ubuntu/\tpriority:1\nhttps://third.example/ubuntu/\tpriority:2\n')
            (source / 'sources.list').write_text(
                f'deb [arch=amd64 signed-by=/keys/ubuntu.gpg] mirror+file:{mirror} jammy main universe\n'
                'deb [signed-by=/keys/vendor.gpg] https://vendor.example/repo stable main\n')
            (parts / 'ubuntu.sources').write_text(
                'Types: deb deb-src\nURIs: http://archive.ubuntu.com/ubuntu http://security.ubuntu.com/ubuntu\n'
                'Suites: noble noble-updates noble-security\nComponents: main universe\n'
                'Architectures: amd64\nSigned-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n')
            (parts / 'vendor.sources').write_text(
                'Types: deb\nURIs: https://packages.example/repo\nSuites: stable\nSigned-By: /keys/vendor.gpg\n')
            originals = {p: p.read_bytes() for p in [source / 'sources.list', mirror, *parts.iterdir()]}
            apt = root / 'apt-get'
            apt.write_text(f'#!{sys.executable}\n' + FAKE)
            apt.chmod(0o755)
            calls = root / 'calls'
            env = dict(os.environ, PATH=f'{root}:' + os.environ['PATH'], CALLS=str(calls), MODE=mode)
            # Child process uses the real bounded wrapper, but only fake apt.
            driver = ('import importlib.util, pathlib, sys; '
                      's=importlib.util.spec_from_file_location("apt", sys.argv[1]); '
                      'm=importlib.util.module_from_spec(s); s.loader.exec_module(m); '
                      'sys.exit(m.provision(["cmake"], pathlib.Path(sys.argv[2])))')
            result = subprocess.run([sys.executable, '-c', driver,
                                     str(ROOT / 'hosted-ubuntu-apt.py'), str(source)],
                                    env=env, capture_output=True, text=True, timeout=10)
            self.assertTrue(calls.exists(), result.stderr)  # Positive control.
            records = [json.loads(line) for line in calls.read_text().splitlines()]
            self.assertTrue(records, 'fake apt did not observe provisioning')
            for p, original in originals.items():
                self.assertEqual(p.read_bytes(), original, f'original config mutated: {p}')
            for record in records:
                self.assertIn('Suites: noble noble-updates noble-security', record['sources'])
                self.assertIn('Architectures: amd64', record['sources'])
                self.assertIn('signed-by=/keys/ubuntu.gpg', record['sources'])
                self.assertIn('Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg', record['sources'])
                self.assertIn('jammy main universe', record['sources'])
                self.assertIn('https://vendor.example/repo stable main', record['sources'])
                self.assertIn(originals[parts / 'vendor.sources'].decode(), record['sources'])
                self.assertIn('https://third.example/ubuntu/\tpriority:2\n', record['mirrors'][0])
                self.assertIn('# GitHub mirror selection\n', record['mirrors'][0])
                self.assertIn('Acquire::https::Verify-Peer=true', record['args'])
                self.assertIn('Acquire::https::Verify-Host=true', record['args'])
                self.assertNotIn('http://', record['sources'] + ''.join(record['mirrors']))
            return result, records

    def test_primary_positive_control(self):
        result, calls = self.invoke('success')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(calls), 3)
        self.assertIn('--no-download', calls[-1]['args'])
        self.assertTrue(calls[-1]['cached'])

    def test_download_timeout_fallback_reuses_cache(self):
        result, calls = self.invoke('fallback')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(calls), 5)
        self.assertIn('https://archive.ubuntu.com/ubuntu', calls[2]['sources'])
        self.assertIn('https://archive.ubuntu.com/ubuntu/\tpriority:1', calls[2]['mirrors'][0])
        self.assertTrue(calls[2]['cached'])
        self.assertEqual(len({c['cache'] for c in calls}), 1)
        self.assertIn('--no-download', calls[-1]['args'])
        self.assertIn('status 124', result.stderr)

    def test_exhaustion_never_installs(self):
        result, calls = self.invoke('both_fail')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(calls), 2)
        self.assertTrue(all(c['args'][-1] == 'update' for c in calls))

    def test_outer_deadline_and_both_build_jobs(self):
        wrapper = (ROOT / 'hosted-ubuntu-apt.sh').read_text()
        self.assertIn('--kill-after=15s 9m', wrapper)
        workflow = (ROOT.parent.parent / '.github/workflows/forgejo-linux.yml').read_text()
        self.assertEqual(workflow.count('sudo bash scripts/ci/hosted-ubuntu-apt.sh'), 2)
        self.assertEqual(workflow.count('timeout-minutes: 10'), 2)


if __name__ == '__main__':
    unittest.main()
