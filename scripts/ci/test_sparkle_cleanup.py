"""Transient late writes must not mask real native or cleanup failures."""
import errno
import importlib.util
from pathlib import Path
import unittest
import shutil
import tempfile
from unittest.mock import Mock

spec = importlib.util.spec_from_file_location(
    "sparkle_fixture", Path(__file__).parent / "sparkle-freshness/run.py")
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class SparkleCleanupTests(unittest.TestCase):
    def test_clean_case_is_removed_once(self):
        remove = Mock()
        sleep = Mock()
        fixture.remove_case("owned-case", remove=remove, clock=lambda: 0, sleep=sleep)
        remove.assert_called_once_with("owned-case")
        sleep.assert_not_called()

    def test_late_write_then_quiescence_really_removes_case(self):
        with tempfile.TemporaryDirectory() as root:
            case = Path(root) / "owned-case"
            case.mkdir()
            calls = []

            def remove(path):
                calls.append(path)
                if len(calls) == 1:
                    (path / "late-write").write_text("fixture helper")
                    raise OSError(errno.ENOTEMPTY, "late helper write")
                shutil.rmtree(path)

            sleep = Mock()
            fixture.remove_case(case, remove=remove, clock=lambda: 0, sleep=sleep)
            self.assertEqual(len(calls), 2)
            self.assertFalse(case.exists())
            sleep.assert_called_once_with(0.1)

    def test_persistent_writes_fail_at_deadline(self):
        error = OSError(errno.ENOTEMPTY, "helper still writing")
        remove = Mock(side_effect=error)
        ticks = iter([0, 0, 10])
        sleep = Mock()
        with self.assertRaises(OSError) as raised:
            fixture.remove_case("owned-case", remove=remove,
                                clock=lambda: next(ticks), sleep=sleep)
        self.assertIs(raised.exception, error)
        self.assertEqual(remove.call_count, 2)
        sleep.assert_called_once_with(0.1)

    def test_other_cleanup_errors_are_not_retried_or_ignored(self):
        for code in [errno.EACCES, errno.EIO, errno.ENOENT]:
            with self.subTest(code=code):
                error = OSError(code, "cleanup failed")
                remove = Mock(side_effect=error)
                sleep = Mock()
                with self.assertRaises(OSError) as raised:
                    fixture.remove_case("owned-case", remove=remove,
                                        clock=lambda: 0, sleep=sleep)
                self.assertIs(raised.exception, error)
                remove.assert_called_once_with("owned-case")
                sleep.assert_not_called()


if __name__ == "__main__":
    unittest.main()
