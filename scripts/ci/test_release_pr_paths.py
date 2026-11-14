"""Beta delivery runs on main push only (no PR trigger, no cron); acceptance keeps its own schedule."""
from pathlib import Path
import re
import unittest
import yaml

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ['linux-release.yml', 'windows-diagnostic.yml', 'macos-release.yml']


def triggers(workflow):
    text = (ROOT / '.forgejo/workflows' / workflow).read_text()
    return re.search(r'^on:\n(.*?)(?=^\S)', text, re.M | re.S)[1]


class ReleasePathsTests(unittest.TestCase):
    def test_prs_never_trigger_native_packaging(self):
        for workflow in WORKFLOWS:
            with self.subTest(workflow=workflow):
                self.assertEqual(re.findall(r'^  ([\w_]+):', triggers(workflow), re.M),
                                 ['push', 'workflow_dispatch'])
                self.assertIn('  push:\n    branches: [main]\n', triggers(workflow))

    def test_heavy_forgejo_workflows_never_run_on_main_push(self):
        # Beta packaging is the only main-push work (Oleg 05.11): suites stay off push.
        for path in (ROOT / '.forgejo/workflows').glob('*.yml'):
            if path.name in ('github-mirror.yml', 'release-publish.yml', *WORKFLOWS):
                continue
            with self.subTest(workflow=path.name):
                self.assertNotIn('push', yaml.load(path.read_text(), Loader=yaml.BaseLoader)['on'])
        for name in ('fast.yml', 'feedback-ci.yml'):
            self.assertEqual(re.findall(r'^  ([\w_]+):', triggers(name), re.M),
                             ['schedule', 'workflow_dispatch'])

    def test_push_delivery_has_no_path_filters_or_cron(self):
        # Every main push ships, including UI/docs, packaging, release logic and
        # shared dependencies. PR #2079 deliberately replaced all path-filtered builds.
        for workflow in WORKFLOWS:
            with self.subTest(workflow=workflow):
                events = triggers(workflow)
                self.assertNotIn('schedule', events)
                self.assertNotIn('cron', events)
                self.assertNotRegex(events, r'(?m)^\s+(?:paths|paths-ignore|branches-ignore|tags):')

    def test_explicit_source_dispatch_keeps_branch_validation_available(self):
        for workflow in WORKFLOWS:
            with self.subTest(workflow=workflow):
                events = triggers(workflow)
                self.assertIn('  workflow_dispatch:\n    inputs:\n', events)
                self.assertIn('      source:\n', events)
                self.assertIn("        default: ''", events)
                text = (ROOT / '.forgejo/workflows' / workflow).read_text()
                self.assertIn('python3 scripts/releases/coalesce.py ', text)
                self.assertIn('OKILUM_SOURCE: ${{ inputs.source }}', text)

    def test_full_acceptance_schedule_is_separate_from_main_beta(self):
        for workflow in WORKFLOWS:
            self.assertNotIn('cron', triggers(workflow))
        acceptance = (ROOT / '.forgejo/workflows/ci.yml').read_text()
        self.assertIn("  schedule:\n    - cron: '0 4 * * *'", acceptance)
        self.assertIn('  workflow_dispatch:', acceptance)

    def test_windows_cross_build_helpers_remain_in_delivery(self):
        workflow = (ROOT / '.forgejo/workflows/windows-diagnostic.yml').read_text()
        self.assertIn('bash scripts/build-windows-ci.sh', workflow)
        self.assertIn('scripts/vendor-setup.sh --verify', workflow)
        self.assertIn('name: windows-payload', workflow)
