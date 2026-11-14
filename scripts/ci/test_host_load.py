"""Controls for the daily full set's host-load guard (Oleg 13.11 evening)."""
import importlib.util
import io
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import yaml

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('host_load', ROOT / 'scripts/ci/host-load.py')
host_load = importlib.util.module_from_spec(spec)
spec.loader.exec_module(host_load)


class Assess(unittest.TestCase):
    def test_limit_is_twice_the_cores_on_the_five_minute_average(self):
        rows = [(1, 90.0, 20.0), (2, 10.0, 24.0)]
        self.assertFalse(host_load.assess(rows, 12)['overloaded'])  # 24 == 2 x 12
        rows.append((3, 5.0, 24.1))
        result = host_load.assess(rows, 12)
        self.assertTrue(result['overloaded'])
        self.assertEqual((result['peak_load5'], result['limit'], result['samples']), (24.1, 24.0, 3))
        self.assertFalse(host_load.assess([], 12)['overloaded'])

    def verdict(self, rows, status):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, 'load.tsv')
            Path(path).write_text(''.join(f'{t}\t{a}\t{b}\n' for t, a, b in rows))
            summary, output = os.path.join(directory, 's'), os.path.join(directory, 'o')
            out = io.StringIO()
            with patch.dict(os.environ, {'GITHUB_STEP_SUMMARY': summary, 'GITHUB_OUTPUT': output}), \
                    patch.object(host_load, 'read_load', return_value=(1.0, 1.0)), \
                    patch.object(host_load, 'host_cores', return_value=12):
                self.assertEqual(host_load.verdict(path, status, out), 0)
            return out.getvalue(), Path(output).read_text(), Path(summary).read_text()

    def test_only_a_failing_overloaded_run_is_no_verdict(self):
        busy = [(1, 150.0, 175.3)]
        text, output, summary = self.verdict(busy, 'failure')
        self.assertIn('::warning::NO VERDICT', text)
        self.assertIn('no_verdict=true', output)
        self.assertIn('NO VERDICT', summary)
        # A green run under load stays green; a quiet failure stays a failure.
        for rows, status in ((busy, 'success'), ([(1, 3.0, 4.0)], 'failure')):
            text, output, _ = self.verdict(rows, status)
            self.assertNotIn('NO VERDICT', text)
            self.assertIn('no_verdict=false', output)


class Workflow(unittest.TestCase):
    def test_full_set_runs_daily_before_stable_and_both_heavy_jobs_are_guarded(self):
        data = yaml.safe_load((ROOT / '.forgejo/workflows/ci.yml').read_text())
        on = data.get('on', data.get(True))
        self.assertEqual(on['schedule'], [{'cron': '0 4 * * *'}])
        for job in ('shared-local', 'linux-local'):
            steps = data['jobs'][job]['steps']
            names = [step.get('name') for step in steps]
            self.assertEqual(names[1], 'Sample host load (no-verdict guard)', job)
            self.assertEqual(names[-1], 'Report host load and verdict', job)
            self.assertEqual(steps[-1]['if'], 'always()')
            self.assertIn('--job-status "${{ job.status }}"', steps[-1]['run'])
        # A timed-out gate step must leave time for the report (run 27378 was
        # killed at the job limit before the always() step could run).
        for job, step, budget in (('shared-local', 'Check every shared suite and summarize failures', 60),
                                  ('linux-local', 'Run full Linux gate', 80)):
            gate = next(s for s in data['jobs'][job]['steps'] if s.get('name') == step)
            self.assertEqual(gate['timeout-minutes'], budget)
            self.assertGreaterEqual(data['jobs'][job]['timeout-minutes'], budget + 5)


if __name__ == '__main__':
    unittest.main()
