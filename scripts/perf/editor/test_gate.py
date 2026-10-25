"""Gate policy/cleanup tests; run on CT141, with no real app or collector launches."""

import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, call, patch

import gate


def summary(value=100, p90=None):
    return {"schema": 1, "boundary": gate.budget.BOUNDARY,
            "samples": {scenario: {metric: [9999, value, value, value, value,
                                           value if p90 is None else p90]
                                   for metric in metrics}
                        for scenario, metrics in gate.budget.SCENARIOS.items()},
            "support": {scenario: {metric: [9999, *([value] * 5)] for metric in metrics}
                        for scenario, metrics in gate.budget.SUPPORT_SCENARIOS.items()},
            "unsupported": list(gate.budget.UNSUPPORTED),
            "evidence": ["CT141/mock-collector/trace.log"]}


def baseline():
    return {"schema": 1, "boundary": gate.budget.BOUNDARY,
            "policy": dict(gate.budget.POLICY),
            "metrics": gate.budget.aggregate(summary())}


class GateTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.binary = self.root / "existing-binary"
        self.binary.write_bytes(b"existing binary, never rebuilt")
        self.original_hash = hashlib.sha256(self.binary.read_bytes()).hexdigest()
        self.base = self.root / "baseline.json"
        self.base.write_text(json.dumps(baseline()))
        self.out = self.root / "out"
        self.step_summary = self.root / "step-summary.md"
        self.step_summary.write_text("Earlier job evidence\n")
        self.now = 0.0
        self.clock = patch.object(gate.time, "monotonic", side_effect=lambda: self.now)
        self.clock.start()
        self.addCleanup(self.clock.stop)
        self.environment = patch.dict(os.environ, {}, clear=True)
        self.environment.start()
        self.addCleanup(self.environment.stop)

    def invoke(self, runner, extra=()):
        with patch.object(gate, "run_collector", side_effect=runner) as mocked:
            with contextlib.redirect_stdout(io.StringIO()):
                code = gate.main([str(self.binary), str(self.out), str(self.base), *extra])
        report = json.loads((self.out / "result.json").read_text()) if self.out.exists() else None
        return code, report, mocked

    def collector(self, *raw_summaries):
        pending = iter(raw_summaries)

        def run(command, log, deadline):
            raw = next(pending)
            attempt = Path(command[-1])
            attempt.mkdir()
            (attempt / "trace.log").write_text("preserve underlying trace\n")
            (attempt / "summary.json").write_text(json.dumps(raw))
            log.write_text("collector log\n")
            return 0, False

        return run

    def test_first_pass_runs_once_with_exact_resolved_binary(self):
        target = self.binary
        commit = "a" * 40
        (self.root / "BUILD.txt").write_text(f"commit {commit}\nother build details\n")
        self.binary = self.root / "binary-symlink"
        self.binary.symlink_to(target)
        code, report, runner = self.invoke(self.collector(summary()),
                                           ["--summary", str(self.step_summary)])
        self.assertEqual(code, 0)
        self.assertEqual(report["result"], "PASS")
        self.assertEqual(len(report["attempts"]), 1)
        self.assertEqual(report["binary_sha256"], self.original_hash)
        self.assertEqual(report["source_commit"], commit)
        self.assertEqual(report["policy"]["max_attempts"], 2)
        self.assertEqual(report["policy"]["total_seconds"], 295)
        self.assertEqual(report["policy"]["budget"], dict(gate.budget.POLICY))
        runner.assert_called_once_with(
            [sys.executable, str(gate.HERE / "probe.py"), str(target.resolve()),
             str(self.out / "attempt-1")], self.out / "attempt-1.collector.log", 295)
        attempt = report["attempts"][0]
        self.assertEqual(attempt["collector_exit"], 0)
        self.assertEqual(attempt["binary_sha256_before"], self.original_hash)
        self.assertEqual(attempt["binary_sha256_after"], self.original_hash)
        self.assertEqual(len(attempt["metrics"]), 20)
        self.assertEqual(attempt["failures"], [])
        text = self.step_summary.read_text()
        self.assertTrue(text.startswith("Earlier job evidence\n"))
        self.assertIn(self.original_hash, text)
        self.assertIn(commit, text)
        self.assertIn("GPU completion", text)
        self.assertIn("Unsupported (unmeasured; no PASS claim)", text)

    def test_missing_large_lp_support_is_not_allowed_to_pass_or_retry(self):
        raw = summary()
        raw["support"] = None
        code, report, runner = self.invoke(self.collector(raw))
        self.assertEqual(code, 2)
        self.assertEqual(report["result"], "NO_EVIDENCE")
        self.assertIn("support measurements", report["error"])
        self.assertEqual(runner.call_count, 1)

    def test_retry_remeasures_only_budgeted_scenarios_after_valid_full_lp_support(self):
        second = summary()
        second["support"] = None
        code, report, runner = self.invoke(self.collector(summary(200), second))
        self.assertEqual(code, 0)
        self.assertNotIn("--budgeted-only", runner.call_args_list[0].args[0])
        self.assertIn("--budgeted-only", runner.call_args_list[1].args[0])
        self.assertEqual(len(report["attempts"][0]["support_metrics"]), 12)
        self.assertEqual(report["attempts"][1]["support_metrics"], {})

    def test_regression_then_pass_retries_once_and_shows_both_measurements(self):
        code, report, runner = self.invoke(self.collector(summary(200, 220), summary(100, 120)),
                                           ["--summary", str(self.step_summary)])
        self.assertEqual(code, 0)
        self.assertEqual(runner.call_count, 2)
        self.assertEqual([row["exit_code"] for row in report["attempts"]], [1, 0])
        self.assertTrue(report["warning"])
        self.assertEqual(len(report["attempts"][0]["failures"]), 40)
        text = self.step_summary.read_text()
        self.assertIn("Attempt 1: FAIL", text)
        self.assertIn("Attempt 2: PASS", text)
        self.assertIn("**Warning:**", text)
        self.assertIn("| 200.000 | 220.000 |", text)
        self.assertIn("| 100.000 | 120.000 |", text)
        self.assertIn("220.000 > 135.000 ms", text)
        self.assertEqual(text.count("| source:tiny.md/input_frame_ms |"), 2)
        self.assertEqual(runner.call_args_list[0].args[0][2], runner.call_args_list[1].args[0][2])
        self.assertEqual(runner.call_args_list[1].args[0][-1], str(self.out / "attempt-2"))
        for number in (1, 2):
            self.assertEqual((self.out / f"attempt-{number}" / "trace.log").read_text(),
                             "preserve underlying trace\n")
            self.assertTrue((self.out / f"attempt-{number}" / "summary.json").exists())

    def test_two_valid_regressions_fail_with_no_third_attempt(self):
        code, report, runner = self.invoke(self.collector(summary(200), summary(180)))
        self.assertEqual((code, report["result"]), (1, "FAIL"))
        self.assertEqual(runner.call_count, 2)
        self.assertEqual([row["exit_code"] for row in report["attempts"]], [1, 1])
        self.assertEqual(report["attempts"][1]["metrics"]["live:tiny.md/input_frame_ms"],
                         {"median_ms": 180.0, "p90_ms": 180.0})
        self.assertFalse((self.out / "attempt-3").exists())

    def test_p90_only_regression_still_retries(self):
        code, report, runner = self.invoke(self.collector(summary(100, 136), summary(100)))
        self.assertEqual(code, 0)
        self.assertEqual(runner.call_count, 2)
        self.assertEqual({row["statistic"] for row in report["attempts"][0]["failures"]},
                         {"p90_ms"})

    def test_malformed_or_missing_evidence_never_retries(self):
        cases = [None, {}, summary(), summary(), summary()]
        cases[2]["evidence"] = []
        del cases[3]["samples"]["source:tiny.md"]
        cases[4]["samples"]["source:tiny.md"]["input_frame_ms"][0] = True
        for index, raw in enumerate(cases):
            with self.subTest(index=index):
                self.out = self.root / f"out-{index}"
                code, report, runner = self.invoke(self.collector(raw),
                                                   ["--summary", str(self.step_summary)])
                self.assertEqual((code, report["result"]), (2, "NO_EVIDENCE"))
                runner.assert_called_once()
                self.assertTrue(report["attempts"][0]["error"])

    def test_unparseable_missing_and_duplicate_json_never_retry(self):
        for index, data in enumerate((None, "{", '{"schema": 1, "schema": 1}')):
            with self.subTest(data=data):
                self.out = self.root / f"out-{index}"

                def collect(command, log, deadline):
                    attempt = Path(command[-1])
                    attempt.mkdir()
                    if data is not None:
                        (attempt / "summary.json").write_text(data)
                    return 0, False

                code, report, runner = self.invoke(collect)
                self.assertEqual(code, 2)
                runner.assert_called_once()
                self.assertEqual(report["attempts"][0]["exit_code"], 2)

    def test_collector_error_with_valid_summary_is_not_a_regression(self):
        collect = self.collector(summary(200))

        def failed(*args):
            collect(*args)
            return 7, False

        code, report, runner = self.invoke(failed)
        self.assertEqual(code, 2)
        runner.assert_called_once()
        self.assertEqual(report["attempts"][0]["collector_exit"], 7)
        self.assertEqual(report["attempts"][0]["metrics"], {})

    def test_spawn_failure_is_no_evidence_without_retry(self):
        code, report, runner = self.invoke(OSError("cannot spawn collector"))
        self.assertEqual(code, 2)
        runner.assert_called_once()
        self.assertIn("cannot spawn", report["error"])
        self.assertEqual(report["attempts"][0]["binary_sha256_after"], self.original_hash)

    def test_invalid_baseline_fails_before_collector_and_appends_error_summary(self):
        for index, raw in enumerate(("{", "{}", json.dumps(dict(baseline(), policy={})))):
            with self.subTest(raw=raw):
                self.out = self.root / f"out-{index}"
                self.base.write_text(raw)
                code, report, runner = self.invoke(self.collector(summary()),
                                                   ["--summary", str(self.step_summary)])
                self.assertEqual(code, 2)
                runner.assert_not_called()
                self.assertEqual(report["attempts"], [])
                self.assertIn("NO EVIDENCE", self.step_summary.read_text())
        self.assertTrue(self.step_summary.read_text().startswith("Earlier job evidence\n"))

    def test_binary_mutation_after_attempt_refuses_retry_even_with_valid_regression(self):
        collect = self.collector(summary(200))

        def mutate(*args):
            result = collect(*args)
            self.binary.write_bytes(b"different binary")
            return result

        code, report, runner = self.invoke(mutate)
        self.assertEqual(code, 2)
        runner.assert_called_once()
        self.assertIn("SHA256 changed", report["error"])
        self.assertEqual(report["binary_sha256"], self.original_hash)

    def test_binary_checked_before_first_and_second_attempt(self):
        for failing_check, launches in ((2, 0), (4, 1)):
            with self.subTest(failing_check=failing_check):
                self.out = self.root / f"out-{failing_check}"
                hashes = [self.original_hash] * (failing_check - 1) + ["changed"]
                with patch.object(gate, "sha256", side_effect=hashes):
                    code, report, runner = self.invoke(self.collector(summary(200)))
                self.assertEqual(code, 2)
                self.assertEqual(runner.call_count, launches)
                self.assertIn("SHA256 changed", report["error"])

    def test_second_infrastructure_failure_cannot_be_reported_as_budget_failure(self):
        collect = self.collector(summary(200))
        invocations = 0

        def fail_second(*args):
            nonlocal invocations
            invocations += 1
            return collect(*args) if invocations == 1 else (2, False)

        code, report, runner = self.invoke(fail_second)
        self.assertEqual((code, report["result"]), (2, "NO_EVIDENCE"))
        self.assertEqual(runner.call_count, 2)
        self.assertEqual([row["exit_code"] for row in report["attempts"]], [1, 2])

    def test_both_attempts_share_original_deadline(self):
        collect = self.collector(summary(200), summary())
        remaining = []

        def advancing(command, log, deadline):
            remaining.append(deadline - self.now)
            self.now += 180 if len(remaining) == 1 else 50
            return collect(command, log, deadline)

        code, report, runner = self.invoke(advancing)
        self.assertEqual(code, 0)
        self.assertEqual(remaining, [295, 115])
        self.assertEqual([row.args[2] for row in runner.call_args_list], [295, 295])
        self.assertEqual(report["duration_seconds"], 230)

    def test_exhausted_deadline_never_launches_second_collector(self):
        collect = self.collector(summary(200))

        def slow(*args):
            self.now = 293
            return collect(*args)

        code, report, runner = self.invoke(slow)
        self.assertEqual(code, 2)
        runner.assert_called_once()
        self.assertIn("deadline", report["error"])
        self.assertEqual([row["exit_code"] for row in report["attempts"]], [1, 2])

    def test_timeout_or_overdue_valid_summary_never_retries(self):
        for timed_out in (False, True):
            with self.subTest(timed_out=timed_out):
                self.out = self.root / f"out-{timed_out}"
                self.now = 0
                collect = self.collector(summary(200))

                def expired(*args):
                    collect(*args)
                    self.now = 295
                    return (-9 if timed_out else 0), timed_out

                code, report, runner = self.invoke(expired)
                self.assertEqual(code, 2)
                runner.assert_called_once()
                self.assertIn("deadline", report["error"])

    def test_existing_output_is_untouched(self):
        self.out.mkdir()
        sentinel = self.out / "result.json"
        sentinel.write_text('{"existing": true}')
        code, report, runner = self.invoke(self.collector(summary()))
        self.assertEqual(code, 2)
        self.assertEqual(report, {"existing": True})
        runner.assert_not_called()
        self.assertEqual(list(self.out.iterdir()), [sentinel])

    def test_environment_summary_defaults_and_explicit_path_takes_precedence(self):
        os.environ["GITHUB_STEP_SUMMARY"] = str(self.step_summary)
        code, report, runner = self.invoke(self.collector(summary()))
        self.assertEqual(code, 0)
        self.assertIn("Editor latency gate", self.step_summary.read_text())
        old_text = self.step_summary.read_text()
        self.out = self.root / "explicit-out"
        other = self.root / "explicit-summary.md"
        code, report, runner = self.invoke(self.collector(summary()), ["--summary", str(other)])
        self.assertEqual(code, 0)
        self.assertEqual(self.step_summary.read_text(), old_text)
        self.assertIn("Editor latency gate", other.read_text())

    def test_summary_cannot_overwrite_input_result_or_evidence(self):
        for index, path in enumerate((self.binary, self.base, self.root / "out-2" / "result.json",
                                      self.root / "out-3" / "attempt-1" / "summary.json")):
            with self.subTest(path=path):
                self.out = self.root / f"out-{index}"
                code, report, runner = self.invoke(self.collector(summary()), ["--summary", str(path)])
                self.assertEqual(code, 2)
                runner.assert_not_called()
        self.assertEqual(gate.sha256(self.binary), self.original_hash)
        self.assertEqual(json.loads(self.base.read_text()), baseline())

    def test_summary_write_error_fails_closed_and_updates_result(self):
        missing = self.root / "missing-parent" / "summary.md"
        code, report, runner = self.invoke(self.collector(summary()), ["--summary", str(missing)])
        self.assertEqual((code, report["result"]), (2, "NO_EVIDENCE"))
        self.assertEqual(report["attempts"][0]["result"], "PASS")
        self.assertIsNotNone(report["error"])

    def test_invalid_build_receipt_never_launches_collector(self):
        (self.root / "BUILD.txt").write_text("commit not-a-source-sha\n")
        code, report, runner = self.invoke(self.collector(summary()))
        self.assertEqual(code, 2)
        runner.assert_not_called()
        self.assertIn("BUILD.txt", report["error"])

    def test_environment_cannot_change_attempt_count_deadline_or_budget(self):
        os.environ.update({"MAX_ATTEMPTS": "1", "TOTAL_SECONDS": "9999",
                           "OKILUM_EDITOR_RELATIVE_BUDGET": "9999"})
        code, report, runner = self.invoke(self.collector(summary(200), summary(200)))
        self.assertEqual(code, 1)
        self.assertEqual(runner.call_count, 2)
        self.assertEqual([row.args[2] for row in runner.call_args_list], [295, 295])
        self.assertEqual(report["attempts"][0]["failures"][0]["limit_ms"], 135)


