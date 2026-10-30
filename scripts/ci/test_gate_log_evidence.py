import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch, MagicMock

import gate_log_evidence as evidence

HERE = Path(__file__).parent
spec = importlib.util.spec_from_file_location('gate_transport', HERE / 'github-macos.py')
bridge = importlib.util.module_from_spec(spec); spec.loader.exec_module(bridge)
SHA = 'a' * 40
STEP = 'Run every shared suite'
IDENTITY = dict(sha=SHA, run=71, attempt=1, job='shared', step=STEP)


def line(kind, **fields):
    return '2026-10-30T17:00:00.1234567Z OKILUM_GATE_V1 ' + kind + ' ' + json.dumps(fields) + '\n'


def good():
    return (line('BEGIN', **IDENTITY) + 'suite output\n' + line('END', **IDENTITY, exit=0)).encode()


class EvidenceTests(unittest.TestCase):
    def test_exact_complete_marker_pair_passes(self):
        self.assertTrue(evidence.executed(good(), **IDENTITY))

    def test_missing_wrong_step_failed_exit_wrong_identity_duplicate_or_reversed_fail(self):
        variants = [b'', b'731 tests passed\n', line('BEGIN', **IDENTITY).encode(),
                    good().replace(STEP.encode(), b'Other step'),
                    good().replace(b'"exit": 0', b'"exit": 1'),
                    good().replace(SHA.encode(), b'b' * 40),
                    good() + good(), good().splitlines(keepends=True)[-1] + good().splitlines(keepends=True)[0],
                    b'echo ' + good(), b'\xff' + good(),
                    good().replace(b'"attempt": 1', b'"attempt": 2')]
        for raw in variants:
            with self.subTest(raw=raw[:60]): self.assertFalse(evidence.executed(raw, **IDENTITY))
        with patch.object(evidence, 'MAX_LOG_BYTES', 5):
            self.assertFalse(evidence.executed(good(), **IDENTITY))

    def test_wrapper_emits_end_only_after_command_and_preserves_exit(self):
        env = dict(os.environ, GITHUB_SHA=SHA, GITHUB_RUN_ID='71', GITHUB_RUN_ATTEMPT='1', GITHUB_JOB='shared')
        for code in [0, 7]:
            run = subprocess.run([sys.executable, str(HERE/'gate-step.py'), '--name', STEP, '--',
                                  sys.executable, '-c', f'print("positive control"); raise SystemExit({code})'],
                                 env=env, capture_output=True)
            self.assertEqual(run.returncode, code)
            self.assertIn(b'positive control', run.stdout)
            self.assertEqual(evidence.executed(run.stdout, **IDENTITY), code == 0)


class API:
    def __init__(self, steps, raw):
        self.raw = raw; self.log_reads = 0; self.evidence_reads = 0
        self.run = dict(id=71, run_attempt=1, head_sha=SHA, head_branch='branch', event='push',
                        path=bridge.WORKFLOW, status='completed', conclusion='success', html_url='https://github.com/run/71')
        self.job = dict(id=99, run_id=71, head_sha=SHA, name='shared', status='completed', conclusion='success', steps=steps)
    def request(self, path):
        if '/jobs?' in path:
            self.evidence_reads += 1; return {'jobs':[self.job]}
        return {'workflow_runs':[self.run]}
    def job_log(self, job):
        assert job == 99; self.log_reads += 1; return self.raw


def wait(api):
    return bridge.wait_for_run(api, 'branch', SHA, job_name='shared', build_step=STEP,
                               clock=lambda:0, sleep=lambda _:None, log=lambda *a,**k:None)[0]


class TransportTests(unittest.TestCase):
    def test_steps_present_does_not_read_log(self):
        api = API([dict(name=STEP, conclusion='success')], b'')
        self.assertEqual(wait(api), 'success'); self.assertEqual(api.log_reads, 0)

    def test_empty_steps_after_three_reads_plus_exact_log_passes(self):
        api = API([], good()); self.assertEqual(wait(api), 'success')
        self.assertEqual(api.evidence_reads, 3); self.assertEqual(api.log_reads, 1)

    def test_empty_steps_and_missing_or_wrong_log_fail_closed(self):
        for raw in [b'', b'No expected step', good().replace(STEP.encode(), b'Other')]:
            api=API([],raw); self.assertEqual(wait(api),'failure'); self.assertEqual(api.log_reads,1)

    def test_pending_failed_skipped_steps_and_wrong_job_identity_never_use_log(self):
        for conclusion in ['pending','failure','skipped',None]:
            api=API([dict(name=STEP,conclusion=conclusion)],good())
            self.assertEqual(wait(api),'failure'); self.assertEqual(api.log_reads,0)
        for field,value in [('head_sha','b'*40),('run_id',72),('status','in_progress'),('conclusion','skipped')]:
            api=API([],good());api.job[field]=value
            self.assertEqual(wait(api),'failure');self.assertEqual(api.log_reads,0)

    def test_log_missing_service_response_cannot_be_success(self):
        api=API([],good())
        with patch.object(api,'job_log',side_effect=bridge.Unavailable('log unavailable')):
            with self.assertRaises(bridge.Unavailable): wait(api)

    def test_each_instrumented_workflow_wraps_the_actual_expected_command(self):
        root = HERE.parents[1] / '.github/workflows'
        for file, step, command in [
            ('forgejo-shared.yml', STEP, 'tini -s -- bash scripts/ci/check-shared.sh'),
            ('forgejo-macos.yml', bridge.BUILD_STEP, 'bash scripts/ci/check-macos.sh'),
            ('forgejo-macos-depot.yml', bridge.BUILD_STEP, 'bash scripts/ci/check-macos.sh'),
            ('forgejo-linux.yml', 'Run full Linux gate', 'bash -c'),
            ('forgejo-windows-release.yml', 'Build and validate Windows package', 'pwsh -NoProfile -File scripts/ci/build-windows-native.ps1'),
        ]:
            text = (root / file).read_text()
            self.assertIn(f"scripts/ci/gate-step.py --name '{step}' -- {command}", text)
        aggregate = (root / 'forgejo-linux.yml').read_text().split('  linux:', 1)[1]
        self.assertIn('uses: actions/checkout@v4', aggregate)
        self.assertIn('test "$CHECKS" = success && test "$APPIMAGE" = success', aggregate)

    def test_log_redirect_drops_credentials_and_is_bounded(self):
        import urllib.error
        from email.message import Message
        headers=Message();headers['Location']='https://example.blob.core.windows.net/log?sig=opaque'
        redirect=urllib.error.HTTPError('https://api.github.com/job/logs',302,'redirect',headers,None)
        credentials=MagicMock();credentials.token='private-test-token'
        api=bridge.GitHub(credentials)
        response=MagicMock();response.read.return_value=good()
        with patch.object(bridge.urllib.request,'build_opener') as opener, patch.object(bridge.urllib.request,'urlopen',return_value=response) as download:
            opener.return_value.open.side_effect=redirect
            self.assertEqual(api.job_log(99),good())
            request=download.call_args.args[0]
            self.assertIsNone(request.get_header('Authorization'))
            response.read.assert_called_once_with(evidence.MAX_LOG_BYTES+1)
        headers.replace_header('Location','https://untrusted.example/log')
        with patch.object(bridge.urllib.request,'build_opener') as opener, patch.object(bridge.urllib.request,'urlopen') as download:
            opener.return_value.open.side_effect=redirect
            with self.assertRaises(bridge.Unavailable): api.job_log(99)
            download.assert_not_called()
