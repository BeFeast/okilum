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
SHA = 'a' * 40
NONCE = 'b' * 32


def archive(entries):
    data = io.BytesIO()
    with zipfile.ZipFile(data, 'w') as out:
        for name, body in entries.items():
            out.writestr(name, body)
    return data.getvalue()


class FakeApi:
    token = 'secret-token'

    def __init__(self, runs, artifacts):
        self.runs, self.artifacts, self.calls = runs, artifacts, []

    def log_quota(self):
        pass

    def request(self, path, method='GET', data=None):
        self.calls.append((method, path))
        if path.startswith('actions/workflows/'):
            return {'workflow_runs': self.runs}
        if '/artifacts' in path:
            return {'total_count': len(self.artifacts), 'artifacts': self.artifacts}
        return {}


class RefTests(unittest.TestCase):
    def test_ref_round_trip_names_build_source_and_harness(self):
        for harness in ('0', '1'):
            branch = handoff.branch_for(25123, SHA, harness, '9', '1', NONCE)
            self.assertTrue(branch.startswith(handoff.PREFIX[harness] + '/25123-' + SHA))
            self.assertEqual(handoff.parse_ref(branch, SHA), (25123, harness))

    def test_ref_for_another_source_or_shape_is_rejected(self):
        branch = handoff.branch_for(25123, SHA, '0', '9', '1', NONCE)
        for ref, sha in [(branch, 'c' * 40), ('main', SHA), (branch + 'x', SHA),
                         (branch.replace('25123', '0'), SHA), ('forgejo-arch-pr/1-' + SHA, SHA)]:
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

    def good(self, build=25123, source=SHA):
        return archive({handoff.package_name(build): b'pkg', handoff.SOURCE_FILE: source + '\n'})

    def test_exact_package_and_source_record_are_staged(self):
        data = self.good()
        path = handoff.unpack(data, 'sha256:' + hashlib.sha256(data).hexdigest(), 25123, SHA, self.directory)
        self.assertEqual(path.name, 'okilum-0.1.25123-1-x86_64.pkg.tar.zst')
        self.assertEqual((self.directory / 'release-source.txt').read_text().strip(), SHA)

    def test_wrong_build_source_digest_or_extra_entries_fail_closed(self):
        cases = [
            (self.good(build=25122), None),
            (self.good(source='c' * 40), None),
            (self.good(), 'sha256:' + '0' * 64),
            (archive({handoff.package_name(25123): b'pkg', handoff.SOURCE_FILE: SHA, 'extra': b''}), None),
            (archive({'../' + handoff.package_name(25123): b'pkg', handoff.SOURCE_FILE: SHA}), None),
        ]
        for data, digest in cases:
            with self.subTest(digest=digest), self.assertRaises(ValueError):
                handoff.unpack(data, digest, 25123, SHA, self.directory)

    def test_run_must_be_the_single_successful_run_of_this_ref(self):
        branch = handoff.branch_for(25123, SHA, '0', '9', '1', NONCE)
        run = dict(id=5, head_branch=branch, head_sha=SHA, path=handoff.WORKFLOW, conclusion='success')
        item = dict(id=8, name='arch-release', expired=False)
        for runs in ([], [dict(run, conclusion='failure')], [run, dict(run, id=6)], [dict(run, head_sha='c' * 40)]):
            with self.subTest(runs=runs), self.assertRaises(ValueError):
                handoff.stage(FakeApi(runs, [item]), branch, SHA, 25123, self.directory, lambda _: self.good())
        for items in ([], [dict(item, expired=True)], [item, dict(item, id=9)]):
            with self.subTest(items=items), self.assertRaises(ValueError):
                handoff.stage(FakeApi([run], items), branch, SHA, 25123, self.directory, lambda _: self.good())
        handoff.stage(FakeApi([run], [item]), branch, SHA, 25123, self.directory, lambda _: self.good())


