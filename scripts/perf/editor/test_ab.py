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


def measured(m, p):
    rows = ab.compare(m, p, CONFIG)
    return {"metrics": {"main": m, "pr": p}, "scenarios": rows,
            "failures": [row for row in rows if row["verdict"] == "regression"],
            "evidence": ["valid mock collector positive controls"], "order": []}


def pair(base, candidate):
    return measured(metrics(base), metrics(candidate))


def failing(rows):
    return [row["scenario"] for row in rows if row["verdict"] == "regression"]


def recorded(samples, shift=0.0):
    return {name: ab.summarize([value + shift for value in values], len(values)) for name, values in samples.items()}


measured_pair = measured
CONFIG = ab.policy(ab.HERE / "ab-policy.json")
HOSTED = json.loads((ab.HERE / "hosted-false-reds.json").read_text())["runs"]


class ComparisonTests(unittest.TestCase):
    def test_hardware_bimodality_cancels_without_weakening_relative_budget(self):
        for value in (25, 51, 100, 240):
            self.assertEqual(failing(ab.compare(metrics(value), metrics(value), CONFIG)), [])

    def test_both_conditions_are_strict_and_a_slow_candidate_fails(self):
        for baseline, candidate in [(100, 115), (10, 13), (10, 12), (100, 110)]:
            self.assertEqual(failing(ab.compare(metrics(baseline), metrics(candidate), CONFIG)), [])
        self.assertEqual(len(failing(ab.compare(metrics(20), metrics(24), CONFIG))), 6)

    def test_exact_rank_test(self):
        self.assertEqual(ab.rank_test([1, 2, 3, 4, 5], [6, 7, 8, 9, 10]), (25, 1 / 252))
        self.assertEqual(ab.rank_test([1, 2, 3, 4, 6], [5, 7, 8, 9, 10]), (24, 2 / 252))
        self.assertEqual(ab.rank_test([6, 7, 8, 9, 10], [1, 2, 3, 4, 5]), (0, 1.0))
        self.assertEqual(ab.rank_test([5] * 5, [5] * 5), (12.5, 1.0))
        # Bonferroni 0.05/6: with five repetitions at most one slower main sample is tolerated.
        self.assertAlmostEqual(ab.scenario_alpha(CONFIG), 0.05 / 6)

    def test_median_mode_flip_without_rank_evidence_is_not_a_regression(self):
        # Hosted samples are bimodal (~36/~45 ms); one median flip is +25% and +9 ms.
        m = {name: ab.summarize([36, 45, 36, 45, 36], 5) for name in metrics(1)}
        p = {name: ab.summarize([45, 36, 45, 36, 45], 5) for name in metrics(1)}
        rows = ab.compare(m, p, CONFIG)
        self.assertEqual(failing(rows), [])
        self.assertEqual({row["verdict"] for row in rows}, {"not-significant"})

    def test_recorded_hosted_false_reds_pass(self):
        for run in HOSTED:
            with self.subTest(run=run["github_run"], change=run["change"]):
                confirmed = None
                for attempt in run["attempts"]:
                    found = set(failing(ab.compare(recorded(attempt["main"]), recorded(attempt["pr"]), CONFIG)))
                    confirmed = found if confirmed is None else confirmed & found
                    if not confirmed:
                        break
                self.assertEqual(confirmed, set())
        self.assertGreaterEqual(sum(run["old_result"] == "FAIL" and run["change"] != "editor change"
                                    for run in HOSTED), 5)

    def test_slowed_pr_on_recorded_hosted_samples_fails(self):
        # Forced-red sensitivity on the noisiest recorded sessions: +15 ms on the
        # PR's own samples is confirmed in both attempts on every run.
        for run in HOSTED:
            with self.subTest(run=run["github_run"]):
                confirmed = None
                for attempt in run["attempts"]:
                    found = set(failing(ab.compare(recorded(attempt["main"]), recorded(attempt["pr"], 15.0), CONFIG)))
                    confirmed = found if confirmed is None else confirmed & found
                self.assertTrue(confirmed)

    def test_absolute_ceiling(self):
        # Within the relative budget but crossing the ceiling, and measurably slower.
        rows = ab.compare(metrics(240), metrics(255), CONFIG)
        self.assertEqual({row["verdict"] for row in rows}, {"regression"})
        self.assertTrue(all(row["reasons"] == ["ceiling"] for row in rows))
        # Above the ceiling only through noise: not a regression.
        noisy = {name: ab.summarize([245, 255, 245, 255, 245], 5) for name in metrics(1)}
        flipped = {name: ab.summarize([255, 245, 255, 245, 255], 5) for name in metrics(1)}
        self.assertEqual(failing(ab.compare(noisy, flipped, CONFIG)), [])
        # Main itself over the ceiling cannot judge the PR.
        self.assertEqual({row["verdict"] for row in ab.compare(metrics(260), metrics(400), CONFIG)},
                         {"main-over-ceiling"})

    def test_policy_rejects_a_rank_test_that_could_never_reject(self):
        for change in ({"repetitions": 4}, {"repetitions": 9}, {"family_alpha": 0}, {"absolute_ceiling_ms": 0},
                       {"schema": 1}, {"max_attempts": 3}):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as folder:
                path = Path(folder) / "policy.json"
                path.write_text(json.dumps({**CONFIG, **change}))
                with self.assertRaises(ValueError):
                    ab.policy(path)

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
        self.assertEqual(len(report['confirmed_failures']), len(ab.SCENARIOS))
        self.assertEqual(measured.call_count, 2)

    def test_remeasurement_must_regress_the_same_scenario(self):
        first, second = metrics(20), metrics(20)
        names = sorted(first)
        slow_first, slow_second = dict(first), dict(second)
        slow_first[names[0]] = ab.summarize([30] * 5, 5)
        slow_second[names[1]] = ab.summarize([30] * 5, 5)
        code, report, measured = self.invoke([measured_pair(first, slow_first), measured_pair(second, slow_second)])
        self.assertEqual(code, 0)
        self.assertEqual(report['result'], 'PASS')
        self.assertEqual([a['result'] for a in report['attempts']], ['FAIL', 'FAIL'])
        self.assertEqual(report['confirmed_failures'], [])
        self.assertEqual(measured.call_count, 2)

    def test_main_over_ceiling_is_no_evidence_not_regression(self):
        code, report, measured = self.invoke([pair(260, 400)], '--report-only')
        self.assertEqual(code, 2)
        self.assertEqual(report['result'], 'NO_EVIDENCE')
        self.assertIn('absolute ceiling', report['error'])
        self.assertEqual(report['attempts'][0]['result'], 'NO_EVIDENCE')
        self.assertIn('metrics', report['attempts'][0])
        self.assertEqual(measured.call_count, 1)

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
