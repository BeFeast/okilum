"""Offline local candidate harness regressions; no server/provider is launched."""
import argparse
import contextlib
import io
import json
import os
from pathlib import Path
import signal
import stat
import sys
import tempfile
import time
import unittest
from unittest import mock

import probe


def running(pid):
    """A killed orphan can briefly remain as a zombie until init reaps it."""
    try:
        return Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[0] != 'Z'
    except (FileNotFoundError, ProcessLookupError):
        return False


class RunningTests(unittest.TestCase):
    def test_live_and_zombie_states(self):
        for state in ('R', 'S', 'D', 'T', 'Z'):
            with self.subTest(state=state), mock.patch.object(
                    Path, 'read_text', autospec=True,
                    return_value=f'12345 (fake probe (child)) {state} 1 2 3') as read:
                self.assertEqual(running(12345), state != 'Z')
                read.assert_called_once_with(Path('/proc/12345/stat'))

    def test_process_vanishes_during_stat_read(self):
        # ENOENT can happen at open; ESRCH can happen after open, during read.
        for error in (FileNotFoundError, ProcessLookupError):
            with self.subTest(error=error), mock.patch.object(
                    Path, 'read_text', autospec=True, side_effect=error) as read:
                self.assertFalse(running(12345))
                read.assert_called_once_with(Path('/proc/12345/stat'))

    def test_other_read_errors_propagate(self):
        with mock.patch.object(Path, 'read_text', side_effect=PermissionError):
            with self.assertRaises(PermissionError):
                running(12345)