class RunTests(unittest.TestCase):
    def invoke(self, result):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        args = argparse.Namespace(source=SHA, build=25123, harness='0', directory=str(Path(tmp.name) / 'arch'), budget=3180)
        pushed = {}
        api = FakeApi([], [])

        def stage(api_, branch, source, build, directory, fetch):
            pushed['staged'] = (branch, source, build)
            return directory / handoff.package_name(build)
        with patch.dict(os.environ, {'GITHUB_RUN_ID': '9', 'GITHUB_RUN_ATTEMPT': '2'}), \
                patch.object(handoff.transport, 'push_head', side_effect=lambda b, s, t: pushed.update(branch=b, sha=s)), \
                patch.object(handoff.transport, 'wait_for_run', return_value=(result, 'msg')) as wait, \
                patch.object(handoff, 'stage', side_effect=stage), contextlib.redirect_stdout(io.StringIO()):
            code = handoff.run(args, api=api, fetch=lambda _: b'', clock=lambda: 100.0)
        return code, pushed, wait, api

    def test_success_waits_for_build_and_validation_then_stages_and_cleans_up(self):
        code, pushed, wait, api = self.invoke('success')
        self.assertEqual(code, 0)
        self.assertTrue(pushed['branch'].startswith('forgejo-arch-release-pr/25123-' + SHA + '-9-2-'))
        self.assertEqual(handoff.parse_ref(pushed['branch'], SHA), (25123, '0'))
        kwargs = wait.call_args.kwargs
        self.assertEqual((kwargs['workflow'], kwargs['job_name'], kwargs['build_step']),
                         (handoff.WORKFLOW, 'arch-release', handoff.VALIDATE_STEP))
        self.assertEqual(kwargs['also_required'], (('arch-release', handoff.BUILD_STEP),))
        self.assertEqual(kwargs['deadline'], 100.0 + 3180)
        self.assertEqual(pushed['staged'], (pushed['branch'], SHA, 25123))
        self.assertIn(('DELETE', 'git/refs/heads/' + pushed['branch']), api.calls)

    def test_failed_or_unavailable_hosted_build_stages_nothing(self):
        for result in ('failure', 'unavailable'):
            with self.subTest(result=result):
                code, pushed, _, api = self.invoke(result)
                self.assertEqual(code, 1)
                self.assertNotIn('staged', pushed)
                self.assertIn(('DELETE', 'git/refs/heads/' + pushed['branch']), api.calls)


class WorkflowTests(unittest.TestCase):
    def test_forgejo_release_is_a_bridge_waiter_with_the_forgejo_build_number(self):
        workflow = yaml.safe_load((ROOT / '.forgejo/workflows/linux-release.yml').read_text())
        job = workflow['jobs']['release']
        self.assertEqual(job['runs-on'], 'bridge')
        self.assertNotIn('container', job)
        self.assertLessEqual(job['timeout-minutes'], 60)
        step = next(s for s in job['steps'] if 'arch_handoff.py run' in s.get('run', ''))
        self.assertIn('--build "$((5000 + GITHUB_RUN_NUMBER))"', step['run'])
        self.assertIn('--directory dist/arch', step['run'])
        self.assertGreaterEqual(step['timeout-minutes'] * 60, 3180 + 120)
        uploads = {s['with']['name']: s for s in job['steps'] if 'upload-artifact' in s.get('uses', '')}
        self.assertEqual(set(uploads), {'arch-publication', 'arch-qa'})
        self.assertIn('dist/arch/release-source.txt', uploads['arch-publication']['with']['path'])
        checkout = job['steps'][0]
        self.assertEqual(checkout['with']['fetch-depth'], 0)

    def test_hosted_build_uses_the_ref_build_and_carries_no_secrets(self):
        text = (ROOT / handoff.WORKFLOW).read_text()
        hosted = yaml.safe_load(text)['jobs']['arch-release']
        names = [s.get('name') for s in hosted['steps']]
        for name in (handoff.BUILD_STEP, handoff.VALIDATE_STEP):
            self.assertEqual(names.count(name), 1)
        self.assertIn('OKILUM_BUILD_VERSION="$BUILD"', text)
        self.assertIn('rev-parse HEAD > dist/arch/release-source.txt', text)
        for forbidden in ('${{ secrets.', 'SCCACHE', 'actions/cache'):
            self.assertNotIn(forbidden, text)


if __name__ == '__main__':
    unittest.main()
