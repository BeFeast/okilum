"""The built Arch package installs a themed app icon that its desktop entry names (#985).

Reads dist/arch/okilum-*.pkg.tar.zst (or OKILUM_ARCH_PACKAGE); skipped when no
package has been built, so the test also runs on hosts that only lint.
"""
import configparser
import glob
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest

RASTER_SIZES = (16, 22, 24, 32, 48, 64, 128, 256, 512)


def built_package():
    explicit = os.environ.get('OKILUM_ARCH_PACKAGE')
    if explicit:
        return explicit
    found = sorted(glob.glob('dist/arch/okilum-*.pkg.tar.zst'))
    return found[-1] if found else None


@unittest.skipUnless(built_package(), 'no built Arch package')
class PackageIcons(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root = tempfile.TemporaryDirectory()
        subprocess.run(['tar', '--zstd', '-xf', built_package(), '-C', cls.root.name, 'usr/share'],
                       check=True)
        cls.share = Path(cls.root.name, 'usr/share')
        entry = configparser.ConfigParser(interpolation=None)
        entry.optionxform = str
        entry.read(cls.share / 'applications/okilum.desktop', encoding='utf-8')
        cls.entry = entry['Desktop Entry']

    @classmethod
    def tearDownClass(cls):
        cls.root.cleanup()

    def test_desktop_entry_names_the_installed_icon(self):
        self.assertEqual(self.entry['Icon'], 'okilum')
        self.assertEqual(self.entry['StartupWMClass'], 'okilum')
        svg = self.share / 'icons/hicolor/scalable/apps/okilum.svg'
        self.assertIn('Okilum', svg.read_text(encoding='utf-8'))

    def test_raster_icons_cover_every_hicolor_size(self):
        for size in RASTER_SIZES:
            png = self.share / f'icons/hicolor/{size}x{size}/apps/okilum.png'
            data = png.read_bytes()
            self.assertEqual(data[:8], b'\x89PNG\r\n\x1a\n', png)
            self.assertEqual(struct.unpack('>II', data[16:24]), (size, size), png)


if __name__ == '__main__':
    unittest.main()
