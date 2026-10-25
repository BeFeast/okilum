"""Hosted pilot must preserve the full exact-head Linux gate."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest
import urllib.parse
from unittest.mock import MagicMock, patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('linux_bridge', Path(__file__).with_name('github-linux.py'))
linux = importlib.util.module_from_spec(spec)
spec.loader.exec_module(linux)
SHA = 'a' * 40
BRANCH = 'forgejo-linux-pr/1/' + SHA


class LinuxBridgeTests(unittest.TestCase):
    def test_completed_workflow_refreshes_cached_queued_job_and_step_visibility(self):
        # Run 39813111900: all three reads returned the same queued aggregate
        # with no steps despite completed/success workflow and successful inputs.
        # Fake HTTP cache replays by URL: retries only progress with fresh keys.
        for final, expected in [('success', 'success'), ('failure', 'failure'),
                                ('skipped', 'failure'), (None, 'failure')]:
            with self.subTest(final=final):
                cache, reads, requests, lines = {}, [], [], []
                run = dict(id=1, head_sha=SHA, head_branch=BRANCH, event='push',
                           path=linux.WORKFLOW, status='completed', conclusion='success',
                           html_url='https://github.com/run/1')
                snapshots = [
                    dict(id=42, name='linux', status='queued', conclusion=None, steps=[]),
                    dict(id=42, name='linux', status='completed', conclusion=final, steps=[]),
                    dict(id=42, name='linux', status='completed', conclusion=final, steps=[
                        dict(name=linux.BUILD_STEP, status='completed', conclusion=final)]),
                ]
                def urlopen(request, timeout):
                    requests.append(request)
                    url = request.full_url
                    if url not in cache:
                        if '/jobs?' in url:
                            cache[url] = {'jobs': [snapshots[min(len(reads), 2)]]}
                            reads.append(url)
                        else:
                            cache[url] = {'workflow_runs': [run]}
                    response = MagicMock()
                    response.__enter__.return_value.read.return_value = json.dumps(cache[url]).encode()
                    return response
                with patch.object(linux.transport.urllib.request, 'urlopen', side_effect=urlopen), \
                     patch.object(linux.transport.uuid, 'uuid4', side_effect=[
                         MagicMock(hex=f'{n:032x}') for n in range(4)]):
                    result, _ = linux.transport.wait_for_run(
                        linux.transport.GitHub('fake'), BRANCH, SHA, workflow=linux.WORKFLOW,
                        job_name='linux', build_step=linux.BUILD_STEP, sleep=lambda _: None,
                        log=lambda line, **_: lines.append(line))
                self.assertEqual(result, expected)
                self.assertEqual(len(reads), 3)
                self.assertEqual(len(set(reads)), 3)
                self.assertIn("'status': 'queued'", lines[0])
                for request in requests:
                    self.assertEqual(request.method, 'GET')
                    self.assertEqual(request.get_header('Cache-control'), 'no-cache')
                    self.assertEqual(request.get_header('Authorization'), 'Bearer fake')
                    self.assertEqual(request.get_header('Accept'), 'application/vnd.github+json')
                    self.assertEqual(request.get_header('X-github-api-version'), '2022-11-28')
                    query = urllib.parse.parse_qs(urllib.parse.urlsplit(request.full_url).query)
                    self.assertRegex(query['_bridge_read'][0], r'^[0-9a-f]{32}$')
                    if '/jobs?' in request.full_url:
                        self.assertEqual(query['per_page'], ['100'])
                    else:
                        self.assertEqual(query['branch'], [BRANCH])
                        self.assertEqual(query['head_sha'], [SHA])
                        self.assertEqual(query['event'], ['push'])

    def test_cache_bypass_preserves_pagination_attempt_and_mutations(self):
        cases = [
            ('actions/runs/1/attempts/2/jobs?per_page=100&page=2', 'GET', None, True),
            ('actions/runs/1', 'GET', None, True),
            ('actions/runs/1/cancel', 'POST', {'reason': 'test'}, False),
            ('git/refs/heads/test', 'DELETE', None, False),
            ('git/matching-refs/heads/test', 'GET', None, False),
        ]
        for path, method, data, bypass in cases:
            with self.subTest(path=path), \
                 patch.object(linux.transport.urllib.request, 'urlopen') as urlopen, \
                 patch.object(linux.transport.uuid, 'uuid4', return_value=MagicMock(hex='a' * 32)):
                urlopen.return_value.__enter__.return_value.read.return_value = b'{}'
                linux.transport.GitHub('fake').request(path, method, data)
                request = urlopen.call_args.args[0]
                expected = path + ('&' if '?' in path else '?') + '_bridge_read=' + 'a' * 32
                self.assertEqual(request.full_url,
                    f'https://api.github.com/repos/{linux.transport.REPOSITORY}/' +
                    (expected if bypass else path))
                self.assertEqual(request.method, method)
                self.assertEqual(request.data, json.dumps(data).encode() if data is not None else None)
                self.assertEqual(request.get_header('Cache-control'), 'no-cache' if bypass else None)

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

    def test_completed_success_repolls_gate_evidence_past_execution_deadline(self):
        # #1499: workflow success was rejected at the jobs/steps evidence check.
        # Exercise stale projections and a real failure as a negative control.
        for final, expected in [('success', 'success'), ('failure', 'failure')]:
            with self.subTest(final=final):
                now = [0]
                calls = []
                snapshots = iter([
                    [],
                    [dict(name='linux', conclusion=None, steps=[])],
                    [dict(name='linux', conclusion=final, steps=[
                        dict(name=linux.BUILD_STEP, conclusion=final)])],
                ])
                def sleep(seconds):
                    now[0] += seconds
                class API:
                    def request(self, path, method='GET'):
                        calls.append((path, method))
                        if method != 'GET':
                            raise AssertionError('Completed run must not be cancelled')
                        if '/jobs?' in path:
                            return {'jobs': next(snapshots)}
                        # First observation starts the execution clock. The next
                        # poll sees completion after the one-second deadline.
                        completed = now[0] > 0
                        return {'workflow_runs': [dict(
                            id=1, head_sha=SHA, head_branch=BRANCH, event='push',
                            path=linux.WORKFLOW, html_url='https://github.com/run/1',
                            status='completed' if completed else 'in_progress',
                            conclusion='success' if completed else None)]}
                result, _ = linux.transport.wait_for_run(
                    API(), BRANCH, SHA, workflow=linux.WORKFLOW, job_name='linux',
                    build_step=linux.BUILD_STEP, queue_timeout=0, run_timeout=1,
                    clock=lambda: now[0], sleep=sleep, log=lambda *a, **k: None)
                self.assertEqual(result, expected)
                self.assertEqual(sum('/jobs?' in path for path, _ in calls), 3)
                self.assertEqual(now[0], 40)

    def test_paginated_jobs_select_exactly_one_gate(self):
        calls = []
        filler = [dict(name=f'other-{n}', conclusion='success', steps=[])
                  for n in range(100)]
        gate = dict(id=42, name='linux', status='completed', conclusion='success',
                    steps=[dict(name=linux.BUILD_STEP, status='completed', conclusion='success')])
        class API:
            def request(self, path, method='GET'):
                calls.append((path, method))
                if '/jobs?' in path:
                    return {'jobs': [gate] if '&page=2' in path else filler}
                return {'workflow_runs': [dict(id=1, head_sha=SHA, head_branch=BRANCH,
                    event='push', path=linux.WORKFLOW, status='completed',
                    conclusion='success', html_url='https://github.com/run/1')]}
        result, _ = linux.transport.wait_for_run(API(), BRANCH, SHA,
            workflow=linux.WORKFLOW, job_name='linux', build_step=linux.BUILD_STEP,
            queue_timeout=0, sleep=lambda _: None)
        self.assertEqual(result, 'success')
        self.assertIn(('actions/runs/1/jobs?per_page=100&page=2', 'GET'), calls)

    def test_persistent_invalid_gate_fails_with_actual_fields(self):
        good = dict(id=42, name='linux', status='completed', conclusion='success',
                    steps=[dict(name=linux.BUILD_STEP, status='completed', conclusion='success')])
        cases = [[], [dict(good, name='wrong')], [good, good],
                 [dict(good, steps=[])], [dict(good, steps=good['steps'] * 2)]]
        for conclusion in ['skipped', 'failure', None]:
            cases.append([dict(good, conclusion=conclusion)])
            cases.append([dict(good, steps=[dict(good['steps'][0], conclusion=conclusion)])])
        for jobs in cases:
            with self.subTest(jobs=jobs):
                lines, calls = [], []
                class API:
                    def request(self, path, method='GET'):
                        calls.append((path, method))
                        if '/jobs?' in path:
                            return {'jobs': jobs}
                        return {'workflow_runs': [dict(id=1, head_sha=SHA, head_branch=BRANCH,
                            event='push', path=linux.WORKFLOW, status='completed',
                            conclusion='success', html_url='https://github.com/run/1')]}
                result, _ = linux.transport.wait_for_run(API(), BRANCH, SHA,
                    workflow=linux.WORKFLOW, job_name='linux', build_step=linux.BUILD_STEP,
                    queue_timeout=0, sleep=lambda _: None,
                    log=lambda line, **_: lines.append(line))
                self.assertEqual(result, 'failure')
                self.assertEqual(sum('/jobs?' in path for path, _ in calls), 3)
                self.assertTrue(all(method == 'GET' for _, method in calls))
                self.assertEqual(len(lines), 3)
                self.assertIn('run_id=1 status=completed conclusion=success', lines[-1])
                self.assertIn(repr(jobs), lines[-1])
                self.assertIn(repr(linux.BUILD_STEP), lines[-1])

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
            env = dict(MIRROR_TOKEN='test-token', PR_HEAD_SHA=SHA, PR_NUMBER='1', GITHUB_RUN_ID='2',
                       GITHUB_OUTPUT=str(Path(tmp) / 'output'))
            for result, code in [('success', 0), ('failure', 1), ('unavailable', 1)]:
                with patch.dict(os.environ, env), patch.object(linux.transport, 'push_head'), \
                     patch.object(linux.transport, 'GitHub'), \
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

    def test_hosted_workflow_has_no_secrets_or_cache_writes(self):
        workflow = (ROOT / '.github/workflows/forgejo-linux.yml').read_text()
        for forbidden in ['secrets.', 'actions/cache/save', 'actions/cache@', 'SCCACHE_']:
            self.assertNotIn(forbidden, workflow)
        self.assertIn("branches: [main, 'forgejo-linux-pr/**']", workflow)
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
            ('push', 'hosted', 1, False, False), ('pull_request', 'local', 1, False, False),
            ('pull_request', 'pr-1', 1, False, True), ('pull_request', 'pr-1', 2, False, False),
            ('pull_request', 'hosted', 2, False, True), ('pull_request', '', 1, False, False),
            ('pull_request', 'hosted', 2, True, False), ('pull_request', 'pr-1', 1, True, False),
        ]:
            self.assertEqual(evaluate('linux-github', event, value, number, fork), hosted)
            self.assertEqual(evaluate('linux-local', event, value, number, fork), not hosted)
