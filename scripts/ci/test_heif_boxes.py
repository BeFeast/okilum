"""HEIF item parser used by the viewer controls and pixel reference (#2177)."""
from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts/appimage'))
import heif_boxes  # noqa: E402

FIXTURES = ROOT / 'crates/okilum-core/tests/fixtures/viewers/images'


def changed(before, after):
    return [index for index, (a, b) in enumerate(zip(before, after)) if a != b]


class HeifBoxesTest(unittest.TestCase):
    def test_items_transforms_and_primary(self):
        still = heif_boxes.Heif((FIXTURES / 'still.heic').read_bytes())
        self.assertEqual((still.primary, still.order), (1, [1]))
        self.assertEqual(heif_boxes.ispe(still, 1), (64, 64))
        (kind, clap), = heif_boxes.transforms(still, 1)
        self.assertEqual(kind, 'clap')
        self.assertEqual(heif_boxes.clap_rect(*clap, still.data, 64, 64), (0, 0, 64, 48))
        rotated = heif_boxes.Heif((FIXTURES / 'rotated.heic').read_bytes())
        self.assertEqual([k for k, _ in heif_boxes.transforms(rotated, 1)], ['clap', 'irot'])
        self.assertEqual(list(heif_boxes.transforms(rotated, 1))[1], ('irot', 1))
        mirrored = heif_boxes.Heif((FIXTURES / 'mirrored.heic').read_bytes())
        self.assertEqual(list(heif_boxes.transforms(mirrored, 1))[1], ('imir', 0))
        second = heif_boxes.Heif((FIXTURES / 'primary-second.heic').read_bytes())
        self.assertEqual((second.primary, second.order), (2, [1, 2]))
        self.assertTrue(heif_boxes.Heif((FIXTURES / 'still.heic').read_bytes()).item_data(1))

    def test_primary_only_sets_one_flag_bit_per_other_item(self):
        for name, hidden_item in (('multiple.heic', 2), ('primary-second.heic', 1)):
            source = (FIXTURES / name).read_bytes()
            result = heif_boxes.primary_only(source)
            self.assertEqual(len(result), len(source))
            offsets = changed(source, result)
            self.assertEqual(len(offsets), 1, name)
            self.assertEqual(result[offsets[0]], source[offsets[0]] | 1)
            parsed = heif_boxes.Heif(result)
            self.assertEqual({i: parsed.items[i].hidden for i in parsed.order},
                             {1: hidden_item == 1, 2: hidden_item == 2})
        still = (FIXTURES / 'still.heic').read_bytes()
        self.assertEqual(heif_boxes.primary_only(still), still)

    def test_unreadable_structure_is_left_unchanged(self):
        source = (FIXTURES / 'multiple.heic').read_bytes()
        for broken in (source[:len(source) // 2], b'\xff\xff\xff\xff' + source[4:],
                       b'not a heif file at all'):
            self.assertEqual(heif_boxes.primary_only(broken), broken)
            with self.assertRaises(heif_boxes.HeifError):
                heif_boxes.Heif(broken)


if __name__ == '__main__':
    unittest.main()
