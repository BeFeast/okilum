"""Execute the actual shared routing and aggregation shells, including absence controls."""
import itertools
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = (ROOT / '.forgejo/workflows/ci.yml').read_text()


def block(name):
    return re.search(rf'^  {name}:\n(.*?)(?=^  [\w-]+:|\Z)', WORKFLOW, re.M | re.S)[1]


def script(name):
    text = block(name).split('        run: |\n', 1)[1]
    return '\n'.join(line[10:] if line.startswith('          ') else line for line in text.splitlines())


def execute(name, env):
    with tempfile.TemporaryDirectory() as directory:
        output = Path(directory) / 'outputs'
        result = subprocess.run(['bash', '-c', script(name)], text=True,
            capture_output=True, timeout=5,
            env=dict(os.environ, GITHUB_OUTPUT=str(output), **env))
        outputs = dict(line.split('=', 1) for line in output.read_text().splitlines()) if output.exists() else {}
        return result.returncode, outputs


class SharedLaneTests(unittest.TestCase):
    def test_selector_matrix_runs_actual_inline_policy(self):
        for setting, event, trusted in itertools.product(('', 'local', 'hosted', 'measure'),
                                                        ('push', 'pull_request', 'schedule'), ('true', 'false')):
            env = dict(SHARED_LANE=setting, EVENT=event, TRUSTED=trusted,
                       MEASURE_UNTIL=str(int(time.time())+3600), MEASURE_FIRST_RUN='100', RUN_NUMBER='100')
            code, outputs = execute('shared-select', env)
            eligible = trusted == 'true' and event in ('push', 'pull_request')
            with self.subTest(setting=setting, event=event, trusted=trusted):
                self.assertEqual(code, 0)
                self.assertEqual(outputs['lane'], 'hosted' if eligible and setting == 'hosted' else 'local')
                self.assertEqual(outputs['measure'], str(eligible and setting == 'measure').lower())

    def test_measurement_expires_and_is_bounded_to_at_most_ten_run_numbers(self):
        base = dict(SHARED_LANE='measure', EVENT='pull_request', TRUSTED='true',
                    MEASURE_UNTIL=str(int(time.time())+3600), MEASURE_FIRST_RUN='100', RUN_NUMBER='100')
        for run in range(98, 113):
            _, output = execute('shared-select', dict(base, RUN_NUMBER=str(run)))
            self.assertEqual(output['measure'], str(100 <= run < 110).lower())
            self.assertEqual(output['lane'], 'local')
        for key, value in [('MEASURE_UNTIL', ''), ('MEASURE_UNTIL', 'invalid'),
                           ('MEASURE_UNTIL', str(int(time.time())-1)),
                           ('MEASURE_UNTIL', str(int(time.time())+90000)),
                           ('MEASURE_FIRST_RUN', ''), ('RUN_NUMBER', '')]:
            _, output = execute('shared-select', dict(base, **{key: value}))
            self.assertEqual(output, {'lane': 'local', 'measure': 'false'})

    def test_aggregate_local_ignores_measurement_but_hosted_requires_exact_success(self):
        states = ('success', 'failure', 'skipped', 'cancelled', '')
        for lane, local, hosted, result in itertools.product(('local', 'hosted', 'unknown'), states, states, states):
            env = dict(SELECTION='success', LANE=lane, LOCAL=local, HOSTED_JOB=hosted, HOSTED_RESULT=result)
            code, _ = execute('shared', env)
            expected = (local == 'success' if lane == 'local' else
                        lane == 'hosted' and local == 'skipped' and hosted == result == 'success')
            self.assertEqual(code == 0, expected, env)
        for selection in states[1:]:
            self.assertNotEqual(execute('shared', dict(SELECTION=selection, LANE='local',
                LOCAL='success', HOSTED_JOB='success', HOSTED_RESULT='success'))[0], 0)

    def test_wiring_and_public_runner_isolation(self):
        self.assertEqual(WORKFLOW.count('vars.OKILUM_SHARED_LANE'), 1)
        self.assertNotIn('checkout', block('shared-select'))
        self.assertIn("if: needs.shared-select.outputs.lane == 'local'", block('shared-local'))
        self.assertIn("if: needs.shared-select.outputs.lane == 'hosted'", block('shared-github'))
        self.assertIn("if: needs.shared-select.outputs.measure == 'true'", block('shared-measure'))
        self.assertNotIn('shared-measure', block('shared'))
        self.assertIn('needs: [shared-select, shared-local, shared-github]', block('shared'))
        self.assertIn('needs: [shared, macos]', block('check'))
        self.assertIn('github.event.pull_request.head.sha || github.sha', block('shared-github'))
        hosted = (ROOT / '.github/workflows/forgejo-shared.yml').read_text()
        self.assertIn('runs-on: ubuntu-24.04', hosted)
        self.assertIn("branches: ['forgejo-shared-pr/**']", hosted)
        self.assertIn('tini -s -- bash scripts/ci/check-shared.sh', hosted)
        for secret in ('${{ secrets.', 'SCCACHE_ENDPOINT', 'AWS_ACCESS_KEY_ID', 'OKILUM_GITHUB_APP_KEY'):
            self.assertNotIn(secret, hosted)
        self.assertNotIn('actions/cache/save', hosted)
        self.assertNotIn('workflow_dispatch', hosted)


if __name__ == '__main__':
    unittest.main()
