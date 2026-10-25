"""Contract and regression tests for the Linux editor latency budget (#1639).

Run with stdlib unittest on the calibration runner; no application is launched.
"""

import contextlib
import copy
import importlib.util
import io
import json
import math
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("editor_budget", HERE / "budget.py")
budget = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(budget)

FIXTURES = ("tiny.md", "sections-460.md", "one-mib.md", "novel-1200k.md")
LIVE_FIXTURES = ("tiny.md", "raw-tail-60k.md")
METRICS = ("open_first_ms", "input_frame_ms", "scroll_frame_ms")


def summary(value=100):
    samples = {f"source:{name}": {metric: [9999, *([value] * 5)] for metric in METRICS}
               for name in FIXTURES}
    samples.update({f"live:{name}": {metric: [9999, *([value] * 5)]
                                     for metric in (*METRICS, "open_full_ms")}
                    for name in LIVE_FIXTURES})
    return {"schema": 1, "boundary": "xvfb-software-post-frame", "samples": samples,
            "support": {f"live:{name}": {metric: [9999, *([value] * 5)]
                         for metric in (*METRICS, "open_full_ms")} for name in FIXTURES[1:]},
            "unsupported": [],
            "evidence": ["CT141/run/source-and-live-traces"]}


def baseline(value=100):
    return {"schema": 1, "boundary": "xvfb-software-post-frame",
            "policy": {"relative": 0.35, "absolute_ms": 25.0, "edit_floor_ms": 16.0},
            "metrics": budget.aggregate(summary(value))}


def replace_samples(raw, key, values):
    scenario, metric = key.split("/")
    raw["samples"][scenario][metric] = values


class AggregateTests(unittest.TestCase):
    def test_exact_scope_and_distribution_with_discarded_warmup(self):
        raw = summary()
        raw["unsupported"].reverse()
        replace_samples(raw, "source:tiny.md/open_first_ms", [100000, 20, 1, 9, 2, 3])
        original = copy.deepcopy(raw)
        result = budget.aggregate(raw)
        expected = {f"source:{name}/{metric}" for name in FIXTURES for metric in METRICS}
        expected.update(f"live:{name}/{metric}" for name in LIVE_FIXTURES
                        for metric in (*METRICS, "open_full_ms"))
        self.assertEqual(set(result), expected)
        self.assertEqual(len(result), 20)
        self.assertEqual(budget.METRIC_KEYS, expected)
        self.assertEqual(budget.UNSUPPORTED, ())
        self.assertEqual(len(budget.aggregate_support(raw, required=True)), 12)
        self.assertEqual(result["source:tiny.md/open_first_ms"], {"median_ms": 3.0, "p90_ms": 20.0})
        self.assertEqual(raw, original)

    def test_warmup_can_be_zero_or_large_but_never_invalid(self):
        for warmup in (0, 1e200):
            raw = summary(0)
            replace_samples(raw, "live:tiny.md/open_full_ms", [warmup, 0, 0, 0, 0, 0])
            self.assertEqual(budget.aggregate(raw)["live:tiny.md/open_full_ms"],
                             {"median_ms": 0.0, "p90_ms": 0.0})

    def test_all_six_samples_must_be_finite_nonnegative_numbers(self):
        for index in range(6):
            for invalid in (True, False, None, "5", -1, math.nan, math.inf, -math.inf, 10 ** 400):
                with self.subTest(index=index, invalid=invalid):
                    raw = summary()
                    raw["samples"]["source:tiny.md"]["input_frame_ms"][index] = invalid
                    with self.assertRaises(ValueError):
                        budget.aggregate(raw)

    def test_sample_count_and_container_are_strict(self):
        for invalid in ([], [1] * 5, [1] * 7, (1,) * 6, "111111", {}, None):
            with self.subTest(invalid=invalid):
                raw = summary()
                replace_samples(raw, "live:tiny.md/open_full_ms", invalid)
                with self.assertRaises(ValueError):
                    budget.aggregate(raw)

    def test_missing_extra_and_wrong_contract_fields_are_rejected(self):
        cases = []
        for field in summary():
            raw = summary()
            del raw[field]
            cases.append(raw)
        for field, value in (("schema", True), ("schema", 1.0), ("schema", 2),
                             ("boundary", "native"), ("samples", {}), ("samples", []),
                             ("evidence", []), ("evidence", None), ("evidence", "trace.log"),
                             ("unsupported", ["live:one-mib.md"]), ("unsupported", None),
                             ("unsupported", [None] * 3),
                             ("unsupported", ["live:sections-460.md"] * 3)):
            raw = summary()
            raw[field] = value
            cases.append(raw)
        raw = summary()
        raw["extra"] = True
        cases.append(raw)
        for scenario in summary()["samples"]:
            raw = summary()
            del raw["samples"][scenario]
            cases.append(raw)
            for metric in summary()["samples"][scenario]:
                raw = summary()
                del raw["samples"][scenario][metric]
                cases.append(raw)
        raw = summary()
        raw["samples"]["source:unknown.md"] = raw["samples"].pop("source:tiny.md")
        cases.append(raw)
        raw = summary()
        raw["samples"]["live:one-mib.md"] = copy.deepcopy(raw["samples"]["live:tiny.md"])
        cases.append(raw)
        raw = summary()
        raw["samples"]["source:raw-tail-60k.md"] = copy.deepcopy(raw["samples"]["source:tiny.md"])
        cases.append(raw)
        raw = summary()
        raw["unsupported"].append("live:raw-tail-60k.md")
        cases.append(raw)
        raw = summary()
        raw["samples"]["source:tiny.md"]["open_full_ms"] = [1] * 6
        cases.append(raw)
        raw = summary()
        raw["samples"]["source:tiny.md"] = []
        cases.append(raw)
        cases.extend((None, [], {}))
        for index, raw in enumerate(cases):
            with self.subTest(case=index), self.assertRaises(ValueError):
                budget.aggregate(raw)

    def test_support_missing_or_malformed_never_proves_full_lp(self):
        for invalid in ({}, None, {"live:one-mib.md": {}}):
            raw = summary()
            raw["support"] = invalid
            with self.assertRaises(ValueError):
                budget.aggregate_support(raw, required=True)
        raw = summary()
        raw["support"]["live:one-mib.md"]["input_frame_ms"] = [1] * 5
        with self.assertRaises(ValueError):
            budget.aggregate_support(raw, required=True)
        raw = summary()
        raw["support"] = None
        self.assertEqual(budget.aggregate_support(raw), {})



