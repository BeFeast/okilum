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
        self.assertFalse(gate('macos', MACOS_LANE='unknown'))

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
