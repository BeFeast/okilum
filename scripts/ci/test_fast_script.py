"""Exercise fast orchestration without compiling or altering any host limit."""
import os
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import re
import shlex
import subprocess
import tempfile
import time
import threading
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[2]


class FastScript(unittest.TestCase):
    def test_certification_checkout_keeps_supervision_for_a_product_without_watchdogs(self):
        workflow = yaml.safe_load((ROOT / '.forgejo/workflows/fast.yml').read_text())
        steps = workflow['jobs']['check']['steps']
        validation = next(s['run'] for s in steps if 'certification-source ' in s.get('run', ''))
        discovery = next(s['run'] for s in steps if 'ci-python ' in s.get('run', ''))
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo = root / 'product'
            repo.mkdir()

            def git(*args):
                return subprocess.check_output(['git', '-C', str(repo), *args],
                                               stderr=subprocess.PIPE, text=True).strip()

            git('init', '-b', 'main')
            git('config', 'user.name', 'Certification fixture')
            git('config', 'user.email', 'fixture@example.invalid')
            git('config', 'commit.gpgsign', 'false')
            suite = repo / 'scripts/ci'
            suite.mkdir(parents=True)
            (suite / 'test_product.py').write_text(
                'import unittest\nclass Product(unittest.TestCase):\n'
                '    def test_product(self): pass\n')
            git('add', '.')
            git('commit', '-m', 'Product predating watchdog tooling')
            source = git('rev-parse', 'HEAD')
            for name in ['test-group.sh', 'test-group.py']:
                (suite / name).write_text((ROOT / 'scripts/ci' / name).read_text())
            releases = repo / 'scripts/releases'
            releases.mkdir()
            (releases / 'hourly_beta.py').write_text((ROOT / 'scripts/releases/hourly_beta.py').read_text())
            git('add', '.')
            git('commit', '-m', 'Queued workflow tooling at a newer revision')
            tooling = git('rev-parse', 'HEAD')
            git('remote', 'add', 'origin', str(repo))

            class API(BaseHTTPRequestHandler):
                def do_GET(self):
                    if self.path.endswith('/branches/main'):
                        data = {'commit': {'id': tooling}}
                    elif self.path.endswith('/git/commits/' + tooling):
                        data = {'sha': tooling, 'parents': [{'sha': source}]}
                    else:
                        self.send_error(404)
                        return
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.write(json.dumps(data).encode())

                def log_message(self, *args):
                    pass

            server = ThreadingHTTPServer(('127.0.0.1', 0), API)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            env = dict(os.environ, RUNNER_OS='Linux', RUNNER_TEMP=str(root),
                       GITHUB_ENV=str(root / 'github-env'), GITHUB_SHA=tooling,
                       GITHUB_REF='refs/heads/main', GITHUB_EVENT_NAME='workflow_dispatch',
                       CERTIFICATION_SOURCE=source, FORGEJO_URL=f'http://127.0.0.1:{server.server_port}')
            try:
                result = subprocess.run(['bash', '-c', validation], cwd=repo, env=env,
                                        capture_output=True, text=True, timeout=10)
            finally:
                server.shutdown()
                server.server_close()
                thread.join(timeout=5)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn('TEST_GROUP_START group=certification-source limit=90s', result.stdout)
            self.assertEqual(git('rev-parse', 'HEAD'), source)
            self.assertEqual(git('rev-parse', 'main'), tooling)
            self.assertEqual(git('status', '--porcelain'), '')
            self.assertFalse((suite / 'test-group.sh').exists())
            key, value = (root / 'github-env').read_text().strip().split('=', 1)
            env[key] = value
            self.assertTrue(Path(value).is_relative_to(root))
            for outcome, status in [('pass', 0), ('self.fail("positive control")', 1),
                                    ('__import__("time").sleep(60)', 124)]:
                with self.subTest(outcome=outcome):
                    (suite / 'test_product.py').write_text(
                        'import unittest\nclass Product(unittest.TestCase):\n'
                        f'    def test_product(self): {outcome}\n')
                    result = subprocess.run(['bash', '-c', discovery.replace('ci-python 180', 'ci-python 1')],
                                            cwd=repo, env=env, capture_output=True, text=True, timeout=5)
                    self.assertEqual(result.returncode, status, result.stdout + result.stderr)
                    self.assertIn('test_product (test_product.Product.test_product)', result.stderr)
                    self.assertIn('TEST_GROUP_START group=ci-python limit=1s', result.stdout)
                    if status == 124:
                        self.assertIn('TEST_GROUP_TIMEOUT group=ci-python limit=1s', result.stdout)

    def test_required_check_runs_full_isolated_python_discovery_and_fails_on_errors(self):
        workflow = (ROOT / '.forgejo/workflows/fast.yml').read_text()
        self.assertNotIn('  pull_request:', workflow)
        check = workflow.split('  check:\n', 1)[1].split('\n  shell-full:', 1)[0]
        self.assertNotIn('continue-on-error:', check)
        for directory, label in [('ci', 'CI tooling'), ('releases', 'releases'), ('updater', 'updater')]:
            with self.subTest(suite=directory):
                name = f'Run the full {label} Python suite'
                match = re.search(r'      - name: ' + name + r'\n(.*?)(?=      - name:)', check, re.S)
                self.assertIsNotNone(match, name)
                step = match[1]
                self.assertNotIn('if:', step)
                limit = '180' if directory == 'ci' else '90'
                self.assertIn('        timeout-minutes: 5\n', step)
                command = shlex.split(step.split('        run: ', 1)[1].strip())
                self.assertEqual(command, ['bash', '${OKILUM_TEST_GROUP:-scripts/ci/test-group.sh}', f'{directory}-python', limit,
                                           'python3', '-I', '-m', 'unittest', 'discover',
                                           '-s', f'scripts/{directory}', '-p', 'test_*.py', '-v'])
                self.assertLess(check.index(name),
                                check.index('Build Linux desktop and run fast unit tests'))
                with tempfile.TemporaryDirectory() as temp:
                    root = Path(temp)
                    suite = root / 'scripts' / directory
                    suite.mkdir(parents=True)
                    ci = root / 'scripts/ci'
                    ci.mkdir(parents=True, exist_ok=True)
                    for name in ['test-group.sh', 'test-group.py']:
                        (ci / name).write_text((ROOT / 'scripts/ci' / name).read_text())
                    for outcome in ['pass', 'self.fail("positive failure control")',
                                    'raise RuntimeError("positive error control")']:
                        with self.subTest(outcome=outcome):
                            (suite / 'test_discovery.py').write_text(
                                'import unittest\nclass Discovery(unittest.TestCase):\n'
                                f'    def test_discovered(self): {outcome}\n')
                            result = subprocess.run(['bash', '-c', step.split('        run: ', 1)[1].strip()], cwd=root, capture_output=True,
                                                    text=True, timeout=5)
                            self.assertEqual(result.returncode == 0, outcome == 'pass', result.stderr)
                            self.assertIn(f'TEST_GROUP_START group={directory}-python limit={limit}s', result.stdout)
                            self.assertIn('Ran 1 test', result.stderr)
                            summary = ('OK' if outcome == 'pass' else 'FAILED (failures=1)'
                                       if outcome.startswith('self.fail') else 'FAILED (errors=1)')
                            self.assertIn(summary, result.stderr)

    def test_platform_arguments_are_nonempty_without_bash32_optional_arrays(self):
        for platform, check_status in [('Linux', 0), ('macOS', 0), ('Windows', 0), ('Linux', 23)]:
            with self.subTest(platform=platform, check_status=check_status), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                for sub in ['bin', 'scripts/ci', 'scripts/updater']:
                    (root / sub).mkdir(parents=True)
                for name in ['test-group.sh', 'test-group.py']:
                    (root / 'scripts/ci' / name).write_text((ROOT / 'scripts/ci' / name).read_text())
                (root / 'scripts/ci/fast.sh').write_text((ROOT / 'scripts/ci/fast.sh').read_text())
                (root / 'scripts/ci/test-nofile.sh').write_text('# no host-limit mutation in this orchestration fixture\n')
                for name, script in {
                    'scripts/vendor-setup.sh': '#!/bin/sh\nexit 0\n',
                    'bin/cargo': '#!/bin/sh\nprintf "%s\\n" "$*" >> "$COMMANDS"\ncase "$*" in check*) exit "${CHECK_STATUS:-0}";; *--list*) echo "parsing_positive: test";; esac\n',
                    'bin/xcrun': '#!/bin/sh\necho /SDK\n',
                    'bin/python': '#!/bin/sh\nexec python3 "$@"\n',
                    'bin/cygpath': '#!/bin/sh\necho "$2"\n',
                }.items():
                    path = root / name; path.write_text(script); path.chmod(0o755)
                for name in ['scripts/ci/test-icon.py', 'scripts/updater/sparkle.py']:
                    (root / name).write_text('# fixture command completed\n')
                commands = root / 'commands'
                env = dict(os.environ, PATH=str(root / 'bin') + os.pathsep + os.environ['PATH'],
                           GITHUB_SHA='a' * 40, RUNNER_OS=platform, RUNNER_TEMP=str(root), COMMANDS=str(commands), CHECK_STATUS=str(check_status))
                result = subprocess.run(['bash', 'scripts/ci/fast.sh'], cwd=root, env=env, capture_output=True)
                self.assertEqual(result.returncode, check_status, result.stderr)
                if platform == 'Linux':
                    self.assertIn(b'TEST_GROUP_START group=build limit=540s', result.stdout)
                    self.assertIn(b'TEST_GROUP_START group=workspace-test-check limit=540s', result.stdout)
                    if not check_status:
                        self.assertIn(b'TEST_GROUP_START group=paste-tests limit=120s', result.stdout)
                calls = commands.read_text().splitlines()
                if platform == 'Linux':
                    self.assertEqual(calls.pop(0), 'test --locked -p okilum-appimage --lib channel -- --test-threads=1')
                self.assertEqual(len(calls), 2 if check_status else 4)
                self.assertEqual('--no-default-features' in calls[0], platform == 'Windows')
                self.assertTrue(calls[0].startswith('build --locked -p okilum-shell -p okilum-sync-supervisor'))
                self.assertEqual(calls[1], 'check --locked --workspace --tests')
                if not check_status:
                    self.assertIn('--list', calls[2])
                    self.assertIn('test --locked -p okilum-paste --lib', calls[3])
        self.assertNotIn('build_args[@]', (ROOT / 'scripts/ci/fast.sh').read_text())


