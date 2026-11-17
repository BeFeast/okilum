"""#2037: the arch-live RSS readout tells allocator noise from growth.

Readings are hosted-runner RSS samples (KiB) from scripts/check-edit-memory-native.py.
"""
import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('edit_memory', ROOT / 'scripts/check-edit-memory-native.py')
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)

# GitHub run 41104767889 (PR #2024, release tooling only): Theil-Sen over the
# 11 cycles after warm-up gave 128 KiB/cycle although cycle 15 equals cycle 6;
# the samples flip between two levels ~2.4 MiB apart.
PR2024_16 = [238664, 236192, 238680, 236332, 236336, 233992, 236464, 233992, 236460,
             233992, 236464, 236332, 236460, 236336, 241148, 236460]
PR2024_OPEN, PR2024_EMPTY = 228092, 225920

# GitHub run 44009804727 (main 7bcb13da, #2037 measurement): 40 cycles with
# ALLOCATOR_ENV; Theil-Sen +7.4 KiB/cycle, level 13 MiB.
MEASURED_40 = [253936, 253840, 252748, 254068, 255068, 254696, 254968, 254604, 252732, 255280,
               253312, 254700, 254980, 254804, 253252, 254256, 255056, 254332, 255224, 253176,
               255928, 254864, 254408, 252764, 254324, 254524, 252928, 252464, 253728, 254920,
               253968, 254656, 254748, 255328, 255396, 253540, 255400, 254408, 255204, 254772]
MEASURED_EMPTY, MEASURED_OPEN = 234072, 241328


def judge(readings, restored=None, empty=None, opened=None):
    restored = [True] * len(readings) if restored is None else restored
    return probe.judge(readings, restored, MEASURED_EMPTY if empty is None else empty,
                       MEASURED_OPEN if opened is None else opened)


class EditMemoryReadout(unittest.TestCase):
    def test_measured_flat_run_passes(self):
        result, checks = judge(MEASURED_40)
        self.assertTrue(all(checks.values()), result)

    def test_sixteen_cycles_cannot_judge_the_slope(self):
        with self.assertRaises(probe.Invalid):
            judge(PR2024_16, empty=PR2024_EMPTY, opened=PR2024_OPEN)

    def test_two_level_flip_is_not_growth(self):
        # The #2024 samples repeated over a full-length run.
        readings = (PR2024_16 * 3)[:40]
        result, checks = judge(readings, empty=PR2024_EMPTY, opened=PR2024_OPEN)
        self.assertTrue(checks['no_growth'], result)

    def test_steady_growth_at_twice_the_limit_fails(self):
        step = 2 * probe.MAX_SLOPE_KIB
        readings = [r + step * i for i, r in enumerate(MEASURED_40)]
        result, checks = judge(readings)
        self.assertFalse(checks['no_growth'], result)

    def test_unfixed_1256_growth_fails(self):
        # #1256 before the arena cap: +1936 KiB per Live Preview cycle.
        readings = [r + 1936 * i for i, r in enumerate(MEASURED_40)]
        result, checks = judge(readings)
        self.assertFalse(checks['no_growth'], result)

    def test_high_plateau_fails_the_level(self):
        # Hosted unfixed #1256 levelled off ~40 MB above the opened editor.
        readings = [r + 40 * 1024 for r in MEASURED_40]
        result, checks = judge(readings)
        self.assertTrue(checks['no_growth'], result)
        self.assertFalse(checks['level_under_20_mib'], result)

    def test_unrestored_cycles_are_the_verdict(self):
        restored = [True] * 10 + [False] * 30
        result, checks = judge(MEASURED_40, restored)
        self.assertEqual(checks, {'note_restored_every_cycle': False}, result)

    def test_allocator_returns_freed_memory_during_the_check(self):
        # Without these, mimalloc keeps 25-38 MiB wandering in RSS (#2037).
        self.assertEqual(probe.ALLOCATOR_ENV, {'MIMALLOC_PURGE_DELAY': '0',
                                               'MIMALLOC_ABANDONED_PAGE_PURGE': '1',
                                               'MIMALLOC_ALLOW_THP': '0'})

    def test_editor_open_must_raise_rss(self):
        with self.assertRaises(probe.Invalid):
            judge(MEASURED_40, empty=MEASURED_OPEN, opened=MEASURED_OPEN)


if __name__ == '__main__':
    unittest.main()
