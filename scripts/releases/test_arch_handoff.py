"""Hosted Arch beta handoff (#2927): exact source, Forgejo build number, fail closed."""
import argparse
import contextlib
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import yaml

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('arch_handoff', Path(__file__).with_name('arch_handoff.py'))
handoff = importlib.util.module_from_spec(spec)
spec.loader.exec_module(handoff)
SOURCE = 'a' * 40
TOOLING = 'c' * 40
NONCE = 'b' * 32
STARTED = '2026-11-14T10:00:00Z'


def archive(entries):
    data = io.BytesIO()
    with zipfile.ZipFile(data, 'w') as out:
        for name, body in entries.items():
            out.writestr(name, body)
    return data.getvalue()


def digest(data):
    return 'sha256:' + hashlib.sha256(data).hexdigest()


class FakeApi:
    token = 'secret-token'

    def __init__(self, runs=(), jobs=(), artifacts=()):
        self.runs, self.jobs, self.artifacts, self.calls = list(runs), list(jobs), list(artifacts), []

    def log_quota(self):
        pass

    def request(self, path, method='GET', data=None):
        self.calls.append((method, path))
        if path.startswith('actions/runs?'):
            return {'workflow_runs': self.runs}
        if '/attempts/' in path:
            return {'jobs': self.jobs}
        if '/artifacts' in path:
            return {'total_count': len(self.artifacts), 'artifacts': self.artifacts}
        return {}


class RefTests(unittest.TestCase):
    def test_unpinned_ref_round_trip_names_build_source_and_harness(self):
        for harness in ('0', '1'):
            branch = handoff.branch_for(25123, SOURCE, SOURCE, harness, '9', '1', NONCE)
            self.assertTrue(branch.startswith(handoff.PREFIX[(False, harness)] + '/25123-' + SOURCE))
            self.assertEqual(handoff.parse_ref(branch, SOURCE), (25123, harness, SOURCE))

    def test_historical_source_uses_a_pinned_ref_pushed_at_the_tooling_commit(self):
        for harness in ('0', '1'):
            branch = handoff.branch_for(25123, SOURCE, TOOLING, harness, '9', '1', NONCE)
            self.assertTrue(branch.startswith(handoff.PREFIX[(True, harness)] + '/25123-' + SOURCE))
            # The hosted job runs the tooling commit's workflow and builds SOURCE.
            self.assertEqual(handoff.parse_ref(branch, TOOLING), (25123, harness, SOURCE))
            # A pinned ref can never claim to be the pushed commit, nor the reverse.
            with self.assertRaises(ValueError):
                handoff.parse_ref(branch, SOURCE)
        unpinned = handoff.branch_for(25123, SOURCE, SOURCE, '0', '9', '1', NONCE)
        with self.assertRaises(ValueError):
            handoff.parse_ref(unpinned, TOOLING)

    def test_malformed_refs_are_rejected(self):
        branch = handoff.branch_for(25123, SOURCE, SOURCE, '0', '9', '1', NONCE)
        for ref, sha in [('main', SOURCE), (branch + 'x', SOURCE), (branch.replace('25123', '0'), SOURCE),
                         ('forgejo-arch-pr/1-' + SOURCE, SOURCE), (branch, 'short')]:
            with self.subTest(ref=ref), self.assertRaises(ValueError):
                handoff.parse_ref(ref, sha)

    def test_workflow_prefixes_match_the_hosted_trigger(self):
        hosted = yaml.safe_load((ROOT / handoff.WORKFLOW).read_text())
        on = hosted.get('on', hosted.get(True))
        self.assertEqual(sorted(on['push']['branches']), sorted(p + '/**' for p in handoff.PREFIX.values()))


class StageTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.directory = Path(self.tmp.name) / 'dist/arch'
        self.branch = handoff.branch_for(25123, SOURCE, TOOLING, '0', '9', '1', NONCE)
        self.data = self.good()

    def good(self, build=25123, source=SOURCE):
        return archive({handoff.package_name(build): b'pkg', handoff.SOURCE_FILE: source + '\n'})

    def run_record(self, **changes):
        run = dict(id=5, run_attempt=2, head_branch=self.branch, head_sha=TOOLING, event='push',
                   path=handoff.WORKFLOW, status='completed', conclusion='success', run_started_at=STARTED)
        run.update(changes)
        return run

    def jobs(self, run_id=5, attempt=2, build='success', validate='success'):
        return [dict(name='arch-release', run_id=run_id, run_attempt=attempt, conclusion='success', steps=[
            dict(name=handoff.BUILD_STEP, conclusion=build), dict(name=handoff.VALIDATE_STEP, conclusion=validate)])]

    def item(self, **changes):
        item = dict(id=8, name='arch-release', expired=False, digest=digest(self.data),
                    created_at='2026-11-14T10:20:00Z', workflow_run=dict(id=5, head_sha=TOOLING))
        item.update(changes)
        return item

    def stage(self, runs=None, jobs=None, items=None):
        api = FakeApi([self.run_record()] if runs is None else runs, self.jobs() if jobs is None else jobs,
                      [self.item()] if items is None else items)
        return handoff.stage(api, self.branch, TOOLING, SOURCE, 25123, self.directory, lambda _: self.data), api

    def test_exact_package_and_source_record_are_staged_from_the_validated_attempt(self):
        path, api = self.stage()
        self.assertEqual(path.name, 'okilum-0.1.25123-1-x86_64.pkg.tar.zst')
        self.assertEqual((self.directory / 'release-source.txt').read_text().strip(), SOURCE)
        self.assertIn(('GET', 'actions/runs/5/attempts/2/jobs?per_page=100'), api.calls)

    def test_missing_or_malformed_digest_fails_closed(self):
        for value in (None, '', 'sha256:', 'sha1:' + '0' * 40, digest(self.data).upper()):
            with self.subTest(digest=value), self.assertRaises(ValueError):
                self.stage(items=[self.item(digest=value)])
        with self.assertRaises(ValueError):
            handoff.unpack(self.data, None, 25123, SOURCE, self.directory)

    def test_wrong_build_source_digest_or_entries_fail_closed(self):
        cases = [
            self.good(build=25122),
            self.good(source='d' * 40),
            archive({handoff.package_name(25123): b'pkg', handoff.SOURCE_FILE: SOURCE, 'extra': b''}),
            archive({'../' + handoff.package_name(25123): b'pkg', handoff.SOURCE_FILE: SOURCE}),
        ]
        for data in cases:
            with self.subTest(), self.assertRaises(ValueError):
                handoff.unpack(data, digest(data), 25123, SOURCE, self.directory)
        with self.assertRaises(ValueError):
            handoff.unpack(self.data, 'sha256:' + '0' * 64, 25123, SOURCE, self.directory)

    def test_staging_is_bound_to_the_single_run_and_its_validated_attempt(self):
        bad_runs = [[], [self.run_record(), self.run_record(id=6)], [self.run_record(conclusion='failure')],
                    [self.run_record(status='in_progress')], [self.run_record(run_attempt=None)],
                    [self.run_record(run_started_at=None)], [self.run_record(head_sha=SOURCE)]]
        for runs in bad_runs:
            with self.subTest(runs=runs), self.assertRaises(ValueError):
                self.stage(runs=runs)
        bad_jobs = [self.jobs(attempt=1), self.jobs(run_id=6), self.jobs(build='skipped'),
                    self.jobs(validate='failure'), []]
        for jobs in bad_jobs:
            with self.subTest(jobs=jobs), self.assertRaises(ValueError):
                self.stage(jobs=jobs)
        bad_items = [[], [self.item(), self.item(id=9)], [self.item(expired=True)],
                     [self.item(workflow_run=dict(id=6, head_sha=TOOLING))],
                     [self.item(workflow_run=dict(id=5, head_sha=SOURCE))],
                     # Uploaded before the accepted attempt started: an earlier attempt's bytes.
                     [self.item(created_at='2026-11-14T09:59:59Z')], [self.item(created_at=None)]]
        for items in bad_items:
            with self.subTest(items=items), self.assertRaises(ValueError):
                self.stage(items=items)


