"""Loader relocation checks need valid and deliberately corrupted ELF controls."""
import importlib.util
from pathlib import Path
import struct
import unittest

spec = importlib.util.spec_from_file_location(
    'appimage_elf', Path(__file__).resolve().parents[1] / 'appimage/check-elf.py')
elf = importlib.util.module_from_spec(spec)
spec.loader.exec_module(elf)


def payload(start=0x200, info=8):
    data = bytearray(0x400)
    data[:6] = b'\x7fELF\x02\x01'
    struct.pack_into('<H', data, 18, 62)
    struct.pack_into('<Q', data, 32, 64)
    struct.pack_into('<HH', data, 54, 56, 2)
    struct.pack_into('<IIQQQQQQ', data, 64, 1, 4, 0, 0, 0, len(data), len(data), 4096)
    struct.pack_into('<IIQQQQQQ', data, 120, 2, 6, 0x300, 0x300, 0x300, 80, 80, 8)
    for i, tag in enumerate(((7, start), (8, 24), (9, 24), (0x6ffffff9, 1), (0, 0))):
        struct.pack_into('<qQ', data, 0x300 + 16 * i, *tag)
    struct.pack_into('<Q', data, start + 8, info)
    return data


class AppImageElfTests(unittest.TestCase):
    def test_accepts_valid_relative_relocation(self):
        self.assertEqual(elf.validate(payload()), 1)

    def test_rejects_program_header_overlap_seen_in_retained_artifact(self):
        with self.assertRaisesRegex(ValueError, 'headers overlap'):
            elf.validate(payload(start=0xa8))

    def test_rejects_wrong_type_or_symbol_in_relative_prefix(self):
        for info in (4, 0x100000008):
            with self.subTest(info=info), self.assertRaisesRegex(ValueError, 'r_info'):
                elf.validate(payload(info=info))


if __name__ == '__main__':
    unittest.main()
