"""The generated corpus is a byte-stable workload, not downloaded test data."""
import hashlib
import json
from pathlib import Path
import re
import unittest

import fixtures


class FixtureTests(unittest.TestCase):
    def test_exact_workloads_and_pinned_hashes(self):
        notes = fixtures.notes()
        self.assertEqual(set(notes), {"tiny.md", "sections-460.md", "one-mib.md", "novel-1200k.md"})
        self.assertEqual(len(notes["one-mib.md"]), 1024 * 1024)
        self.assertEqual(len(notes["novel-1200k.md"].decode()), 1_200_000)
        self.assertEqual(len(re.findall(rb"^#{1,6} ", notes["sections-460.md"], re.M)), 460)
        self.assertEqual(len(notes["sections-460.md"]), 130_000)
        self.assertEqual(fixtures.manifest(), json.loads((Path(__file__).parent / "manifest.json").read_text()))
        for name, data in notes.items():
            with self.subTest(name=name):
                self.assertEqual(data.count(b"edit-marker"), 1)
                self.assertLessEqual(max(map(len, data.splitlines())), 120)
                self.assertEqual(hashlib.sha256(data).hexdigest(), fixtures.manifest()[name]["sha256"])


if __name__ == "__main__":
    unittest.main()
