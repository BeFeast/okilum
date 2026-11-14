"""Hosted Windows beta handoff (#2927): exact source, Forgejo build number, fail closed."""
import argparse
import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import tempfile
import unittest
import urllib.error
import zipfile

import yaml

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('windows_handoff', Path(__file__).with_name('windows_handoff.py'))
handoff = importlib.util.module_from_spec(spec)
spec.loader.exec_module(handoff)
SHA = 'a' * 40
TOOLING = 'c' * 40
NONCE = 'b' * 32
BUILD = 25123
VERSION = f'0.1.{BUILD}'
BRANCH = f'forgejo-windows-beta-src/{BUILD}-{SHA}-77-1-{NONCE}'
STARTED = '2026-11-14T10:00:00Z'
ENV = {'GITHUB_RUN_ID': '77', 'GITHUB_RUN_ATTEMPT': '1', 'GITHUB_REF': 'refs/heads/main',
       'GITHUB_EVENT_NAME': 'push', 'GITHUB_SHA': TOOLING,
       'OKILUM_WINDOWS_SIGN_SUBJECT': 'CN=Example Publisher,C=IL',
       'OKILUM_WINDOWS_SIGN_ISSUER_CHAIN': '["CN=Example CA,C=PL"]'}


def identity(mode='delta', **changes):
    return {'source': SHA, 'tooling': TOOLING, 'build': BUILD, 'mode': mode, 'producer': '77',
            'attempt': '1', 'invocation': NONCE, **changes}


def feed(*versions):
    return json.dumps({'Assets': [{'Version': v, 'Type': 'Full'} for v in versions]})


def entries(mode='delta', **changes):
    files = {handoff.HANDOFF: json.dumps(identity(mode)),
             'windows-dist/okilum.exe': b'MZexe', 'windows-dist/okilum-sync-supervisor.exe': b'MZhelper',
             f'windows-dist/okilum-{VERSION}-x86_64.zip': b'zip', 'windows-dist/gpui-shaders/src/shaders.hlsl': b'h'}
    if mode != 'payload':
        files.update({'windows-release/release-source.txt': SHA + '\n',
                      'windows-release/releases.beta.json': feed('0.1.25000', VERSION),
                      'windows-release/BeFeast.Okilum-beta-Setup.exe': b'MZsetup',
                      f'windows-release/BeFeast.Okilum-{VERSION}-beta-full.nupkg': b'full',
                      f'windows-release/BeFeast.Okilum-{VERSION}-beta-delta.nupkg': b'delta'})
    files.update(changes)
    return {name: body for name, body in files.items() if body is not None}


def archive(files):
    data = io.BytesIO()
    with zipfile.ZipFile(data, 'w') as out:
        for name, body in files.items():
            out.writestr(name, body)
    return data.getvalue()


def digest(data):
    return 'sha256:' + hashlib.sha256(data).hexdigest()


def job(*steps, conclusion='success', run_id=9, attempt=1):
    return {'name': handoff.JOB, 'conclusion': conclusion, 'run_id': run_id, 'run_attempt': attempt,
            'steps': [{'name': step, 'conclusion': 'success'} for step in steps]}


def hosted(status='completed', conclusion='success', **changes):
    run = {'id': 9, 'path': handoff.WORKFLOW, 'event': 'workflow_dispatch', 'head_branch': BRANCH,
           'head_sha': TOOLING, 'display_title': handoff.label('77', '1', NONCE), 'status': status,
           'conclusion': conclusion, 'run_attempt': 1, 'run_started_at': STARTED,
           'html_url': 'https://github.com/run/9'}
    run.update(changes)
    return run


def uploaded(data, **changes):
    item = {'id': 5, 'name': handoff.ARTIFACT, 'expired': False, 'digest': digest(data),
            'created_at': '2026-11-14T10:20:00Z', 'workflow_run': {'id': 9, 'head_sha': TOOLING}}
    item.update(changes)
    return item


class FakeApi:
    token = 'secret-token'

    def __init__(self, runs=(), jobs=None, artifacts=None):
        self.runs = list(runs) if runs else [[hosted()]]
        self.jobs = jobs if jobs is not None else [job(handoff.BUILD_STEP, handoff.PACK_STEP)]
        self.artifacts = artifacts or []
        self.calls = []

    def log_quota(self):
        pass

    def request(self, path, method='GET', data=None):
        self.calls.append((method, path, data))
        if path.startswith('actions/workflows/') and path.endswith('/dispatches'):
            return None
        if path.startswith('actions/workflows/'):
            runs = self.runs.pop(0) if len(self.runs) > 1 else self.runs[0]
            return {'workflow_runs': runs}
        if '/jobs' in path:
            return {'total_count': len(self.jobs), 'jobs': self.jobs}
        if path.endswith('/artifacts?per_page=100'):
            return {'total_count': len(self.artifacts), 'artifacts': self.artifacts}
        return {}


class Clock:
    def __init__(self):
        self.now = 0

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


