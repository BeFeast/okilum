"""Cold build probe controls: real small children, fake Cargo, no compiler work."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('cold_writer', HERE / 'cold-writer-diagnostic.py')
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


@unittest.skipUnless(sys.platform.startswith('linux'), 'Linux proc/cgroup probe')
class ColdWriterControls(unittest.TestCase):
    def test_samples_actual_owned_child_and_preserves_failure(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            status = probe.observe([sys.executable, '-c',
                'import time,sys; buf=bytearray(1024*1024); time.sleep(.15); sys.exit(7)'],
                dict(os.environ), interval=.02)
        self.assertEqual(status, 7)
        values = [json.loads(line.split(' ', 1)[1]) for line in output.getvalue().splitlines()]
        self.assertTrue(any(value['processes'] for value in values))
        self.assertTrue(any(process['command'][:2] == [sys.executable, '-c']
                            for value in values for process in value['processes']))
        self.assertEqual(values[-1]['status'], 7)
        self.assertGreater(len(values), 2)

    def test_original_prelude_boundary_fires_and_never_restarts_later_filters(self):
        for group_status in (0, 7):
            with self.subTest(status=group_status), tempfile.TemporaryDirectory() as directory:
                root = Path(directory) / 'source with spaces'
                scripts = root / 'scripts/ci'
                scripts.mkdir(parents=True)
                shutil.copy2(HERE / 'test-group.sh', scripts)
                shutil.copy2(HERE / 'test-group.py', scripts)
                shutil.copy2(HERE / 'require-target-tests.py', scripts)
                (scripts / 'check-linux.sh').write_text(
                    '#!/bin/bash\nset -e\necho ORIGINAL_PRELUDE\n'
                    # The unchanged prelude copies which('bash') as its fake
                    # window executable; a PATH wrapper breaks /proc identity.
                    + shlex.join([sys.executable, '-c',
                        "import pathlib,shutil; assert pathlib.Path(shutil.which('bash')).read_bytes().startswith(b'\\x7fELF')"])
                    + '\n' + shlex.join([sys.executable, '-m', 'unittest',
                        'test_dev_builds.LinuxRestart'])
                    + '\n'
                    # A child shell must retain ordinary bash dispatch too.
                    'bash -c \'if declare -F bash; then exit 99; fi; echo CHILD_BASH_UNCHANGED\'\n'
                    'bash scripts/ci/linux-rust-tests.sh\n')
                (scripts / 'linux-rust-tests.sh').write_text('echo WRONG_LATER_FILTERS\n')
                (scripts / 'local-writer-admission.sh').write_text(
                    'set -euo pipefail\necho ORIGINAL_ADMISSION\n'
                    'cargo test sentinel -- --nocapture 2>&1 | tee receipt\n'
                    'python3 scripts/ci/require-target-tests.py sentinel receipt\n'
                    f'exit {group_status}\n')
                binary = root / 'bin'
                binary.mkdir()
                for name, value in [('git', 'a' * 40), ('cargo', 'fake Cargo'),
                                    ('rustc', 'fake rustc'), ('mold', 'fake mold')]:
                    path = binary / name
                    path.write_text('#!/bin/sh\nprintf "%s\\n" ' + repr(value) + '\n'
                                    + ('''printf "CARGO_ARGV:%s\\n" "$*"
if [ "$1" = -vv ]; then
    echo 'Running `CARGO=value /target/debug/deps/sentinel-abc --nocapture`'
else
    echo 'Running unittests src/lib.rs (/target/debug/deps/sentinel-abc)'
fi
echo 'test result: ok. 1 passed; 0 failed; 0 ignored;'
''' if name == 'cargo' else ''))
                    path.chmod(0o755)
                result = subprocess.run([sys.executable, str(HERE / 'cold-writer-diagnostic.py'),
                    '--source', 'a' * 40], cwd=root, capture_output=True, text=True, timeout=10,
                    env=dict(os.environ, PATH=str(binary) + os.pathsep + os.environ['PATH'],
                             PYTHONPATH=str(HERE),
                             RUNNER_OS='Linux'))
                self.assertEqual(result.returncode, group_status, result.stderr)
                self.assertIn('ORIGINAL_PRELUDE', result.stdout)
                self.assertIn('CHILD_BASH_UNCHANGED', result.stdout)
                self.assertIn('Ran 3 tests', result.stderr)
                self.assertIn('ORIGINAL_ADMISSION', result.stdout)
                self.assertIn('TEST_GROUP_START group=cold-writer limit=900s', result.stdout)
                self.assertIn('COLD_WRITER_FINAL', result.stdout)
                self.assertIn('CARGO_ARGV:test sentinel -- --nocapture', result.stdout)
                self.assertIn('"invoked": true', result.stdout)
                self.assertNotIn('WRONG_LATER_FILTERS', result.stdout)

    def test_successful_prelude_that_never_fires_group_is_not_success(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'scripts/ci').mkdir(parents=True)
            (root / 'scripts/ci/check-linux.sh').write_text('echo UNFIRED_PRELUDE\n')
            output = io.StringIO()
            old = Path.cwd()
            try:
                os.chdir(root)
                with contextlib.redirect_stdout(output):
                    status = probe.prelude('a' * 40)
            finally:
                os.chdir(old)
            self.assertEqual(status, 125)
            self.assertIn('"invoked": false', output.getvalue())


if __name__ == '__main__':
    unittest.main()
