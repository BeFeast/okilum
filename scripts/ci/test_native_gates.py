"""Execute the workflow's real aggregate scripts across failure/skip states."""
import os
from pathlib import Path
import re
import subprocess
import textwrap
import unittest

WORKFLOW = (Path(__file__).resolve().parents[2] / '.forgejo/workflows/ci.yml').read_text()


def gate(job, **overrides):
    block = re.search(rf'^  {job}:\n(.*?)(?=^  [\w-]+:|\Z)', WORKFLOW, re.M | re.S)[1]
    script = textwrap.dedent(block.split('        run: |\n', 1)[1])
    env = dict(os.environ, EVENT='pull_request', SHARED_RESULT='success', LINUX_RESULT='success',
               SCOPE_RESULT='success', MACOS_REQUIRED='true', MACOS_LANE='local',
               HOSTED_JOB='skipped', HOSTED_RESULT='', LOCAL_RESULT='success',
               MACOS_RESULT='success', WINDOWS_RESULT='success', PERF_RESULT='success', PERF_REQUIRED='true')
    env.update(overrides)
    return subprocess.run(['bash', '-c', script], env=env, capture_output=True).returncode == 0


class NativeGates(unittest.TestCase):
    def test_selected_lanes_pass(self):
        self.assertTrue(gate('macos'))
        self.assertTrue(gate('macos', MACOS_LANE='hosted', HOSTED_JOB='success',
                             HOSTED_RESULT='success', LOCAL_RESULT='skipped'))

    def test_lane_switch_after_native_completion(self):
        # 03:05: hosted finished, but the repository variable now says local.
        self.assertTrue(gate('macos', MACOS_LANE='local', HOSTED_JOB='success',
                             HOSTED_RESULT='success', LOCAL_RESULT='skipped'))
        # 09:00: local finished, but the repository variable now says hosted.
        self.assertTrue(gate('macos', MACOS_LANE='hosted'))

    def test_draft_pr_skips_native_lanes_and_never_passes(self):
        # Draft (WIP) PRs skip both macOS lanes; the required gate stays red.
        self.assertFalse(gate('macos', DRAFT='true', LOCAL_RESULT='skipped'))
        self.assertFalse(gate('macos', DRAFT='true'))
        # Documentation-only drafts need no native runner at all.
        self.assertTrue(gate('macos', DRAFT='true', MACOS_REQUIRED='false'))
        # Ready PRs are unaffected.
        self.assertTrue(gate('macos', DRAFT='false'))
        workflow_if = re.findall(r'!github\.event\.pull_request\.draft', WORKFLOW)
        self.assertEqual(len(workflow_if), 4, 'macOS (hosted, local), Windows and perf-linux skip draft PRs')

    def test_exactly_one_lane_must_execute_successfully(self):
        self.assertFalse(gate('macos', LOCAL_RESULT='skipped'))
        self.assertFalse(gate('macos', HOSTED_JOB='success', HOSTED_RESULT='success'))
        self.assertFalse(gate('macos', HOSTED_JOB='failure'))
        self.assertFalse(gate('macos', HOSTED_JOB='cancelled'))

    def test_scope_failure_never_passes(self):
        for status in ('failure', 'cancelled', 'skipped', ''):
            for scope in ('true', 'false'):
                self.assertFalse(gate('macos', SCOPE_RESULT=status, MACOS_REQUIRED=scope))

    def test_linux_windows_never_block_mac_or_check(self):
        for status in ('failure', 'cancelled', 'skipped', ''):
            for scope in ('true', 'false'):
                values = dict(LINUX_RESULT=status, WINDOWS_RESULT=status,
                              PERF_RESULT=status, MACOS_REQUIRED=scope)
                self.assertTrue(gate('macos', **values))
                self.assertTrue(gate('check', LINUX_RESULT=status, WINDOWS_RESULT=status))

    def test_selected_native_failure_and_fork_skip_fail(self):
        for status in ('failure', 'cancelled', 'skipped', ''):
            self.assertFalse(gate('macos', LOCAL_RESULT=status))
            self.assertFalse(gate('macos', MACOS_LANE='hosted', HOSTED_JOB='success',
                                  HOSTED_RESULT=status, LOCAL_RESULT='skipped'))

    def test_docs_skip_and_unknown_scope(self):
        self.assertTrue(gate('macos', MACOS_REQUIRED='false', LOCAL_RESULT='skipped'))
        self.assertFalse(gate('macos', MACOS_REQUIRED=''))

    def test_check_requires_shared_and_mac_for_every_pr(self):
        self.assertTrue(gate('check'))
        for status in ('failure', 'cancelled', 'skipped', ''):
            self.assertFalse(gate('check', MACOS_RESULT=status))
            self.assertFalse(gate('check', SHARED_RESULT=status))
            # Even docs-only PRs must have a successful scope/native aggregate.
            self.assertFalse(gate('check', MACOS_REQUIRED='false', MACOS_RESULT=status))

    def test_check_requires_applicable_perf_and_rejects_missing_evidence(self):
        for status in ('failure', 'cancelled', 'skipped', ''):
            self.assertFalse(gate('check', PERF_RESULT=status))
        self.assertTrue(gate('check', PERF_REQUIRED='false', PERF_RESULT='skipped'))
        self.assertFalse(gate('check', PERF_REQUIRED='', PERF_RESULT='skipped'))
        self.assertFalse(gate('check', PERF_REQUIRED='false', PERF_RESULT='failure'))
        self.assertTrue(gate('check', EVENT='push', PERF_RESULT='skipped'))

    def test_main_requires_shared_without_pr_native_scope(self):
        self.assertTrue(gate('check', EVENT='push', MACOS_RESULT='skipped'))
        self.assertFalse(gate('check', EVENT='push', SHARED_RESULT='failure'))

    def test_blocking_dependency_graph_is_independent_of_platform_lanes(self):
        def needs(job):
            block = re.search(rf'^  {job}:\n(.*?)(?=^  [\w-]+:|\Z)', WORKFLOW, re.M | re.S)[1]
            value = re.search(r'^    needs: (.*)$', block, re.M)
            return set(value[1].strip('[]').split(', ')) if value else set()
        self.assertEqual(needs('check'), {'native-scope', 'shared', 'macos', 'perf-linux'})
        self.assertEqual(needs('macos'), {'native-scope', 'macos-github', 'macos-local'})
        self.assertEqual(needs('shared'), set())
        for job in ('macos-github', 'macos-local'):
            self.assertEqual(needs(job), {'native-scope'})
        shared = re.search(r'^  shared:\n(.*?)(?=^  [\w-]+:)', WORKFLOW, re.M | re.S)[1]
        self.assertIn('tini -s -- bash scripts/ci/check-shared.sh', shared)
        self.assertIn('scripts/vendor-setup.sh --verify', shared)
        self.assertNotIn('check-linux.sh', shared)



