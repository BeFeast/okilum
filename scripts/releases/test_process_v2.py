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
            self.assertEqual(data['on']['schedule'][0]['cron'], '5 22 * * 2,6')
        self.assertEqual(workflow('.forgejo/workflows/sprint.yml')['on'], {})
        self.assertIn('test-nofile.sh', (ROOT / 'scripts/ci/check-linux.sh').read_text())

    def test_fast_lane_is_local_linux_with_six_minute_budget(self):
        local = workflow('.forgejo/workflows/fast.yml')
        job = local['jobs']['check']
        self.assertEqual(job['runs-on'], 'fast-linux')
        self.assertEqual(job['timeout-minutes'], '6')
        text = (ROOT / '.forgejo/workflows/fast.yml').read_text()
        self.assertNotIn('github-pr.py', text)
        self.assertIn('fast-local.sh', text)
        pool = (ROOT / 'scripts/ci/fast-local.sh').read_text()
        self.assertIn('CARGO_TARGET_DIR', pool)
        self.assertIn('flock -n', pool)
        self.assertFalse((ROOT / '.github/workflows/forgejo-fast.yml').exists())
        script = (ROOT / 'scripts/ci/fast.sh').read_text()
        self.assertIn('cargo build --locked -p okilum-shell -p okilum-sync-supervisor', script)
        self.assertIn('cargo test --locked -p okilum-paste --lib', script)
        self.assertNotIn('cargo test --workspace', script)
        self.assertNotIn('check-linux.sh', script)

    def test_all_platform_beta_builds_start_on_main_push_without_cancelling_signing(self):
        for file in ['linux-release', 'windows-diagnostic', 'macos-release']:
            data = workflow(f'.forgejo/workflows/{file}.yml')
            self.assertEqual(data['on']['push']['branches'], ['main'])
            self.assertNotIn('pull_request', data['on'])
            self.assertEqual(data['concurrency']['cancel-in-progress'], 'false')
        windows = (ROOT / '.forgejo/workflows/windows-diagnostic.yml').read_text()
        self.assertIn("github.event_name != 'push' && vars.OKILUM_WINDOWS_SIGN_BACKEND", windows)
        self.assertIn('resign_build:', windows)

    @patch.object(coalesce, 'main_tip', return_value='a' * 40)
    @patch.object(coalesce, 'descriptor')
    def test_newest_source_only_reuses_existing_payload_and_rejects_bad_identity(self, descriptor, tip):
        for platform in ['linux', 'windows', 'macos']:
            descriptor.return_value = None
            self.assertTrue(coalesce.push_ready('a' * 40, platform, 'refs/heads/main'))
            descriptor.return_value = {'source': 'a' * 40, 'platform': platform}
            self.assertFalse(coalesce.push_ready('a' * 40, platform, 'refs/heads/main'))
            descriptor.reset_mock()
            self.assertFalse(coalesce.push_ready('b' * 40, platform, 'refs/heads/main'))
            descriptor.assert_not_called()
        with self.assertRaises(ValueError):
            coalesce.push_ready('a' * 40, 'windows', 'refs/heads/untrusted')
        descriptor.return_value = {'source': 'b' * 40, 'platform': 'macos'}
        with self.assertRaises(ValueError):
            coalesce.push_ready('a' * 40, 'macos', 'refs/heads/main')


if __name__ == '__main__':
    unittest.main()
