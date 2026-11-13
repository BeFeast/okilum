"""Executable policy for approved 31 October release cadence."""
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch
import yaml

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('v2_coalesce', Path(__file__).with_name('coalesce.py'))
coalesce = importlib.util.module_from_spec(spec)
spec.loader.exec_module(coalesce)


def workflow(path):
    # YAML 1.1 treats "on" as a boolean; BaseLoader keeps the workflow vocabulary.
    return yaml.load((ROOT / path).read_text(), Loader=yaml.BaseLoader)


class ProcessV2(unittest.TestCase):
    def test_full_suites_have_no_pr_or_main_push_trigger(self):
        for file in ['ci', 'brain-ci', 'inbox-ci', 'excalidraw-compat']:
            data = workflow(f'.forgejo/workflows/{file}.yml')
            self.assertEqual(set(data['on']), {'schedule', 'workflow_dispatch'})
            # ci.yml is the daily full set before Stable (Oleg 13.11 evening).
            expected = '0 4 * * *' if file == 'ci' else '5 22 * * 2,6'
            self.assertEqual(data['on']['schedule'][0]['cron'], expected)
        self.assertEqual(workflow('.forgejo/workflows/sprint.yml')['on'], {})
        self.assertIn('test-nofile.sh', (ROOT / 'scripts/ci/check-linux.sh').read_text())

    def test_fast_check_runs_on_github_with_core_rebuild_budget(self):
        local = workflow('.forgejo/workflows/fast.yml')
        job = local['jobs']['check']
        # #2589: baldr co-tenant load timed out the build phase; the certification
        # check waits on a bridge while GitHub's dedicated runner executes it.
        self.assertEqual(job['runs-on'], 'bridge')
        self.assertEqual(job['timeout-minutes'], '60')
        text = (ROOT / '.forgejo/workflows/fast.yml').read_text()
        check = text.split('  check:\n', 1)[1].split('\n  shell-full:', 1)[0]
        self.assertIn('CI_LANE: check', check)
        self.assertIn('CI_LANE: shell-full', text)
        # Never the retired three-platform fast lane or the baldr slot pool.
        self.assertNotIn('CI_LANE: fast\n', text)
        self.assertNotIn('fast-local.sh', text)
        self.assertFalse((ROOT / 'scripts/ci/fast-local.sh').exists())
        self.assertFalse((ROOT / '.github/workflows/forgejo-fast.yml').exists())
        hosted = (ROOT / '.github/workflows/forgejo-check.yml').read_text()
        self.assertIn('bash scripts/ci/test-group.sh fast.sh 2040 bash scripts/ci/fast.sh', hosted)
        script = (ROOT / 'scripts/ci/fast.sh').read_text()
        self.assertIn('cargo build --locked -p okilum-shell -p okilum-sync-supervisor', script)
        self.assertIn('cargo check --locked --workspace --tests', script)
        self.assertIn('cargo test --locked -p okilum-appimage --lib channel', script)
        self.assertIn('cargo test --locked -p okilum-paste --lib', script)
        self.assertNotIn('cargo test --workspace', script)
        self.assertNotIn('check-linux.sh', script)

    def test_all_platform_beta_builds_run_on_main_push_and_manual_signing_is_isolated(self):
        # Oleg 05.11: beta on every main push, no cron; a newer push cancels
        # an unfinished older push build, manual dispatches always finish.
        for file in ['linux-release', 'windows-diagnostic', 'macos-release']:
            data = workflow(f'.forgejo/workflows/{file}.yml')
            self.assertEqual(set(data['on']), {'push', 'workflow_dispatch'})
            self.assertEqual(data['on']['push'], {'branches': ['main']})
            self.assertEqual(data['concurrency']['cancel-in-progress'], "${{ github.event_name == 'push' }}")
            self.assertTrue(data['concurrency']['group'].endswith(
                "-beta-${{ github.event_name == 'push' && 'main' || inputs.hourly_source || github.run_id }}"))
        windows = (ROOT / '.forgejo/workflows/windows-diagnostic.yml').read_text()
        self.assertIn("github.event_name == 'workflow_dispatch' && inputs.hourly_source == '' && vars.OKILUM_WINDOWS_SIGN_BACKEND", windows)
        self.assertIn('resign_build:', windows)


if __name__ == '__main__':
    unittest.main()
