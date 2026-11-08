"""Transport failures must not relax the pinned CI archive checks."""
import hashlib
import io
from pathlib import Path
import runpy
import tempfile
import unittest
import urllib.error
from unittest.mock import patch

helper = runpy.run_path(str(Path(__file__).with_name('fetch-ci-dependency.py')))
fetch = helper['fetch']
DATA = b'synthetic pinned archive'
SHA = hashlib.sha256(DATA).hexdigest()
URL = 'https://github.com/example/fixture/releases/download/v1/archive'
API = 'https://api.github.com/repos/example/fixture/releases/assets/1'


class PinnedDownloadTests(unittest.TestCase):
    def test_504_uses_official_api_and_reuses_only_verified_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / 'archive'
            error = urllib.error.HTTPError(URL, 504, 'Gateway Time-out', {}, None)
            self.addCleanup(error.close)
            with patch('urllib.request.urlopen', side_effect=[error, io.BytesIO(DATA)]) as request:
                fetch(URL, API, SHA, destination)
            fallback = request.call_args_list[1].args[0]
            self.assertEqual(fallback.full_url, API)
            self.assertEqual(fallback.get_header('Accept'), 'application/octet-stream')
            self.assertEqual(destination.read_bytes(), DATA)
            with patch('urllib.request.urlopen') as request:
                fetch(URL, API, SHA, destination)
                request.assert_not_called()

    def test_primary_checksum_failure_never_falls_back_or_publishes(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / 'archive'
            with patch('urllib.request.urlopen', return_value=io.BytesIO(b'wrong')) as request:
                with self.assertRaisesRegex(ValueError, 'SHA256 mismatch'):
                    fetch(URL, API, SHA, destination)
                self.assertEqual(request.call_count, 1)
            self.assertEqual(list(Path(directory).iterdir()), [])

    def test_api_metadata_or_wrong_bytes_never_publish(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / 'archive'
            with patch('urllib.request.urlopen', side_effect=[
                urllib.error.URLError('connection reset'), io.BytesIO(b'{"id": 1}')
            ]):
                with self.assertRaisesRegex(ValueError, 'SHA256 mismatch'):
                    fetch(URL, API, SHA, destination)
            self.assertEqual(list(Path(directory).iterdir()), [])

    def test_404_does_not_hide_an_incorrect_primary_url(self):
        with tempfile.TemporaryDirectory() as directory:
            error = urllib.error.HTTPError(URL, 404, 'Not Found', {}, None)
            self.addCleanup(error.close)
            with patch('urllib.request.urlopen', side_effect=error) as request:
                with self.assertRaises(urllib.error.HTTPError):
                    fetch(URL, API, SHA, Path(directory) / 'archive')
                self.assertEqual(request.call_count, 1)

    def test_corrupt_existing_archive_is_refused_and_preserved(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / 'archive'
            destination.write_bytes(b'corrupt')
            with patch('urllib.request.urlopen') as request:
                with self.assertRaisesRegex(ValueError, 'SHA256 mismatch'):
                    fetch(URL, API, SHA, destination)
                request.assert_not_called()
            self.assertEqual(destination.read_bytes(), b'corrupt')


if __name__ == '__main__':
    unittest.main()
