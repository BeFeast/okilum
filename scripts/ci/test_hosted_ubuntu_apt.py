"""Offline fake-apt regression contracts; run only in the normal hosted gate."""
import importlib.util
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
rewritten = any(h in sources + ''.join(mirrors) for h in (
    'https://azure.archive.ubuntu.com', 'https://archive.ubuntu.com', 'https://mirrors.edge.kernel.org'))
if '--download-only' in args:
    (cache / 'verified.deb').write_text('fixture standing in for a verified download')
if os.environ['MODE'] == 'both_fail' or (
    os.environ['MODE'] == 'fallback' and primary and '--download-only' in args) or (
    os.environ['MODE'] == 'ubuntu_dns_fail' and 'https://mirrors.edge.kernel.org' not in sources) or (
    os.environ['MODE'] == 'https_fail' and rewritten):
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
            # Hosted image since 2026-11: an AWS-region archive host first (run 44244922674).
            mirror.write_text('# GitHub mirror selection\n'
                              'http://us-east-2.ec2.archive.ubuntu.com/ubuntu/\tpriority:1\n'
                              'http://azure.archive.ubuntu.com/ubuntu/\tpriority:2\n'
                              'https://third.example/ubuntu/\tpriority:3\n')
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
                self.assertIn('https://third.example/ubuntu/\tpriority:3\n', record['mirrors'][0])
                self.assertIn('# GitHub mirror selection\n', record['mirrors'][0])
                self.assertIn('Acquire::https::Verify-Peer=true', record['args'])
                self.assertIn('Acquire::https::Verify-Host=true', record['args'])
                record['original'] = f'Dir::Etc::sourcelist={source / "sources.list"}' in record['args']
                if record['original']:
                    # Last route: the runner's own files, read in place and unchanged.
                    self.assertIn(f'Dir::Etc::sourceparts={parts}', record['args'])
                    self.assertIn(f'mirror+file:{mirror}', record['sources'])
                    self.assertEqual(record['mirrors'][0], originals[mirror].decode())
                else:
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
        self.assertIn('https://archive.ubuntu.com/ubuntu/\tpriority:2', calls[2]['mirrors'][0])
        self.assertFalse(any(c['original'] for c in calls))
        self.assertTrue(calls[2]['cached'])
        self.assertEqual(len({c['cache'] for c in calls}), 1)
        self.assertIn('--no-download', calls[-1]['args'])
        self.assertIn('status 124', result.stderr)

    def test_exhaustion_never_installs(self):
        result, calls = self.invoke('both_fail')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(calls), 4)
        self.assertTrue(all(c['args'][-1] == 'update' for c in calls))
        self.assertEqual([c['original'] for c in calls], [False, False, False, True])
        self.assertIn('every route failed', result.stderr)

    def test_https_failure_falls_back_to_runner_original_sources(self):
        result, calls = self.invoke('https_fail')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(calls), 6)
        self.assertTrue(all(c['args'][-1] == 'update' and not c['original'] for c in calls[:3]))
        self.assertTrue(all(c['original'] for c in calls[3:]))
        self.assertIn('http://us-east-2.ec2.archive.ubuntu.com/ubuntu/', calls[3]['mirrors'][0])
        self.assertIn('http://archive.ubuntu.com/ubuntu', calls[3]['sources'])
        self.assertIn('--download-only', calls[4]['args'])
        self.assertIn('--no-download', calls[-1]['args'])
        self.assertTrue(calls[-1]['cached'])
        self.assertEqual(len({c['cache'] for c in calls}), 1)
        self.assertIn('all HTTPS Ubuntu routes failed', result.stderr)

    def test_official_pattern_covers_regional_hosts_only(self):
        spec = importlib.util.spec_from_file_location('hosted_apt', ROOT / 'hosted-ubuntu-apt.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        for host in ('archive', 'azure.archive', 'us.archive', 'us-east-2.ec2.archive',
                     'eu-west-1.ec2.archive', 'security'):
            self.assertTrue(module.OFFICIAL.search(f'http://{host}.ubuntu.com/ubuntu/ x'), host)
        for url in ('http://archive.ubuntu.com.evil.example/ubuntu', 'http://ports.ubuntu.com/ubuntu',
                    'https://esm.ubuntu.com/apps/ubuntu', 'http://archive.ubuntu.com/ubuntu-ports',
                    'http://ppa.launchpadcontent.net/ubuntu'):
            self.assertIsNone(module.OFFICIAL.search(url), url)

    def test_ubuntu_dns_failure_uses_independent_signed_mirror(self):
        result, calls = self.invoke('ubuntu_dns_fail')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(calls), 5)
        self.assertTrue(all(call['args'][-1] == 'update' for call in calls[:2]))
        self.assertIn('https://mirrors.edge.kernel.org/ubuntu', calls[2]['sources'])
        self.assertIn('https://mirrors.edge.kernel.org/ubuntu/', calls[2]['mirrors'][0])
        self.assertIn('--no-download', calls[-1]['args'])
        self.assertTrue(calls[-1]['cached'])

    def test_outer_deadline_and_both_build_jobs(self):
        wrapper = (ROOT / 'hosted-ubuntu-apt.sh').read_text()
        self.assertIn('--kill-after=15s 9m', wrapper)
        workflow = (ROOT.parent.parent / '.github/workflows/forgejo-linux.yml').read_text()
        self.assertEqual(workflow.count('sudo bash scripts/ci/hosted-ubuntu-apt.sh'), 2)
        self.assertEqual(workflow.count('timeout-minutes: 10'), 2)



