"""Exercise fast orchestration without compiling or altering any host limit."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class FastScript(unittest.TestCase):
    def test_platform_arguments_are_nonempty_without_bash32_optional_arrays(self):
        for platform, check_status, shell_status, empty_shell in [('Linux', 0, 0, 0), ('macOS', 0, 0, 0), ('Windows', 0, 0, 0), ('Linux', 23, 0, 0), ('Linux', 0, 41, 0), ('Linux', 0, 0, 1)]:
            with self.subTest(platform=platform, check_status=check_status, shell_status=shell_status, empty_shell=empty_shell), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                for sub in ['bin', 'scripts/ci', 'scripts/updater']:
                    (root / sub).mkdir(parents=True)
                (root / 'scripts/ci/fast.sh').write_text((ROOT / 'scripts/ci/fast.sh').read_text())
                (root / 'scripts/ci/test-nofile.sh').write_text('# no host-limit mutation in this orchestration fixture\n')
                for name, script in {
                    'scripts/vendor-setup.sh': '#!/bin/sh\nexit 0\n',
                    'bin/cargo': '#!/bin/sh\nprintf "%s\\n" "$*" >> "$COMMANDS"\ncase "$*" in check*) exit "${CHECK_STATUS:-0}";; *--bin*--list*) if [ "${EMPTY_SHELL:-0}" = 0 ]; then echo "shell_positive: test"; fi;; *--bin*) exit "${SHELL_STATUS:-0}";; *--list*) echo "parsing_positive: test";; esac\n',
                    'bin/xcrun': '#!/bin/sh\necho /SDK\n',
                    'bin/python': '#!/bin/sh\nexec python3 "$@"\n',
                    'bin/cygpath': '#!/bin/sh\necho "$2"\n',
                }.items():
                    path = root / name; path.write_text(script); path.chmod(0o755)
                for name in ['scripts/ci/test-icon.py', 'scripts/updater/sparkle.py']:
                    (root / name).write_text('# fixture command completed\n')
                commands = root / 'commands'
                env = dict(os.environ, PATH=str(root / 'bin') + os.pathsep + os.environ['PATH'],
                           GITHUB_SHA='a' * 40, RUNNER_OS=platform, RUNNER_TEMP=str(root), COMMANDS=str(commands), CHECK_STATUS=str(check_status), SHELL_STATUS=str(shell_status), EMPTY_SHELL=str(empty_shell))
                result = subprocess.run(['bash', 'scripts/ci/fast.sh'], cwd=root, env=env, capture_output=True)
                expected_status = check_status or shell_status or empty_shell
                self.assertEqual(result.returncode, expected_status, result.stderr)
                calls = commands.read_text().splitlines()
                if platform == 'Linux':
                    self.assertEqual(calls.pop(0), 'test --locked -p okilum-appimage --lib channel -- --test-threads=1')
                    if not check_status:
                        self.assertEqual(calls.pop(2), 'test --offline --locked -p okilum-shell --bin okilum -- --list')
                        if not empty_shell:
                            self.assertEqual(calls.pop(2), 'test --offline --locked -p okilum-shell --bin okilum -- --test-threads=4')
                self.assertEqual(len(calls), 2 if expected_status else 4)
                self.assertEqual('--no-default-features' in calls[0], platform == 'Windows')
                self.assertTrue(calls[0].startswith('build --locked -p okilum-shell -p okilum-sync-supervisor'))
                self.assertEqual(calls[1], 'check --locked --workspace --tests')
                if not expected_status:
                    self.assertIn('--list', calls[2])
                    self.assertIn('test --locked -p okilum-paste --lib', calls[3])
        self.assertNotIn('build_args[@]', (ROOT / 'scripts/ci/fast.sh').read_text())


if __name__ == '__main__':
    unittest.main()
