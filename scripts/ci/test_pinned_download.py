"""A release CDN failure must not prevent fetching the same pinned build bytes."""
import hashlib
import io
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import urllib.error

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import pinned_download


class PinnedDownloadTests(unittest.TestCase):
    def test_primary_504_uses_binary_asset_api_and_preserves_digest(self):
        content = b'pinned synthetic build dependency'
        seen = []

        def server(request, timeout):
            seen.append(request)
            self.assertEqual(timeout, 60)
            if len(seen) == 1:
                raise urllib.error.HTTPError(request.full_url, 504, 'Gateway Time-out', {}, None)
            self.assertEqual(request.get_header('Accept'), 'application/octet-stream')
            return io.BytesIO(content)

        with tempfile.TemporaryDirectory() as tmp, patch.object(pinned_download.urllib.request, 'urlopen', server):
            destination = Path(tmp) / 'artifact'
            pinned_download.fetch('https://github.com/test/release', hashlib.sha256(content).hexdigest(), destination,
                                  'https://api.github.com/repos/test/releases/assets/1')
            self.assertEqual(destination.read_bytes(), content)
        self.assertEqual([request.full_url for request in seen], [
            'https://github.com/test/release', 'https://api.github.com/repos/test/releases/assets/1'])

    def test_corrupt_primary_is_not_retried_or_written(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(pinned_download.urllib.request, 'urlopen', return_value=io.BytesIO(b'wrong')) as network:
            destination = Path(tmp) / 'artifact'
            with self.assertRaisesRegex(ValueError, 'SHA256 mismatch'):
                pinned_download.fetch('https://github.com/test/release', '0' * 64, destination, 'https://api.github.com/test')
            self.assertFalse(destination.exists())
            self.assertEqual(network.call_count, 1)

    def test_corrupt_fallback_and_failed_download_never_publish(self):
        for last in [io.BytesIO(b'wrong'), urllib.error.URLError('offline')]:
            with tempfile.TemporaryDirectory() as tmp, patch.object(pinned_download.urllib.request, 'urlopen', side_effect=[urllib.error.URLError('offline'), last]):
                destination = Path(tmp) / 'artifact'
                with self.assertRaises((ValueError, urllib.error.URLError)):
                    pinned_download.fetch('https://github.com/test/release', '0' * 64, destination, 'https://api.github.com/test')
                self.assertFalse(destination.exists())

    def test_successful_primary_needs_no_fallback_and_existing_files_are_kept(self):
        content = b'synthetic'
        with tempfile.TemporaryDirectory() as tmp, patch.object(pinned_download.urllib.request, 'urlopen', return_value=io.BytesIO(content)) as network:
            destination = Path(tmp) / 'artifact'
            pinned_download.fetch('https://github.com/test/release', hashlib.sha256(content).hexdigest(), destination)
            self.assertEqual(destination.read_bytes(), content)
            with self.assertRaises(FileExistsError):
                pinned_download.fetch('https://github.com/test/release', '0' * 64, destination)
            self.assertEqual(network.call_count, 1)


if __name__ == '__main__':
    unittest.main()