class BaselineTests(unittest.TestCase):
    def test_complete_zero_and_positive_baselines_are_valid_and_unmodified(self):
        for value in (0, 0.5, 100):
            old = baseline(value)
            saved = copy.deepcopy(old)
            self.assertEqual(budget.validate_baseline(old), saved)
            self.assertEqual(old, saved)

    def test_each_statistic_rejects_invalid_values_and_inverted_distribution(self):
        for key in baseline()["metrics"]:
            for stat in ("median_ms", "p90_ms"):
                for invalid in (True, False, -1, math.nan, math.inf, -math.inf, "100", None):
                    with self.subTest(key=key, stat=stat, invalid=invalid):
                        old = baseline()
                        old["metrics"][key][stat] = invalid
                        with self.assertRaises(ValueError):
                            budget.validate_baseline(old)
            old = baseline()
            old["metrics"][key]["p90_ms"] = 99
            with self.assertRaises(ValueError):
                budget.validate_baseline(old)

    def test_full_schema_policy_and_key_sets_are_immutable(self):
        cases = [None, [], {}]
        for field in baseline():
            old = baseline()
            del old[field]
            cases.append(old)
        for field, value in (("schema", True), ("schema", 1.0), ("schema", 2),
                             ("boundary", "native"), ("metrics", []), ("policy", [])):
            old = baseline()
            old[field] = value
            cases.append(old)
        for field in baseline()["policy"]:
            for value in (0, 999, True, math.nan):
                old = baseline()
                old["policy"][field] = value
                cases.append(old)
            old = baseline()
            del old["policy"][field]
            cases.append(old)
        for location in ((), ("policy",), ("metrics",), ("metrics", "source:tiny.md/open_first_ms")):
            old = baseline()
            node = old
            for part in location:
                node = node[part]
            node["extra"] = 0
            cases.append(old)
        for key in baseline()["metrics"]:
            old = baseline()
            del old["metrics"][key]
            cases.append(old)
        for stat in ("median_ms", "p90_ms"):
            old = baseline()
            del old["metrics"]["source:tiny.md/open_first_ms"][stat]
            cases.append(old)
        for index, old in enumerate(cases):
            with self.subTest(case=index), self.assertRaises(ValueError):
                budget.validate_baseline(old)


