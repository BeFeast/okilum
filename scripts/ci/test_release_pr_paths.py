"""Linux/Windows beta runs on main push, the hosted Mac beta once a day (Oleg 13.11);
no PR trigger; acceptance keeps its own schedule."""
from pathlib import Path
import re
import unittest
import yaml

ROOT = Path(__file__).resolve().parents[2]
PUSH_BETAS = ['linux-release.yml', 'windows-diagnostic.yml']
MAC_BETA = 'macos-release.yml'
WORKFLOWS = [*PUSH_BETAS, MAC_BETA]


def triggers(workflow):
    text = (ROOT / '.forgejo/workflows' / workflow).read_text()
    return re.search(r'^on:\n(.*?)(?=^\S)', text, re.M | re.S)[1]


class ReleasePathsTests(unittest.TestCase):
    def test_hosted_mac_beta_runs_once_a_day_and_on_dispatch(self):
        # Oleg 13.11: no Mac beta per main push; one daily run from the newest main
        # (05:00 UTC = 07:00/08:00 Israel, before the 09:00 Stable promotion).
        events = triggers(MAC_BETA)
        self.assertEqual(re.findall(r'^  ([\w_]+):', events, re.M), ['schedule', 'workflow_dispatch'])
        self.assertIn("  schedule:\n    - cron: '0 5 * * *'\n", events)
        self.assertNotIn('push', yaml.load((ROOT / '.forgejo/workflows' / MAC_BETA).read_text(),
                                           Loader=yaml.BaseLoader)['on'])

    def test_prs_never_trigger_native_packaging(self):
        for workflow in PUSH_BETAS:
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
        # Every main push ships Linux and Windows, including UI/docs, packaging, release
        # logic and shared dependencies. PR #2079 deliberately replaced all path-filtered builds.
        for workflow in PUSH_BETAS:
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
        for workflow in PUSH_BETAS:
            self.assertNotIn('cron', triggers(workflow))
        self.assertNotIn('2,6', triggers(MAC_BETA))
        acceptance = (ROOT / '.forgejo/workflows/ci.yml').read_text()
        self.assertIn("  schedule:\n    - cron: '0 4 * * *'", acceptance)
        self.assertIn('  workflow_dispatch:', acceptance)

    def test_windows_cross_build_helpers_remain_in_delivery(self):
        workflow = (ROOT / '.forgejo/workflows/windows-diagnostic.yml').read_text()
        self.assertIn('bash scripts/build-windows-ci.sh', workflow)
        self.assertIn('scripts/vendor-setup.sh --verify', workflow)
        self.assertIn('name: windows-payload', workflow)
