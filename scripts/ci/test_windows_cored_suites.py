"""The Windows okilum-cored suites cannot pass empty, short or out of disk (#2060)."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "windows_cored_suites", Path(__file__).with_name("windows-cored-suites.py"))
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)


def listing(suite, count=None, drop=()):
    names = [name for name in check.SUITES[suite]["controls"] if name not in drop]
    total = check.SUITES[suite]["count"] if count is None else count
    names += [f"extra_{index}" for index in range(total - len(names))]
    text = "".join(f"{name}: test\n" for name in names) + f"\n{total} tests, 0 benchmarks\n"
    return subprocess.CompletedProcess([], 0, text)


def summary(passed, ignored=0, filtered=0, verdict="ok"):
    return (f"running {passed} tests\n"
            f"test result: {verdict}. {passed} passed; 0 failed; {ignored} ignored; "
            f"0 measured; {filtered} filtered out; finished in 1.00s\n")


def artifact(target, executable="C:/t/cli-1.exe", test=True):
    return json.dumps({"reason": "compiler-artifact", "executable": executable,
                       "profile": {"test": test}, "target": {"name": target}})


class WindowsCoredSuitesTests(unittest.TestCase):
    def setUp(self):
        self.quiet = contextlib.redirect_stdout(io.StringIO())
        self.quiet.__enter__()
        self.addCleanup(self.quiet.__exit__, None, None, None)

    def test_expected_counts_cover_every_portable_cli_test_and_the_windows_control(self):
        source = (check.ROOT / "crates/okilum-cored/tests/cli.rs").read_text()
        self.assertEqual(source.count("#[test]"), check.SUITES["cli"]["count"])
        self.assertEqual(source.count("#[cfg(not(unix))]"), 1)
        mcp = (check.ROOT / "crates/okilum-cored/tests/mcp.rs").read_text()
        self.assertEqual(mcp.count("#[test]"), check.SUITES["mcp"]["count"])
        for suite, source in (("cli", source), ("mcp", mcp)):
            for control in check.SUITES[suite]["controls"]:
                self.assertIn(f"fn {control}(", source)

    def test_discovery_requires_the_exact_count_and_named_controls(self):
        with patch.object(check.subprocess, "run", return_value=listing("cli")):
            self.assertEqual(len(check.discover("cli", "cli.exe")), 14)
        for broken in (listing("cli", count=13), listing("cli", count=0),
                       listing("mcp", drop=("handshake_then_tools_list",))):
            suite = "mcp" if "notification" in broken.stdout else "cli"
            with patch.object(check.subprocess, "run", return_value=broken):
                with self.assertRaisesRegex(check.SuiteFailure, "expected exactly"):
                    check.discover(suite, "x.exe")

    def test_run_requires_every_test_to_pass_and_none_skipped(self):
        with patch.object(check, "tee", return_value=(0, summary(7), [])):
            check.execute("mcp", "mcp.exe")
        for output in (summary(6), summary(7, ignored=1), summary(7, filtered=1), "no summary\n"):
            with patch.object(check, "tee", return_value=(0, output, [])):
                with self.assertRaises(check.SuiteFailure):
                    check.execute("mcp", "mcp.exe")
        with patch.object(check, "tee", return_value=(101, summary(6, verdict="FAILED"), [])):
            with self.assertRaisesRegex(check.SuiteFailure, "failed"):
                check.execute("mcp", "mcp.exe")

    def test_build_selects_the_one_named_test_executable(self):
        stdout = "\n".join([artifact("okilum_cored", "C:/t/okilum-cored.exe", test=False),
                            artifact("cli"), "not json"])
        with patch.object(check, "tee", return_value=(0, stdout, [])) as tee:
            self.assertEqual(check.build("cli"), "C:/t/cli-1.exe")
        command = tee.call_args.args[0]
        self.assertEqual(command[command.index("--test") + 1], "cli")
        self.assertIn("--locked", command)
        with patch.object(check, "tee", return_value=(0, artifact("mcp"), [])):
            with self.assertRaisesRegex(check.SuiteFailure, "one test executable"):
                check.build("cli")
        with patch.object(check, "tee", return_value=(101, "", [])):
            with self.assertRaisesRegex(check.SuiteFailure, "build failed"):
                check.build("cli")

    def test_disk_exhaustion_is_named_as_enospc_with_accounting(self):
        for line in ("error: could not write output: There is not enough space on the disk. (os error 112)",
                     "LINK : fatal error LNK1180: insufficient disk space to complete link",
                     "No space left on device (os error 28)"):
            self.assertTrue(check.ENOSPC.search(line), line)
        for line in ("os error 1120", "test result: ok. 12 passed"):
            self.assertFalse(check.ENOSPC.search(line), line)
        state = {"free": 0, "target": 10 * check.GIB}
        out = io.StringIO()
        with (patch.object(check, "tee", return_value=(101, "", ["os error 112"])),
              patch.object(check, "snapshot", return_value=state),
              patch.object(check, "largest_target_dirs", return_value=[(8 * check.GIB, "deps")]),
              contextlib.redirect_stdout(out)):
            with self.assertRaisesRegex(check.SuiteFailure, "disk exhausted"):
                check.build("mcp")
        self.assertIn("::error::No space left on device: okilum-cored mcp build", out.getvalue())
        self.assertIn("target/debug/deps: 8.00 GiB", out.getvalue())

    def test_low_free_disk_stops_before_any_build(self):
        state = {"free": int(check.MIN_FREE_GIB * check.GIB) - 1, "target": 0}
        with (patch.object(check, "snapshot", return_value=state),
              patch.object(check, "build") as build):
            with self.assertRaisesRegex(check.SuiteFailure, "insufficient free disk"):
                check.main()
        build.assert_not_called()

    def test_main_runs_both_suites_and_reports_the_budget(self):
        states = iter([{"free": 50 * check.GIB, "target": index * check.GIB} for index in range(3)])
        out = io.StringIO()
        with (patch.object(check, "snapshot", side_effect=lambda label: next(states)),
              patch.object(check, "build", side_effect=lambda suite: suite + ".exe") as build,
              patch.object(check, "discover") as discover,
              patch.object(check, "execute") as execute,
              contextlib.redirect_stdout(out)):
            self.assertEqual(check.main(), 0)
        self.assertEqual([call.args[0] for call in build.call_args_list], ["cli", "mcp"])
        self.assertEqual(discover.call_count, 2)
        self.assertEqual(execute.call_count, 2)
        self.assertIn("| cli |", out.getvalue())
        self.assertIn("target grew 2.00 GiB", out.getvalue())

    def test_tree_accounting_is_bounded(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "a").mkdir()
            (root / "a" / "one").write_bytes(b"x" * 10)
            (root / "two").write_bytes(b"x" * 5)
            self.assertEqual(check.tree_bytes(root), (15, True))
            self.assertFalse(check.tree_bytes(root, entries=1)[1])
            self.assertFalse(check.tree_bytes(root, seconds=-1)[1])


if __name__ == "__main__":
    unittest.main()