class HostedUbuntuAptDeadlineTests(unittest.TestCase):
    """Fake clock: every apt call advances it by its granted timeout or by `took`."""

    def setUp(self):
        spec = importlib.util.spec_from_file_location('hosted_apt_clock', ROOT / 'hosted-ubuntu-apt.py')
        self.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.module)

    @staticmethod
    def slow(phase):
        # The 2026-11-18 failure shape: signed update succeeds, download times out.
        return (0, None) if phase == 'update' else (124, None)

    def timeline(self, outcome):
        """outcome(original, phase) -> (status, seconds taken or None for the full window)."""
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / 'apt'
            (source / 'sources.list.d').mkdir(parents=True)
            (source / 'sources.list.d' / 'ubuntu.sources').write_text(
                'Types: deb\nURIs: http://us-east-2.ec2.archive.ubuntu.com/ubuntu\n'
                'Suites: noble\nComponents: main\n'
                'Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n')
            now = [1000.0]
            calls = []

            def run(argv, env, check):
                original = f'Dir::Etc::sourcelist={source / "sources.list"}' in argv
                phase = ('update' if argv[-1] == 'update' else
                         'download' if '--download-only' in argv else 'install')
                granted = int(env['OKILUM_APT_TIMEOUT'].removesuffix('s'))
                status, took = outcome(original, phase)
                calls.append({'original': original, 'phase': phase, 'granted': granted,
                              'start': now[0] - 1000})
                now[0] += granted if took is None else min(took, granted)
                return subprocess.CompletedProcess(argv, status)

            result = self.module.provision(['cmake'], source, clock=lambda: now[0], run=run)
            return result, calls, now[0] - 1000

    def test_wrapper_deadline_matches_budget(self):
        wrapper = (ROOT / 'hosted-ubuntu-apt.sh').read_text()
        self.assertIn('--kill-after=15s 9m', wrapper)
        self.assertEqual(self.module.OUTER_DEADLINE, 9 * 60)
        self.assertLess(self.module.BUDGET, self.module.OUTER_DEADLINE)

    def test_https_timeouts_leave_fallback_its_reserved_window(self):
        # Every HTTPS phase uses its whole window; the runner sources then work.
        result, calls, elapsed = self.timeline(
            lambda original, phase: (0, 20) if original else self.slow(phase))
        self.assertEqual(result, 0)
        self.assertLessEqual(elapsed, self.module.BUDGET)
        https = [c for c in calls if not c['original']]
        fallback = [c for c in calls if c['original']]
        # Positive control: without the reserve these phases alone would take
        # 3 * (60 + 150) = 630 s, beyond the 540 s outer deadline.
        self.assertEqual([c['granted'] for c in https[:2]], [60, 150])
        self.assertLessEqual(sum(c['granted'] for c in https),
                             self.module.BUDGET - self.module.FALLBACK_RESERVE
                             - self.module.INSTALL_RESERVE)
        self.assertEqual([c['phase'] for c in fallback], ['update', 'download', 'install'])
        self.assertEqual(fallback[0]['granted'], self.module.UPDATE)
        self.assertGreaterEqual(fallback[1]['granted'], 90)
        self.assertGreaterEqual(fallback[2]['granted'], self.module.INSTALL_RESERVE)

    def test_exhausted_timeline_stays_inside_outer_deadline(self):
        result, calls, elapsed = self.timeline(lambda original, phase: self.slow(phase))
        self.assertNotEqual(result, 0)
        self.assertLessEqual(elapsed, self.module.BUDGET)
        self.assertTrue(all(c['granted'] >= self.module.MIN_PHASE for c in calls))
        self.assertTrue(any(c['original'] and c['phase'] == 'download' for c in calls))
        self.assertFalse(any(c['phase'] == 'install' for c in calls))

    def test_fast_failures_keep_full_windows_for_every_route(self):
        # DNS failure: updates fail at once, so later routes keep nominal bounds.
        result, calls, _ = self.timeline(
            lambda original, phase: (0, 20) if original else (100, 1))
        self.assertEqual(result, 0)
        self.assertEqual([c['granted'] for c in calls], [60, 60, 60, 60, 150, 240])

    def test_https_success_install_keeps_full_bound(self):
        result, calls, _ = self.timeline(lambda original, phase: (0, 30))
        self.assertEqual(result, 0)
        self.assertEqual([(c['phase'], c['granted']) for c in calls],
                         [('update', 60), ('download', 150), ('install', 240)])


if __name__ == '__main__':
    unittest.main()