class CollectorProcessTests(unittest.TestCase):
    def test_timeout_terminates_then_kills_entire_group_and_reaps_collector(self):
        proc = Mock(pid=4242, returncode=-9)
        proc.wait.side_effect = [subprocess.TimeoutExpired("collector", 291),
                                 subprocess.TimeoutExpired("collector", 1), -9]
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(gate.subprocess, "Popen", return_value=proc) as popen:
                with patch.object(gate.os, "killpg") as killpg:
                    with patch.object(gate.time, "monotonic", side_effect=[0, 1, 292, 293]):
                        result = gate.run_collector(["python", "probe.py", "binary", "out"],
                                                    Path(directory) / "collector.log", 295)
        self.assertEqual(result, (-9, True))
        self.assertTrue(popen.call_args.kwargs["start_new_session"])
        self.assertEqual(popen.call_args.kwargs["stderr"], subprocess.STDOUT)
        self.assertEqual(killpg.call_args_list, [call(4242, signal.SIGTERM),
                                                call(4242, signal.SIGKILL)])
        self.assertEqual(proc.wait.call_args_list, [call(timeout=291), call(timeout=1.0),
                                                    call(timeout=2)])

    def test_kills_descendants_even_if_collector_exits_on_term(self):
        proc = Mock(pid=4242, returncode=-15)
        proc.wait.side_effect = [subprocess.TimeoutExpired("collector", 10), -15, -15]
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(gate.subprocess, "Popen", return_value=proc):
                with patch.object(gate.os, "killpg") as killpg:
                    with patch.object(gate.time, "monotonic", return_value=0):
                        self.assertEqual(gate.run_collector(["collector"],
                                         Path(directory) / "collector.log", 20), (-15, True))
        self.assertEqual(killpg.call_args_list, [call(4242, signal.SIGTERM),
                                                call(4242, signal.SIGKILL)])

    def test_no_process_spawn_without_cleanup_time(self):
        with patch.object(gate.time, "monotonic", return_value=293):
            with patch.object(gate.subprocess, "Popen") as popen:
                with self.assertRaises(gate.NoEvidence):
                    gate.run_collector(["collector"], Path("unused.log"), 295)
        popen.assert_not_called()


if __name__ == "__main__":
    unittest.main()
