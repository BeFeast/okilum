"""Same-runner comparison: paired provenance and false-red/forced-red controls."""
import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import ab


def metrics(value):
    return {f"{'live' if live else 'source'}:{name}": ab.summarize([value] * 5, 5)
            for name, live in ab.SCENARIOS}


def pair(base, candidate):
    m, p = metrics(base), metrics(candidate)
    return {"metrics": {"main": m, "pr": p}, "failures": ab.compare(m, p, CONFIG),
            "evidence": ["valid mock collector positive controls"], "order": []}


CONFIG = ab.policy(ab.HERE / "ab-policy.json")


class ComparisonTests(unittest.TestCase):
    def test_hardware_bimodality_cancels_without_weakening_relative_budget(self):
        for value in (25, 51, 100, 250):
            self.assertEqual(ab.compare(metrics(value), metrics(value), CONFIG), [])

    def test_both_conditions_are_strict_and_a_slow_candidate_fails(self):
        for baseline, candidate in [(100, 115), (10, 13), (10, 12), (100, 110)]:
            self.assertEqual(ab.compare(metrics(baseline), metrics(candidate), CONFIG), [])
        self.assertEqual(len(ab.compare(metrics(20), metrics(24), CONFIG)), 6)

    def test_identical_number_of_samples_and_valid_scope_required(self):
        with self.assertRaises(ValueError):
            ab.summarize([1] * 4, 5)
        with self.assertRaises(ValueError):
            ab.summarize([1, 2, 3, 4, float('nan')], 5)
        m = metrics(25); m.pop(next(iter(m)))
        with self.assertRaises(ValueError):
            ab.compare(m, metrics(25), CONFIG)

    def test_counterbalanced_order_and_equal_warmups(self):
        calls = []
        def collect(binary, directory, name, live, rep, deadline, *, input_only):
            self.assertTrue(input_only)
            calls.append((str(binary), name, live, rep))
            return {"input_frame_ms": 10000 if rep == 0 else 25}, {"pixels_changed": True, "buffer_verified": True}
        with patch.object(ab.probe, 'run_once', side_effect=collect):
            report = ab.paired(Path('main'), Path('pr'), Path('out'), CONFIG, ab.time.monotonic() + 100)
        self.assertEqual(len(calls), 72)
        self.assertEqual([r[0] for r in calls[:4]], ['main', 'pr', 'pr', 'main'])
        self.assertTrue(all(m['median_ms'] == 25 for rows in report['metrics'].values() for m in rows.values()))
        self.assertEqual(report['failures'], [])


class RunnerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.binaries = []
        for role, sha in [('main', 'a'), ('pr', 'b')]:
            folder = self.root / role; folder.mkdir()
            binary = folder / 'okilum'; binary.write_bytes(role.encode())
            (folder / 'BUILD.txt').write_text(f'commit {sha * 40}\nshipped-unpatched release\n')
            self.binaries.append(binary)
        self.out = self.root / 'out'

    def invoke(self, results, *options):
        with patch.object(ab, 'hardware', return_value={'cpu_models': ['test CPU'], 'available_cpus': 4}), \
             patch.object(ab, 'paired', side_effect=results) as measured, \
             contextlib.redirect_stdout(io.StringIO()):
            code = ab.main([*(str(x) for x in self.binaries), str(self.out), *options])
        return code, json.loads((self.out / 'result.json').read_text()), measured

    def test_valid_regression_gets_one_paired_retry_same_binary_and_cpu(self):
        code, report, measured = self.invoke([pair(20, 30), pair(20, 21)])
        self.assertEqual(code, 0)
        self.assertEqual(measured.call_count, 2)
        self.assertEqual([r['result'] for r in report['attempts']], ['FAIL', 'PASS'])
        self.assertEqual(report['binaries']['main']['source_commit'], 'a' * 40)
        self.assertEqual(report['hardware']['cpu_models'], ['test CPU'])
        self.assertEqual(measured.call_args_list[0].args[:2], measured.call_args_list[1].args[:2])
        self.assertEqual(measured.call_args_list[0].args[-1], measured.call_args_list[1].args[-1])

    def test_two_valid_regressions_fail(self):
        code, report, measured = self.invoke([pair(20, 30), pair(20, 30)])
        self.assertEqual(code, 1)
        self.assertEqual(report['result'], 'FAIL')
        self.assertEqual(measured.call_count, 2)

    def test_report_only_retains_both_valid_failures_without_failing_process(self):
        code, report, measured = self.invoke([pair(20, 30), pair(20, 30)], '--report-only')
        self.assertEqual(code, 0)
        self.assertEqual(report['result'], 'FAIL')
        self.assertEqual(report['exit_code'], 1)
        self.assertEqual(report['process_exit_code'], 0)
        self.assertEqual(report['mode'], 'report-only')
        self.assertEqual([a['result'] for a in report['attempts']], ['FAIL', 'FAIL'])
        self.assertEqual(measured.call_count, 2)
        self.assertIn('**FAIL** (report-only)', ab.markdown(report))

    def test_report_only_missing_controls_still_fail_without_retry(self):
        code, report, measured = self.invoke([ab.probe.NoEvidence('unchanged pixels')], '--report-only')
        self.assertEqual(code, 2)
        self.assertEqual(report['result'], 'NO_EVIDENCE')
        self.assertIn('unchanged pixels', report['error'])
        self.assertEqual(measured.call_count, 1)

    def test_report_only_integrity_failure_is_not_a_performance_failure(self):
        def mutate(*args):
            self.binaries[1].write_bytes(b'changed')
            return pair(20, 30)
        code, report, measured = self.invoke(mutate, '--report-only')
        self.assertEqual(code, 2)
        self.assertEqual(report['result'], 'NO_EVIDENCE')
        self.assertIn('SHA256 changed', report['error'])
        self.assertEqual(measured.call_count, 1)

    def test_missing_evidence_does_not_retry(self):
        code, report, measured = self.invoke([ab.probe.NoEvidence('missing generation')])
        self.assertEqual(code, 2)
        self.assertEqual(report['result'], 'NO_EVIDENCE')
        self.assertEqual(measured.call_count, 1)

    def test_binary_mutation_invalidates_measurement(self):
        def mutate(*args):
            self.binaries[1].write_bytes(b'changed')
            return pair(25, 25)
        code, report, measured = self.invoke(mutate)
        self.assertEqual(code, 2)
        self.assertIn('SHA256 changed', report['error'])
        self.assertEqual(measured.call_count, 1)

    def test_measurement_build_is_refused(self):
        (self.binaries[1].parent / 'BUILD.txt').write_text('commit ' + 'b' * 40 + '\nmeasurement trace\n')
        with patch.object(ab, 'hardware', return_value={}), patch.object(ab, 'paired') as measured, contextlib.redirect_stdout(io.StringIO()):
            code = ab.main([*(str(x) for x in self.binaries), str(self.out)])
        self.assertEqual(code, 2); measured.assert_not_called()


if __name__ == '__main__':
    unittest.main()
