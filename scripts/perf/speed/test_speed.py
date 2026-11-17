import json
from pathlib import Path
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import collect  # noqa: E402
import judge  # noqa: E402
import notes  # noqa: E402


def measured(median):
    return {"status": "measured", "median_ms": median, "p90_ms": median + 1}


def search_report(**changes):
    report = {"schema": 1, "notes": 10_000, "runs": 30, "source_bytes": 1, "scan_ms": 1,
              "build_ms": 1, "open_ms": 1,
              "queries": [{"query": "a", "hits": 100, "first_ms": 9, "samples_ms": [4.0] * 30},
                          {"query": "b", "hits": 1, "first_ms": 9, "samples_ms": [6.0] * 26 + [60.0] * 4}]}
    report.update(changes)
    return report


class Budgets(unittest.TestCase):
    def test_committed_budgets_are_complete_and_above_the_product_budget(self):
        budgets = judge.load_budgets(HERE / "budgets.json")
        self.assertEqual(
            {name: row["product_budget_ms"] for name, row in budgets["metrics"].items()},
            {"cold_start_ms": 300, "typing_ms": 16, "open_10mib_ms": 1000, "search_10k_ms": 100})

    def test_refuses_missing_metric_and_ceiling_below_product(self):
        good = json.loads((HERE / "budgets.json").read_text())
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "b.json"
            del good["metrics"]["typing_ms"]
            path.write_text(json.dumps(good))
            with self.assertRaises(ValueError):
                judge.load_budgets(path)
            good = json.loads((HERE / "budgets.json").read_text())
            good["metrics"]["search_10k_ms"]["hosted_limit_ms"] = 50
            path.write_text(json.dumps(good))
            with self.assertRaises(ValueError):
                judge.load_budgets(path)


class Verdict(unittest.TestCase):
    budgets = judge.load_budgets(HERE / "budgets.json")

    def summary(self, **override):
        metrics = {name: measured(1) for name in judge.METRICS}
        metrics.update(override)
        return {"metrics": metrics, "source_commit": "abc"}

    def run_main(self, summary, *flags):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "s.json"
            path.write_text(json.dumps(summary))
            return judge.main([str(path), *flags])

    def test_pass(self):
        self.assertEqual(judge.judge(self.summary(), self.budgets)["result"], "PASS")
        self.assertEqual(self.run_main(self.summary(), "--enforce"), 0)

    def test_over_product_only_is_reported_not_failed(self):
        report = judge.judge(self.summary(typing_ms=measured(17)), self.budgets)
        self.assertEqual((report["result"], report["over_product"]), ("PASS", ["typing_ms"]))

    def test_hosted_ceiling_fails_but_exits_zero_unless_enforced(self):
        limit = self.budgets["metrics"]["search_10k_ms"]["hosted_limit_ms"]
        summary = self.summary(search_10k_ms=measured(limit + 0.1))
        self.assertEqual(judge.judge(summary, self.budgets)["result"], "FAIL")
        self.assertEqual(self.run_main(summary), 0)
        self.assertEqual(self.run_main(summary, "--enforce"), 1)

    def test_missing_evidence_exits_2_in_every_mode(self):
        summary = self.summary(open_10mib_ms={"status": "no_evidence", "reason": "x"})
        self.assertEqual(judge.judge(summary, self.budgets)["result"], "NO_EVIDENCE")
        self.assertEqual(self.run_main(summary), 2)
        self.assertEqual(self.run_main(summary, "--enforce"), 2)
        absent = self.summary()
        del absent["metrics"]["cold_start_ms"]
        self.assertEqual(self.run_main(absent), 2)
        self.assertEqual(self.run_main(self.summary(typing_ms=measured(float("nan")))), 2)


class Search(unittest.TestCase):
    def test_slowest_query_median_is_the_metric(self):
        metric = collect.search_metric(search_report())
        self.assertEqual((metric["slowest_query"], metric["median_ms"], metric["p90_ms"]), ("b", 6.0, 60.0))

    def test_refuses_wrong_corpus_or_short_series(self):
        for bad in (search_report(notes=9_999), search_report(runs=10), search_report(queries=[]),
                    search_report(queries=[{"query": "a", "hits": 1, "first_ms": 1, "samples_ms": [1.0] * 29}])):
            with self.assertRaises(ValueError):
                collect.search_metric(bad)


class App(unittest.TestCase):
    files = {notes.SMALL: b"tiny", notes.LARGE: b"large"}

    def fake(self, fail=None):
        calls = []

        def run_once(binary, base, name, live, rnd, deadline, **options):
            calls.append((name, rnd, options))
            if fail and fail == (name, rnd):
                raise collect.probe.NoEvidence("control failed")
            if options.get("open_only"):
                return {"open_first_ms": 900.0 + rnd}, {"rep": rnd}
            return {"open_first_ms": 200.0 + rnd, "input_frame_ms": 30.0 + rnd}, {"rep": rnd}
        return calls, run_once

    def test_interleaves_rounds_and_drops_the_warmup(self):
        calls, run_once = self.fake()
        metrics, _, errors = collect.app_metrics(Path("bin"), Path("out"), self.files, 0, run_once)
        self.assertEqual(errors, {})
        self.assertEqual([c[:2] for c in calls[:4]],
                         [(notes.SMALL, 0), (notes.LARGE, 0), (notes.SMALL, 1), (notes.LARGE, 1)])
        self.assertTrue(calls[0][2]["keep_open"] and calls[0][2]["input_only"])
        self.assertTrue(calls[1][2]["open_only"])
        self.assertEqual(calls[1][2]["source"], b"large")
        self.assertEqual(metrics["cold_start_ms"]["samples_ms"], [201.0, 202.0, 203.0, 204.0, 205.0])
        self.assertEqual(metrics["typing_ms"]["median_ms"], 33.0)
        # Paired per round: the shared startup cancels out.
        self.assertEqual(metrics["open_10mib_ms"]["samples_ms"], [700.0] * 5)
        self.assertEqual(metrics["open_10mib_ms"]["absolute"]["median_ms"], 903.0)

    def test_a_failed_control_marks_only_the_dependent_metrics(self):
        _, run_once = self.fake(fail=(notes.LARGE, 3))
        metrics, _, errors = collect.app_metrics(Path("bin"), Path("out"), self.files, 0, run_once)
        self.assertIn("large", errors)
        self.assertEqual(metrics["open_10mib_ms"]["status"], "no_evidence")
        self.assertEqual(metrics["typing_ms"]["status"], "measured")
        _, run_once = self.fake(fail=(notes.SMALL, 0))
        metrics, _, _ = collect.app_metrics(Path("bin"), Path("out"), self.files, 0, run_once)
        self.assertEqual({m["status"] for m in metrics.values()}, {"no_evidence"})


class Notes(unittest.TestCase):
    def test_pinned_notes(self):
        files = notes.checked()
        self.assertEqual(len(files[notes.LARGE]), 10 * 1024 * 1024)
        self.assertEqual(files[notes.SMALL], collect.probe.fixtures.notes()[notes.SMALL])


if __name__ == "__main__":
    unittest.main()
