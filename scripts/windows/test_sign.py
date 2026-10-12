"""sign.sh refusals, pinning and retries, with stub jsign and ssign binaries (#1104)."""
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


class SsignTests(unittest.TestCase):
    """certum-ssign: pinned binary, one login per run, retries only after a session exists."""

    def setUp(self):
        self.dir = Path(tempfile.mkdtemp())
        self.file = self.dir / 'a.exe'
        self.file.write_bytes(b'MZ')
        self.ssign = self.dir / 'ssign'
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(('OKILUM_', 'GITHUB_', 'CERTUM_'))}
        self.env.update(OKILUM_WINDOWS_SIGN_BACKEND='certum-ssign', OKILUM_SSIGN=str(self.ssign),
                        OKILUM_SSIGN_LOCK=str(self.dir / 'lock'), OKILUM_WINDOWS_SIGN_RETRY='0',
                        XDG_RUNTIME_DIR=str(self.dir / 'run'), CERTUM_EMAIL='qa@example.com',
                        CERTUM_TOKEN='123456')

    def stub(self, script):
        """An ssign that answers from a script, records its arguments and the code it saw."""
        self.ssign.write_text('#!/bin/bash\nn=$(($(cat "$0.count" 2>/dev/null || echo 0) + 1)); echo $n > "$0.count"\n'
                              'echo "$CERTUM_TOKEN $*" >> "$0.args"\n' + script)
        self.ssign.chmod(0o755)
        digest = subprocess.run(['sha256sum', str(self.ssign)], capture_output=True, text=True).stdout.split()[0]
        self.env['OKILUM_SSIGN_SHA256'] = digest

    def run_sign(self, *args, **env):
        return subprocess.run(['bash', str(SIGN), *args], env={**self.env, **env}, capture_output=True, text=True)

    def calls(self):
        path = Path(str(self.ssign) + '.count')
        return int(path.read_text()) if path.exists() else 0

    def test_signs_through_ssign_with_name_url_and_timestamp(self):
        self.stub('exit 0\n')
        result = self.run_sign(str(self.file))
        self.assertEqual(result.returncode, 0, result.stderr)
        args = Path(str(self.ssign) + '.args').read_text()
        self.assertIn('123456 --timestamp-url http://time.certum.pl --name Okilum --url https://okilum.app', args)
        self.assertIn(str(self.file), args)

    def test_a_binary_other_than_the_pinned_build_is_refused(self):
        self.stub('exit 0\n')
        result = self.run_sign(str(self.file), OKILUM_SSIGN_SHA256='0' * 64)
        self.assertIn('not the pinned ssign build', result.stderr)
        self.assertEqual(self.calls(), 0)

    def test_a_rejected_login_is_never_replayed(self):
        # No session was cached: the code was refused, and a retry would spend it again.
        self.stub('exit 1\n')
        self.assertNotEqual(self.run_sign(str(self.file)).returncode, 0)
        self.assertEqual(self.calls(), 1)

    def test_failures_after_a_login_get_three_tries(self):
        self.stub('mkdir -p "$XDG_RUNTIME_DIR/ssign"; touch "$XDG_RUNTIME_DIR/ssign/session.json"\n'
                  '[ $n -ge 3 ]\n')
        self.assertEqual(self.run_sign(str(self.file)).returncode, 0)
        self.assertEqual(self.calls(), 3)

    def test_probe_needs_a_session_or_a_code_and_never_logs_in(self):
        self.stub('exit 0\n')
        pin = 'ab' * 32
        self.assertIn('no Certum session', self.run_sign('--probe', CERTUM_TOKEN='', OKILUM_WINDOWS_SIGN_CERT_SHA256=pin).stderr)
        (self.dir / 'run' / 'ssign').mkdir(parents=True)
        (self.dir / 'run' / 'ssign' / 'session.json').write_text('{}')
        self.assertEqual(self.run_sign('--probe', CERTUM_TOKEN='', OKILUM_WINDOWS_SIGN_CERT_SHA256=pin).returncode, 0)
        result = self.run_sign('--probe', OKILUM_WINDOWS_SIGN_CERT_SHA256='ab' * 32)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('pins the certificate', result.stdout)
        self.assertEqual(self.calls(), 0)

if __name__ == '__main__':
    unittest.main()
