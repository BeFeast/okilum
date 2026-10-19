"""Hosted pilot must preserve the full exact-head Linux gate."""
import copy
from contextlib import redirect_stdout
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, call, patch
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('linux_bridge', Path(__file__).with_name('github-linux.py'))
linux = importlib.util.module_from_spec(spec)
spec.loader.exec_module(linux)
SHA = 'a' * 40
BRANCH = 'forgejo-linux-pr/1/' + SHA


class PublicArchiveReadTests(unittest.TestCase):
    KEY = f'okilum/releases/{SHA}/linux.json'
    URL = 'https://updates.befeast.com/' + KEY
    SENTINEL = 'private-error-detail-sentinel'

    def http_error(self, status):
        error = urllib.error.HTTPError(self.URL, status, self.SENTINEL, {}, io.BytesIO())
        self.addCleanup(error.close)
        return error

    def assert_request(self, opener, key=KEY):
        opener.assert_called_once()
        args, kwargs = opener.call_args
        self.assertEqual(len(args), 1)
        request = args[0]
        self.assertIsInstance(request, urllib.request.Request)
        self.assertEqual(request.full_url, 'https://updates.befeast.com/' + key)
        self.assertEqual(request.get_method(), 'GET')
        self.assertIsNone(request.data)
        self.assertEqual(dict(request.header_items()), {'User-agent': 'okilum/0.1.0'})
        self.assertEqual(kwargs, {'timeout': 60})

    def failures(self):
        for status in (401, 403, 500, 503):
            yield str(status), self.http_error(status)
        yield 'timeout', TimeoutError(self.SENTINEL)
        yield 'url-timeout', urllib.error.URLError(TimeoutError(self.SENTINEL))
        yield 'network', urllib.error.URLError(self.SENTINEL)
        yield 'network-404-text', urllib.error.URLError('HTTP 404 ' + self.SENTINEL)

    def test_product_request_returns_exact_200_bytes(self):
        for key in (self.KEY, 'okilum/arch/builds/7000/okilum-0.1.7000-1-x86_64.pkg.tar.zst.sig'):
            with self.subTest(key=key):
                body = b'\x00\xffarchive\r\nbytes\x80'
                with patch.object(linux.urllib.request, 'urlopen', return_value=io.BytesIO(body)) as opener:
                    self.assertEqual(linux.public_read(key), body)
                    self.assert_request(opener, key)

    def test_only_actual_http_404_is_missing(self):
        error = self.http_error(404)
        with patch.object(linux.urllib.request, 'urlopen', side_effect=error) as opener:
            self.assertIsNone(linux.public_read(self.KEY))
            self.assert_request(opener)

    def test_http_timeout_and_network_errors_are_sanitized_unavailable(self):
        for name, error in self.failures():
            with self.subTest(error=name), \
                    patch.object(linux.urllib.request, 'urlopen', side_effect=error) as opener:
                with self.assertRaises(linux.transport.Unavailable) as raised:
                    linux.public_read(self.KEY)
                self.assertNotIn(self.SENTINEL, str(raised.exception))
                self.assertTrue(raised.exception.__suppress_context__)
                self.assert_request(opener)

    def test_response_read_timeout_is_sanitized_unavailable(self):
        with patch.object(linux.urllib.request, 'urlopen') as opener:
            opener.return_value.__enter__.return_value.read.side_effect = TimeoutError(self.SENTINEL)
            with self.assertRaises(linux.transport.Unavailable) as raised:
                linux.public_read(self.KEY)
            self.assertNotIn(self.SENTINEL, str(raised.exception))
            self.assert_request(opener)

    def test_unsafe_keys_fail_before_opening(self):
        with patch.object(linux.urllib.request, 'urlopen') as opener:
            for key in ('../linux.json', 'okilum/../linux.json', 'https://other.test/linux.json',
                        '/?token=sentinel', 'okilum/secret\nheader'):
                with self.subTest(key=key), self.assertRaisesRegex(ValueError, 'Unsafe archive key'):
                    linux.public_read(key)
            opener.assert_not_called()

    def test_public_read_failure_stops_bridge_consumer_and_producer(self):
        current = dict(id=7, run_attempt=1, head_sha=SHA, event='push', path=linux.WORKFLOW,
            repository={'full_name': 'BeFeast/okilum'},
            head_branch=f'forgejo-linux-pr/1/{SHA}-91-1-b9108-' + 'a' * 32)
        for command in ('request', 'resolve', 'produce'):
            for name, error in self.failures():
                with self.subTest(command=command, error=name), tempfile.TemporaryDirectory() as tmp:
                    api = Mock()
                    api.request.return_value = current
                    output = io.StringIO()
                    with patch.dict(os.environ, dict(PR_HEAD_SHA=SHA, MIRROR_TOKEN='fixture-token',
                            GITHUB_RUN_ID='7', GITHUB_RUN_ATTEMPT='1', OKILUM_ARTIFACT_DIR=tmp,
                            GITHUB_OUTPUT=str(Path(tmp) / 'result')), clear=True), \
                            patch.object(linux.payload, 'config', return_value='b' * 64), \
                            patch.object(linux.transport, 'GitHub', return_value=api), \
                            patch.object(linux.urllib.request, 'urlopen', side_effect=error) as opener, \
                            patch.object(linux, 'discover') as discover, \
                            patch.object(linux.transport, 'push_head') as push, \
                            patch.object(linux.transport, 'wait_for_run') as wait, \
                            patch.object(linux, 'authorize_claim') as authorize, \
                            patch.object(linux, 'await_claim') as claim, \
                            patch.object(linux.subprocess, 'run') as compiler, redirect_stdout(output):
                        self.assertEqual(linux.main(command), 1)
                        self.assert_request(opener)
                        for boundary in (discover, push, wait, authorize, claim, compiler):
                            boundary.assert_not_called()
                        self.assertEqual(api.request.call_args_list,
                            [call('actions/runs/7')] if command == 'produce' else [])
                    self.assertIn('Linux production unavailable; no compiler fallback:', output.getvalue())
                    self.assertNotIn(self.SENTINEL, output.getvalue())
                    self.assertFalse((Path(tmp) / 'receipt.json').exists())

    def test_404_positive_control_reaches_cold_request_dispatch_stub(self):
        with tempfile.TemporaryDirectory() as tmp, \
                patch.dict(os.environ, dict(PR_HEAD_SHA=SHA, MIRROR_TOKEN='fixture-token',
                    GITHUB_RUN_ID='91', GITHUB_RUN_ATTEMPT='1', GITHUB_RUN_NUMBER='9108',
                    OKILUM_ARTIFACT_DIR=tmp), clear=True), \
                patch.object(linux.payload, 'config', return_value='b' * 64), \
                patch.object(linux.transport, 'GitHub'), \
                patch.object(linux.urllib.request, 'urlopen', side_effect=
                    self.http_error(404)) as opener, \
                patch.object(linux, 'discover', return_value=(None, [])) as discover, \
                patch.object(linux, 'read_claim', return_value=None), \
                patch.object(linux.transport, 'push_head') as push, \
                patch.object(linux.transport, 'wait_for_run', return_value=('failure', 'fixture gate stop')) as wait, \
                patch.object(linux.subprocess, 'run') as compiler, redirect_stdout(io.StringIO()):
            self.assertEqual(linux.main('request'), 1)  # Stub gate deliberately fails after dispatch.
            self.assert_request(opener)
            discover.assert_called_once()
            push.assert_called_once()
            self.assertEqual(push.call_args.args[1:], (SHA, 'fixture-token'))
            wait.assert_called_once()
            compiler.assert_not_called()


