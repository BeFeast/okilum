"""Optional cache installation must not turn network failures into build failures."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('release-cache.sh').resolve()


class CacheFallbackTests(unittest.TestCase):
    def test_macos_limit_restart_and_fallback(self):
        scratch = SCRIPT.parents[2] / 'target/cache-limit-tests'
        scratch.mkdir(parents=True, exist_ok=True)
        for mode in ['raised', 'hard-fallback', 'denied', 'low', 'start-failed']:
            with self.subTest(mode=mode), tempfile.TemporaryDirectory(dir=scratch) as tmp:
                root = Path(tmp)
                cache = root / 'cache/0.18.0-aarch64-apple-darwin/sccache'
                cache.parent.mkdir(parents=True)
                cache.write_text('#!/bin/bash\necho "$1" >> "$CALLS"\n'
                                 'if [[ $1 == --start-server && $MODE == start-failed ]]; then exit 2; fi\n')
                cache.chmod(0o755)
                for name, body in [('uname', 'if [[ $1 == -s ]]; then echo Darwin; else echo arm64; fi'),
                                   ('curl', 'exit 0')]:
                    path = root / name
                    path.write_text('#!/bin/bash\n' + body + '\n')
                    path.chmod(0o755)
                env = dict(os.environ, PATH=str(root) + ':' + os.environ['PATH'],
                           TESSERA_SCCACHE_HOME=str(root / 'cache'), AWS_ACCESS_KEY_ID='test',
                           SCCACHE_ENDPOINT='test', MODE=mode, CALLS=str(root / 'calls'))
                shell = r"""
                ulimit() {
                  case "$1" in
                    -Hn) echo 16384 ;;
                    -Sn) if [[ $MODE == low ]]; then echo 4096; else echo 8192; fi ;;
                    -n) [[ $MODE != denied ]] && { [[ $MODE != hard-fallback ]] || [[ $2 == 16384 ]]; } ;;
                  esac
                }
                source "$1"
                echo "wrapper=${RUSTC_WRAPPER:-}"
                """
                result = subprocess.run(['bash', '-eu', '-c', shell, 'test', str(SCRIPT)],
                                        env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                enabled = mode in ['raised', 'hard-fallback']
                self.assertEqual('wrapper=' + str(cache) in result.stdout, enabled)
                calls = (root / 'calls').read_text().splitlines() if (root / 'calls').exists() else []
                if mode in ['denied', 'low']:
                    self.assertEqual(calls, [])
                else:
                    self.assertEqual(calls[:2], ['--stop-server', '--start-server'])
                    self.assertEqual(len(calls), 3 if enabled else 2)
                if not enabled:
                    self.assertIn('::warning::', result.stdout)

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
