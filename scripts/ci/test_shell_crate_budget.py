"""shell-crate-budget.py: the okilum-shell line budget guard (#3232)."""
import importlib.util
import io
import json
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
_spec = importlib.util.spec_from_file_location("shell_crate_budget", Path(__file__).with_name("shell-crate-budget.py"))
BUDGET = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(BUDGET)


class ShellCrateBudgetTests(unittest.TestCase):
    def tree(self, limit, files):
        root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        source = root / "crates/okilum-shell/src"
        for name, text in files.items():
            path = source / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        (root / "scripts/ci").mkdir(parents=True)
        (root / BUDGET.BUDGET).write_text(json.dumps(
            {"crates": {"crates/okilum-shell/src": {"max_lines": limit}}}))
        return root

    def run_main(self, root):
        out, err = io.StringIO(), io.StringIO()
        with redirect_stdout(out), redirect_stderr(err):
            code = BUDGET.main(["--root", str(root)])
        return code, out.getvalue(), err.getvalue()

    def test_counts_rust_lines_recursively_and_ignores_other_files(self):
        root = self.tree(10, {"main.rs": "a\nb\n", "x/y.rs": "c\n", "notes.md": "1\n2\n3\n4\n"})
        self.assertEqual(BUDGET.count_lines(root / "crates/okilum-shell/src"), (3, 2))

    def test_at_budget_passes(self):
        code, out, err = self.run_main(self.tree(3, {"main.rs": "a\nb\nc\n"}))
        self.assertEqual(code, 0, err)
        self.assertIn("3 lines in 1 .rs files, budget 3", out)

    def test_over_budget_fails_and_points_at_the_split(self):
        code, _, err = self.run_main(self.tree(2, {"main.rs": "a\nb\nc\n"}))
        self.assertEqual(code, 1)
        self.assertIn("exceed the budget of 2 by 1", err)
        self.assertIn("crates/okilum-shell-<area>/src", err)

    def test_missing_directory_or_bad_budget_fails(self):
        root = self.tree(5, {"main.rs": "a\n"})
        (root / BUDGET.BUDGET).write_text(json.dumps({"crates": {"crates/gone/src": {"max_lines": 5}}}))
        self.assertEqual(self.run_main(root)[0], 1)
        (root / BUDGET.BUDGET).write_text(json.dumps({"crates": {"crates/okilum-shell/src": {"max_lines": "5"}}}))
        self.assertEqual(self.run_main(root)[0], 1)
        (root / BUDGET.BUDGET).write_text("{")
        self.assertEqual(self.run_main(root)[0], 1)

    def test_repository_budget_holds_and_is_wired_into_both_pre_merge_gates(self):
        code, _, err = self.run_main(ROOT)
        self.assertEqual(code, 0, err)
        for gate in ("scripts/ci/pool-pre-merge.sh", "scripts/ci/pool-linux-gate.sh"):
            self.assertIn("python3 scripts/ci/shell-crate-budget.py", (ROOT / gate).read_text(), gate)


if __name__ == "__main__":
    unittest.main()
