import importlib.util
import io
import json
import os
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

VERIFY_PATH = Path(__file__).resolve().parent / 'verify.py'
spec = importlib.util.spec_from_file_location('okilum_verify', VERIFY_PATH)
verify_mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify_mod)


class VerifyOutputTests(unittest.TestCase):
    def test_pool_verify_redirects_to_log_and_caps_stdout_at_4kib(self):
        with tempfile.TemporaryDirectory() as tmp:
            out_dir = Path(tmp)
            (out_dir / 'result.json').write_text(json.dumps({
                'scenario': 'reader-basic',
                'host': 'okilum-dev',
                'passed': False,
                'verify_seconds': 12.3,
                'error': 'AssertionError: text not found',
            }))

            def fake_run(cmd, cwd=None, env=None, stdout=None, stderr=None):
                for i in range(1, 3000):
                    stdout.write(f'cargo build line {i} тест\n')
                stdout.flush()
                return type('Completed', (), {'returncode': 1})()

            buf = io.StringIO()
            with mock.patch.object(verify_mod, 'capture', return_value='deadbeef'), \
                 mock.patch.object(verify_mod.subprocess, 'run', side_effect=fake_run), \
                 mock.patch.dict(os.environ, {}, clear=False), \
                 mock.patch.object(sys, 'argv', ['verify.py', 'reader-basic', '--output', str(out_dir)]), \
                 redirect_stdout(buf):
                os.environ.pop('OKILUM_VERIFY_RAW_OUTPUT', None)
                # Ensure output check passes regardless of ~/work/qa-evidence
                with mock.patch.object(Path, 'is_relative_to', return_value=True):
                    rc = verify_mod.main()

            out = buf.getvalue()
            self.assertEqual(rc, 1)
            self.assertLessEqual(len(out.encode('utf-8')), 4096)
            self.assertIn('status=FAIL', out)
            self.assertIn('exit_code=1', out)
            self.assertIn('verify.log', out)
            self.assertIn('AssertionError: text not found', out)
            self.assertIn('cargo build line 2999', out)
            log_text = (out_dir / 'verify.log').read_text(encoding='utf-8')
            self.assertIn('cargo build line 1 ', log_text)
            self.assertIn('cargo build line 2999 ', log_text)

    def test_pass_summary_is_compact(self):
        with tempfile.TemporaryDirectory() as tmp:
            out_dir = Path(tmp)
            log_path = out_dir / 'verify.log'
            log_path.write_text('lots of build output\n' * 500, encoding='utf-8')
            (out_dir / 'result.json').write_text(json.dumps({
                'scenario': 'reader-basic',
                'host': 'okilum-dev',
                'passed': True,
                'verify_seconds': 7.1,
            }))
            summary = verify_mod.format_verify_summary('reader-basic', 0, 11.2, out_dir, log_path)
            self.assertLessEqual(len(summary.encode('utf-8')), 4096)
            self.assertIn('status=PASS', summary)
            self.assertIn('passed=True', summary)
            self.assertNotIn('lots of build output', summary)


if __name__ == '__main__':
    unittest.main()
