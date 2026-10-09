"""Cancellation ownership and real process-signal regression controls."""
import copy
import importlib.util
import os
import json
import tempfile
from pathlib import Path
import signal
import subprocess
import sys
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('bridge', Path(__file__).with_name('github-macos.py'))
bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)
SHA = 'a' * 40
BRANCH = bridge.ref_name('844', SHA, '123', '1', 'b' * 32)
RUN = dict(id=42, head_sha=SHA, head_branch=BRANCH, event='push', path=bridge.WORKFLOW,
           status='in_progress', conclusion=None)


class API:
    timeout = 30

    def __init__(self, run=None, discoveries=None, refuse=False):
        self.run = copy.deepcopy(RUN if run is None else run)
        self.discoveries = discoveries
        self.refuse = refuse
        self.calls = []

    def request(self, path, method='GET'):
        self.calls.append((path, method))
        if method == 'POST':
            if self.refuse:
                raise bridge.Unavailable('cancel refused')
            return None
        if '?' in path:
            return {'workflow_runs': self.discoveries.pop(0) if self.discoveries else [self.run]}
        return self.run


class CancellationTests(unittest.TestCase):
    def cancel(self, api, known=True):
        owner = bridge.Cancellation(api, BRANCH, SHA, bridge.WORKFLOW)
        if known:
            owner.observed(RUN)
        owner.cancel(sleep=lambda _: None)
        return [path for path, method in api.calls if method == 'POST']

    def test_known_owned_run_is_cancelled_once(self):
        api = API()
        self.assertEqual(self.cancel(api), ['actions/runs/42/cancel'])
        self.assertEqual(api.timeout, 1)

    def test_wrong_identity_including_same_sha_replacement_is_never_cancelled(self):
        for field, value in [('id', 43), ('head_sha', 'c' * 40),
                             ('head_branch', BRANCH + '-replacement'),
                             ('event', 'workflow_dispatch'), ('path', 'release.yml')]:
            with self.subTest(field=field):
                self.assertEqual(self.cancel(API(dict(RUN, **{field: value}))), [])

    def test_completed_run_needs_no_cancel(self):
        self.assertEqual(self.cancel(API(dict(RUN, status='completed'))), [])

    def test_dispatch_discovery_race_finds_only_exact_invocation(self):
        other = dict(RUN, id=43, head_branch=BRANCH + '-replacement')
        api = API(discoveries=[[], [other], [other, RUN]])
        self.assertEqual(self.cancel(api, known=False), ['actions/runs/42/cancel'])

    def test_missing_or_ambiguous_discovery_does_not_guess(self):
        for discoveries in [[[], [], []], [[RUN, dict(RUN, id=43)]]]:
            api = API(discoveries=discoveries)
            self.assertEqual(self.cancel(api, known=False), [])
            self.assertLessEqual(len(api.calls), 3)

    def test_refusal_and_api_outage_are_bounded_without_retry(self):
        api = API(refuse=True)
        self.assertEqual(self.cancel(api), ['actions/runs/42/cancel'])
        with patch.object(api, 'request', side_effect=bridge.Unavailable('offline')) as request:
            self.cancel(api)
        self.assertEqual(request.call_count, 1)

    def test_success_and_source_failure_do_not_trigger_cancellation(self):
        api = API()
        for result in ['success', 'failure']:
            with bridge.cancellation_scope(api, BRANCH, SHA, bridge.WORKFLOW):
                self.assertIn(result, ['success', 'failure'])
        self.assertEqual(api.calls, [])

    def test_missing_status_is_not_a_cancel_target(self):
        run = dict(RUN)
        del run['status']
        self.assertEqual(self.cancel(API(run)), [])

    def test_receipt_survives_before_discovery_and_records_owned_id(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'receipt.json'
            with patch.dict(os.environ, TESSERA_CANCELLATION_RECEIPT=str(path)):
                owner = bridge.Cancellation(API(), BRANCH, SHA, bridge.WORKFLOW)
                self.assertIsNone(json.loads(path.read_text())['run_id'])
                owner.observed(RUN)
                data = json.loads(path.read_text())
                self.assertEqual(data, dict(run_id=42, branch=BRANCH, sha=SHA, workflow=bridge.WORKFLOW, finished=False))
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                self.assertFalse(path.with_suffix('.tmp').exists())

    def test_post_action_registers_unique_receipt_and_invokes_cleanup_after_step_death(self):
        action = Path(__file__).resolve().parents[2] / '.forgejo/actions/hosted-cleanup/index.js'
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state, envfile, evidence = [root / name for name in ['state', 'env', 'evidence']]
            env = dict(os.environ, RUNNER_TEMP=directory, GITHUB_STATE=str(state),
                       GITHUB_ENV=str(envfile), GITHUB_WORKSPACE=str(root), INPUT_TOKEN='fixture')
            env.pop('STATE_receipt', None)
            subprocess.run(['node', str(action)], env=env, check=True, timeout=5)
            receipt = state.read_text().strip().split('=', 1)[1]
            self.assertEqual(envfile.read_text(), f'TESSERA_CANCELLATION_RECEIPT={receipt}\n')
            Path(receipt).write_text('{}')
            python = root / 'python3'
            python.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$EVIDENCE"\n')
            python.chmod(0o755)
            env.update(STATE_receipt=receipt, EVIDENCE=str(evidence), PATH=directory+':'+env['PATH'])
            Path(receipt).write_text('{"finished": true}')
            subprocess.run(['node', str(action)], env=env, check=True, timeout=5)
            self.assertFalse(evidence.exists(), 'Normal completion must never invoke cancellation')
            self.assertFalse(Path(receipt).exists())
            Path(receipt).write_text('{"finished": false}')
            subprocess.run(['node', str(action)], env=env, check=True, timeout=5)
            self.assertEqual(evidence.read_text().splitlines(), [str(root / 'scripts/ci/github-cancel.py'), receipt])
            self.assertFalse(Path(receipt).exists())
            Path(receipt).write_text('malformed')
            result = subprocess.run(['node', str(action)], env=env, check=True, timeout=5,
                                    capture_output=True, text=True)
            self.assertIn('receipt could not be processed', result.stdout)
            self.assertFalse(Path(receipt).exists())

    @unittest.skipUnless(os.name == 'posix', 'Forgejo bridge runs on Linux')
    def test_real_sigint_and_sigterm_cancel_only_owned_run_and_exit_nonzero(self):
        # Separate processes exercise installed signal handlers, not a mocked
        # exception. A control process demonstrates no cancel on normal exit.
        script = '''
import importlib.util, os, signal, sys
spec = importlib.util.spec_from_file_location('tests', sys.argv[1])
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
a = m.API()
try:
    with m.bridge.cancellation_scope(a, m.BRANCH, m.SHA, m.bridge.WORKFLOW) as owner:
        owner.observed(m.RUN)
        if int(sys.argv[2]): os.kill(os.getpid(), int(sys.argv[2]))
except m.bridge.BridgeCancelled:
    assert a.calls == [('actions/runs/42', 'GET'), ('actions/runs/42/cancel', 'POST')], a.calls
    sys.exit(130)
assert a.calls == [], a.calls
'''
        for sig in [0, signal.SIGINT, signal.SIGTERM]:
            run = subprocess.run([sys.executable, '-c', script, str(Path(__file__).resolve()), str(int(sig))],
                                 capture_output=True, text=True, timeout=5)
            self.assertEqual(run.returncode, 130 if sig else 0, run.stderr)
            self.assertEqual('Cancellation accepted' in run.stdout, bool(sig))


if __name__ == '__main__':
    unittest.main()
