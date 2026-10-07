"""Optional cache installation must not turn network failures into build failures."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('release-cache.sh').resolve()


class CacheFallbackTests(unittest.TestCase):
    def test_download_and_corrupt_archive_fall_back_under_errexit(self):
        for behavior in ['exit 22', 'while [ "$1" != -o ]; do shift; done; shift; printf bad > "$1"']:
            with self.subTest(behavior=behavior), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                curl = root / 'curl'
                curl.write_text('#!/bin/bash\nprintf called > "$CACHE_TEST_CALLED"\n'
                                'if [[ " $* " == *" --output "* ]]; then exit 0; fi\n'
                                + behavior + '\n')
                curl.chmod(0o755)
                env = {**os.environ, 'PATH': str(root) + ':' + os.environ['PATH'],
                       'AWS_ACCESS_KEY_ID': 'test', 'SCCACHE_ENDPOINT': 'test',
                       'TESSERA_SCCACHE_HOME': str(root / 'cache'),
                       'CACHE_TEST_CALLED': str(root / 'called')}
                env.pop('RUSTC_WRAPPER', None)
                result = subprocess.run(['bash', '-eu', '-c',
                    'source "$1"; test -z "${RUSTC_WRAPPER:-}"; echo compiler-continues',
                    'test', str(SCRIPT)], env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn('compiler-continues', result.stdout)
                self.assertEqual((root / 'called').read_text(), 'called')
                self.assertFalse(list((root / 'cache').rglob('sccache')))

    def test_endpoint_and_compiler_probe_control_wrapper(self):
        # Positive control: a healthy cache really is selected. Failure cases
        # must still reach the ordinary compiler, even with a stale wrapper.
        cases = [('offline', 'exit 7', 'exit 0', False),
                 ('healthy', 'exit 0', 'exit 0', True),
                 ('storage-failure', 'exit 0', 'exit 2', False),
                 ('startup-timeout', 'exit 0', 'sleep 5', False)]
        for name, curl_body, cache_body, enabled in cases:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                cache = root / 'cache/0.18.0-x86_64-unknown-linux-musl/sccache'
                cache.parent.mkdir(parents=True)
                cache.write_text('#!/bin/bash\n' + cache_body + '\n')
                cache.chmod(0o755)
                for command, body in {
                    'curl': 'printf "%s\\n" "$*" > "$PROBE_ARGS"\n' + curl_body,
                    'uname': 'case "$1" in -s) echo Linux;; -m) echo x86_64;; esac',
                }.items():
                    p = root / command
                    p.write_text('#!/bin/bash\n' + body + '\n')
                    p.chmod(0o755)
                env = {**os.environ, 'PATH': str(root) + ':' + os.environ['PATH'],
                       'AWS_ACCESS_KEY_ID': 'test', 'SCCACHE_ENDPOINT': 'http://cache.invalid',
                       'TESSERA_SCCACHE_HOME': str(root / 'cache'),
                       'RUSTC_WRAPPER': '/stale/wrapper', 'PROBE_ARGS': str(root / 'args')}
                result = subprocess.run(['bash', '-eu', '-c',
                    'source "$1"; printf "wrapper=%s\\n" "${RUSTC_WRAPPER:-}"; printf "socket=%s\\n" "${SCCACHE_SERVER_UDS:-}"; echo compiler-continues',
                    'test', str(SCRIPT)], env=env, capture_output=True, text=True, timeout=8)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn('compiler-continues', result.stdout)
                self.assertIn('wrapper=' + (str(cache) if enabled else '') + '\n', result.stdout)
                self.assertEqual('::warning::' in result.stdout, not enabled)
                if not enabled:
                    self.assertIn('socket=\n', result.stdout)
                self.assertIn('--connect-timeout 3 --max-time 3', (root / 'args').read_text())
