import hashlib
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import urllib.error

spec = importlib.util.spec_from_file_location('sparkle_fixture', Path(__file__).with_name('sparkle.py'))
sparkle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sparkle)


class FetchTests(unittest.TestCase):
    def test_official_api_fallback_retains_the_exact_archive_pin(self):
        data = b'synthetic archive'
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / 'archive'
            with patch.dict(sparkle.LOCK, archive_sha256=hashlib.sha256(data).hexdigest()), \
                 patch.object(sparkle.urllib.request, 'urlopen', side_effect=[
                     urllib.error.URLError('504'), io.BytesIO(data),
                 ]) as request:
                sparkle.fetch(destination)
            self.assertEqual(destination.read_bytes(), data)
            fallback = request.call_args_list[1].args[0]
            self.assertEqual(fallback.full_url, sparkle.LOCK['asset_url'])
            self.assertEqual(fallback.get_header('Accept'), 'application/octet-stream')

    def test_wrong_api_bytes_never_create_an_archive(self):
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / 'archive'
            with patch.object(sparkle.urllib.request, 'urlopen', side_effect=[
                urllib.error.URLError('504'), io.BytesIO(b'wrong asset'),
            ]):
                with self.assertRaisesRegex(ValueError, 'SHA256 mismatch'):
                    sparkle.fetch(destination)
            self.assertFalse(destination.exists())


if __name__ == '__main__':
    unittest.main()
