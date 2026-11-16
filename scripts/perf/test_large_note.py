"""#653: the large-note fixture stays pinned and the benchmark rejects unfired runs."""
import hashlib
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent / "large-note"))
import bench  # noqa: E402
import fixture  # noqa: E402


def run(**overrides):
    data = {
        "open_ms": 700., "first_frame_ms": 20., "blocks": 4500, "outline_rows": 2000,
        "scroll_start_item": 0, "scroll_end_item": 900,
        "scroll": {"frames": 240, "p50_ms": 5., "p95_ms": 8., "max_ms": 12.},
        "jump": {"frames": 32, "p50_ms": 5., "p95_ms": 9., "max_ms": 15.},
        "jumps": [{"outline_row": r, "target_block": r * 2, "top_block": r * 2, "landed_frame": 1} for r in (999, 1899, 499, 1499)],
    }
    data.update(overrides)
    return data


class LargeNote(unittest.TestCase):
    def test_note_is_pinned_and_has_the_stated_mix(self):
        data, counts = fixture.note()
        self.assertEqual(hashlib.sha256(data).hexdigest(), fixture.NOTE_SHA256)
        self.assertEqual(counts["lines"], 10_000)
        self.assertEqual(counts["headings"], 2_000)
        self.assertEqual(counts["links"], 500)
        self.assertEqual(counts["image_refs"], 200)
        self.assertEqual(data.count(b"\n| r249c0 value"), 4)

    def test_written_vault_resolves_its_targets(self):
        with tempfile.TemporaryDirectory() as work:
            path, counts = fixture.write(Path(work) / "v", check=True)
            self.assertEqual(path.read_bytes(), fixture.note()[0])
            self.assertEqual(len(list((Path(work) / "v/attachments").glob("*.png"))), fixture.IMAGE_FILES)
            self.assertTrue((Path(work) / "v/Targets/Target 099.md").exists())
            self.assertEqual(counts["sha256"], fixture.NOTE_SHA256)

    def test_controls_accept_a_complete_run(self):
        counts = fixture.note()[1]
        self.assertEqual(bench.controls(run(), counts), [])

    def test_controls_reject_unfired_or_wrong_runs(self):
        counts = fixture.note()[1]
        cases = {
            "blocks": run(blocks=10),
            "outline": run(outline_rows=436),
            "did not move": run(scroll_end_item=0),
            "jumps recorded": run(jumps=[]),
            "landed on": run(jumps=[{"outline_row": 999, "target_block": 1998, "top_block": 1500,
                                        "landed_frame": None}] * 4),
            "no timed frames": run(jump={"frames": 0, "p50_ms": 0, "p95_ms": 0, "max_ms": 0}),
        }
        for needle, data in cases.items():
            with self.subTest(needle):
                self.assertTrue(any(needle in problem for problem in bench.controls(data, counts)),
                                bench.controls(data, counts))

    def test_budgets_use_the_median_of_runs(self):
        counts = fixture.note()[1]
        slow = run(open_ms=5000.)
        summary, table = bench.summarize([run(), slow, run()], counts)
        self.assertEqual(summary["over"], [])
        summary, table = bench.summarize([slow, slow, run()], counts)
        self.assertEqual(summary["over"], ["open_ms"])
        self.assertIn("OVER", table)


if __name__ == "__main__":
    unittest.main()
