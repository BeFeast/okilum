import hashlib
import importlib.util
import io
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import urllib.error

spec = importlib.util.spec_from_file_location('sparkle_fixture', Path(__file__).with_name('sparkle.py'))
sparkle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sparkle)


class FetchTests(unittest.TestCase):
    @patch.dict(os.environ, GITHUB_TOKEN='')
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
            self.assertEqual(fallback.full_url, sparkle.LOCK['asset_api_url'])
            self.assertEqual(fallback.get_header('Accept'), 'application/octet-stream')

    def test_hosted_fallback_keeps_the_token_out_of_argv(self):
        data = b'synthetic hosted archive'
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / 'archive'
            with patch.dict(sparkle.LOCK, archive_sha256=hashlib.sha256(data).hexdigest()), \
                 patch.dict(os.environ, GITHUB_TOKEN='synthetic-token'), \
                 patch.object(sparkle.urllib.request, 'urlopen', side_effect=urllib.error.URLError('504')), \
                 patch.object(sparkle.subprocess, 'check_output', return_value=data) as download:
                sparkle.fetch(destination)
            self.assertEqual(destination.read_bytes(), data)
            self.assertNotIn('synthetic-token', ' '.join(download.call_args.args[0]))
            self.assertEqual(download.call_args.kwargs['env']['GH_TOKEN'], 'synthetic-token')

    @patch.dict(os.environ, GITHUB_TOKEN='')
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