class LinuxBridgeTests(unittest.TestCase):
    def wait(self, change=None, jobs=None):
        run = dict(id=1, head_sha=SHA, head_branch=BRANCH, event='push', path=linux.WORKFLOW,
                   status='completed', conclusion='success', html_url='https://github.com/run/1')
        run.update(change or {})
        jobs = jobs if jobs is not None else [dict(name='linux', conclusion='success', steps=[
            dict(name=linux.BUILD_STEP, conclusion='success')])]
        class API:
            def request(self, path, method='GET'):
                return {'jobs': jobs} if '/jobs?' in path else {'workflow_runs': [run]}
        return linux.transport.wait_for_run(API(), BRANCH, SHA, workflow=linux.WORKFLOW,
            job_name='linux', build_step=linux.BUILD_STEP, queue_timeout=0, clock=lambda: 1, sleep=lambda n: None)[0]

    def test_exact_executed_linux_gate_passes(self):
        self.assertEqual(self.wait(), 'success')

    def test_old_sha_other_workflow_or_mirror_lag_never_passes(self):
        for field, value in [('head_sha', 'b' * 40), ('head_branch', 'main'),
                             ('path', '.github/workflows/forgejo-macos.yml'), ('event', 'workflow_dispatch')]:
            with self.assertRaises(linux.transport.Unavailable):
                self.wait({field: value})

    def test_green_workflow_with_missing_or_skipped_gate_fails(self):
        self.assertEqual(self.wait(jobs=[]), 'failure')
        for step in ['skipped', 'failure']:
            self.assertEqual(self.wait(jobs=[dict(name='linux', conclusion='success', steps=[
                dict(name=linux.BUILD_STEP, conclusion=step)])]), 'failure')

    def test_bridge_exit_failure_and_success_are_not_interchangeable(self):
        with tempfile.TemporaryDirectory() as tmp:
            env = dict(MIRROR_TOKEN='test-token', PR_HEAD_SHA=SHA, PR_NUMBER='1', GITHUB_RUN_ID='2', GITHUB_RUN_NUMBER='9108',
                       GITHUB_OUTPUT=str(Path(tmp) / 'output'), OKILUM_ARTIFACT_DIR=str(Path(tmp) / 'artifact'))
            for result, code in [('success', 0), ('failure', 1)]:
                with patch.dict(os.environ, env), patch.object(linux.transport, 'push_head'), \
                     patch.object(linux.transport, 'GitHub'), \
                     patch.object(linux, 'archived', return_value=None), \
                     patch.object(linux, 'discover', return_value=(None, [])), \
                     patch.object(linux, 'read_claim', return_value=None), \
                     patch.object(linux, 'resolve'), \
                     patch.object(linux.transport, 'wait_for_run', return_value=(result, 'test')):
                    self.assertEqual(linux.main(), code)
                self.assertTrue(Path(env['GITHUB_OUTPUT']).read_text().endswith(f'result={result}\n'))

    def test_aggregate_counts_actual_lanes_not_mutable_variable(self):
        workflow = (ROOT / '.forgejo/workflows/ci.yml').read_text()
        block = re.search(r'^  linux:\n(.*?)(?=^  [\w-]+:)', workflow, re.M | re.S)[1]
        import textwrap
        script = textwrap.dedent(block.split('        run: |\n')[1])
        for local in ['success', 'skipped', 'failure', 'cancelled']:
            for hosted in ['success', 'skipped', 'failure', 'cancelled']:
                for result in ['success', 'failure', 'unavailable', '']:
                    env = dict(os.environ, LOCAL_RESULT=local, HOSTED_JOB=hosted, HOSTED_RESULT=result,
                               OKILUM_LINUX_LANE='changed-mid-flight')
                    code = subprocess.run(['bash', '-c', script], env=env, capture_output=True).returncode
                    expected = (local == 'success' and hosted == 'skipped') or (
                        local == 'skipped' and hosted == 'success' and result == 'success')
                    self.assertEqual(code == 0, expected, (local, hosted, result))

    def test_hosted_workflow_has_no_secrets_or_cache_writes_or_main_trigger(self):
        workflow = (ROOT / '.github/workflows/forgejo-linux.yml').read_text()
        for forbidden in ['secrets.', 'actions/cache/save', 'actions/cache@', 'SCCACHE_', 'branches: [main']:
            self.assertNotIn(forbidden, workflow)
        self.assertIn('actions/cache/restore@v4', workflow)
        self.assertIn('persist-credentials: false', workflow)
        self.assertIn('bash scripts/ci/check-linux.sh', workflow)

    def test_full_gate_retains_all_existing_command_families(self):
        script = (ROOT / 'scripts/ci/check-linux.sh').read_text()
        for command in ['cargo fmt --check', 'cargo clippy --workspace --all-targets -- -D warnings',
                        'cargo test -p okilum-core -p okilum-shell -p okilum-sync',
                        'cargo check -p okilum-shell --no-default-features',
                        'cargo test -p okilum-core --no-default-features --test portable_reader',
                        'python3 scripts/test-maintenance-matrix.py', 'python3 scripts/brand-assets.py verify',
                        'python3 scripts/test-third-party-notices.py']:
            self.assertIn(command, script)
        for directory in ['updater', 'arch', 'windows', 'releases', 'ci']:
            self.assertIn(f"python3 -m unittest discover -s scripts/{directory} -p 'test_*.py'", script)