class ShellFullScript(unittest.TestCase):
    def fixture(self, root):
        (root / 'scripts/ci').mkdir(parents=True)
        (root / 'crates/okilum-shell').mkdir(parents=True)
        (root / 'bin').mkdir()
        for name in ['shell-full.sh', 'test-nofile.sh', 'test-group.sh', 'test-group.py']:
            (root / 'scripts/ci' / name).write_text((ROOT / 'scripts/ci' / name).read_text())
        vendor = root / 'scripts/vendor-setup.sh'
        vendor.write_text('#!/bin/sh\nexit 0\n')
        vendor.chmod(0o755)
        cargo = root / 'bin/cargo'
        cargo.write_text('''#!/usr/bin/env python3
import json, os, sys, time
from pathlib import Path
with open(os.environ['COMMANDS'], 'a') as log:
    print('cargo ' + ' '.join(sys.argv[1:]), file=log)
Path(os.environ['RUNNER_TEMP'], 'compiler.pid').write_text(str(os.getpid()))
for i in range(int(os.environ.get('COMPILE_TICKS', '0'))):
    with open(os.environ['RUNNER_TEMP'] + '/compile-progress', 'a') as progress:
        print(i, file=progress)
    time.sleep(0.1)
if int(os.environ['COMPILE_STATUS']):
    sys.exit(int(os.environ['COMPILE_STATUS']))
print(json.dumps({'reason': 'compiler-artifact', 'target': {'name': 'okilum'},
                  'profile': {'test': False}, 'executable': '/not-the-test-artifact'}))
print(json.dumps({'reason': 'compiler-artifact', 'target': {'name': 'okilum'},
                  'profile': {'test': True}, 'executable': os.environ['TEST_BINARY']}))
''')
        cargo.chmod(0o755)
        binary = root / 'bin/shell test binary'
        binary.write_text('''#!/usr/bin/env python3
import os, sys, time
from pathlib import Path
assert Path.cwd() == Path(os.environ['RUNNER_TEMP']) / 'crates/okilum-shell'
with open(os.environ['COMMANDS'], 'a') as log:
    print('binary ' + ' '.join(sys.argv[1:]), file=log)
if '--list' in sys.argv:
    if os.environ['EMPTY'] == '0': print('shell_positive: test')
    sys.exit(int(os.environ['LIST_STATUS']))
Path(os.environ['RUNNER_TEMP'], 'test.pid').write_text(str(os.getpid()))
if os.environ.get('HANG') == '1':
    while True: time.sleep(1)
sys.exit(int(os.environ['TEST_STATUS']))
''')
        binary.chmod(0o755)
        return dict(os.environ, PATH=str(root / 'bin') + os.pathsep + os.environ['PATH'],
                    GITHUB_SHA='a' * 40, RUNNER_TEMP=str(root), COMMANDS=str(root / 'commands'),
                    TEST_BINARY=str(binary), COMPILE_STATUS='0', LIST_STATUS='0', TEST_STATUS='0', EMPTY='0')

    def test_unfiltered_suite_inventory_and_failures(self):
        for compile_status, list_status, test_status, empty in [
                (0, 0, 0, False), (19, 0, 0, False), (0, 23, 0, False),
                (0, 0, 41, False), (0, 0, 0, True)]:
            with self.subTest(compile_status=compile_status, list_status=list_status,
                              test_status=test_status, empty=empty), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                env = self.fixture(root)
                env.update(COMPILE_STATUS=str(compile_status), LIST_STATUS=str(list_status),
                           TEST_STATUS=str(test_status), EMPTY=str(int(empty)))
                result = subprocess.run(['bash', 'scripts/ci/shell-full.sh'], cwd=root, env=env,
                                        capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, compile_status or list_status or test_status or int(empty), result.stderr)
                self.assertIn('TEST_GROUP_START group=shell-binary-compile limit=840s', result.stdout)
                expected = ['cargo test --locked -p okilum-shell --bin okilum --no-run --message-format=json-render-diagnostics']
                if not compile_status:
                    self.assertIn('TEST_GROUP_START group=shell-binary-list limit=30s', result.stdout)
                    expected.append('binary --list')
                    if not list_status and not empty:
                        self.assertIn('TEST_GROUP_START group=shell-binary-tests limit=990s', result.stdout)
                        expected.append('binary --test-threads=4')
                self.assertEqual((root / 'commands').read_text().splitlines(), expected)
                self.assertEqual(list(root.glob('shell-artifacts-*')), [])

    def test_progressing_compile_outlives_inventory_limit_and_tests_keep_their_own_deadline(self):
        # Exercise the actual shell entrypoint and real supervisor with scaled
        # budgets. Never compile Rust on maestro. The compiler writes progress
        # throughout a 3 s build, three times the inventory deadline.
        for hang in [False, True]:
            with self.subTest(hang=hang), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                env = self.fixture(root)
                env.update(COMPILE_TICKS='30', HANG=str(int(hang)))
                script = root / 'scripts/ci/shell-full.sh'
                script.write_text(script.read_text().replace('compile 840', 'compile 10')
                                  .replace('list 30', 'list 1').replace('tests 990', 'tests 2'))
                started = time.monotonic()
                result = subprocess.run(['bash', str(script)], cwd=root, env=env,
                                        capture_output=True, text=True, timeout=45)
                self.assertEqual(result.returncode, 124 if hang else 0, result.stdout + result.stderr)
                self.assertEqual((root / 'compile-progress').read_text().splitlines(), list(map(str, range(30))))
                self.assertGreater(time.monotonic() - started, 3)
                self.assertIn('TEST_GROUP_END group=shell-binary-compile', result.stdout)
                self.assertNotIn('TEST_GROUP_TIMEOUT group=shell-binary-compile', result.stdout)
                self.assertEqual(result.stdout.count('TEST_GROUP_TIMEOUT'), int(hang))
                if hang:
                    self.assertIn('TEST_GROUP_TIMEOUT group=shell-binary-tests limit=2s', result.stdout)
                    self.assertLess(time.monotonic() - started, 10 + 2 + 30 + 1)
                for name in ['compiler.pid', 'test.pid']:
                    self.assertFalse(Path('/proc', (root / name).read_text()).exists(), name)


if __name__ == '__main__':
    unittest.main()
