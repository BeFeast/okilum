"""Phase receipt reporting. No compiler and no build-slot."""
import json
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PHASE = ROOT / "scripts/ci/pool-phase.sh"


class PoolPhase(unittest.TestCase):
    def test_only_a_recorded_success_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            receipt = Path(directory) / "phases.json"
            receipt.write_text(json.dumps([
                {"name": "Run Linux Rust suites offline", "code": 0},
                {"name": "Lint every workspace target offline", "code": 1},
            ]))
            env = {"OKILUM_POOL_RECEIPT": str(receipt), "PATH": "/usr/bin:/bin"}
            ok = subprocess.run(["bash", str(PHASE), "Run Linux Rust suites offline"], env=env,
                                capture_output=True, text=True, timeout=30)
            bad = subprocess.run(["bash", str(PHASE), "Lint every workspace target offline"], env=env,
                                 capture_output=True, text=True, timeout=30)
            missing = subprocess.run(["bash", str(PHASE), "Not a phase"], env=env,
                                     capture_output=True, text=True, timeout=30)
            self.assertEqual(ok.returncode, 0, ok.stderr)
            self.assertEqual(bad.returncode, 1)
            self.assertIn("failed", bad.stderr)
            self.assertEqual(missing.returncode, 1)
            self.assertIn("not run", missing.stderr)


if __name__ == "__main__":
    unittest.main()
