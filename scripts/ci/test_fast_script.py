"""Exercise fast orchestration without compiling or altering any host limit."""
import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class FastScript(unittest.TestCase):
    def test_release_and_updater_suites_are_required_on_the_same_check_job(self):
        workflow = (ROOT / '.forgejo/workflows/fast.yml').read_text()
        check = workflow.split('  check:\n', 1)[1].split('\n  shell-full:', 1)[0]
        for suite in ['releases', 'updater']:
            step = re.search(r'      - name: Run the full ' + suite + r' Python suite\n(.*?)(?=      - name:)',
                             check, re.S)[1]
            self.assertNotIn('if:', step)
            self.assertNotIn('continue-on-error:', step)
            command = shlex.split(step.split('        run: ', 1)[1].strip())
            self.assertEqual(command, ['python3', '-I', '-m', 'unittest', 'discover',
                                      '-s', 'scripts/' + suite, '-p', 'test_*.py', '-v'])
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                tests = root / 'scripts' / suite
                tests.mkdir(parents=True)
                for outcome in ['pass', 'self.fail("positive failure control")',
                                'raise RuntimeError("positive error control")']:
                    (tests / 'test_contract.py').write_text('import unittest\nclass Contract(unittest.TestCase):\n'
                        f'    def test_discovered(self): {outcome}\n')
                    result = subprocess.run(command, cwd=root, capture_output=True, text=True, timeout=5)
                    self.assertEqual(result.returncode == 0, outcome == 'pass', result.stderr)
                    self.assertIn('Ran 1 test', result.stderr)

    def test_required_check_runs_full_isolated_python_discovery_and_fails_on_errors(self):
        workflow = (ROOT / '.forgejo/workflows/fast.yml').read_text()
        self.assertIn('on:\n  pull_request:\n', workflow)
        check = workflow.split('  check:\n', 1)[1].split('\n  shell-full:', 1)[0]
        step = re.search(r'      - name: Run the full CI tooling Python suite\n(.*?)(?=      - name:)',
                         check, re.S)[1]
        self.assertNotIn('if:', step)
        self.assertNotIn('continue-on-error:', step)
        command = shlex.split(step.split('        run: ', 1)[1].strip())
        self.assertEqual(command, ['python3', '-I', '-m', 'unittest', 'discover',
                                   '-s', 'scripts/ci', '-p', 'test_*.py', '-v'])
        self.assertLess(check.index('Run the full CI tooling Python suite'),
                        check.index('Build Linux desktop and run fast unit tests'))
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            suite = root / 'scripts/ci'
            suite.mkdir(parents=True)
            for outcome in ['pass', 'self.fail("positive failure control")',
                            'raise RuntimeError("positive error control")']:
                (suite / 'test_discovery.py').write_text(
                    'import unittest\nclass Discovery(unittest.TestCase):\n'
                    f'    def test_discovered(self): {outcome}\n')
                result = subprocess.run(command, cwd=root, capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode == 0, outcome == 'pass', result.stderr)
                self.assertIn('Ran 1 test', result.stderr)

    def test_platform_arguments_are_nonempty_without_bash32_optional_arrays(self):
        for platform, check_status in [('Linux', 0), ('macOS', 0), ('Windows', 0), ('Linux', 23)]:
            with self.subTest(platform=platform, check_status=check_status), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                for sub in ['bin', 'scripts/ci', 'scripts/updater']:
                    (root / sub).mkdir(parents=True)
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
    def test_unfiltered_suite_inventory_and_failures(self):
        for list_status, test_status, empty in [(0, 0, False), (23, 0, False), (0, 41, False), (0, 0, True)]:
            with self.subTest(list_status=list_status, test_status=test_status, empty=empty), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                (root / 'scripts/ci').mkdir(parents=True)
                (root / 'bin').mkdir()
                for name in ['shell-full.sh', 'test-nofile.sh']:
                    (root / 'scripts/ci' / name).write_text((ROOT / 'scripts/ci' / name).read_text())
                vendor = root / 'scripts/vendor-setup.sh'
                vendor.write_text('#!/bin/sh\nexit 0\n')
                vendor.chmod(0o755)
                cargo = root / 'bin/cargo'
                cargo.write_text('''#!/bin/sh
printf '%s\\n' "$*" >> "$COMMANDS"
case "$*" in
    *--list*)
        if [ "$EMPTY" = 0 ]; then echo 'shell_positive: test'; fi
        exit "$LIST_STATUS" ;;
    *) exit "$TEST_STATUS" ;;
esac
''')
                cargo.chmod(0o755)
                commands = root / 'commands'
                env = dict(os.environ, PATH=str(root / 'bin') + os.pathsep + os.environ['PATH'],
                           GITHUB_SHA='a' * 40, RUNNER_TEMP=str(root), COMMANDS=str(commands),
                           LIST_STATUS=str(list_status), TEST_STATUS=str(test_status), EMPTY=str(int(empty)))
                result = subprocess.run(['bash', 'scripts/ci/shell-full.sh'], cwd=root, env=env, capture_output=True)
                self.assertEqual(result.returncode, list_status or test_status or int(empty), result.stderr)
                expected = ['test --locked -p okilum-shell --bin okilum -- --list']
                if not list_status and not empty:
                    expected.append('test --offline --locked -p okilum-shell --bin okilum -- --test-threads=4')
                self.assertEqual(commands.read_text().splitlines(), expected)


if __name__ == '__main__':
    unittest.main()
