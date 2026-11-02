import hashlib
import io
from pathlib import Path
import struct
import tarfile
import tempfile
import unittest
import linux_bundle as subject

# No system tempfs, user GPG database or network needed by these fixtures.
tempfile.tempdir = str(Path(__file__).resolve().parents[2] / 'fixture-scratch')
Path(tempfile.tempdir).mkdir(exist_ok=True)

class LinuxPayloadTests(unittest.TestCase):
    def archive(self, *, linked=False, duplicate=False, architecture=62, extras=0, unsafe=None):
        binary = bytearray(64)
        binary[:6] = b'\x7fELF\x02\x01'
        struct.pack_into('<H', binary, 18, architecture)
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode='w:gz') as archive:
            names = ['syncthing', 'LICENSE.txt', 'AUTHORS.txt', 'README.txt']
            if duplicate:
                names.append('syncthing')
            for name in names:
                data = bytes(binary) if name == 'syncthing' else b'notice'
                member = tarfile.TarInfo(subject.PREFIX + name)
                member.size = len(data)
                if linked and name == 'syncthing':
                    member.type = tarfile.SYMTYPE
                    member.linkname = '/outside'
                    member.size = 0
                archive.addfile(member, io.BytesIO(data))
            for index in range(extras):
                member = tarfile.TarInfo(subject.PREFIX + f'etc/example-{index}')
                archive.addfile(member, io.BytesIO())
            if unsafe:
                archive.addfile(tarfile.TarInfo(unsafe), io.BytesIO())
        return stream.getvalue(), bytes(binary)

    def read(self, data, binary):
        return subject.payload(data, subject.bundle.digest(data), subject.bundle.digest(binary))

    def test_expected_elf_and_exact_hash_positive_control(self):
        data, binary = self.archive()
        self.assertEqual(self.read(data, binary)['syncthing'], binary)
        with self.assertRaisesRegex(ValueError, 'archive pin'):
            subject.payload(data)
        with self.assertRaisesRegex(ValueError, 'runtime pin'):
            subject.payload(data, subject.bundle.digest(data), '0' * 64)

    def test_links_duplicates_and_wrong_architecture_are_refused(self):
        for options in ({'linked': True}, {'duplicate': True}, {'architecture': 183}):
            data, binary = self.archive(**options)
            with self.assertRaises((ValueError, KeyError)):
                self.read(data, binary)

    def test_truncated_archive_is_not_staged(self):
        data, binary = self.archive()
        with self.assertRaises((ValueError, tarfile.TarError, EOFError)):
            self.read(data[:25], binary)

    def test_release_sized_inventory_is_bounded_and_paths_are_safe(self):
        data, binary = self.archive(extras=41)
        self.assertEqual(self.read(data, binary)['syncthing'], binary)
        data, binary = self.archive(extras=61)
        with self.assertRaisesRegex(ValueError, 'ambiguous archive'):
            self.read(data, binary)
        for name in ('/outside', subject.PREFIX + '../outside', 'other/file'):
            data, binary = self.archive(unsafe=name)
            with self.assertRaisesRegex(ValueError, 'unsafe Linux archive entry'):
                self.read(data, binary)

    def test_repo_linux_pin_is_signed_by_the_existing_release_key(self):
        proof = subject.bundle.verify_upstream()
        self.assertEqual(proof['validsig'], subject.bundle.FINGERPRINT)
        signed = (subject.bundle.ROOT / 'sha256sum-v2.1.6.txt.asc').read_text()
        self.assertIn(subject.ARCHIVE_SHA + '  ' + subject.NAME, signed)

if __name__ == '__main__':
    unittest.main()
