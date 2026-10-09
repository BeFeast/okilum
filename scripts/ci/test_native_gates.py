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
    env = dict(os.environ, EVENT='pull_request', LINUX_RESULT='success',
               SCOPE_RESULT='success', MACOS_REQUIRED='true', MACOS_LANE='local',
               HOSTED_JOB='skipped', HOSTED_RESULT='', LOCAL_RESULT='success',
               MACOS_RESULT='success')
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
        self.assertEqual(len(workflow_if), 2, 'both native lanes skip draft PRs')

    def test_exactly_one_lane_must_execute_successfully(self):
        self.assertFalse(gate('macos', LOCAL_RESULT='skipped'))
        self.assertFalse(gate('macos', HOSTED_JOB='success', HOSTED_RESULT='success'))
        self.assertFalse(gate('macos', HOSTED_JOB='failure'))
        self.assertFalse(gate('macos', HOSTED_JOB='cancelled'))

    def test_linux_or_scope_failure_never_passes(self):
        for key in ('LINUX_RESULT', 'SCOPE_RESULT'):
            for status in ('failure', 'cancelled', 'skipped', ''):
                for scope in ('true', 'false'):
                    with self.subTest(key=key, status=status, scope=scope):
                        self.assertFalse(gate('macos', **{key: status, 'MACOS_REQUIRED': scope}))

    def test_selected_native_failure_and_fork_skip_fail(self):
        for status in ('failure', 'cancelled', 'skipped', ''):
            self.assertFalse(gate('macos', LOCAL_RESULT=status))
            self.assertFalse(gate('macos', MACOS_LANE='hosted', HOSTED_JOB='success',
                                  HOSTED_RESULT=status, LOCAL_RESULT='skipped'))

    def test_docs_skip_and_unknown_scope(self):
        self.assertTrue(gate('macos', MACOS_REQUIRED='false', LOCAL_RESULT='skipped'))
        self.assertFalse(gate('macos', MACOS_REQUIRED=''))

    def test_check_still_requires_native_for_code(self):
        self.assertTrue(gate('check'))
        for status in ('failure', 'cancelled', 'skipped', ''):
            self.assertFalse(gate('check', MACOS_RESULT=status))
        self.assertFalse(gate('check', MACOS_REQUIRED=''))

    def test_main_and_docs_keep_linux_gate(self):
        for case in ({'EVENT': 'push', 'MACOS_REQUIRED': '', 'MACOS_RESULT': 'skipped'},
                     {'MACOS_REQUIRED': 'false', 'MACOS_RESULT': 'success'}):
            self.assertTrue(gate('check', **case))
            self.assertFalse(gate('check', **case, LINUX_RESULT='failure'))


if __name__ == '__main__':
    unittest.main()