class RunTests(unittest.TestCase):
    def invoke(self, result, source=SOURCE, tooling=SOURCE):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        args = argparse.Namespace(source=source, tooling=tooling, build=25123, harness='0',
                                  directory=str(Path(tmp.name) / 'arch'), budget=3180)
        seen = {}
        api = FakeApi()

        def stage(api_, branch, pushed, product, build, directory, fetch):
            seen['staged'] = (branch, pushed, product, build)
            return directory / handoff.package_name(build)
        with patch.dict(os.environ, {'GITHUB_RUN_ID': '9', 'GITHUB_RUN_ATTEMPT': '2'}), \
                patch.object(handoff.transport, 'push_head', side_effect=lambda b, s, t: seen.update(branch=b, sha=s)), \
                patch.object(handoff.transport, 'wait_for_run', return_value=(result, 'msg')) as wait, \
                patch.object(handoff, 'stage', side_effect=stage), contextlib.redirect_stdout(io.StringIO()):
            code = handoff.run(args, api=api, fetch=lambda _: b'', clock=lambda: 100.0)
        return code, seen, wait, api

    def test_success_waits_for_build_and_validation_then_stages_and_cleans_up(self):
        code, seen, wait, api = self.invoke('success')
        self.assertEqual(code, 0)
        self.assertTrue(seen['branch'].startswith('forgejo-arch-release-pr/25123-' + SOURCE + '-9-2-'))
        self.assertEqual(seen['sha'], SOURCE)
        kwargs = wait.call_args.kwargs
        self.assertEqual((kwargs['workflow'], kwargs['job_name'], kwargs['build_step']),
                         (handoff.WORKFLOW, 'arch-release', handoff.VALIDATE_STEP))
        self.assertEqual(kwargs['also_required'], (('arch-release', handoff.BUILD_STEP),))
        self.assertEqual(kwargs['deadline'], 100.0 + 3180)
        self.assertEqual(seen['staged'], (seen['branch'], SOURCE, SOURCE, 25123))
        self.assertIn(('DELETE', 'git/refs/heads/' + seen['branch']), api.calls)

    def test_historical_source_pushes_tooling_and_stages_the_pinned_product(self):
        code, seen, wait, _ = self.invoke('success', source=SOURCE, tooling=TOOLING)
        self.assertEqual(code, 0)
        self.assertEqual(seen['sha'], TOOLING, 'GitHub must run the current workflow commit')
        self.assertTrue(seen['branch'].startswith('forgejo-arch-pinned-pr/25123-' + SOURCE + '-9-2-'))
        self.assertEqual(wait.call_args.args[1:], (seen['branch'], TOOLING))
        self.assertEqual(seen['staged'], (seen['branch'], TOOLING, SOURCE, 25123))

    def test_failed_or_unavailable_hosted_build_stages_nothing(self):
        for result in ('failure', 'unavailable'):
            with self.subTest(result=result):
                code, seen, _, api = self.invoke(result)
                self.assertEqual(code, 1)
                self.assertNotIn('staged', seen)
                self.assertIn(('DELETE', 'git/refs/heads/' + seen['branch']), api.calls)


class WorkflowTests(unittest.TestCase):
    def test_forgejo_release_keeps_tooling_from_the_workflow_commit(self):
        workflow = yaml.safe_load((ROOT / '.forgejo/workflows/linux-release.yml').read_text())
        job = workflow['jobs']['release']
        self.assertEqual(job['runs-on'], 'bridge')
        self.assertNotIn('container', job)
        self.assertLessEqual(job['timeout-minutes'], 60)
        checkout = job['steps'][0]
        # The checkout is the workflow commit; a historical source would lack the handoff.
        self.assertNotIn('ref', checkout['with'])
        self.assertEqual(checkout['with']['fetch-depth'], 0)
        step = next(s for s in job['steps'] if 'arch_handoff.py run' in s.get('run', ''))
        self.assertEqual(step['env']['PRODUCT_SOURCE'],
                         "${{ (inputs.source != '' || inputs.hourly_source != '') && needs.select.outputs.source || github.sha }}")
        self.assertIn('--source "$PRODUCT_SOURCE" --tooling "$tooling"', step['run'])
        self.assertIn('test "$tooling" = "$GITHUB_SHA"', step['run'])
        self.assertIn('--build "$((5000 + GITHUB_RUN_NUMBER))"', step['run'])
        self.assertIn('--directory dist/arch', step['run'])
        self.assertGreaterEqual(step['timeout-minutes'] * 60, 3180 + 120)
        uploads = {s['with']['name']: s for s in job['steps'] if 'upload-artifact' in s.get('uses', '')}
        self.assertEqual(set(uploads), {'arch-publication', 'arch-qa'})
        self.assertIn('dist/arch/release-source.txt', uploads['arch-publication']['with']['path'])

    def test_hosted_build_runs_tooling_and_builds_the_ref_product(self):
        text = (ROOT / handoff.WORKFLOW).read_text()
        steps = yaml.safe_load(text)['jobs']['arch-release']['steps']
        names = [s.get('name') for s in steps]
        for name in (handoff.BUILD_STEP, handoff.VALIDATE_STEP):
            self.assertEqual(names.count(name), 1)
        checkouts = [s for s in steps if str(s.get('uses', '')).startswith('actions/checkout@')]
        self.assertEqual([c['with']['path'] for c in checkouts], ['tooling', 'product'])
        self.assertNotIn('ref', checkouts[0]['with'])
        self.assertEqual(checkouts[1]['with']['ref'], '${{ steps.build.outputs.source }}')
        self.assertIn('tooling/scripts/releases/arch_handoff.py ref', text)
        for name in (handoff.BUILD_STEP, handoff.VALIDATE_STEP):
            self.assertEqual(next(s for s in steps if s.get('name') == name)['working-directory'], 'product')
        self.assertIn('OKILUM_BUILD_VERSION="$BUILD"', text)
        self.assertIn('product/dist/arch/release-source.txt', text)
        for forbidden in ('${{ secrets.', 'SCCACHE', 'actions/cache'):
            self.assertNotIn(forbidden, text)


if __name__ == '__main__':
    unittest.main()
