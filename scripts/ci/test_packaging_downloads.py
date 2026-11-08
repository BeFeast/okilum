"""Exercise real packaging fetch sections through a failed primary and pinned API."""
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest

ROOT = Path(__file__).resolve().parents[2]


@unittest.skipUnless(sys.platform == 'linux', 'These packaging stages execute on Linux')
class PackagingDownloadTests(unittest.TestCase):
    def test_packaging_fetches_recover_cache_and_reject_corrupt_api_bytes(self):
        payload = b'synthetic pinned packaging dependency'
        digest = hashlib.sha256(payload).hexdigest()
        observed = []
        state = {'corrupt': False}

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                observed.append((self.path, self.headers.get('Accept')))
                if self.path == '/primary':
                    self.send_response(504)
                    self.end_headers()
                else:
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.write(b'wrong' if state['corrupt'] else payload)

            def log_message(self, *_):
                pass

        server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=lambda: server.serve_forever(poll_interval=.01))
        thread.start()
        self.addCleanup(thread.join)
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        base = f'http://127.0.0.1:{server.server_port}'

        for tool in ['appimage', 'velopack']:
            with self.subTest(tool=tool), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / 'scripts/windows').mkdir(parents=True)
                shutil.copy2(ROOT / 'scripts/pinned_download.py', root / 'scripts/pinned_download.py')
                if tool == 'appimage':
                    cwd = root / 'dist/appimage'
                    (cwd / 'tools').mkdir(parents=True)
                    body = re.search(r'^fetch\(\) \{\n.*?^\}',
                                     (ROOT / 'scripts/appimage/build.sh').read_text(), re.M | re.S).group()
                    script = root / 'fetch.sh'
                    script.write_text('set -euo pipefail\n' + body + '\nfetch "$@"\n')
                    command = ['bash', str(script), 'tool', base + '/primary', digest, base + '/api']
                    output = cwd / 'tools/tool'
                else:
                    # Run the production download/cache section before any
                    # extraction or .NET installation, using synthetic pins.
                    prefix = (ROOT / 'scripts/windows/install-vpk.sh').read_text().split('python3 - "$root"')[0]
                    original_digest = re.search(r'\b[0-9a-f]{64}\b', prefix).group()
                    prefix = prefix.replace(original_digest, digest)
                    prefix = re.sub(r'https://github.com/\S+', base + '/primary', prefix)
                    prefix = re.sub(r'https://api.github.com/\S+', base + '/api', prefix)
                    script = root / 'scripts/windows/install-vpk.sh'
                    script.write_text(prefix)
                    cwd = root
                    directory = root / 'packager with spaces'
                    directory.mkdir()
                    command = ['bash', str(script), str(directory)]
                    output = directory / 'vpk.nupkg'
                state['corrupt'] = False
                output.write_bytes(b'broken cached download')
                before = len(observed)
                result = subprocess.run(command, cwd=cwd, capture_output=True, text=True, timeout=20)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(output.read_bytes(), payload)
                self.assertEqual(observed[before:], [('/primary', None), ('/api', 'application/octet-stream')])
                result = subprocess.run(command, cwd=cwd, capture_output=True, text=True, timeout=20)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(len(observed), before + 2, 'a valid pinned cache needs no network')
                state['corrupt'] = True
                output.write_bytes(b'broken cached download')
                result = subprocess.run(command, cwd=cwd, capture_output=True, text=True, timeout=20)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('SHA256 mismatch', result.stderr)
                self.assertFalse(output.exists(), 'corrupt bytes cannot become a packaging executable')


if __name__ == '__main__':
    unittest.main()
