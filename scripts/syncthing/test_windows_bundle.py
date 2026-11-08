import hashlib
import io
import struct
import unittest
import zipfile
import windows_bundle as w


def binary():
    result = bytearray(1040)
    result[:2] = b'MZ'
    struct.pack_into('<I', result, 0x3c, 128)
    result[128:132] = b'PE\0\0'
    struct.pack_into('<H', result, 132, 0x8664)
    struct.pack_into('<H', result, 148, 240)
    struct.pack_into('<H', result, 152, 0x20b)
    struct.pack_into('<I', result, 216, 0x12345678)
    struct.pack_into('<II', result, 296, 1024, 16)
    result[1024:] = bytes(range(16))
    return bytes(result)


class WindowsBundleTests(unittest.TestCase):
    def archive(self, executable, extra=None):
        stream = io.BytesIO()
        with zipfile.ZipFile(stream, 'w') as archive:
            for name in ['syncthing.exe', 'LICENSE.txt', 'AUTHORS.txt', 'README.txt']:
                archive.writestr(w.PREFIX + name, executable if name.endswith('.exe') else b'notice')
            if extra is not None:
                archive.writestr(extra, b'unexpected')
        return stream.getvalue()

    def read(self, data, executable, body=None):
        return w.payload(data, hashlib.sha256(data).hexdigest(), hashlib.sha256(executable).hexdigest(),
                         body or hashlib.sha256(w.normalized_body(executable, 1024)).hexdigest(), 1024)

    def test_resigning_changes_only_certificate_and_checksum(self):
        original = binary()
        changed = bytearray(original)
        struct.pack_into('<I', changed, 216, 0xaabbccdd)
        changed[1024:] = b'new signature!!!'
        self.assertEqual(w.normalized_body(original, 1024), w.normalized_body(bytes(changed), 1024))
        self.assertEqual(self.read(self.archive(original), original)['syncthing.exe'], original)

    def test_executable_body_and_architecture_are_pinned(self):
        original = binary()
        changed = bytearray(original)
        changed[700] ^= 1
        with self.assertRaisesRegex(ValueError, 'body changed'):
            self.read(self.archive(changed), changed, hashlib.sha256(w.normalized_body(original, 1024)).hexdigest())
        changed[132] = 0
        with self.assertRaises(ValueError):
            w.normalized_body(changed, 1024)

    def test_certificate_layout_cannot_hide_extra_bytes_or_executable_sections(self):
        for offset, size, extra in [(1023, 17, b''), (1024, 15, b''), (1024, 16, b'extra')]:
            changed = bytearray(binary())
            struct.pack_into('<II', changed, 296, offset, size)
            with self.assertRaises(ValueError):
                w.normalized_body(changed + extra, 1024)

    def test_pins_duplicate_names_and_archive_traversal_are_refused(self):
        executable = binary()
        data = self.archive(executable)
        with self.assertRaisesRegex(ValueError, 'archive pin'):
            w.payload(data)
        for extra in [w.PREFIX + 'syncthing.exe', w.PREFIX + '../escape', '/absolute', w.PREFIX + 'dir\\escape']:
            with self.subTest(extra=extra), self.assertRaises(ValueError):
                self.read(self.archive(executable, extra), executable)


if __name__ == '__main__':
    unittest.main()