def args(**changes):
    values = dict(source=SHA, tooling=TOOLING, build=BUILD, signed='false', archive='false',
                  directory='target', budget=3180)
    values.update(changes)
    return argparse.Namespace(**values)


class InputTests(unittest.TestCase):
    GOOD = {'SOURCE': SHA, 'BUILD': str(BUILD), 'MODE': 'delta', 'PRODUCER': '77', 'ATTEMPT': '1',
            'INVOCATION': NONCE, 'GITHUB_SHA': TOOLING}

    def test_good_inputs_and_recorded_identity(self):
        handoff.check_inputs(self.GOOD)
        self.assertEqual(handoff.identity(self.GOOD), identity())

    def test_malformed_inputs_fail_before_cargo(self):
        for name, value in [('SOURCE', 'main'), ('SOURCE', SHA[:39]), ('BUILD', '0'), ('BUILD', '12a'),
                            ('MODE', 'publish'), ('PRODUCER', ''), ('ATTEMPT', '-1'),
                            ('INVOCATION', NONCE.upper()), ('INVOCATION', NONCE + '0'), ('GITHUB_SHA', '')]:
            with self.subTest(name=name, value=value), self.assertRaises(ValueError):
                handoff.check_inputs({**self.GOOD, name: value})

    def test_tooling_is_the_dispatched_commit_and_product_the_requested_source(self):
        heads = {'tooling': TOOLING, 'product': SHA}
        handoff.check_checkouts(self.GOOD, 'tooling', 'product', head=heads.get)
        for swapped in ({'tooling': SHA, 'product': SHA}, {'tooling': TOOLING, 'product': TOOLING}):
            with self.subTest(swapped=swapped), self.assertRaises(ValueError):
                handoff.check_checkouts(self.GOOD, 'tooling', 'product', head=swapped.get)

    def test_mode_keeps_the_former_step_conditions(self):
        main = {'GITHUB_REF': 'refs/heads/main', 'GITHUB_EVENT_NAME': 'push'}
        self.assertEqual(handoff.mode_for(True, False, main), 'payload')
        self.assertEqual(handoff.mode_for(False, False, main), 'delta')
        self.assertEqual(handoff.mode_for(False, False, {**main, 'GITHUB_EVENT_NAME': 'workflow_dispatch'}), 'delta')
        self.assertEqual(handoff.mode_for(False, True, main), 'pack')
        self.assertEqual(handoff.mode_for(False, False, {**main, 'GITHUB_REF': 'refs/heads/topic'}), 'pack')
        self.assertEqual(handoff.mode_for(False, False, {**main, 'GITHUB_EVENT_NAME': 'pull_request'}), 'pack')

    def test_ref_is_write_once_and_triggers_no_hosted_workflow(self):
        branch = handoff.branch_for(BUILD, SHA, '77', '1', NONCE)
        self.assertEqual(branch, BRANCH)
        for bad in [(0, SHA, '77', '1', NONCE), (BUILD, 'main', '77', '1', NONCE),
                    (BUILD, SHA, '77', '1', 'x' * 32)]:
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                handoff.branch_for(*bad)
        for path in sorted((ROOT / '.github/workflows').glob('*.yml')):
            data = yaml.safe_load(path.read_text()) or {}
            on = data.get('on', data.get(True)) or {}
            push = on.get('push') if isinstance(on, dict) else None
            for pattern in (push or {}).get('branches', []) if isinstance(push, dict) else []:
                prefix = pattern.split('*', 1)[0]
                with self.subTest(workflow=path.name, pattern=pattern):
                    self.assertFalse(branch.startswith(prefix) and prefix, pattern)


WORKFLOW_REVISION = ('', '${{ github.sha }}')


def checkouts(steps):
    """Checkout path -> requested ref ('' is the workflow revision)."""
    return {os.path.normpath((s.get('with') or {}).get('path', '.')): (s.get('with') or {}).get('ref', '')
            for s in steps if 'actions/checkout' in s.get('uses', '')}


def tooling_root(step):
    """The checkout whose windows_handoff.py a run step executes."""
    prefix = re.search(r'(\S*)scripts/releases/windows_handoff\.py', step['run'])[1].strip('"')
    if prefix.startswith('$GITHUB_WORKSPACE'):
        return os.path.normpath(prefix.replace('$GITHUB_WORKSPACE', '.', 1) or '.')
    return os.path.normpath(os.path.join(step.get('working-directory', '.'), prefix or '.'))


