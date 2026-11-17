"""#844: a cancelled or superseded Forgejo job cancels only the GitHub run it owns."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import yaml

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).parent


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, HERE / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


canceller = load('hosted_cancel', 'github-cancel.py')
transport = canceller.transport
pr_bridge = load('pr_bridge_844', 'github-pr.py')
linux_bridge = load('linux_bridge_844', 'github-linux.py')
macos_bridge = load('macos_bridge_844', 'github-macos.py')

SHA = 'a' * 40
UUID = 'b' * 32
BRANCH = f'forgejo-shared-pr/12-{SHA}-345-1-{UUID}'
WORKFLOW = '.github/workflows/forgejo-shared.yml'
ENV = {'GITHUB_RUN_ID': '345', 'GITHUB_RUN_ATTEMPT': '1'}


def run(run_id=7, branch=BRANCH, status='in_progress', sha=SHA, workflow=WORKFLOW):
    return dict(id=run_id, head_sha=sha, head_branch=branch, event='push', path=workflow,
                status=status, conclusion=None, html_url=f'https://github.com/run/{run_id}')


class FakeAPI:
    """Lists `listings` in order (last one repeats), serves `runs` by id."""

    def __init__(self, listings=(), runs=(), refuse_cancel=False):
        self.listings = list(listings) or [[]]
        self.runs = {item['id']: item for item in runs}
        self.refuse_cancel = refuse_cancel
        self.calls = []
        self.timeout = 30

    def request(self, path, method='GET', data=None):
        self.calls.append((method, path.split('?')[0]))
        if method == 'GET' and path.startswith('actions/runs?'):
            return {'workflow_runs': self.listings.pop(0) if len(self.listings) > 1 else self.listings[0]}
        if method == 'GET' and path.startswith('actions/runs/'):
            return self.runs[int(path.rsplit('/', 1)[1])]
        if method == 'POST' and path.endswith('/cancel'):
            if self.refuse_cancel:
                raise transport.Unavailable('GitHub POST actions/runs/7/cancel: HTTP 403 (permission denied)')
            return None
        if method == 'DELETE':
            return None
        raise AssertionError(f'unexpected {method} {path}')

    def mutations(self):
        return [call for call in self.calls if call[0] != 'GET']


def write_receipt(directory, **fields):
    data = dict(version=1, branch=BRANCH, sha=SHA, workflow=WORKFLOW, run_id=None, finished=False)
    data.update(fields)
    path = Path(directory) / 'receipt.json'
    path.write_text(json.dumps(data))
    return path


def hook(receipt, api, env=ENV):
    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        code = canceller.main(['github-cancel.py', str(receipt)], dict(env),
                              sleep=lambda _: None, github=lambda: api)
    return code, output.getvalue()


class OwnershipTests(unittest.TestCase):
    def test_only_unique_non_release_invocation_refs_are_cancellable(self):
        self.assertEqual(transport.owned_identity(BRANCH, SHA, WORKFLOW), ('345', '1'))
        for lane in ('linux', 'shared', 'brain', 'native-shell', 'check', 'shell-full', 'perf-linux'):
            with self.subTest(lane=lane):
                branch = f'forgejo-{lane}-pr/9-{SHA}-11-2-{UUID}'
                workflow = f'.github/workflows/forgejo-{lane}.yml'
                self.assertEqual(transport.owned_identity(branch, SHA, workflow), ('11', '2'))
        self.assertEqual(transport.owned_identity(f'forgejo-pr/9-{SHA}-11-1-{UUID}', SHA,
                                                  '.github/workflows/forgejo-macos.yml'), ('11', '1'))

    def test_release_producers_shared_refs_and_foreign_identities_are_never_cancellable(self):
        rejected = [
            # #2891: hosted release producers are never cancelled automatically.
            (f'forgejo-arch-pr/9-{SHA}-11-1-{UUID}', '.github/workflows/forgejo-arch.yml'),
            (f'forgejo-windows-release-pr/9-{SHA}-11-1-{UUID}', '.github/workflows/forgejo-windows-release.yml'),
            (BRANCH, '.github/workflows/forgejo-macos-shipping.yml'),
            (BRANCH, '.github/workflows/forgejo-arch-release.yml'),
            # The fast-forward fast ref identifies no single invocation.
            ('forgejo-fast-pr/9', '.github/workflows/forgejo-fast.yml'),
            (f'forgejo-fast-pr/9-{SHA}-11-1-{UUID}', WORKFLOW),
            # Legacy refs without an invocation id, other SHAs, main and hourly producers.
            (f'forgejo-shared-pr/12-{SHA}-345-1', WORKFLOW),
            (f'forgejo-shared-pr/12-{"c" * 40}-345-1-{UUID}', WORKFLOW),
            ('main', WORKFLOW), (f'forgejo-linux-pr/hourly-{SHA}', '.github/workflows/forgejo-linux.yml'),
            (BRANCH, '.github/workflows/release.yml'), (BRANCH, '../forgejo-shared.yml'),
        ]
        for branch, workflow in rejected:
            with self.subTest(branch=branch, workflow=workflow):
                self.assertIsNone(transport.owned_identity(branch, SHA, workflow))
        self.assertIsNone(transport.owned_identity(BRANCH, SHA.upper(), WORKFLOW))
        self.assertIsNone(transport.owned_identity(None, SHA, WORKFLOW))


class HookTests(unittest.TestCase):
    def test_matching_unfinished_run_is_cancelled_and_its_ref_removed(self):
        with tempfile.TemporaryDirectory() as tmp:
            api = FakeAPI(runs=[run()])
            code, output = hook(write_receipt(tmp, run_id=7), api)
        self.assertEqual(code, 0)
        self.assertEqual(api.mutations(), [('POST', 'actions/runs/7/cancel'),
                                           ('DELETE', f'git/refs/heads/{BRANCH}')])
        self.assertIn('outcome=accepted run=https://github.com/BeFeast/okilum/actions/runs/7', output)
        self.assertEqual(api.timeout, canceller.REQUEST_TIMEOUT)

    def test_finished_bridge_makes_no_request(self):
        with tempfile.TemporaryDirectory() as tmp:
            api = FakeAPI(runs=[run()])
            code, output = hook(write_receipt(tmp, run_id=7, finished=True), api)
        self.assertEqual((code, api.calls), (0, []))
        self.assertIn('no cancellation', output)

    def test_completed_run_is_not_cancelled(self):
        with tempfile.TemporaryDirectory() as tmp:
            api = FakeAPI(runs=[run(status='completed')])
            _, output = hook(write_receipt(tmp, run_id=7), api)
        self.assertEqual(api.mutations(), [('DELETE', f'git/refs/heads/{BRANCH}')])
        self.assertIn('outcome=already-completed', output)

    def test_replacement_or_foreign_run_is_never_cancelled(self):
        replacement = f'forgejo-shared-pr/12-{SHA}-346-1-{"c" * 32}'
        for recorded in [run(branch=replacement), run(sha='c' * 40),
                         run(workflow='.github/workflows/forgejo-linux.yml'), dict(run(), event='workflow_dispatch'),
                         dict(run(), id=8)]:
            with self.subTest(recorded=recorded), tempfile.TemporaryDirectory() as tmp:
                api = FakeAPI()
                api.runs[7] = recorded
                _, output = hook(write_receipt(tmp, run_id=7), api)
                self.assertEqual(api.mutations(), [])
                self.assertIn('outcome=refused', output)

    def test_discovery_waits_for_the_run_listing_and_ignores_the_replacement(self):
        # Dispatch accepted, run not yet listed: the replacement invocation of the
        # same PR is already listed and must not be selected.
        replacement = run(run_id=99, branch=f'forgejo-shared-pr/12-{SHA}-346-1-{"c" * 32}')
        with tempfile.TemporaryDirectory() as tmp:
            api = FakeAPI(listings=[[replacement], [replacement], [replacement, run()]], runs=[run(), replacement])
            _, output = hook(write_receipt(tmp), api)
        self.assertEqual(api.mutations(), [('POST', 'actions/runs/7/cancel'),
                                           ('DELETE', f'git/refs/heads/{BRANCH}')])
        self.assertEqual([call for call in api.calls if call == ('GET', 'actions/runs')], [('GET', 'actions/runs')] * 3)
        self.assertIn('outcome=accepted', output)

    def test_undiscovered_run_is_bounded_and_reported(self):
        with tempfile.TemporaryDirectory() as tmp:
            api = FakeAPI(listings=[[run(run_id=99, branch=f'forgejo-shared-pr/12-{SHA}-346-1-{"c" * 32}')]])
            _, output = hook(write_receipt(tmp), api)
        self.assertEqual(api.calls.count(('GET', 'actions/runs')), canceller.DISCOVERY_READS)
        self.assertEqual(api.mutations(), [('DELETE', f'git/refs/heads/{BRANCH}')])
        self.assertIn('::warning::Hosted cancellation outcome=not-found run=none', output)

    def test_ambiguous_discovery_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            api = FakeAPI(listings=[[run(), run(run_id=8)]])
            _, output = hook(write_receipt(tmp), api)
        self.assertEqual(api.mutations(), [])
        self.assertIn('outcome=refused', output)

    def test_refused_or_failed_cancel_is_reported_without_failing_the_job(self):
        with tempfile.TemporaryDirectory() as tmp:
            api = FakeAPI(runs=[run()], refuse_cancel=True)
            code, output = hook(write_receipt(tmp, run_id=7), api)
        self.assertEqual(code, 0)
        self.assertIn('HTTP 403 (permission denied)', output)
        self.assertIn('::warning::Hosted cancellation outcome=failed', output)
        # One refused request, no retry or force-cancel.
        self.assertEqual(api.mutations(), [('POST', 'actions/runs/7/cancel'),
                                           ('DELETE', f'git/refs/heads/{BRANCH}')])

    def test_receipt_of_another_attempt_release_or_malformed_receipt_is_ignored(self):
        cases = [
            (dict(), dict(ENV, GITHUB_RUN_ATTEMPT='2')),
            (dict(), dict(ENV, GITHUB_RUN_ID='346')),
            (dict(branch=f'forgejo-arch-pr/12-{SHA}-345-1-{UUID}',
                  workflow='.github/workflows/forgejo-arch.yml'), ENV),
            (dict(run_id='7'), ENV), (dict(run_id=True), ENV), (dict(finished='no'), ENV),
            (dict(version=2), ENV),
        ]
        for fields, env in cases:
            with self.subTest(fields=fields, env=env), tempfile.TemporaryDirectory() as tmp:
                api = FakeAPI(runs=[run()])
                code, output = hook(write_receipt(tmp, **fields), api, env)
                self.assertEqual((code, api.calls), (0, []))
                self.assertIn('receipt ignored', output)
        with tempfile.TemporaryDirectory() as tmp:
            broken = Path(tmp) / 'receipt.json'
            broken.write_text('{')
            api = FakeAPI()
            self.assertEqual(hook(broken, api)[0], 0)
            self.assertEqual(api.calls, [])


class BridgeReceiptTests(unittest.TestCase):
    def bridge_env(self, tmp, lane='shared'):
        return dict(CI_LANE=lane, MIRROR_TOKEN='test', PR_HEAD_SHA=SHA, PR_NUMBER='12', GITHUB_RUN_ID='345',
                    GITHUB_OUTPUT=str(Path(tmp) / 'output'), OKILUM_HOSTED_RECEIPT=str(Path(tmp) / 'receipt.json'))

    def run_pr_bridge(self, tmp, wait, lane='shared'):
        with patch.dict(os.environ, self.bridge_env(tmp, lane)), \
                patch.object(pr_bridge.transport, 'push_head'), patch.object(pr_bridge.transport, 'GitHub'), \
                patch.object(pr_bridge.transport, 'wait_for_run', side_effect=wait) as waiter, \
                contextlib.redirect_stdout(io.StringIO()):
            try:
                pr_bridge.main()
            except KeyboardInterrupt:
                pass
        receipt = Path(tmp) / 'receipt.json'
        return waiter, json.loads(receipt.read_text()) if receipt.exists() else None

    def test_verdict_marks_the_receipt_finished_with_the_observed_run(self):
        def wait(api, branch, sha, **kwargs):
            kwargs['on_run'](run(branch=branch))
            return 'failure', 'red'
        with tempfile.TemporaryDirectory() as tmp:
            waiter, receipt = self.run_pr_bridge(tmp, wait)
        branch = waiter.call_args.args[1]
        self.assertEqual(receipt, dict(version=1, branch=branch, sha=SHA, workflow=WORKFLOW, run_id=7, finished=True))
        self.assertEqual(transport.owned_identity(branch, SHA, WORKFLOW), ('345', '1'))

    def test_unavailable_verdict_is_finished_and_keeps_the_live_run_policy(self):
        def wait(api, branch, sha, **kwargs):
            raise pr_bridge.transport.Unavailable('queue timeout, cancel refused')
        with tempfile.TemporaryDirectory() as tmp:
            _, receipt = self.run_pr_bridge(tmp, wait)
        self.assertTrue(receipt['finished'])

    def test_interrupted_bridge_leaves_an_unfinished_receipt_for_the_hook(self):
        def wait(api, branch, sha, **kwargs):
            kwargs['on_run'](run(branch=branch))
            raise KeyboardInterrupt
        with tempfile.TemporaryDirectory() as tmp:
            _, receipt = self.run_pr_bridge(tmp, wait)
        self.assertEqual((receipt['run_id'], receipt['finished']), (7, False))

    def test_release_lanes_never_write_a_receipt(self):
        for lane in ('arch', 'windows-release'):
            with self.subTest(lane=lane), tempfile.TemporaryDirectory() as tmp:
                waiter, receipt = self.run_pr_bridge(tmp, lambda *a, **k: ('success', 'ok'), lane)
                self.assertIsNone(receipt)
                self.assertNotIn('on_run', waiter.call_args.kwargs)

    def test_linux_and_macos_bridges_record_and_finish_their_own_run(self):
        for module, workflow in ((linux_bridge, linux_bridge.WORKFLOW), (macos_bridge, macos_bridge.WORKFLOW)):
            with self.subTest(workflow=workflow), tempfile.TemporaryDirectory() as tmp:
                seen = {}
                def wait(api, branch, sha, **kwargs):
                    seen['branch'] = branch
                    kwargs['on_run'](run(branch=branch, workflow=workflow))
                    return 'success', 'ok'
                target = module.transport if module is linux_bridge else module
                with patch.dict(os.environ, self.bridge_env(tmp)), patch.object(target, 'push_head'), \
                        patch.object(target, 'GitHub'), patch.object(target, 'wait_for_run', side_effect=wait), \
                        patch('sys.argv', ['github-macos.py', 'run']), contextlib.redirect_stdout(io.StringIO()):
                    module.main()
                receipt = json.loads((Path(tmp) / 'receipt.json').read_text())
                self.assertEqual(receipt, dict(version=1, branch=seen['branch'], sha=SHA, workflow=workflow,
                                               run_id=7, finished=True))

    def test_wait_for_run_reports_the_listed_run(self):
        seen = []
        api = FakeAPI(listings=[[run(status='completed') | dict(conclusion='failure')]])
        result = transport.wait_for_run(api, BRANCH, SHA, workflow=WORKFLOW, queue_timeout=10,
                                        clock=lambda: 0, sleep=lambda _: None, on_run=seen.append)
        self.assertEqual(result[0], 'failure')
        self.assertEqual([item['id'] for item in seen], [7])

    def test_receipt_write_failure_never_changes_the_gate(self):
        with tempfile.TemporaryDirectory() as tmp, contextlib.redirect_stdout(io.StringIO()) as output:
            receipt = transport.Receipt(Path(tmp) / 'missing' / 'receipt.json', BRANCH, SHA, WORKFLOW)
            receipt.observed(run())
            receipt.finish()
        self.assertIn('receipt not written', output.getvalue())


@unittest.skipUnless(shutil.which('node'), 'node is required for the post-action lifecycle')
class PostActionTests(unittest.TestCase):
    ACTION = ROOT / '.forgejo/actions/hosted-cancel/index.js'

    def node(self, env):
        return subprocess.run(['node', str(self.ACTION)], env=env, capture_output=True, text=True, timeout=60)

    def test_main_registers_and_post_hands_only_an_existing_receipt_to_the_canceller(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            state, github_env, fake_bin = tmp / 'state', tmp / 'env', tmp / 'bin'
            fake_bin.mkdir()
            calls = tmp / 'calls'
            (fake_bin / 'python3').write_text(f'#!/bin/sh\necho "$@" >> {calls}\nexit 3\n')
            (fake_bin / 'python3').chmod(0o755)
            base = dict(PATH=f'{fake_bin}:{os.environ["PATH"]}', RUNNER_TEMP=str(tmp),
                        GITHUB_STATE=str(state), GITHUB_ENV=str(github_env))
            main = self.node(base)
            self.assertEqual(main.returncode, 0, main.stderr)
            receipt = state.read_text().strip().removeprefix('receipt=')
            self.assertEqual(github_env.read_text(), f'OKILUM_HOSTED_RECEIPT={receipt}\n')
            self.assertTrue(receipt.startswith(str(tmp) + '/okilum-hosted-'))

            # No bridge dispatch: nothing to cancel, canceller never runs.
            post = self.node(dict(base, STATE_receipt=receipt))
            self.assertEqual(post.returncode, 0, post.stderr)
            self.assertIn('nothing to cancel', post.stdout)
            self.assertFalse(calls.exists())

            # A recorded dispatch reaches the canceller; its failure is a warning only.
            Path(receipt).write_text('{}')
            post = self.node(dict(base, STATE_receipt=receipt))
            self.assertEqual(post.returncode, 0, post.stderr)
            self.assertIn('::warning::Owned hosted run cancellation did not complete', post.stdout)
            script = ROOT / 'scripts/ci/github-cancel.py'
            self.assertEqual(calls.read_text(), f'-I {script} {receipt}\n')
            self.assertFalse(Path(receipt).exists())


class WorkflowWiringTests(unittest.TestCase):
    WIRED = ('ci.yml', 'commit-ci.yml', 'fast.yml', 'brain-ci.yml', 'inbox-ci.yml',
             'feedback-ci.yml', 'excalidraw-compat.yml')
    BRIDGES = ('python3 scripts/ci/github-pr.py', 'python3 scripts/ci/github-linux.py',
               'python3 scripts/ci/github-macos.py run', 'python3 "$OKILUM_HOSTED_BRIDGE/github-pr.py"')

    def test_every_bridge_step_registers_cancellation_immediately_before_dispatch(self):
        count = 0
        for name in self.WIRED:
            workflow = yaml.safe_load((ROOT / '.forgejo/workflows' / name).read_text())
            for job_name, job in workflow['jobs'].items():
                steps = job.get('steps', [])
                for index, step in enumerate(steps):
                    if not any(bridge in step.get('run', '') for bridge in self.BRIDGES):
                        continue
                    count += 1
                    with self.subTest(workflow=name, job=job_name):
                        hook = steps[index - 1]
                        self.assertEqual(hook.get('uses'), './.forgejo/actions/hosted-cancel')
                        self.assertEqual(set(hook['env']), {'MIRROR_TOKEN', 'OKILUM_GITHUB_APP_ID',
                            'OKILUM_GITHUB_APP_INSTALLATION_ID', 'OKILUM_GITHUB_APP_KEY'})
                        if name == 'fast.yml':
                            # Certification may check out a source that predates the hook.
                            self.assertEqual(hook.get('if'), "inputs.certification_source == ''")
                        else:
                            self.assertNotIn('if', hook)
        self.assertEqual(count, 15)

    def test_release_and_publication_workflows_never_register_cancellation(self):
        for path in sorted((ROOT / '.forgejo/workflows').glob('*.yml')):
            if path.name not in self.WIRED:
                with self.subTest(workflow=path.name):
                    self.assertNotIn('hosted-cancel', path.read_text())

    def test_post_hook_runs_after_cancellation(self):
        action = yaml.safe_load((ROOT / '.forgejo/actions/hosted-cancel/action.yml').read_text())
        self.assertEqual(action['runs'], {'using': 'node20', 'main': 'index.js', 'post': 'index.js',
                                          'post-if': 'always()'})


if __name__ == '__main__':
    unittest.main()
