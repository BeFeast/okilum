"""Hosted regressions: fake cache/compiler only; never touch a host daemon."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('release-cache.sh').resolve()


class IsolatedCacheTests(unittest.TestCase):
    def test_parallel_jobs_and_fallback_preserve_compiler_status(self):
        for mode in ('healthy', 'start-failed', 'probe-failed', 'start-timeout'):
            for compiler_status in (0, 23):
                with self.subTest(mode=mode, compiler_status=compiler_status), tempfile.TemporaryDirectory() as tmp:
                    root = Path(tmp)
                    cache = root / 'cache/0.18.0-aarch64-apple-darwin/sccache'
                    cache.parent.mkdir(parents=True)
                    commands = {
                        root / 'uname': 'if [[ $1 == -s ]]; then echo Darwin; else echo arm64; fi',
                        root / 'curl': 'exit 0',
                        root / 'rustc': 'echo compiler >> "$CALLS"; exit "$COMPILER_STATUS"',
                        cache: r'''
                        echo "$1" >> "$CALLS"
                        case "$1" in
                          --stop-server) echo forbidden-shared-stop >> "$CALLS"; exit 99 ;;
                          --start-server)
                            echo "$SCCACHE_SERVER_UDS" > "$SOCKET_RECORD"
                            if [[ $MODE == start-failed ]]; then
                              echo 'sccache: error: Server startup failed: File exists (os error 17)' >&2
                              exit 2
                            fi
                            [[ $MODE != start-timeout ]] || sleep 5
                            exit 0 ;;
                        esac
                        if [[ $2 == -vV ]]; then
                          [[ $MODE != probe-failed ]]; exit $?
                        fi
                        exec "$@"
                        ''',
                    }
                    for path, body in commands.items():
                        path.write_text('#!/bin/bash\n' + body + '\n')
                        path.chmod(0o755)
                    processes = []
                    for job in range(2):
                        env = dict(os.environ, PATH=str(root) + ':' + os.environ['PATH'],
                                   OKILUM_SCCACHE_HOME=str(root / 'cache'),
                                   OKILUM_SCCACHE_ISOLATED='1', AWS_ACCESS_KEY_ID='test',
                                   SCCACHE_ENDPOINT='test', MODE=mode,
                                   CALLS=str(root / f'calls-{job}'),
                                   SOCKET_RECORD=str(root / f'socket-{job}'),
                                   COMPILER_STATUS=str(compiler_status),
                                   RUSTC_WRAPPER='/stale/wrapper',
                                   SCCACHE_SERVER_UDS='/shared/stale.sock',
                                   SCCACHE_SERVER_PORT='4226')
                        shell = r'''
                        ulimit() { if [[ $1 == -Sn ]]; then echo 8192; fi; return 0; }
                        source "$1"
                        printf 'wrapper=%s\n' "${RUSTC_WRAPPER:-}"
                        if [[ -n ${RUSTC_WRAPPER:-} ]]; then
                          "$RUSTC_WRAPPER" "$(command -v rustc)" compile
                        else
                          rustc compile
                        fi
                        '''
                        processes.append(subprocess.Popen(
                            ['bash', '-eu', '-c', shell, 'test', str(SCRIPT)],
                            env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True))
                    sockets = []
                    try:
                        for job, proc in enumerate(processes):
                            stdout, stderr = proc.communicate(timeout=10)
                            self.assertEqual(proc.returncode, compiler_status, stderr)
                            calls = (root / f'calls-{job}').read_text().splitlines()
                            self.assertEqual(calls[0], '--start-server')
                            self.assertNotIn('--stop-server', calls)
                            self.assertEqual(calls.count('compiler'), 1)
                            enabled = mode == 'healthy'
                            self.assertIn('wrapper=' + (str(cache) if enabled else '') + '\n', stdout)
                            self.assertEqual('::warning::' in stdout, not enabled)
                            sockets.append(Path((root / f'socket-{job}').read_text().strip()))
                        self.assertNotEqual(sockets[0], sockets[1])
                        for socket in sockets:
                            self.assertEqual(socket.parent.parent, Path('/tmp'))
                            self.assertLess(len(str(socket).encode()), 104)
                            self.assertTrue(socket.parent.is_dir())
                    finally:
                        for proc in processes:
                            if proc.poll() is None:
                                proc.kill()
                                proc.wait()
                        # The fake never binds a real socket or starts a daemon.
                        for job in range(2):
                            record = root / f'socket-{job}'
                            if record.exists():
                                Path(record.read_text().strip()).parent.rmdir()
