"""Exercise fast orchestration without compiling or altering any host limit."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class FastScript(unittest.TestCase):
    def test_platform_arguments_are_nonempty_without_bash32_optional_arrays(self):
        for platform in ['Linux', 'macOS', 'Windows']:
            with self.subTest(platform=platform), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                for sub in ['bin', 'scripts/ci', 'scripts/updater']:
                    (root / sub).mkdir(parents=True)
                (root / 'scripts/ci/fast.sh').write_text((ROOT / 'scripts/ci/fast.sh').read_text())
                (root / 'scripts/ci/test-nofile.sh').write_text('# no host-limit mutation in this orchestration fixture\n')
                for name, script in {
                    'scripts/vendor-setup.sh': '#!/bin/sh\nexit 0\n',
                    'bin/cargo': '#!/bin/sh\nprintf "%s\\n" "$*" >> "$COMMANDS"\ncase "$*" in *--list*) echo "parsing_positive: test";; esac\n',
                    'bin/xcrun': '#!/bin/sh\necho /SDK\n',
                    'bin/python': '#!/bin/sh\nexec python3 "$@"\n',
                    'bin/cygpath': '#!/bin/sh\necho "$2"\n',
                }.items():
                    path = root / name; path.write_text(script); path.chmod(0o755)
                for name in ['scripts/ci/test-icon.py', 'scripts/updater/sparkle.py']:
                    (root / name).write_text('# fixture command completed\n')
                commands = root / 'commands'
                env = dict(os.environ, PATH=str(root / 'bin') + os.pathsep + os.environ['PATH'],
                           GITHUB_SHA='a' * 40, RUNNER_OS=platform, RUNNER_TEMP=str(root), COMMANDS=str(commands))
                subprocess.run(['bash', 'scripts/ci/fast.sh'], cwd=root, env=env, check=True, capture_output=True)
                calls = commands.read_text().splitlines()
                self.assertEqual(len(calls), 3)
                self.assertEqual('--no-default-features' in calls[0], platform == 'Windows')
                self.assertTrue(calls[0].startswith('build --locked -p okilum-shell -p okilum-sync-supervisor'))
                self.assertIn('--list', calls[1])
                self.assertIn('test --locked -p okilum-paste --lib', calls[2])
        self.assertNotIn('build_args[@]', (ROOT / 'scripts/ci/fast.sh').read_text())


if __name__ == '__main__':
    unittest.main()
