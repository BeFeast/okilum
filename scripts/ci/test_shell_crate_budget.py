"""Controls for scripts/ci/shell-crate-budget.py (#3232)."""
import contextlib
import importlib.util
import io
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("shell_crate_budget", HERE / "shell-crate-budget.py")
budget = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(budget)


def run(*args):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        rc = budget.main([str(a) for a in args])
    return rc, out.getvalue(), err.getvalue()


class ShellCrateBudgetTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.src = self.root / budget.SOURCE
        (self.src / "nested").mkdir(parents=True)
        (self.src / "main.rs").write_text("fn main() {}\n// two\n")
        # No trailing newline: the last line still counts.
        (self.src / "nested" / "a.rs").write_text("one\ntwo\nthree")
        # Not Rust: never counted.
        (self.src / "nested" / "bridge.m").write_text("x\n" * 100)

    def tearDown(self):
        self.tmp.cleanup()

    def test_counts_only_rust_lines(self):
        self.assertEqual(budget.count_lines(self.src), (5, 2))

    def test_at_budget_passes(self):
        rc, out, _ = run("--root", self.root, "--budget", 5)
        self.assertEqual(rc, 0)
        self.assertIn("SHELL_CRATE_LINES=5 files=2 budget=5 headroom=0", out)

    def test_one_line_over_budget_fails(self):
        rc, _, err = run("--root", self.root, "--budget", 4)
        self.assertEqual(rc, 1)
        self.assertIn("over its budget of 4 by 1", err)

    def test_growth_past_budget_fails(self):
        self.assertEqual(run("--root", self.root, "--budget", 5)[0], 0)
        (self.src / "nested" / "grown.rs").write_text("fn grown() {}\n")
        self.assertEqual(run("--root", self.root, "--budget", 5)[0], 1)

    def test_missing_or_empty_source_is_an_error_not_a_pass(self):
        self.assertEqual(run("--root", self.root / "nowhere", "--budget", 10**9)[0], 2)
        for path in sorted(self.src.rglob("*.rs")):
            path.unlink()
        self.assertEqual(run("--root", self.root, "--budget", 10**9)[0], 2)

    def test_repository_is_within_its_budget(self):
        lines, files = budget.count_lines(budget.ROOT / budget.SOURCE)
        self.assertGreater(files, 100)
        self.assertLessEqual(lines, budget.BUDGET)


if __name__ == "__main__":
    unittest.main()
