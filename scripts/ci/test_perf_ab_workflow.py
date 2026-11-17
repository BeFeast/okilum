"""#1662 (Oleg 17.11): the editor latency A/B is strict but never required.

It runs on native-scope pre-merge heads as its own workflow run; neither the
Forgejo `ci / check` aggregate nor `gh-check.sh --fast` waits on it.
"""
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[2]
PERF = ROOT / '.github/workflows/forgejo-perf-linux.yml'
PRE_MERGE = "startsWith(github.ref, 'refs/heads/agent-checks-fast/')"


def load(path):
    data = yaml.safe_load(path.read_text())
    data['on'] = data.pop(True, data.get('on'))
    return data


def scope_script():
    steps = load(PERF)['jobs']['scope']['steps']
    return next(step for step in steps if step.get('id') == 'scope')['run']


def git(cwd, *args):
    return subprocess.run(['git', '-c', 'user.name=t', '-c', 'user.email=t@example.invalid', *args],
                          cwd=cwd, check=True, capture_output=True, text=True).stdout.strip()


class PerfWorkflowShape(unittest.TestCase):
    def setUp(self):
        self.workflow = load(PERF)
        self.jobs = self.workflow['jobs']

    def test_runs_on_pre_merge_heads_and_the_forgejo_bridge(self):
        self.assertEqual(self.workflow['on']['push']['branches'],
                         ['forgejo-perf-linux-pr/**', 'agent-checks-fast/**'])
        # A newer pre-merge push supersedes; bridged runs are never cancelled here.
        self.assertEqual(self.workflow['concurrency']['cancel-in-progress'], '${{ ' + PRE_MERGE + ' }}')

    def test_ab_is_strict(self):
        text = PERF.read_text()
        self.assertNotIn('--report-only', text)
        self.assertNotIn('inputs.strict', text)
        run = next(step['run'] for step in self.jobs['perf-linux']['steps']
                   if step.get('name') == 'Check editor latency and the baseline ratchet')
        self.assertIn('python3 scripts/perf/editor/ab.py', run)
        self.assertIn('set -euo pipefail', run)
        self.assertNotIn('continue-on-error', self.jobs['perf-linux'])
        self.assertFalse(any('continue-on-error' in step for step in self.jobs['perf-linux']['steps']))

    def test_ab_waits_for_native_scope_and_scroll_stays_off_pre_merge(self):
        self.assertEqual(self.jobs['perf-linux']['needs'], 'scope')
        self.assertEqual(self.jobs['perf-linux']['if'], "needs.scope.outputs.native == 'true'")
        self.assertEqual(self.jobs['scroll-report']['if'], '${{ !' + PRE_MERGE + ' }}')
        self.assertTrue(self.jobs['scroll-report']['continue-on-error'])

    def test_public_namespace_carries_no_secrets(self):
        text = PERF.read_text()
        self.assertNotIn('secrets.', text)
        self.assertEqual(self.workflow['permissions'], {'contents': 'read', 'actions': 'read'})
        for job in self.jobs.values():
            checkout = next(step for step in job['steps'] if step.get('uses', '').startswith('actions/checkout@'))
            self.assertIs(checkout['with']['persist-credentials'], False)


class NeverRequired(unittest.TestCase):
    def test_forgejo_aggregates_do_not_depend_on_perf(self):
        jobs = yaml.safe_load((ROOT / '.forgejo/workflows/ci.yml').read_text())['jobs']
        for name, job in jobs.items():
            needs = job.get('needs', [])
            needs = [needs] if isinstance(needs, str) else needs
            if name != 'perf-linux':
                self.assertNotIn('perf-linux', needs, name)
        self.assertEqual(sorted(jobs['check']['needs']), ['macos', 'shared'])

    def test_fast_helper_judges_only_its_own_workflow(self):
        import importlib.util
        spec = importlib.util.spec_from_file_location('gh_check', ROOT / 'scripts/agent/gh_check.py')
        helper = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(helper)
        self.assertEqual(helper.WORKFLOW, 'agent-checks.yml')
        self.assertEqual(helper.ab_runs_url('agent-checks-fast/feat/x'),
                         'https://github.com/BeFeast/okilum/actions/workflows/forgejo-perf-linux.yml'
                         '?query=branch%3Aagent-checks-fast%2Ffeat%2Fx')


class ScopeSelection(unittest.TestCase):
    """Execute the workflow's real scope step against synthetic histories."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)
        self.origin = self.tmp / 'origin'
        self.origin.mkdir()
        git(self.origin, 'init', '-q', '-b', 'main')
        (self.origin / 'scripts/ci').mkdir(parents=True)
        shutil.copy(ROOT / 'scripts/ci/macos-scope.py', self.origin / 'scripts/ci/macos-scope.py')
        (self.origin / 'crates').mkdir()
        (self.origin / 'crates/lib.rs').write_text('fn main() {}\n')
        git(self.origin, 'add', '-A')
        git(self.origin, 'commit', '-q', '-m', 'base')

    def head(self, path, *, orphan=False):
        git(self.origin, 'checkout', '-q', *(['--orphan'] if orphan else ['-b']), 'topic')
        target = self.origin / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text('change\n')
        git(self.origin, 'add', '-A')
        git(self.origin, 'commit', '-q', '-m', 'topic')
        sha = git(self.origin, 'rev-parse', 'HEAD')
        git(self.origin, 'checkout', '-q', 'main')
        return sha

    def select(self, sha, pre_merge='true'):
        work = self.tmp / 'work'
        # actions/checkout: a depth-1 fetch of the exact head.
        work.mkdir()
        git(work, 'init', '-q')
        git(work, 'remote', 'add', 'origin', self.origin.as_uri())
        git(work, 'fetch', '-q', '--depth=1', 'origin', sha)
        git(work, 'checkout', '-q', '--detach', sha)
        output, summary = self.tmp / 'output', self.tmp / 'summary'
        env = dict(os.environ, PRE_MERGE=pre_merge, GITHUB_SHA=sha,
                   GITHUB_OUTPUT=str(output), GITHUB_STEP_SUMMARY=str(summary))
        subprocess.run(['bash', '-c', scope_script()], cwd=work, env=env, check=True, capture_output=True)
        found = re.findall(r'^native=(.*)$', output.read_text(), re.M)
        self.assertEqual(len(found), 1)
        return found[0]

    def test_documentation_only_head_is_skipped(self):
        self.assertEqual(self.select(self.head('docs/note.md')), 'false')

    def test_code_head_is_measured(self):
        self.assertEqual(self.select(self.head('crates/other.rs')), 'true')

    def test_unknown_base_is_measured(self):
        self.assertEqual(self.select(self.head('docs/note.md', orphan=True)), 'true')

    def test_bridged_runs_were_selected_upstream(self):
        self.assertEqual(self.select(self.head('docs/note.md'), pre_merge='false'), 'true')


if __name__ == '__main__':
    unittest.main()
