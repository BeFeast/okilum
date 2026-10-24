"""Calibration cannot silently compare a binary from another commit or build."""
import hashlib
from pathlib import Path
import tempfile
import unittest

import reuse


class ReuseTests(unittest.TestCase):
    def test_source_and_binary_receipts_are_both_required(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            trace = root / "trace"
            trace.mkdir()
            commit = "a" * 40
            binary = b"calibration executable bytes"
            digest = hashlib.sha256(binary).hexdigest()
            (trace / "okilum").write_bytes(binary)
            (trace / "BUILD.txt").write_text(f"commit {commit}\nrelease\n")
            (trace / "SHA256SUMS").write_text(f"{digest}  okilum\n")
            output = root / "reused"
            with self.assertRaises(ValueError):
                reuse.reuse(root, output, "b" * 40)
            self.assertFalse(output.exists())
            (trace / "okilum").write_bytes(binary + b" changed")
            with self.assertRaises(ValueError):
                reuse.reuse(root, output, commit)
            self.assertFalse(output.exists())
            (trace / "okilum").write_bytes(binary)
            self.assertEqual(reuse.reuse(root, output, commit), digest)
            self.assertEqual((output / "okilum").read_bytes(), binary)
            self.assertTrue((output / "okilum").stat().st_mode & 0o111)
            with self.assertRaises(FileExistsError):
                reuse.reuse(root, output, commit)


if __name__ == "__main__":
    unittest.main()