class WorkflowTests(unittest.TestCase):
    def setUp(self):
        self.text = (ROOT / handoff.WORKFLOW).read_text()
        self.data = yaml.safe_load(self.text)
        self.on = self.data.get('on', self.data.get(True))
        self.beta = self.data['jobs'][handoff.JOB]
        self.release = yaml.safe_load((ROOT / '.forgejo/workflows/windows-diagnostic.yml').read_text())['jobs']['release']

    def test_dispatch_inputs_match_the_waiter(self):
        inputs = set(self.on['workflow_dispatch']['inputs'])
        self.assertEqual(inputs, {'source', 'build', 'mode', 'producer', 'attempt', 'invocation',
                                  'sign_subject', 'sign_issuer_chain'})
        # The PR lane keeps its push trigger; main pushes seed the beta caches.
        self.assertEqual(self.on['push'], {'branches': ['forgejo-windows-release-pr/**', 'main']})

    def test_run_name_is_the_waiter_label(self):
        expected = handoff.label('{0}', '{1}', '{2}')
        self.assertIn(f"format('{expected}', inputs.producer, inputs.attempt, inputs.invocation)", self.data['run-name'])

    def test_each_beta_has_its_own_concurrency_group(self):
        self.assertIn('inputs.invocation', self.data['concurrency']['group'])
        self.assertIn(self.data['concurrency']['cancel-in-progress'], (False, 'false'))

    def test_beta_job_runs_only_on_dispatch_and_pr_lane_only_on_push(self):
        jobs = self.data['jobs']
        self.assertEqual(self.beta['if'], "github.event_name == 'workflow_dispatch' && inputs.mode != 'seed'")
        for name in ('icon', 'windows-release'):
            self.assertEqual(jobs[name]['if'], "github.event_name == 'push' && github.ref != 'refs/heads/main'")
        steps = [step.get('name') for step in self.beta['steps']]
        for step in (handoff.BUILD_STEP, handoff.PACK_STEP):
            self.assertEqual(steps.count(step), 1)
        upload = self.beta['steps'][-1]
        self.assertEqual(upload['with']['name'], handoff.ARTIFACT)
        self.assertEqual(self.beta['timeout-minutes'] * 60 + 120, handoff.RUN_TIMEOUT)

    def test_hosted_build_holds_no_secrets_and_separates_tooling_from_product(self):
        self.assertNotIn('secrets.', self.text)
        self.assertEqual(checkouts(self.beta['steps']), {'tooling': '', 'product': '${{ inputs.source }}'})
        self.assertEqual(self.beta['steps'][2]['run'],
                         'python3 tooling/scripts/releases/windows_handoff.py inputs --tooling tooling --product product')
        # Every product step runs in the product tree; the artifact comes from it.
        for step in self.beta['steps'][3:-1]:
            if 'run' in step:
                with self.subTest(step=step.get('name')):
                    self.assertEqual(step.get('working-directory'), 'product')
        self.assertTrue(all(line.startswith('product/target/')
                            for line in self.beta['steps'][-1]['with']['path'].split()))

    def test_beta_restores_but_never_saves_the_seeded_caches(self):
        seed = self.data['jobs']['windows-cache']
        deps = [s for s in self.beta['steps'] if 'rust-cache' in s.get('uses', '')]
        self.assertEqual(len(deps), 1)
        self.assertEqual(deps[0]['with'], {'workspaces': 'product -> target', 'shared-key': 'beta-windows-v1',
                                           'save-if': "${{ github.ref == 'refs/heads/main' }}"})
        xwin = [s for s in self.beta['steps'] if 'actions/cache' in s.get('uses', '')]
        self.assertEqual([s['uses'] for s in xwin], ['actions/cache/restore@v4'])
        self.assertEqual(xwin[0]['with'], {'path': 'target/xwin-sdk/', 'key': '${{ steps.xwin-key.outputs.key }}'})
        names = [s.get('name') for s in self.beta['steps']]
        # Restored before the checked prepare and the build.
        self.assertLess(names.index(xwin[0]['name']), names.index('Download and verify the xwin SDK'))
        self.assertLess(names.index(deps[0]['name']), names.index(handoff.BUILD_STEP))
        # The seed writes exactly the entries the beta reads, with the beta's env.
        seed_deps = next(s for s in seed['steps'] if 'rust-cache' in s.get('uses', ''))
        self.assertEqual({k: v for k, v in seed_deps['with'].items() if k != 'save-if'},
                         {'workspaces': 'product -> target', 'shared-key': 'beta-windows-v1'})
        for name in ('CARGO_INCREMENTAL', 'CARGO_BUILD_JOBS', 'XWIN_CACHE_DIR', 'XWIN_ARCH', 'XWIN_VARIANT', 'XWIN_VERSION'):
            self.assertEqual(seed['env'][name], self.beta['env'][name], name)
        self.assertNotIn('CARGO', ''.join(k for k in seed['env'] if k not in self.beta['env']))
        seed_xwin = [s for s in seed['steps'] if 'actions/cache' in s.get('uses', '')]
        self.assertEqual([s['uses'] for s in seed_xwin], ['actions/cache/restore@v4', 'actions/cache/save@v4'])
        self.assertTrue(all(s['with'] == xwin[0]['with'] for s in seed_xwin))
        build = next(s for s in seed['steps'] if s.get('name') == 'Build the Windows beta graph to fill the cache')
        self.assertIn('bash scripts/build-windows-ci.sh', build['run'])
        self.assertEqual(checkouts(seed['steps']), {'product': '${{ inputs.source || github.sha }}'})

    def test_caches_are_seeded_only_on_main_or_an_explicit_seed(self):
        seed = self.data['jobs']['windows-cache']
        self.assertEqual(seed['if'], "(github.event_name == 'push' && github.ref == 'refs/heads/main') || "
                                     "(github.event_name == 'workflow_dispatch' && inputs.mode == 'seed')")
        # Only main saves (#2881); beta refs are never main.
        saves = [s for s in seed['steps'] if s.get('uses') == 'actions/cache/save@v4']
        self.assertTrue(all("github.ref == 'refs/heads/main'" in s['if'] for s in saves))
        prune = self.data['jobs']['windows-cache-prune']
        self.assertIn("github.ref == 'refs/heads/main'", prune['if'])
        script = prune['steps'][0]['run']
        xwin = (ROOT / 'scripts/ci/xwin-cache.py').read_text()
        self.assertIn("key = f'xwin-v2-linux-x86_64-cargo-{VERSION}-'", xwin)
        self.assertIn('for prefix in v0-rust-beta-windows-v1-Linux- xwin-v2-linux-x86_64-cargo-; do', script)
        self.assertNotIn('seed', handoff.MODES)

    def test_handoff_tooling_runs_at_the_workflow_revision(self):
        # An older source (#1040) predates windows_handoff.py; only the product
        # comes from the requested source.
        for name, steps in [('forgejo release', self.release['steps']),
                            ('hosted windows-beta', self.beta['steps'])]:
            with self.subTest(job=name):
                roots = checkouts(steps)
                calls = [s for s in steps if 'windows_handoff.py' in s.get('run', '')]
                self.assertTrue(calls, name)
                for step in calls:
                    with self.subTest(run=step['run'].split('windows_handoff.py')[1][:12]):
                        self.assertIn(roots.get(tooling_root(step)), WORKFLOW_REVISION,
                                      f'{name}: windows_handoff.py runs from the requested source checkout')

    def test_forgejo_waiter_checks_out_only_the_workflow_commit_and_pins_the_product(self):
        self.assertEqual(checkouts(self.release['steps']), {'.': ''})
        step = next(s for s in self.release['steps'] if 'windows_handoff.py run' in s.get('run', ''))
        self.assertEqual(step['env']['PRODUCT_SOURCE'],
                         "${{ (inputs.source != '' || inputs.hourly_source != '') && needs.select.outputs.source || github.sha }}")
        self.assertIn('test "$tooling" = "$GITHUB_SHA"', step['run'])
        self.assertIn('--source "$PRODUCT_SOURCE" --tooling "$tooling"', step['run'])

    def test_forgejo_push_beta_never_queues_behind_another_merge(self):
        data = yaml.safe_load((ROOT / '.forgejo/workflows/windows-diagnostic.yml').read_text())
        self.assertEqual(data['concurrency']['group'],
                         "windows-beta-${{ github.event_name == 'push' && github.sha || inputs.hourly_source || github.run_id }}")
        self.assertIn(data['concurrency']['cancel-in-progress'], (False, 'false'))
        checkout = next(s for s in self.release['steps'] if 'actions/checkout' in s.get('uses', ''))
        self.assertEqual(checkout['with']['fetch-depth'], 200)

    def test_main_beta_hands_publication_a_receipt_instead_of_the_bytes(self):
        uploads = {s['with']['name']: s['if'] for s in self.release['steps'] if 'upload-artifact' in s.get('uses', '')}
        main = "github.ref == 'refs/heads/main' && github.event_name != 'pull_request'"
        self.assertEqual(uploads, {
            'windows-payload': "steps.signing.outputs.on == 'true'",
            'okilum-windows-velopack': "steps.signing.outputs.on != 'true' && (github.ref != 'refs/heads/main' || github.event_name == 'pull_request')",
            'windows-handoff': f"steps.signing.outputs.on != 'true' && {main}"})
        step = next(s for s in self.release['steps'] if 'windows_handoff.py run' in s.get('run', ''))
        self.assertIn('--receipt target/windows-handoff/windows-handoff.json', step['run'])
        receipt = next(s for s in self.release['steps'] if s.get('with', {}).get('name') == 'windows-handoff')
        self.assertEqual(receipt['with']['path'], 'target/windows-handoff/windows-handoff.json')
        publisher = yaml.safe_load((ROOT / '.forgejo/workflows/release-publish.yml').read_text())['jobs']['publish']
        env = next(s for s in publisher['steps'] if 'publication.py publish' in s.get('run', ''))['env']
        for name in ('OKILUM_GITHUB_APP_ID', 'OKILUM_GITHUB_APP_INSTALLATION_ID', 'OKILUM_GITHUB_APP_KEY', 'MIRROR_TOKEN'):
            self.assertIn(name, env)
        self.assertIn('openssl', publisher['steps'][0]['run'])

    def test_forgejo_release_is_a_bridge_waiter(self):
        self.assertEqual(self.release['runs-on'], 'bridge')
        text = yaml.safe_dump(self.release)
        for forbidden in ('cargo', 'build-windows-ci.sh', 'pack.sh', 'R2_SECRET_ACCESS_KEY', 'SCCACHE'):
            self.assertNotIn(forbidden, text)
        step = next(s for s in self.release['steps'] if 'windows_handoff.py run' in s.get('run', ''))
        self.assertLessEqual(step['timeout-minutes'], self.release['timeout-minutes'] - 3)
        self.assertIn('--build "$((5000 + GITHUB_RUN_NUMBER))"', step['run'])


class UnpackTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.directory = Path(self.tmp.name)

    def unpack(self, files, mode='delta', expected=None, data_digest=None):
        data = archive(files)
        return handoff.unpack(data, data_digest or digest(data), expected or identity(mode), self.directory)

    def test_delta_package_and_payload_are_staged_where_the_local_build_put_them(self):
        staged = self.unpack(entries())
        self.assertNotIn(handoff.HANDOFF, staged)
        self.assertFalse((self.directory / handoff.HANDOFF).exists())
        self.assertEqual((self.directory / 'windows-release/release-source.txt').read_text(), SHA + '\n')
        self.assertEqual((self.directory / f'windows-dist/okilum-{VERSION}-x86_64.zip').read_bytes(), b'zip')
        self.assertTrue((self.directory / 'windows-dist/gpui-shaders/src/shaders.hlsl').exists())

    def test_signer_payload_is_staged_without_a_package(self):
        self.unpack(entries('payload'), 'payload')
        self.assertTrue((self.directory / 'windows-dist/okilum.exe').exists())
        self.assertFalse((self.directory / 'windows-release').exists())

    def test_wrong_identity_digest_shape_or_content_fails_closed(self):
        cases = {
            'digest': (entries(), identity(), 'sha256:' + '0' * 64),
            'no digest': (entries(), identity(), 'md5:x'),
            'other build': (entries(), identity(build=BUILD + 1), None),
            'other tooling': (entries(), identity(tooling='e' * 40), None),
            'other invocation': (entries(), identity(invocation='d' * 32), None),
            'other mode': (entries(), identity('pack'), None),
            'traversal': (entries(**{'windows-dist/../x.exe': b'x'}), identity(), None),
            'outside': (entries(**{'other/x': b'x'}), identity(), None),
            'absolute': (entries(**{'/windows-dist/x': b'x'}), identity(), None),
            'missing setup': (entries(**{'windows-release/BeFeast.Okilum-beta-Setup.exe': None}), identity(), None),
            'missing helper': (entries(**{'windows-dist/okilum-sync-supervisor.exe': None}), identity(), None),
            'other source': (entries(**{'windows-release/release-source.txt': 'f' * 40}), identity(), None),
            'feed behind': (entries(**{'windows-release/releases.beta.json': feed('0.1.25000')}), identity(), None),
            'feed ahead': (entries(**{'windows-release/releases.beta.json': feed(VERSION, '0.1.25200')}), identity(), None),
            'package in payload': (entries('payload', **{'windows-release/x.nupkg': b'x'}), identity('payload'), None),
        }
        for name, (files, expected, given) in cases.items():
            with self.subTest(name), self.assertRaises((ValueError, KeyError)):
                data = archive(files)
                handoff.unpack(data, given or digest(data), expected, self.directory / name)