class ProbeTests(unittest.TestCase):
    def test_origin_boundary(self):
        for value in ('https://example.com', 'http://localhost:444', 'http://127.0.0.1:444',
                      'http://10.1.2.3', 'http://100.64.1.2', 'http://[fd00::1]',
                      'http://[::ffff:192.168.1.2]'):
            self.assertEqual(probe.origin(value), value)
        for value in ('http://example.com', 'http://8.8.8.8', 'http://0.0.0.0',
                      'http://192.0.0.1', 'https://u:p@example.com', 'https://example.com/a',
                      'https://example.com?token=secret', 'https://example.com/#fragment',
                      'https://example.com:99999', 'https://example.com\\@localhost',
                      ' https://example.com', 'http://[::]'):
            with self.subTest(value=value), self.assertRaises(ValueError):
                probe.origin(value)

    def test_receipt_atomically_replaces_with_private_permissions(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'result.json'
            path.write_text('old incomplete output')
            path.chmod(0o644)
            probe.atomic_receipt(path, {'status': 'PASS'})
            self.assertEqual(json.loads(path.read_text()), {'status': 'PASS'})
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            self.assertEqual(sorted(p.name for p in path.parent.iterdir()), ['result.json'])

    def test_child_output_is_private_and_bounded(self):
        env = {'PATH': os.defpath}
        with self.assertRaises(probe.ProbeFailure) as caught:
            probe.run_private([sys.executable, '-c',
                               "import sys;print('SECRET-TOKEN');print('SECRET-LOG',file=sys.stderr);sys.exit(2)"],
                              env=env, timeout=2)
        self.assertEqual(str(caught.exception), 'nonzero_exit')
        with self.assertRaises(probe.ProbeFailure) as caught:
            probe.run_private([sys.executable, '-c', "print('x' * 1100000)"], env=env, timeout=2)
        self.assertEqual(str(caught.exception), 'oversized_output')
        self.assertEqual(probe.run_private([sys.executable, '-c', "print('positive control')"],
                                          env=env, timeout=2), b'positive control\n')

    @unittest.skipUnless(hasattr(os, 'fork'), 'POSIX process groups')
    def test_timeout_kills_ignoring_leader_and_descendant_and_reaps_leader(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / 'pids'
            program = ("import os,signal,time\n"
                       "signal.signal(signal.SIGTERM,signal.SIG_IGN)\n"
                       "child=os.fork()\n"
                       "if child:\n"
                       f" open({str(marker)!r},'w').write(str(os.getpid())+' '+str(child))\n"
                       "while True: time.sleep(.02)\n")
            started = time.monotonic()
            with self.assertRaises(probe.ProbeFailure) as caught:
                probe.run_private([sys.executable, '-c', program], env={'PATH': os.defpath}, timeout=.4)
            self.assertEqual(caught.exception.code, 'timeout')
            self.assertLess(time.monotonic() - started, 3)
            leader, child = map(int, marker.read_text().split())
            with self.assertRaises(ChildProcessError):
                os.waitpid(leader, os.WNOHANG)  # Already reaped by run_private.
            # Orphaned grandchildren can briefly be zombies until init reaps them;
            # either absence or a zombie is proof that no work survived the group kill.
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                if not running(child):
                    break
                time.sleep(.01)
            else:
                self.fail('descendant survived hard deadline')

    def test_uninterruptible_leader_preserves_primary_failure_and_closes_stdin(self):
        # No real D-state process is needed: both bounded reap attempts expire.
        # The original diagnostic must survive, and credentials must lose their pipe.
        for code in ('timeout', 'nonzero_exit', 'cleanup_failed'):
            with self.subTest(code=code):
                process = mock.Mock(pid=12345, returncode=7 if code == 'nonzero_exit' else 0)
                if code == 'timeout':
                    process.communicate.side_effect = probe.subprocess.TimeoutExpired('SECRET-ARGV', 1)
                process.wait.side_effect = probe.subprocess.TimeoutExpired('SECRET-REAP-LOG', 5)
                with mock.patch.object(probe.subprocess, 'Popen', return_value=process), \
                     mock.patch.object(probe.os, 'killpg') as kill:
                    with self.assertRaises(probe.ProbeFailure) as caught:
                        probe.run_private(['/not-executed'], env={}, timeout=1, input_data=b'SECRET')
                self.assertEqual(caught.exception.stage, 'subprocess')
                self.assertEqual(caught.exception.code, code)
                self.assertNotIn('SECRET', str(caught.exception))
                self.assertEqual(process.wait.call_args_list,
                                 [mock.call(timeout=0.5), mock.call(timeout=5)])
                self.assertEqual(kill.call_args_list,
                                 [mock.call(12345, signal.SIGTERM), mock.call(12345, signal.SIGKILL)])
                process.stdin.close.assert_called_once_with()

    def test_private_environment_has_no_inherited_credentials(self):
        with mock.patch.dict(os.environ, {'OPENAI_API_KEY': 'SECRET', 'GITHUB_TOKEN': 'SECRET',
                                          'T3_TOKEN': 'SECRET', 'HTTP_PROXY': 'http://secret'}):
            env = probe.private_env(Path('/isolated/home'), Path('/candidate'))
        self.assertEqual(set(env), {'PATH', 'LANG', 'TZ', 'HOME', 'T3CODE_HOME',
                                    'XDG_CONFIG_HOME', 'XDG_CACHE_HOME'})
        self.assertNotIn('SECRET', json.dumps(env))

    def run_fixture(self, *, native=None, incompatible=False, fail_stage=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary, client, schema, base = [root / name for name in ('server', 'client', 'schema', 'state')]
            binary.write_text('official binary bytes')
            client.write_text('compiled client bytes')
            schema.write_text('{}')
            base.mkdir()
            receipt = root / 'receipt.json'
            args = argparse.Namespace(url='https://candidate.example', server_binary=str(binary),
                                      server_base_dir=str(base), candidate_schema=str(schema),
                                      client_probe=str(client), receipt=str(receipt))
            calls = []
            def command(argv, *, env, timeout, input_data=None):
                calls.append((argv, env, timeout))
                self.assertNotIn('serve', argv)
                self.assertNotIn('update', argv)
                if '--internal-worker' in argv:
                    config = json.loads(input_data)
                    if argv[-1] == 'descriptor':
                        return json.dumps({'protocol': 2, 'version': '0.1.7-nightly.20261031.3991'}).encode()
                    self.assertRegex(config['title'], r'^Okilum contract scratch [a-f0-9-]{36}$')
                    if fail_stage == 'fixture':
                        raise RuntimeError('SECRET-TOKEN server log')
                    return b'{"thread_id":"scratch-fixture"}'
                if 'pairing' in argv:
                    return b'{"credential":"SECRET-TOKEN"}'
                data = json.loads(input_data)
                self.assertEqual(data['credential'], 'SECRET-TOKEN')
                self.assertRegex(data['expected_title'], r'^Okilum contract scratch ')
                return json.dumps(native if native is not None else dict.fromkeys(probe.FLAGS, True)).encode()
            with mock.patch('compatibility.load_baseline', return_value={'baseline': 'verified'}), \
                 mock.patch('compatibility.compare', return_value={'status': 'FAIL' if incompatible else 'PASS',
                            'informational': [{'message': 'addition'}], 'breaking': [], 'errors': []}) as compare, \
                 mock.patch.object(probe, 'run_private', side_effect=command):
                result = probe.execute(args)
                # Default consumer expectations must never be disabled by a caller.
                self.assertEqual(compare.call_args.kwargs, {})
            serialized = receipt.read_text()
            self.assertNotIn('SECRET', serialized)
            self.assertNotIn('candidate.example', serialized)
            self.assertEqual(stat.S_IMODE(receipt.stat().st_mode), 0o600)
            return result, calls

    def test_full_five_proofs_and_exact_inputs_required_for_pass(self):
        result, calls = self.run_fixture()
        self.assertEqual(result['status'], 'PASS')
        self.assertTrue(all(result[flag] for flag in probe.FLAGS))
        for key in ('client_binary_sha256', 'server_binary_sha256',
                    'candidate_schema_sha256', 'baseline_sha256'):
            self.assertRegex(result[key], '^[a-f0-9]{64}$')
        self.assertEqual(result['schema_informational_count'], 1)
        self.assertEqual(len(calls), 4)

    def test_no_full_pass_for_legacy_or_partial_proof(self):
        for flags in ({'paired': True, 'snapshot': True, 'resume': True},
                      {**dict.fromkeys(probe.FLAGS, True), 'dispatch_command': False},
                      {**dict.fromkeys(probe.FLAGS, True), 'subscribe_shell': 1}):
            result, _ = self.run_fixture(native=flags)
            self.assertEqual(result['status'], 'FAIL')
            self.assertEqual(result['code'], 'incomplete_native_proof')

    def test_incompatible_schema_stops_before_any_setup_or_pairing(self):
        result, calls = self.run_fixture(incompatible=True)
        self.assertEqual(result['status'], 'FAIL')
        self.assertEqual(result['stage'], 'schema')
        self.assertEqual(calls, [])

    def test_arbitrary_peer_exception_is_not_serialized(self):
        result, calls = self.run_fixture(fail_stage='fixture')
        self.assertEqual(result['stage'], 'scratch_fixture')
        self.assertEqual(result['code'], 'failed')
        self.assertEqual(len(calls), 2)


if __name__ == '__main__':
    unittest.main()
