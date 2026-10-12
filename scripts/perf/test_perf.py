"""The scroll probe's own tests (#1290, #1004): the notes are the same bytes everywhere and the
analyzer reads a trace the way scripts/perf/frame-trace/patch.py writes it."""
import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class NotesTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.gen = load(HERE / "scroll/gen-notes.py", "gen_notes")

    def test_notes_match_the_pinned_manifest(self):
        manifest = json.loads((HERE / "scroll/manifest.json").read_text())
        notes = self.gen.notes()
        self.assertEqual(manifest, {name: self.gen.describe(data) for name, data in notes.items()})

    def test_the_24k_note_is_the_powershell_one(self):
        data = self.gen.big_note()
        self.assertEqual((data.count(b"\r\n"), data.count(b"\n")), (24000, 24000), "CRLF only")
        self.assertTrue(data.startswith(b"## Section 1\r\n\r\nParagraph 1 with **bold**, *italic*, `code` and a [[link 1]]. "))
        self.assertTrue(data.endswith(b"| 1500 | 3000 |\r\n\r\n"))

    def test_long_notes_are_the_1004_corpus(self):
        # What the #1004 comments recorded of the Hedva vault's notes: 10,000 lines / 872 KB /
        # 436 headings and 700 lines / 63 KB / 32 headings, LF only.
        long_10k, long_700 = self.gen.corpus_1004()
        for data, lines, headings, kib in ((long_10k, 10_000, 436, 872), (long_700, 700, 32, 63)):
            self.assertEqual((data.count(b"\n"), data.count(b"\r")), (lines, 0))
            self.assertEqual(self.gen.describe(data)["headings"], headings)
            self.assertEqual(round(len(data) / 1000), kib)
        self.assertEqual((long_10k, long_700), self.gen.corpus_1004(), "seeded: the same every time")


class AnalyzerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.analyze = load(HERE / "scroll/analyze.py", "analyze")

    def frame(self, t, n, draws=1, draw=70000, present=4000):
        return (f"[frame] t={t} n={n} gap=80000 draws={draws} draw={draw} req=10000 layout=20000 "
                f"prepaint=5000 paint=30000 post={draw - 45000} present={present} cache=15000 shape=2000 "
                f"cache_calls=900 shapes=3 quads=800 glyphs=6000 paths=0 underlines=0")

    def run_dir(self, lines):
        root = tempfile.TemporaryDirectory()
        self.addCleanup(root.cleanup)
        rep = Path(root.name) / "run/rep1"
        rep.mkdir(parents=True)
        (rep / "trace.log").write_text("\n".join(lines) + "\n")
        (rep / "wheel-start.txt").write_text("1000000\n")
        (rep / "wheel-end.txt").write_text("1010000\n")
        return Path(root.name) / "run"

    def test_only_redraws_inside_the_burst_count_and_win_lines_split_present(self):
        base = 1_000_000_000
        lines = [self.frame(base - 5_000_000, 1)]  # before the wheel
        for i in range(1, 101):
            t = base + i * 80_000
            lines += [f"[win] t={t - 10} setup=1500 swap=300", self.frame(t, i + 1)]
        lines += [self.frame(base + 5_000_000, 200, draws=0), self.frame(base + 20_000_000, 300)]
        row = self.analyze.summarise(self.run_dir(lines))
        self.assertEqual(row["redraws"], 100)
        self.assertAlmostEqual(row["int50"], 80.0)
        self.assertAlmostEqual(row["fps"], 12.5)
        self.assertAlmostEqual(row["draw"], 70.0)
        self.assertAlmostEqual(row["batches"], 4.0 - 1.5 - 0.3)

    def test_message_classes_account_for_the_interval(self):
        # 80 ms between frames: 11 ms is WM_PAINT (draw + present), 50 ms two wheel messages,
        # 14 ms a foreground task, 5 ms blocked in GetMessageW: 80 ms, nothing unaccounted.
        base = 1_000_000_000
        lines = []
        for i in range(1, 51):
            t = base + i * 80_000
            lines += [f"[win] t={t - 10} setup=100 swap=200 idle=5000 wheel=50000 wheel_n=2 paint_msg=11000 paint_n=1 "
                      f"task=14000 task_n=3 timer=0 timer_n=0 move=0 move_n=0 other=0 other_n=0 "
                      f"vs_n=5 vs_us=79000 vs_max=17000",
                      self.frame(t, i, draw=10000, present=500)]
        row = self.analyze.accounting(self.analyze.summarise(self.run_dir(lines)))
        self.assertAlmostEqual(row["wheel"], 50.0)
        self.assertEqual((row["wheel_n"], row["task_n"]), (2, 3))
        self.assertAlmostEqual(row["accounted"], 80.0)
        self.assertAlmostEqual(row["unaccounted"], 0.0)
        self.assertEqual(row["vs_n"], 5)
        self.assertAlmostEqual(row["vs_mean"], 15.8)
        self.assertAlmostEqual(row["vs_max"], 17.0)

    def test_a_trace_without_message_fields_has_no_accounting(self):
        base = 1_000_000_000
        rows = [self.frame(base + i * 80_000, i) for i in range(1, 11)]
        self.assertIsNone(self.analyze.accounting(self.analyze.summarise(self.run_dir(rows))))

    def test_mac_lines_carry_the_drawable_wait(self):
        base = 1_000_000_000
        lines = []
        for i in range(1, 51):
            t = base + i * 130_000
            lines += [f"[mac] t={t - 10} drawable=104000 encode=2500 commit=300",
                      self.frame(t, i, draw=14000, present=107000)]
        row = self.analyze.summarise(self.run_dir(lines))
        self.assertAlmostEqual(row["drawable"], 104.0)
        self.assertAlmostEqual(row["int50"], 130.0)

    def test_powershell_utf16_summaries_and_stamps_are_read(self):
        base = 1_000_000_000
        lines = [self.frame(base + i * 80_000, i) for i in range(1, 11)]
        run = self.run_dir(lines)
        rep = run / "rep1"
        (rep / "summary.txt").write_bytes("rep=1 valid=True quiet=True host_capped=True fps=12.6\r\n".encode("utf-16"))
        (rep / "wheel-start.txt").write_bytes("1000000\r\n".encode("utf-16"))
        row = self.analyze.summarise(run)
        self.assertEqual((row["valid"], row["quiet"], row["capped"]), ("T", "T", "T"))
        self.assertAlmostEqual(row["ext_fps"], 12.6)
        self.assertEqual(row["redraws"], 10)

    def test_no_frames_in_the_burst_is_reported_not_zero(self):
        self.assertIsNone(self.analyze.summarise(self.run_dir([self.frame(5, 1)])))


class PatchScriptTests(unittest.TestCase):
    def test_patch_refuses_a_source_it_does_not_recognise(self):
        with tempfile.TemporaryDirectory() as root:
            (Path(root) / "src/text_system").mkdir(parents=True)
            (Path(root) / "src/window.rs").write_text("fn unrelated() {}\n")
            done = subprocess.run([sys.executable, str(HERE / "frame-trace/patch.py"), root],
                                  capture_output=True, text=True)
            self.assertNotEqual(done.returncode, 0, "a patch that matched nothing must fail loudly")


if __name__ == "__main__":
    unittest.main()
