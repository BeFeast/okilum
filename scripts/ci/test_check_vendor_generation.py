"""Same-host vendor formatting must not overlap.

Run 45007121115: shards base and admission both formatted on baldr and each
hit the 180s cargo fmt limit. Without the host lock this test's two callers
overlap.
"""
from pathlib import Path
import importlib.util
import os
import tempfile
import threading
import time
import unittest

SPEC = importlib.util.spec_from_file_location(
    "check_vendor_generation",
    Path(__file__).with_name("check-vendor-generation.py"))
VENDOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VENDOR)


class VendorFmtLock(unittest.TestCase):
    def test_same_host_formatters_do_not_overlap(self):
        with tempfile.TemporaryDirectory() as temporary:
            previous = os.environ.get("OKILUM_VENDOR_FMT_LOCK")
            os.environ["OKILUM_VENDOR_FMT_LOCK"] = str(Path(temporary) / "fmt.lock")
            state = {"active": 0, "peak": 0}
            commands = []
            guard = threading.Lock()

            def fake_run(args, root, *, success=True):
                del root, success
                with guard:
                    state["active"] += 1
                    state["peak"] = max(state["peak"], state["active"])
                    commands.append(list(args))
                time.sleep(0.15)
                with guard:
                    state["active"] -= 1

            original = VENDOR.run
            VENDOR.run = fake_run
            try:
                threads = [
                    threading.Thread(target=VENDOR.format_generated_clone, args=(Path(temporary),))
                    for _ in range(2)
                ]
                for thread in threads:
                    thread.start()
                for thread in threads:
                    thread.join(5)
                    self.assertFalse(thread.is_alive())
            finally:
                VENDOR.run = original
                if previous is None:
                    os.environ.pop("OKILUM_VENDOR_FMT_LOCK", None)
                else:
                    os.environ["OKILUM_VENDOR_FMT_LOCK"] = previous
            self.assertEqual(state["peak"], 1)
            self.assertEqual(commands.count(["cargo", "fmt"]), 2)
            self.assertEqual(commands.count(["cargo", "fmt", "--all"]), 2)


if __name__ == "__main__":
    unittest.main()
