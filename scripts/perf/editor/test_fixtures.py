"""The generated corpus is a byte-stable workload, not downloaded test data."""
import hashlib
import json
from pathlib import Path
import re
import unittest

import fixtures


class FixtureTests(unittest.TestCase):
    def test_raw_tail_native_proof_uses_the_exact_generated_fixture(self):
        self.assertEqual((Path(__file__).parent / "raw-tail-60k.md").read_bytes(),
                         fixtures.notes()["raw-tail-60k.md"])

    def test_exact_workloads_and_pinned_hashes(self):
        notes = fixtures.notes()
        self.assertEqual(set(notes), {"tiny.md", "sections-460.md", "one-mib.md", "novel-1200k.md",
                                      "raw-tail-60k.md"})
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

    def test_raw_tail_fallback_scans_long_unclosed_fence_at_eof(self):
        data = fixtures.notes()["raw-tail-60k.md"]
        self.assertEqual(len(data), 60 * 1024)
        self.assertTrue(data.isascii())
        self.assertTrue(data.startswith(b"# Raw tail performance\n\n"))
        opening, body = data.split(b"```text\n")
        self.assertEqual(opening.count(b"edit-marker"), 1)
        self.assertEqual(data.count(b"edit-marker"), 1)
        self.assertEqual(re.findall(rb"^\s*(`{3,}|~{3,}).*$", data, re.M), [b"```"])
        self.assertGreater(len(body), 60_000)
        self.assertTrue(body.endswith(b".\n"))
        self.assertTrue(all(line.strip() for line in body.splitlines()))


if __name__ == "__main__":
    unittest.main()
