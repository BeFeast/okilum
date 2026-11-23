"""shell-test-inventory.py: no test may disappear during the crate split (#3232)."""
import importlib.util
import io
from contextlib import redirect_stdout
from pathlib import Path
import tempfile
import unittest

_spec = importlib.util.spec_from_file_location("shell_test_inventory", Path(__file__).with_name("shell-test-inventory.py"))
INVENTORY = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(INVENTORY)


class ShellTestInventoryTests(unittest.TestCase):
    def files(self, **contents):
        root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        paths = {}
        for name, text in contents.items():
            paths[name] = root / name
            paths[name].write_text(text)
        return paths

    def run_compare(self, *args):
        out = io.StringIO()
        with redirect_stdout(out):
            code = INVENTORY.main(["compare", *map(str, args)])
        return code, out.getvalue()

    def test_parse_list_keeps_tests_only(self):
        text = "a::b: test\nc::bench: benchmark\n\nnoise\nd::e: test\n"
        self.assertEqual(INVENTORY.parse_list(text), ["a::b", "d::e"])

    def test_moved_module_keeps_its_name_and_passes(self):
        f = self.files(base="a::x\nb::y\n", head="b::y\na::x\nc::new\n")
        code, out = self.run_compare(f["base"], f["head"])
        self.assertEqual(code, 0)
        self.assertIn("lost 0, added 1", out)
        self.assertIn("+ c::new", out)

    def test_lost_test_fails_and_is_named(self):
        f = self.files(base="a::x\nb::y\n", head="a::x\n")
        code, out = self.run_compare(f["base"], f["head"])
        self.assertEqual(code, 1)
        self.assertIn("- b::y", out)

    def test_duplicate_names_are_counted(self):
        f = self.files(base="tests::same\ntests::same\n", head="tests::same\n")
        self.assertEqual(self.run_compare(f["base"], f["head"])[0], 1)

    def test_declared_rename_passes_and_undeclared_does_not(self):
        f = self.files(base="tests::old\n", head="kit_tests::old\n",
                       renames="# moved out of main.rs\ntests::old -> kit_tests::old\n")
        self.assertEqual(self.run_compare(f["base"], f["head"])[0], 1)
        self.assertEqual(self.run_compare(f["base"], f["head"], "--renames", f["renames"])[0], 0)

    def test_empty_base_is_not_a_pass(self):
        f = self.files(base="", head="a::x\n")
        self.assertEqual(self.run_compare(f["base"], f["head"])[0], 1)


if __name__ == "__main__":
    unittest.main()
