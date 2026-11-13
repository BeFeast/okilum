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
    def test_shared_runtime_probe_executes_full_entrypoint_and_keeps_its_verdict(self):
        for status in (0, 7):
            with self.subTest(status=status), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                scripts = root / 'scripts/ci'
                scripts.mkdir(parents=True)
                (scripts / 'check-shared.sh').write_text(
                    '[[ "$OKILUM_WORKSPACE_ORDINARY_SHARDS" == 4 ]] || exit 99\n'
                    f'echo COMPLETE_SHARED_ENTRYPOINT\nexit {status}\n')
                result = subprocess.run(['bash', str(HERE / 'runtime-fixture-diagnostic.sh'), 'shared'],
                    cwd=root, capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, status, result.stderr)
                self.assertIn('COMPLETE_SHARED_ENTRYPOINT', result.stdout)

    def test_runtime_probe_requires_real_target_receipts_and_retains_failures(self):
        for passed, cargo_status in [(1, 0), (0, 0), (1, 7)]:
            with self.subTest(passed=passed, status=cargo_status), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                scripts = root / 'scripts/ci'
                scripts.mkdir(parents=True)
                for name in ('test-group.sh', 'test-group.py'):
                    shutil.copy2(HERE / name, scripts / name)
                (scripts / 'workspace-rust-tests.py').write_text(
                    'import os\nassert os.environ["OKILUM_WORKSPACE_ORDINARY_SHARDS"] == "4"\n'
                    'print("FULL_WORKSPACE_FIRED")\n')
                binary = root / 'bin'
                binary.mkdir()
                git = binary / 'git'
                git.write_text('#!/bin/sh\necho ' + 'a' * 40 + '\n')
                git.chmod(0o755)
                cargo = binary / 'cargo'
                cargo.write_text('''#!/bin/sh
case " $* " in *" --bin okilum "*) target=okilum;; *) target=okilum_core;; esac
echo "Running unittests src/lib.rs (target/debug/deps/$target-abc)"
''' + f'echo "test result: ok. {passed} passed; 0 failed; 0 ignored;"\nexit {cargo_status}\n')
                cargo.chmod(0o755)
                result = subprocess.run(['bash', str(HERE / 'runtime-fixture-diagnostic.sh')],
                    cwd=root, capture_output=True, text=True, timeout=10,
                    env=dict(os.environ, PATH=str(binary) + os.pathsep + os.environ['PATH']))
                self.assertEqual(result.returncode, int(not passed or bool(cargo_status)), result.stderr)
                self.assertIn('FULL_WORKSPACE_FIRED', result.stdout)
                for name in ('priority-source-workers', 'cancellation-source-workers',
                             'reader-editor-live-preview'):
                    self.assertIn(f'TEST_GROUP_START group={name} limit=120s', result.stdout)
                self.assertIn('TEST_GROUP_START group=workspace-full limit=1320s', result.stdout)

    def test_large_process_sample_survives_job_log_line_limits(self):
        sample = {'processes': [{'command': ['rustc', 'x' * 90000]}]}
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            probe.emit('COLD_WRITER_SAMPLE', sample)
        lines = output.getvalue().splitlines()
        self.assertTrue(all(len(line.encode()) < 16000 for line in lines))
        chunks = [json.loads(line.split(' ', 1)[1]) for line in lines]
        self.assertTrue(all(line.startswith('COLD_WRITER_SAMPLE_CHUNK ') for line in lines))
        self.assertEqual([chunk['index'] for chunk in chunks], list(range(len(chunks))))
        self.assertTrue(all(chunk['count'] == len(chunks) for chunk in chunks))
        self.assertEqual(json.loads(''.join(chunk['data'] for chunk in chunks)), sample)

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