if __name__ == '__main__':
    unittest.main()


class MacosLaneOnceTests(unittest.TestCase):
    """PR #1214: the lane flips at 03:05 and 09:00; one run must read it exactly once."""

    def test_only_native_scope_reads_the_lane_and_lanes_are_exclusive(self):
        import re
        workflow = (Path(__file__).resolve().parents[2] / '.forgejo/workflows/ci.yml').read_text()
        self.assertEqual(workflow.count('vars.OKILUM_MACOS_LANE'), 1)
        scope = re.search(r'^  native-scope:\n(.*?)(?=^  [\w-]+:)', workflow, re.M | re.S)[1]
        self.assertIn('vars.OKILUM_MACOS_LANE', scope)
        self.assertIn('scripts/ci/macos-lane.py', scope)
        for job, lane in [('macos-github', 'hosted'), ('macos-local', 'local')]:
            block = re.search(rf'^  {job}:\n(.*?)(?=^  [\w-]+:)', workflow, re.M | re.S)[1]
            self.assertIn(f"needs.native-scope.outputs.macos_lane == '{lane}'", block)


class MacosLaneIdleTests(unittest.TestCase):
    """2026-10-11: in the M4 window a PR takes the M4 only while it is idle."""

    def setUp(self):
        import importlib.util
        spec = importlib.util.spec_from_file_location('lane', Path(__file__).with_name('macos-lane.py'))
        self.lane = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.lane)

    def test_busy_m4_sends_the_pr_to_hosted_and_idle_keeps_it_local(self):
        running = [{'workflow_id': 'ci.yml', 'name': 'macos-local', 'status': 'running', 'run_number': 7}]
        self.assertEqual(self.lane.choose('local', running)[0], 'hosted')
        release = [{'workflow_id': 'macos-release.yml', 'name': 'release', 'status': 'running'}]
        self.assertEqual(self.lane.choose('local', release)[0], 'hosted')
        finished = [{'workflow_id': 'ci.yml', 'name': 'macos-local', 'status': 'success'},
                    {'workflow_id': 'ci.yml', 'name': 'linux-local', 'status': 'running'}]
        self.assertEqual(self.lane.choose('local', finished)[0], 'local')

    def test_variable_still_rules_outside_the_window_and_when_tasks_are_unknown(self):
        running = [{'workflow_id': 'ci.yml', 'name': 'macos-local', 'status': 'running'}]
        self.assertEqual(self.lane.choose('hosted', running)[0], 'hosted')
        self.assertEqual(self.lane.choose('', [])[0], 'hosted')
        self.assertEqual(self.lane.choose('local', None)[0], 'local')