class VerdictTests(unittest.TestCase):
    def test_each_real_metric_slowdown_fails_independently(self):
        old = baseline()
        self.assertEqual(budget.verdict(old, budget.aggregate(summary())), [])  # Positive control.
        for key in old["metrics"]:
            with self.subTest(key=key):
                raw = summary()
                replace_samples(raw, key, [9999, 200, 200, 200, 200, 200])
                failures = budget.verdict(old, budget.aggregate(raw))
                self.assertEqual({failure["metric"] for failure in failures}, {key})
                self.assertEqual({failure["statistic"] for failure in failures}, {"median_ms", "p90_ms"})

    def test_p90_only_regression_is_not_hidden_by_stable_median(self):
        for key in baseline()["metrics"]:
            with self.subTest(key=key):
                raw = summary()
                replace_samples(raw, key, [9999, 100, 100, 100, 100, 136])
                failures = budget.verdict(baseline(), budget.aggregate(raw))
                self.assertEqual(len(failures), 1)
                self.assertEqual(failures[0]["metric"], key)
                self.assertEqual(failures[0]["statistic"], "p90_ms")
                self.assertEqual(failures[0]["limit_ms"], 135)

    def test_relative_absolute_and_input_floor_boundaries_are_inclusive(self):
        for base, limit in ((100, 135), (20, 45), (0, 25)):
            for key in baseline()["metrics"]:
                with self.subTest(base=base, key=key):
                    raw = summary(base)
                    replace_samples(raw, key, [9999, *([limit] * 5)])
                    self.assertEqual(budget.verdict(baseline(base), budget.aggregate(raw)), [])
                    replace_samples(raw, key, [9999, *([math.nextafter(limit, math.inf)] * 5)])
                    self.assertEqual(len(budget.verdict(baseline(base), budget.aggregate(raw))), 2)
        # For nonnegative baselines, the 25 ms absolute allowance already exceeds
        # the 16 ms input floor. The floor must never narrow that allowance.
        for observed in (16, 25):
            raw = summary(0)
            replace_samples(raw, "source:tiny.md/input_frame_ms", [0, *([observed] * 5)])
            self.assertEqual(budget.verdict(baseline(0), budget.aggregate(raw)), [])

    def test_statistics_use_their_own_baseline_and_validate_observed_sets(self):
        old = baseline()
        key = "source:tiny.md/open_first_ms"
        old["metrics"][key]["p90_ms"] = 200
        raw = summary()
        replace_samples(raw, key, [9999, 135, 135, 135, 135, 270])
        self.assertEqual(budget.verdict(old, budget.aggregate(raw)), [])
        observed = budget.aggregate(raw)
        del observed[key]
        with self.assertRaises(ValueError):
            budget.verdict(old, observed)

    def test_environment_cannot_loosen_the_gate(self):
        with patch.dict(os.environ, {"RELATIVE": "99", "ABSOLUTE_MS": "99999",
                                     "EDIT_FLOOR_MS": "99999", "EDITOR_BUDGET_RELATIVE": "99"}):
            self.assertTrue(budget.verdict(baseline(), budget.aggregate(summary(200))))


class RatchetTests(unittest.TestCase):
    def test_equal_or_tighter_stats_are_accepted_without_mutating_inputs(self):
        old = baseline()
        observed = baseline(99)
        original = copy.deepcopy(observed)
        proposed = budget.tighten(old, observed)
        self.assertEqual(proposed, original)
        proposed["metrics"]["source:tiny.md/open_first_ms"]["p90_ms"] = 0
        self.assertEqual(observed, original)
        self.assertEqual(budget.tighten(old, old), old)
        self.assertEqual(budget.tighten(baseline(0), baseline(0)), baseline(0))

    def test_any_increase_is_rejected_even_inside_the_check_allowance(self):
        old = baseline()
        for row in old["metrics"].values():
            row["p90_ms"] = 120
        for key in old["metrics"]:
            for statistic in ("median_ms", "p90_ms"):
                with self.subTest(key=key, statistic=statistic):
                    observed = copy.deepcopy(old)
                    observed["metrics"][key][statistic] += 0.01
                    self.assertEqual(budget.verdict(old, observed["metrics"]), [])
                    with self.assertRaises(budget.RelaxationError):
                        budget.tighten(old, observed)

    def test_both_full_baselines_are_validated_before_ratcheting(self):
        for field, value in (("schema", 2), ("boundary", "native"),
                             ("policy", {"relative": 0.5, "absolute_ms": 25, "edit_floor_ms": 16}),
                             ("metrics", {})):
            for side in (0, 1):
                with self.subTest(field=field, side=side):
                    pair = [baseline(), baseline(99)]
                    pair[side][field] = value
                    with self.assertRaises(ValueError):
                        budget.tighten(*pair)


class CliTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.old = self.write("old.json", baseline())
        self.raw = self.write("summary.json", summary())

    def write(self, name, value):
        path = self.root / name
        path.write_text(json.dumps(value), encoding="utf-8")
        return path

    def cli(self, *arguments):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = budget.main([str(argument) for argument in arguments])
        return code, output.getvalue()

    def test_check_pass_and_regression_write_scoped_aggregate_reports(self):
        report = self.root / "report.json"
        for value, expected_code in ((100, 0), (200, 1)):
            self.write("summary.json", summary(value))
            code, text = self.cli("check", self.raw, self.old, "--report", report)
            self.assertEqual(code, expected_code, text)
            self.assertIn("xvfb-software-post-frame", text)
            self.assertIn("software CPU cost", text)
            for fixture in FIXTURES[1:]:
                self.assertIn(f"MEASURED WITHOUT STATIC BUDGET: live:{fixture}", text)
            saved = json.loads(report.read_text())
            self.assertEqual(saved["boundary"], budget.BOUNDARY)
            self.assertEqual(saved["unsupported"], list(budget.UNSUPPORTED))
            self.assertEqual(saved["metrics"], budget.aggregate(summary(value)))
            self.assertEqual(saved["evidence"], summary()["evidence"])
            self.assertEqual(bool(saved["failures"]), expected_code == 1)

    def test_no_evidence_missing_or_malformed_input_returns_two(self):
        for value in ({}, None, summary() | {"evidence": []}, summary() | {"samples": {}}):
            self.write("summary.json", value)
            code, text = self.cli("check", self.raw, self.old)
            self.assertEqual(code, 2, text)
            self.assertIn("NO EVIDENCE", text)
        for malformed in ('{"schema": 1, "schema": 1}', '{"samples": NaN}', 'not json'):
            self.raw.write_text(malformed)
            self.assertEqual(self.cli("check", self.raw, self.old)[0], 2)
        self.raw.unlink()
        self.assertEqual(self.cli("check", self.raw, self.old)[0], 2)
        self.write("summary.json", summary())
        self.old.write_text("{}")
        self.assertEqual(self.cli("check", self.raw, self.old)[0], 2)

    def test_update_uses_summary_only_and_can_replace_validated_baseline_atomically(self):
        self.write("summary.json", summary(99))
        code, text = self.cli("update", self.old, self.raw, "--output", self.old)
        self.assertEqual(code, 0, text)
        self.assertEqual(json.loads(self.old.read_text()), baseline(99))

    def test_update_refusal_never_changes_existing_or_creates_missing_output(self):
        output = self.root / "proposal.json"
        for exists in (False, True):
            for value, expected_code in ((101, 1), (-1, 2)):
                with self.subTest(exists=exists, value=value):
                    if exists:
                        output.write_bytes(b"keep this proposal byte-for-byte\n")
                    self.write("summary.json", summary(value))
                    code, text = self.cli("update", self.old, self.raw, "--output", output)
                    self.assertEqual(code, expected_code, text)
                    if exists:
                        self.assertEqual(output.read_bytes(), b"keep this proposal byte-for-byte\n")
                    else:
                        self.assertFalse(output.exists())
        self.assertEqual(json.loads(self.old.read_text()), baseline())

    def test_ratchet_checks_both_full_baselines_without_changing_them(self):
        new = self.write("new.json", baseline(99))
        for value, expected_code in ((99, 0), (100, 0), (101, 1)):
            self.write("new.json", baseline(value))
            before = new.read_bytes()
            code, text = self.cli("ratchet", self.old, new)
            self.assertEqual(code, expected_code, text)
            self.assertEqual(new.read_bytes(), before)
        for side in (self.old, new):
            saved = side.read_bytes()
            side.write_text("{}")
            self.assertEqual(self.cli("ratchet", self.old, new)[0], 2)
            side.write_bytes(saved)

    def test_report_cannot_overwrite_inputs(self):
        for destination in (self.old, self.raw):
            saved = destination.read_bytes()
            self.assertEqual(self.cli("check", self.raw, self.old, "--report", destination)[0], 2)
            self.assertEqual(destination.read_bytes(), saved)


if __name__ == "__main__":
    unittest.main()
