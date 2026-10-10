"""sign.sh refusals, pinning and jsign retries, with a stub jsign (#1104)."""
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SIGN = Path(__file__).with_name('sign.sh')
MAKE = Path(__file__).with_name('make-test-signer.sh')


class SignTests(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp())
        self.file = self.dir / 'a.exe'
        self.file.write_bytes(b'MZ')
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(('OKILUM_', 'GITHUB_'))}
        self.env.update(OKILUM_WINDOWS_SIGN_BACKEND='pkcs12-test', OKILUM_WINDOWS_SIGN_TEST_P12='x.p12',
                        OKILUM_WINDOWS_SIGN_TEST_PASSWORD_FILE='pw', OKILUM_WINDOWS_SIGN_ALIAS='a',
                        OKILUM_WINDOWS_SIGN_WAIT='0', OKILUM_WINDOWS_SIGN_RETRY='0')

    def run_sign(self, *args, **env):
        return subprocess.run(['bash', str(SIGN), *args], env={**self.env, **env},
                              capture_output=True, text=True)

    def stub(self, script):
        """A jsign that answers from a list of outcomes, one per call, and counts calls."""
        jsign = self.dir / 'jsign'
        jsign.write_text('#!/bin/bash\nn=$(($(cat "$0.count" 2>/dev/null || echo 0) + 1)); echo $n > "$0.count"\n'
                         + script)
        jsign.chmod(0o755)
        return jsign

    def calls(self, jsign):
        return int(Path(str(jsign) + '.count').read_text())

    def test_pull_request_is_refused_before_any_key_is_touched(self):
        jsign = self.stub('exit 0\n')
        result = self.run_sign(str(self.file), GITHUB_EVENT_NAME='pull_request', OKILUM_JSIGN=str(jsign))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('pull_request', result.stderr)
        self.assertFalse(Path(str(jsign) + '.count').exists())

    def test_unknown_or_missing_backend_is_refused(self):
        self.assertIn('unknown backend', self.run_sign(str(self.file), OKILUM_WINDOWS_SIGN_BACKEND='usb').stderr)
        self.assertIn('not set', self.run_sign(str(self.file), OKILUM_WINDOWS_SIGN_BACKEND='').stderr)

    def test_late_certificate_is_waited_for(self):
        # SimplySign populates the certificate a little after login.
        jsign = self.stub('[ $n -lt 3 ] && { echo "No certificate found in the keystore"; exit 1; }; exit 0\n')
        result = self.run_sign(str(self.file), OKILUM_JSIGN=str(jsign))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(jsign), 3)

    def test_certificate_that_never_arrives_fails_after_the_wait(self):
        jsign = self.stub('echo "No certificate found in the keystore"; exit 1\n')
        result = self.run_sign(str(self.file), OKILUM_JSIGN=str(jsign))
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.calls(jsign), 16)

    def test_other_failures_get_three_tries(self):
        jsign = self.stub('echo "timestamp server unavailable"; exit 1\n')
        result = self.run_sign(str(self.file), OKILUM_JSIGN=str(jsign))
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.calls(jsign), 3)
        self.assertIn('timestamp server unavailable', result.stderr)

    def test_probe_accepts_only_the_pinned_certificate(self):
        exports = subprocess.run(['bash', str(MAKE), str(self.dir / 'id')], capture_output=True, text=True,
                                 check=True).stdout
        env = dict(line.removeprefix('export ').split('=', 1) for line in exports.splitlines())
        env = {k: v.strip("'") for k, v in env.items()}
        self.assertEqual(self.run_sign('--probe', **env).returncode, 0)
        wrong = self.run_sign('--probe', **{**env, 'OKILUM_WINDOWS_SIGN_CERT_SHA256': '00' * 32})
        self.assertNotEqual(wrong.returncode, 0)
        self.assertIn('expected', wrong.stderr)


if __name__ == '__main__':
    unittest.main()
