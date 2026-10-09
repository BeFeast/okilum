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
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('linux_bridge', Path(__file__).with_name('github-linux.py'))
linux = importlib.util.module_from_spec(spec)
spec.loader.exec_module(linux)
SHA = 'a' * 40
BRANCH = 'forgejo-linux-pr/1/' + SHA


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
            ('push', 'hosted', 1, False, False), ('pull_request', 'local', 1, False, False),
            ('pull_request', 'pr-1', 1, False, True), ('pull_request', 'pr-1', 2, False, False),
            ('pull_request', 'hosted', 2, False, True), ('pull_request', '', 1, False, False),
            ('pull_request', 'hosted', 2, True, False), ('pull_request', 'pr-1', 1, True, False),
        ]:
            self.assertEqual(evaluate('linux-github', event, value, number, fork), hosted)
            self.assertEqual(evaluate('linux-local', event, value, number, fork), not hosted)
