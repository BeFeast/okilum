"""Run the real fast script with a fake compiler; shell failures must block admission."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name('fast.sh')
SHELL = 'test --offline --locked -p okilum-shell --bin okilum'


class ShellAdmission(unittest.TestCase):
    def execute(self, status):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            scripts = root / 'scripts/ci'
            scripts.mkdir(parents=True)
            shutil.copyfile(SCRIPT, scripts / 'fast.sh')
            (scripts / 'test-nofile.sh').write_text(':\n')
            vendor = root / 'scripts/vendor-setup.sh'
            vendor.write_text('#!/usr/bin/env bash\nexit 0\n')
            vendor.chmod(0o755)
            commands = root / 'commands'
            cargo = root / 'cargo'
            cargo.write_text('''#!/usr/bin/env bash
printf '%s\\n' "$*" >> "$COMMAND_LOG"
if [[ "$*" == 'test --offline --locked -p okilum-shell --bin okilum' ]]; then
    exit "$SHELL_STATUS"
fi
if [[ "$*" == *'--list' ]]; then echo 'parsing_control: test'; fi
''')
            cargo.chmod(0o755)
            result = subprocess.run(
                ['bash', str(scripts / 'fast.sh')], cwd=root,
                env={**os.environ, 'PATH': f'{root}:{os.environ["PATH"]}',
                     'RUNNER_OS': 'Linux', 'GITHUB_SHA': 'a' * 40,
                     'RUNNER_TEMP': str(root), 'COMMAND_LOG': str(commands),
                     'SHELL_STATUS': str(status), 'OKILUM_LIVE_PREVIEW_TESTS': '0'},
                capture_output=True, text=True, timeout=5,
            )
            return result, commands.read_text().splitlines()

    def test_complete_shell_suite_runs_without_filters_before_workspace_check(self):
        result, commands = self.execute(0)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(commands.count(SHELL), 1)
        self.assertLess(commands.index(SHELL), commands.index('check --locked --workspace --tests'))
        self.assertIn('FAST_PHASE_END=shell-binary-tests', result.stdout)

    def test_a_shell_failure_stops_the_gate_with_the_original_verdict(self):
        result, commands = self.execute(34)
        self.assertEqual(result.returncode, 34, result.stdout + result.stderr)
        self.assertEqual(commands[-1], SHELL)
        self.assertIn('status=34', result.stdout)


if __name__ == '__main__':
    unittest.main()
