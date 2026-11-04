"""Follow-up must include failures/unfinished runs and keep queue time distinct."""
from contextlib import redirect_stdout
from datetime import datetime, timezone
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('fast_runtime_report', Path(__file__).with_name('fast-runtime-report.py'))
REPORT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REPORT)


class Clock(datetime):
    @classmethod
    def now(cls, tz=None):
        return datetime(2026, 11, 3, 18, tzinfo=timezone.utc)


class RuntimeReport(unittest.TestCase):
    def test_partial_window_includes_failed_and_live_runs_without_claiming_a_day(self):
        since = '2026-11-03T17:00:00Z'
        def run(number, started, stopped, status):
            return dict(id=number, workflow_id='fast.yml', created=since, started=started,
                        stopped=stopped, status=status, html_url=f'https://example.invalid/runs/{number}')
        rows = [run(1, '2026-11-03T17:30:00Z', '2026-11-03T17:31:40Z', 'success'),
                run(2, '2026-11-03T17:35:00Z', '2026-11-03T17:45:00Z', 'cancelled'),
                run(3, '2026-11-03T17:40:00Z', None, 'running'),
                run(4, None, None, 'waiting')]
        pr = dict(merged=True, merged_at=since, merge_commit_sha='a'*40)
        with tempfile.TemporaryDirectory() as temp:
            evidence = Path(temp) / 'evidence.json'
            output = io.StringIO()
            with patch.object(REPORT, 'get', side_effect=[pr, dict(workflow_runs=rows)]) as get, \
                 patch.object(REPORT, 'datetime', Clock), \
                 patch('sys.argv', ['report', '2504', '--json', str(evidence)]), redirect_stdout(output):
                REPORT.main()
            self.assertIn('partial observation', output.getvalue())
            self.assertIn('1200 s (20.00 min), lower bound', output.getvalue())
            self.assertIn('Creation-to-stop/observation: 3600 s', output.getvalue())
            self.assertIn('incident baseline 2700 s (45 min)', output.getvalue())
            self.assertIn('https://example.invalid/runs/3', output.getvalue())
            self.assertEqual(json.loads(evidence.read_text())['workflow_runs'], rows)
            self.assertEqual(get.call_args.args, ('actions/runs?workflow_id=fast.yml&limit=50&page=1',))

    def test_unmerged_pr_cannot_start_an_observation_window(self):
        with patch.object(REPORT, 'get', return_value=dict(merged=False)) as get, \
             patch('sys.argv', ['report', '2504']), redirect_stdout(io.StringIO()), \
             patch('sys.stderr', new=io.StringIO()):
            with self.assertRaises(SystemExit) as error:
                REPORT.main()
            self.assertEqual(error.exception.code, 2)
            get.assert_called_once_with('pulls/2504')


if __name__ == '__main__':
    unittest.main()
