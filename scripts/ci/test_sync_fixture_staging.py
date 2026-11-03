"""Linux exec controls for immutable, concurrent fixture publication."""
from concurrent.futures import ThreadPoolExecutor
import errno
import gzip
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import unittest
from unittest.mock import patch
import zlib


spec = importlib.util.spec_from_file_location('fixture_stage', Path(__file__).with_name('sync-fixture-stage.py'))
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


@unittest.skipUnless(sys.platform == 'linux', 'Linux ETXTBSY controls')
class FixtureStagingTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.cache = Path(self.directory.name) / 'okilum-sync-fixture'
        self.cache.mkdir()
        # A real ELF that stays executing until explicitly reaped; no compiler,
        # network, shell interpreter or daemon readiness can bypass execve.
        self.executable = Path('/bin/sleep').read_bytes()
        archive = io.BytesIO()
        with tarfile.open(fileobj=archive, mode='w:gz') as bundle:
            member = tarfile.TarInfo(helper.MEMBER)
            member.size = len(self.executable)
            member.mode = 0o755
            bundle.addfile(member, io.BytesIO(self.executable))
        self.archive = archive.getvalue()
        self.pins = patch.multiple(helper,
            BINARY_SHA256=hashlib.sha256(self.executable).hexdigest(),
            ARCHIVE_SHA256=hashlib.sha256(self.archive).hexdigest())
        self.pins.start()
        self.addCleanup(self.pins.stop)
        (self.cache / helper.ARCHIVE).write_bytes(self.archive)

    def running(self, binary):
        process = subprocess.Popen(['sleep', '60'], executable=binary)
        def stop():
            process.kill()
            process.wait()
        self.addCleanup(stop)
        self.assertIsNone(process.poll())
        self.assertTrue(Path(f'/proc/{process.pid}/exe').samefile(binary))
        return process

    def test_publish_while_executing_and_reuse_verified_inode(self):
        binary = helper.stage(self.cache)
        process = self.running(binary)
        # Positive control: opening the existing executable for writing fails.
        with self.assertRaises(OSError) as error:
            with binary.open('wb'):
                pass
        self.assertEqual(error.exception.errno, errno.ETXTBSY)
        original_inode = binary.stat().st_ino
        helper.publish(binary, self.executable, 0o555)
        self.assertNotEqual(original_inode, binary.stat().st_ino)
        self.assertIsNone(process.poll())
        subprocess.run(['sleep', '0'], executable=binary, check=True)
        with patch.object(helper, 'publish', side_effect=AssertionError('rewrote verified cache')):
            self.assertEqual(helper.stage(self.cache), binary)

    def test_old_tar_staging_positive_control(self):
        # The old scripts used tar -xzf directly in the shared cache. Keep an
        # earlier daemon executing there and pause that exact extraction path
        # while tar holds the replacement inode open for writing.
        legacy = self.cache / helper.MEMBER
        helper.publish(legacy, self.executable, 0o755)
        self.running(legacy)
        raw = gzip.decompress(self.archive)
        compressor = zlib.compressobj(wbits=31)
        cut = 40 * 1024  # gzip must emit a full output block before pausing.
        process = subprocess.Popen(['tar', '-xzf', '-', '-C', str(self.cache)],
                                   stdin=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            process.stdin.write(compressor.compress(raw[:cut]) + compressor.flush(zlib.Z_SYNC_FLUSH))
            process.stdin.flush()
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                descriptors = Path(f'/proc/{process.pid}/fd')
                if any(os.readlink(fd) == str(legacy) for fd in descriptors.iterdir()):
                    break
                time.sleep(.01)
            else:
                self.fail('old tar control never opened the executable for writing')
            with self.assertRaises(OSError) as error:
                subprocess.run(['sleep', '0'], executable=legacy, check=True)
            self.assertEqual(error.exception.errno, errno.ETXTBSY)
            print('Old tar staging positive control: live executable + concurrent exec returned ETXTBSY', flush=True)
            process.stdin.write(compressor.compress(raw[cut:]) + compressor.flush())
            process.stdin.close()
            self.assertEqual(process.wait(timeout=10), 0, process.stderr.read().decode())
        finally:
            if process.poll() is None:
                process.kill()
            process.wait()
            process.stdin.close()
            process.stderr.close()

    def test_concurrent_cold_staging_and_exec(self):
        publish = helper.publish
        barrier = threading.Barrier(8, timeout=10)
        def synchronized(destination, data, mode):
            if destination.name == 'syncthing':
                barrier.wait()  # Every caller must take the publication path.
            publish(destination, data, mode)
            if destination.name == 'syncthing':
                subprocess.run(['sleep', '0'], executable=destination, check=True)
        with patch.object(helper, 'publish', side_effect=synchronized):
            with ThreadPoolExecutor(max_workers=8) as pool:
                binaries = list(pool.map(lambda _: helper.stage(self.cache), range(8)))
        self.assertEqual(len(set(binaries)), 1)
        self.assertEqual(binaries[0].read_bytes(), self.executable)
        self.assertEqual(binaries[0].stat().st_mode & 0o777, 0o555)
        self.assertEqual(list(self.cache.rglob('.stage-*')), [])

    def test_spawn_during_publication_has_no_writable_descriptor(self):
        binary = helper.stage(self.cache)
        replace = os.replace
        def exec_at_publication(source, destination):
            replace(source, destination)
            # Run before publish returns, at the first instant the name becomes
            # visible. A still-open temp writer would deterministically ETXTBSY.
            subprocess.run(['sleep', '0'], executable=destination, check=True)
        with patch.object(helper.os, 'replace', side_effect=exec_at_publication):
            helper.publish(binary, self.executable, 0o555)

    def test_concurrent_downloads_do_not_share_partial_archive(self):
        (self.cache / helper.ARCHIVE).unlink()
        barrier = threading.Barrier(4, timeout=10)
        def download(_command):
            barrier.wait()
            return self.archive
        with patch.object(helper.subprocess, 'check_output', side_effect=download):
            with ThreadPoolExecutor(max_workers=4) as pool:
                list(pool.map(lambda _: helper.stage(self.cache), range(4)))
        self.assertEqual((self.cache / helper.ARCHIVE).read_bytes(), self.archive)
        self.assertEqual(list(self.cache.rglob('.stage-*')), [])

    def test_corrupt_archive_is_rejected_before_publication(self):
        (self.cache / helper.ARCHIVE).write_bytes(b'incomplete download')
        with self.assertRaisesRegex(ValueError, 'checksum mismatch'):
            helper.stage(self.cache)
        self.assertFalse((self.cache / helper.BINARY_SHA256).exists())

    def test_corrupt_member_is_rejected_before_publication(self):
        with patch.object(helper, 'BINARY_SHA256', '0' * 64):
            with self.assertRaisesRegex(ValueError, 'checksum mismatch'):
                helper.stage(self.cache)
            self.assertFalse((self.cache / helper.BINARY_SHA256).exists())

    def test_corrupt_cached_executable_is_repaired_atomically(self):
        binary = helper.stage(self.cache)
        binary.unlink()
        binary.write_bytes(b'corrupt cache')
        self.assertEqual(helper.stage(self.cache).read_bytes(), self.executable)


if __name__ == '__main__':
    unittest.main()