class WaitTests(unittest.TestCase):
    def wait(self, api, mode='delta', budget=3600):
        clock = Clock()
        with contextlib.redirect_stdout(io.StringIO()):
            result = handoff.wait(api, BRANCH, TOOLING, mode, clock=clock, sleep=clock.sleep,
                                  deadline=budget, log=lambda *a, **k: print(*a))
        return result

    def test_success_needs_the_build_and_pack_steps_of_that_attempt(self):
        api = FakeApi(runs=[[], [hosted('queued', None)], [hosted()]])
        self.assertEqual(self.wait(api)['id'], 9)
        self.assertIn(('GET', 'actions/runs/9/attempts/1/jobs?per_page=100', None), api.calls)
        with self.assertRaises(ValueError):
            self.wait(FakeApi(jobs=[job(handoff.BUILD_STEP)]))
        # A signer payload has no pack step.
        self.assertEqual(self.wait(FakeApi(jobs=[job(handoff.BUILD_STEP)]), mode='payload')['id'], 9)

    def test_a_rerun_is_accepted_only_through_its_own_attempt(self):
        api = FakeApi(runs=[[hosted(run_attempt=2)]], jobs=[job(handoff.BUILD_STEP, handoff.PACK_STEP, attempt=2)])
        self.assertEqual(self.wait(api)['run_attempt'], 2)
        self.assertIn(('GET', 'actions/runs/9/attempts/2/jobs?per_page=100', None), api.calls)
        for jobs in ([job(handoff.BUILD_STEP, handoff.PACK_STEP, attempt=1)],
                     [job(handoff.BUILD_STEP, handoff.PACK_STEP, run_id=10, attempt=2)]):
            with self.subTest(jobs=jobs), self.assertRaises(ValueError):
                self.wait(FakeApi(runs=[[hosted(run_attempt=2)]], jobs=jobs))

    def test_failed_or_malformed_hosted_build_is_not_accepted(self):
        self.assertIsNone(self.wait(FakeApi(runs=[[hosted(conclusion='failure')]])))
        for bad in (hosted(run_attempt=0), hosted(run_attempt='1'), hosted(run_started_at=None)):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                self.wait(FakeApi(runs=[[bad]]))

    def test_only_one_run_of_this_ref_at_the_tooling_commit_is_accepted(self):
        other = [hosted(head_branch=BRANCH + 'x'), hosted(head_sha=SHA), hosted(event='push'),
                 hosted(path='.github/workflows/forgejo-linux.yml')]
        with self.assertRaises(ValueError):
            self.wait(FakeApi(runs=[other]), budget=120)
        with self.assertRaises(ValueError):
            self.wait(FakeApi(runs=[[hosted(), hosted(id=10)]]))

    def test_deadline_cancels_the_hosted_run(self):
        api = FakeApi(runs=[[hosted('in_progress', None)]])
        with self.assertRaises(ValueError):
            self.wait(api, budget=600)
        self.assertIn(('POST', 'actions/runs/9/cancel', None), api.calls)


