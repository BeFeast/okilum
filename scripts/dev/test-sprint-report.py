#!/usr/bin/env python3
"""Isolated Sprint regressions. HTTP is loopback; git resets touch temp clones only."""
import contextlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts/dev/okilum-sprint.sh'
SOURCE = SCRIPT.read_text().split("<<'SPRINT_REPORTER'\n", 1)[1].split('\nSPRINT_REPORTER\n', 1)[0]
MODULE = {'__name__': 'sprint_report_test'}
exec(compile(SOURCE, '<embedded-reporter>', 'exec'), MODULE)


@contextlib.contextmanager
def endpoint():
    requests = []
    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            requests.append(self.rfile.read(int(self.headers['Content-Length'])))
            self.send_response(201)
            self.end_headers()
            self.wfile.write(json.dumps({'issue_number': 42, 'issue_url': 'https://git.oklabs.uk/BeFeast/okilum/issues/42'}).encode())
        def log_message(self, *args):
            pass
    server = HTTPServer(('127.0.0.1', 0), Handler)
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    try:
        yield f'http://127.0.0.1:{server.server_port}/feedback', requests
    finally:
        server.shutdown()
        server.server_close()
        worker.join()


class SprintTests(unittest.TestCase):
    def test_failure_upload_is_bounded_redacted_and_returns_issue(self):
        context = dict(branch='fix/example', sha='abcd123456', root='/tmp/sprint', durations={'fetch': 2})
        log = MODULE['redact']('Authorization: Bearer private-token\nclipboard=private clipboard\nsecret=not-for-server\n')
        with endpoint() as (url, requests):
            receipt = MODULE['send'](context, 'fetch', log, url)
        self.assertEqual(receipt['issue_number'], 42)
        self.assertEqual(len(requests), 1)
        body = requests[0].decode()
        self.assertIn('[Sprint report] fix/example abcd1234 fetch FAILED', body)
        self.assertIn('name="log"; filename="sprint-report.txt"', body)
        self.assertNotIn('private-token', body)
        self.assertNotIn('private clipboard', body)
        self.assertNotIn('not-for-server', body)

    def test_root_move_removes_only_stale_build_outputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            base = root / 'target/aarch64-apple-darwin/sprint'
            for name, origin in [('stale', '/old/okilum-sprint'), ('warm', str(root))]:
                output = base / 'build' / name / 'output'
                output.parent.mkdir(parents=True)
                output.write_text(f'cargo:rustc-env=GPUI_KIT_DEFAULT_ICONS_DIR={origin}/src/vendor/icons\n')
                (base / '.fingerprint' / name).mkdir(parents=True)
            MODULE['repair_cache'](root)
            self.assertFalse((base / 'build/stale').exists())
            self.assertFalse((base / '.fingerprint/stale').exists())
            self.assertTrue((base / 'build/warm/output').exists())
            self.assertEqual((root / 'target/.okilum-sprint-root').read_text().strip(), str(root))

    def test_fetch_failure_trap_uploads_once_without_checkout_helper(self):
        with tempfile.TemporaryDirectory() as directory, endpoint() as (url, requests):
            home = Path(directory)
            root = home / 'sprint'
            (root / 'src').mkdir(parents=True)
            commands = home / 'bin'
            commands.mkdir()
            mocks = {
                'uname': '#!/bin/bash\nif [[ $1 == -s ]]; then echo Darwin; else echo arm64; fi\n',
                'cargo': '#!/bin/bash\nexit 0\n',
                'git': f'''#!/bin/bash
case "$1 $2" in
 'check-ref-format --branch') exit 0;;
 'rev-parse --show-toplevel') echo '{root}/src';;
 'remote get-url') echo https://git.oklabs.uk/BeFeast/okilum.git;;
 'status --porcelain') exit 0;;
 'fetch origin') echo 'secret=private-fetch-token' >&2; exit 42;;
 *) exit 0;;
esac
''',
            }
            for name, text in mocks.items():
                path = commands / name
                path.write_text(text)
                path.chmod(0o700)
            script = home / 'sprint.sh'
            script.write_text(SCRIPT.read_text().replace(MODULE['ENDPOINT'], url))
            env = dict(os.environ, HOME=str(home), OKILUM_SPRINT_HOME=str(root), PATH=str(commands) + os.pathsep + os.environ['PATH'])
            result = subprocess.run(['bash', str(script), 'fix/test'], env=env, capture_output=True, text=True, timeout=40)
            self.assertEqual(result.returncode, 42, result.stdout + result.stderr)
            self.assertEqual(len(requests), 1)
            self.assertIn('fetch FAILED', requests[0].decode())
            self.assertNotIn('private-fetch-token', requests[0].decode())
            self.assertIn('https://git.oklabs.uk/BeFeast/okilum/issues/42', result.stdout)
            self.assertFalse((root / 'build.lock').exists())

    def test_vendor_patch_set_switch_resets_only_temp_clone(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            vendor = root / 'vendor/gpui-component'
            vendor.mkdir(parents=True)
            def git(*args):
                return subprocess.check_output(['git', '-C', str(vendor), *args], text=True).strip()
            git('init', '-q')
            git('config', 'user.email', 'sprint-test@example.invalid')
            git('config', 'user.name', 'Sprint test')
            source = vendor / 'crates/base/src/text/selection_adapter.rs'
            source.parent.mkdir(parents=True)
            source.write_text('pristine\n')
            git('add', '.')
            git('commit', '-qm', 'pinned fixture')
            pinned = git('rev-parse', 'HEAD')
            scripts = root / 'scripts'
            patches = scripts / 'patches'
            patches.mkdir(parents=True)
            setup = scripts / 'vendor-setup.sh'
            original = (ROOT / 'scripts/vendor-setup.sh').read_text()
            setup.write_text(original.replace('928c3eb776a3d733d9b771f7dea27a6a79242ced', pinned))
            def patch(value):
                (patches / '0001.diff').write_text('--- a/crates/base/src/text/selection_adapter.rs\n+++ b/crates/base/src/text/selection_adapter.rs\n@@ -1 +1 @@\n-pristine\n+' + value + '\n')
            patch('branch A')
            subprocess.run(['bash', str(setup)], check=True, capture_output=True)
            self.assertEqual(source.read_text(), 'branch A\n')
            (vendor / 'old-branch-artifact').write_text('generated')
            patch('branch B')
            result = subprocess.run(['bash', str(setup), '--verify'], capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue((vendor / 'old-branch-artifact').exists())
            self.assertEqual(source.read_text(), 'branch A\n')
            subprocess.run(['bash', str(setup)], check=True, capture_output=True)
            self.assertEqual(source.read_text(), 'branch B\n')
            self.assertFalse((vendor / 'old-branch-artifact').exists())
            subprocess.run(['bash', str(setup), '--verify'], check=True, capture_output=True)
            subprocess.run(['bash', str(setup)], check=True, capture_output=True)


if __name__ == '__main__':
    unittest.main()