class CanaryRoutingTests(unittest.TestCase):
    def test_real_guards_preserve_main_forks_and_canary_isolation(self):
        workflow = (ROOT / '.forgejo/workflows/ci.yml').read_text()
        def evaluate(job, event, value, number, fork=False):
            block = re.search(rf'^  {job}:\n(.*?)(?=^  [\w-]+:)', workflow, re.M | re.S)[1]
            expression = re.search(r'    if: >-\n(.*?)(?=^    [a-z])', block, re.M | re.S)[1]
            replacements = {
                'github.event.pull_request.head.repo.full_name': 'fork/repo' if fork else 'BeFeast/okilum',
                'github.event.pull_request.number': number,
                'github.event_name': event,
                'github.repository': 'BeFeast/okilum',
                'vars.OKILUM_LINUX_LANE': value,
            }
            for key, item in replacements.items():
                expression = expression.replace(key, repr(item))
            expression = ' '.join(expression.split()).replace('&&', ' and ').replace('||', ' or ')
            return eval(expression, {'__builtins__': {}, 'format': lambda pattern, n: pattern.format(n)})
        for event, value, number, fork, hosted in [
            ('push', 'hosted', 1, False, True), ('pull_request', 'local', 1, False, True),
            ('pull_request', 'pr-1', 1, False, True), ('pull_request', 'pr-1', 2, False, True),
            ('pull_request', 'hosted', 2, False, True), ('pull_request', '', 1, False, True),
            ('pull_request', 'hosted', 2, True, False), ('pull_request', 'pr-1', 1, True, False),
        ]:
            self.assertEqual(evaluate('linux-github', event, value, number, fork), hosted)
            self.assertEqual(evaluate('linux-local', event, value, number, fork), not hosted)
