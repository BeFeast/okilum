"""Vendor patches apply in file-name order: each number appears once (2026-10-11).

Two executors once both added 0067 and both 0068. A new patch takes the next free number
after rebasing on main; two patches with one number would apply in an order nobody chose.
"""
import collections
import re
import unittest
from pathlib import Path

PATCHES = Path(__file__).resolve().parents[1] / 'patches'
# Duplicates from before this check, frozen: renumbering them would rename every later
# patch and every executor's patch in flight. Their file-name order is what main applies.
FROZEN = {'0023', '0024', '0042', '0043'}


class PatchNumberTests(unittest.TestCase):
    def test_every_patch_has_a_unique_four_digit_number(self):
        names = sorted(p.name for p in PATCHES.glob('*.diff'))
        self.assertTrue(names)
        bad = [n for n in names if not re.fullmatch(r'\d{4}-[a-z0-9-]+\.diff', n)]
        self.assertEqual(bad, [], 'patch names are NNNN-lowercase-words.diff')
        counts = collections.Counter(n[:4] for n in names)
        duplicates = {number: [n for n in names if n.startswith(number)]
                      for number, c in counts.items() if c > 1 and number not in FROZEN}
        self.assertEqual(duplicates, {}, 'rebase on main and take the next free patch number')
        self.assertTrue(all(counts[n] == 2 for n in FROZEN), 'the frozen pairs stay as they are')


if __name__ == '__main__':
    unittest.main()
