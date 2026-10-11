import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('ci_logs', Path(__file__).with_name('ci-logs.py'))
logs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(logs)


class SummaryTests(unittest.TestCase):
    def test_summary_points_at_the_real_failure(self):
        cargo = ('2026Z    Compiling okilum\n2026Z ---- brain::a::tests::b stdout ----\n'
                 '2026Z test result: FAILED. 1 failed\n')
        self.assertEqual(logs.summary(cargo, 'ci.yml', 'linux-local'), 'tests: brain::a::tests::b')
        self.assertTrue(logs.summary('2026Z error[E0308]: mismatched types\n').startswith('error[E0308]'))
        self.assertTrue(logs.summary('', 'ci.yml', 'check').startswith('aggregate'))
        self.assertTrue(logs.summary('2026Z see https://github.com/BeFeast/okilum/actions/runs/1\n').startswith('hosted lane'))
        self.assertIn('infrastructure', logs.summary('2026Z Job failed\n'))

    def test_web_run_number_is_resolved_to_the_api_id(self):
        pages = {1: [{'index_in_repo': 6123, 'id': 10902}, {'index_in_repo': 6122, 'id': 10901}]}
        logs.RUN_IDS.clear()
        original = logs.fetch
        logs.fetch = lambda path, raw=False: pages.get(int(path.split('page=')[1]), [])
        try:
            self.assertEqual(logs.run_id(6123)[0], 10902)
            with self.assertRaises(LookupError):
                logs.run_id(5)
        finally:
            logs.fetch = original
            logs.RUN_IDS.clear()


if __name__ == '__main__':
    unittest.main()