class StageTests(unittest.TestCase):
    def test_artifact_must_come_from_the_accepted_attempt(self):
        data = archive(entries())
        for name, item in {
                'other run': uploaded(data, workflow_run={'id': 10, 'head_sha': TOOLING}),
                'other commit': uploaded(data, workflow_run={'id': 9, 'head_sha': SHA}),
                'earlier attempt': uploaded(data, created_at='2026-11-14T09:59:59Z'),
                'no time': uploaded(data, created_at=None),
                'expired': uploaded(data, expired=True),
                'no digest': uploaded(data, digest=None)}.items():
            with self.subTest(name), tempfile.TemporaryDirectory() as tmp, self.assertRaises(ValueError):
                handoff.stage(FakeApi(artifacts=[item]), hosted(), identity(), tmp, lambda _: data)
        with tempfile.TemporaryDirectory() as tmp, self.assertRaises(ValueError):
            handoff.stage(FakeApi(artifacts=[uploaded(data), uploaded(data, id=6)]), hosted(), identity(), tmp,
                          lambda _: data)


class RunTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.directory = Path(self.tmp.name) / 'target'
        self.receipt = Path(self.tmp.name) / 'receipt/windows-handoff.json'
        self.pushed = []
        self.fetched = []

    def push(self, branch, sha, token):
        self.pushed.append((branch, sha))

    def go(self, api, files, env=ENV, **changes):
        data = archive(files)
        api.artifacts = [uploaded(data)]
        clock = Clock()

        def fetch(artifact_id):
            self.fetched.append(artifact_id)
            return data
        with contextlib.redirect_stdout(io.StringIO()):
            return handoff.run(args(directory=str(self.directory), receipt=str(self.receipt), **changes),
                               dict(env), api=api, fetch=fetch, push=self.push, clock=clock,
                               sleep=clock.sleep, invocation=NONCE)

    def test_pushes_the_tooling_commit_dispatches_once_on_its_ref_and_writes_a_receipt(self):
        api = FakeApi()
        self.assertEqual(self.go(api, entries()), 0)
        # The write-once ref is the workflow commit; the product source is an input.
        self.assertEqual(self.pushed, [(BRANCH, TOOLING)])
        dispatches = [c for c in api.calls if c[1].endswith('/dispatches')]
        self.assertEqual(dispatches, [('POST', 'actions/workflows/forgejo-windows-release.yml/dispatches', {
            'ref': BRANCH, 'inputs': {
                'source': SHA, 'build': str(BUILD), 'mode': 'delta', 'producer': '77', 'attempt': '1',
                'invocation': NONCE, 'sign_subject': ENV['OKILUM_WINDOWS_SIGN_SUBJECT'],
                'sign_issuer_chain': ENV['OKILUM_WINDOWS_SIGN_ISSUER_CHAIN']}})])
        self.assertEqual(api.calls[-1], ('DELETE', 'git/refs/heads/' + BRANCH, None))
        # A main beta copies nothing: publication fetches and re-verifies the bytes.
        self.assertEqual(self.fetched, [])
        self.assertFalse(self.directory.exists())
        data = json.loads(self.receipt.read_text())
        self.assertEqual(data, {'schema': 1, 'github_run_id': 9, 'github_run_attempt': 1, 'branch': BRANCH,
                                'artifact_id': 5, 'digest': api.artifacts[0]['digest'], **identity()})
        self.assertEqual(handoff.check_receipt(data, 77, BUILD), identity())

    def test_qa_build_is_staged_for_the_forgejo_artifact(self):
        api = FakeApi()
        env = {**ENV, 'GITHUB_REF': 'refs/heads/topic', 'GITHUB_EVENT_NAME': 'workflow_dispatch'}
        self.assertEqual(self.go(api, entries('pack'), env=env), 0)
        self.assertEqual(self.fetched, [5])
        self.assertTrue((self.directory / f'windows-release/BeFeast.Okilum-{VERSION}-beta-full.nupkg').exists())
        self.assertFalse(self.receipt.exists())

    def test_historical_source_builds_with_current_tooling(self):
        old = 'f' * 40
        api = FakeApi(runs=[[hosted(head_branch=BRANCH.replace(SHA, old))]])
        files = entries('pack', **{handoff.HANDOFF: json.dumps(identity('pack', source=old)),
                                   'windows-release/release-source.txt': old})
        self.assertEqual(self.go(api, files, source=old, archive='true'), 0)
        branch = BRANCH.replace(SHA, old)
        self.assertEqual(self.pushed, [(branch, TOOLING)])
        dispatch = next(c for c in api.calls if c[1].endswith('/dispatches'))[2]
        self.assertEqual((dispatch['ref'], dispatch['inputs']['source'], dispatch['inputs']['mode']),
                         (branch, old, 'pack'))
        self.assertEqual(json.loads(self.receipt.read_text())['source'], old)

    def test_signed_build_stages_the_payload_for_the_signer(self):
        api = FakeApi(jobs=[job(handoff.BUILD_STEP)])
        self.assertEqual(self.go(api, entries('payload'), signed='true'), 0)
        self.assertEqual(next(c for c in api.calls if c[1].endswith('/dispatches'))[2]['inputs']['mode'], 'payload')
        self.assertEqual(self.fetched, [5])
        self.assertTrue((self.directory / 'windows-dist/okilum.exe').exists())
        self.assertFalse(self.receipt.exists())

    def test_branch_build_packs_without_delta(self):
        api = FakeApi()
        env = {**ENV, 'GITHUB_REF': 'refs/heads/topic', 'GITHUB_EVENT_NAME': 'workflow_dispatch'}
        self.assertEqual(self.go(api, entries('pack'), env=env), 0)

    def test_malformed_identities_push_nothing(self):
        for changes in ({'source': 'main'}, {'tooling': 'HEAD'}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.go(FakeApi(), entries(), **changes)
        self.assertEqual(self.pushed, [])

    def test_failed_build_publishes_nothing_and_still_removes_the_ref(self):
        api = FakeApi(runs=[[hosted(conclusion='failure')]])
        self.assertEqual(self.go(api, entries()), 1)
        self.assertFalse(self.directory.exists())
        self.assertEqual(api.calls[-1][0], 'DELETE')
        api = FakeApi()
        qa = {**ENV, 'GITHUB_REF': 'refs/heads/topic', 'GITHUB_EVENT_NAME': 'workflow_dispatch'}
        with self.assertRaises(ValueError):
            self.go(api, entries('pack', **{'windows-release/release-source.txt': 'f' * 40}), env=qa)
        self.assertEqual(api.calls[-1][0], 'DELETE')
        # An artifact without a digest is refused before any receipt exists.
        api = FakeApi()
        api_artifacts = uploaded(archive(entries()), digest=None)
        with self.assertRaises(ValueError):
            handoff.select_artifact(FakeApi(artifacts=[api_artifacts]), hosted(), identity())
        self.assertFalse(self.receipt.exists())


class ReceiptTests(unittest.TestCase):
    def setUp(self):
        self.data = archive(entries())
        self.receipt = {'schema': 1, 'github_run_id': 9, 'github_run_attempt': 1, 'branch': BRANCH,
                        'artifact_id': 5, 'digest': digest(self.data), **identity()}

    def publish(self, receipt=None, run=None, artifacts=None, data=None, jobs=None):
        api = FakeApi(runs=[[]], jobs=jobs, artifacts=artifacts or [uploaded(self.data)])
        api.request = self.routed(api, run or hosted())
        with tempfile.TemporaryDirectory() as tmp:
            source = handoff.fetch_published(receipt or self.receipt, 77, BUILD, tmp, api=api,
                                             fetch=lambda _: data or self.data, sleep=lambda _: None)
            root = Path(tmp)
            return source, sorted(p.relative_to(root).as_posix() for p in root.rglob('*')
                                  if p.is_file() and not p.relative_to(root).as_posix().startswith('hosted/'))

    @staticmethod
    def routed(api, run):
        original = api.request

        def request(path, method='GET', data=None):
            if path == f"actions/runs/{run['id']}" or path == 'actions/runs/9':
                api.calls.append((method, path, data))
                return run
            return original(path, method, data)
        return request

    def test_publication_lays_out_exactly_what_the_forgejo_copies_held(self):
        source, files = self.publish()
        self.assertEqual(source, SHA)
        self.assertEqual(files, ['portable/okilum-0.1.25123-x86_64.zip',
                                 'windows/BeFeast.Okilum-0.1.25123-beta-delta.nupkg',
                                 'windows/BeFeast.Okilum-0.1.25123-beta-full.nupkg',
                                 'windows/BeFeast.Okilum-beta-Setup.exe',
                                 'windows/release-source.txt', 'windows/releases.beta.json'])

    def test_receipt_of_another_run_build_mode_or_shape_is_refused(self):
        for name, change in {'run': {'producer': '78'}, 'build': {'build': BUILD + 1},
                             'payload': {'mode': 'payload'}, 'schema': {'schema': 2},
                             'ref': {'branch': BRANCH + 'x'}, 'digest': {'digest': 'md5:x'},
                             'extra': {'url': 'https://example'}, 'id': {'artifact_id': '5'}}.items():
            with self.subTest(name), self.assertRaises(ValueError):
                handoff.check_receipt({**self.receipt, **change}, 77, BUILD)

    def test_hosted_evidence_that_differs_from_the_receipt_is_refused(self):
        cases = {
            'other run head': dict(run=hosted(head_sha=SHA)),
            'other ref': dict(run=hosted(head_branch=BRANCH + 'x')),
            'other attempt': dict(run=hosted(run_attempt=2), jobs=[job(handoff.BUILD_STEP, handoff.PACK_STEP, attempt=2)]),
            'failed run': dict(run=hosted(conclusion='failure')),
            'no pack step': dict(jobs=[job(handoff.BUILD_STEP)]),
            'other artifact': dict(artifacts=[uploaded(self.data, id=6)]),
            'other digest': dict(artifacts=[uploaded(self.data, digest='sha256:' + '0' * 64)]),
            'tampered bytes': dict(data=archive(entries(**{'windows-release/release-source.txt': 'f' * 40}))),
        }
        for name, kwargs in cases.items():
            with self.subTest(name), self.assertRaises(ValueError):
                self.publish(**kwargs)

    def test_publication_takes_the_receipt_or_the_forgejo_copies_never_both(self):
        import importlib
        import sys
        sys.path.insert(0, str(Path(__file__).parent))
        publication = importlib.import_module('publication')
        receipt = {'name': 'windows-handoff', 'expired': False, 'run_id': 77, 'id': 1}
        copies = [{'name': 'okilum-windows-velopack', 'expired': False, 'run_id': 77, 'id': 2},
                  {'name': 'windows-portable', 'expired': False, 'run_id': 77, 'id': 3}]
        self.assertEqual(publication.windows_receipt([receipt], 77), receipt)
        self.assertIsNone(publication.windows_receipt(copies, 77))
        self.assertIsNone(publication.windows_receipt([{**receipt, 'run_id': 78}], 77))
        for artifacts in ([receipt, copies[0]], [receipt, {**receipt, 'id': 4}]):
            with self.subTest(artifacts=artifacts), self.assertRaises(ValueError):
                publication.windows_receipt(artifacts, 77)


class PublicFeedTests(unittest.TestCase):
    def test_previous_reads_the_latest_full_package_from_the_public_root(self):
        package = b'full-package'
        sha = hashlib.sha256(package).hexdigest().upper()
        current = json.dumps({'Assets': [
            {'PackageId': 'BeFeast.Okilum', 'Version': '0.1.25000', 'Type': 'Full', 'Size': len(package),
             'FileName': 'BeFeast.Okilum-0.1.25000-beta-full.nupkg', 'SHA256': sha},
            {'PackageId': 'BeFeast.Okilum', 'Version': '0.1.25000', 'Type': 'Delta', 'Size': 1,
             'FileName': 'BeFeast.Okilum-0.1.25000-beta-delta.nupkg', 'SHA256': '0'}]}).encode()
        urls = []

        class Response(io.BytesIO):
            def __enter__(self):
                return self

            def __exit__(self, *exc):
                return False

        def opener(request, timeout):
            urls.append(request.full_url)
            name = request.full_url.rsplit('/', 1)[1]
            return Response({'releases.beta.json': current,
                             'BeFeast.Okilum-0.1.25000-beta-full.nupkg': package}[name])

        root = handoff.CHANNEL['public_root'] + '/' + handoff.CHANNEL['prefix'] + '/beta/'
        with tempfile.TemporaryDirectory() as tmp:
            handoff.previous(tmp, handoff.PublicFeed(opener=opener))
            self.assertEqual(sorted(p.name for p in Path(tmp).iterdir()),
                             ['BeFeast.Okilum-0.1.25000-beta-full.nupkg', 'releases.beta.json'])
        self.assertEqual(urls, [root + 'releases.beta.json', root + 'BeFeast.Okilum-0.1.25000-beta-full.nupkg'])

    def test_missing_feed_is_no_delta_base_and_writes_are_refused(self):
        def opener(request, timeout):
            raise urllib.error.HTTPError(request.full_url, 404, 'Not Found', {}, None)

        feed = handoff.PublicFeed(opener=opener)
        self.assertIsNone(feed.call('GET', 'okilum/windows/beta/releases.beta.json'))
        with self.assertRaises(ValueError):
            feed.call('PUT', 'okilum/windows/beta/releases.beta.json')


if __name__ == '__main__':
    unittest.main()
