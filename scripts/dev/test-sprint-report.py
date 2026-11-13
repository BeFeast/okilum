#!/usr/bin/env python3
"""Isolated Sprint regressions. HTTP is loopback; git resets touch temp clones only."""
import contextlib
import io
from http.server import BaseHTTPRequestHandler, HTTPServer
import importlib.util
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch

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
            self.assertEqual((root / 'target/.okilum-sprint-root').read_text().strip(), str(root.resolve()))

    def test_root_alias_keeps_warm_outputs_and_migrates_the_path_stamp(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            physical = base / 'physical'
            physical.mkdir()
            alias = base / 'alias'
            alias.symlink_to(physical, target_is_directory=True)
            root = alias / 'sprint'
            output = root / 'target/aarch64-apple-darwin/sprint/build/warm/output'
            output.parent.mkdir(parents=True)
            expected = f'cargo:rustc-env=GPUI_KIT_DEFAULT_ICONS_DIR={root}/src/vendor/icons\n'
            output.write_text(expected)
            stamp = root / 'target/.okilum-sprint-root'
            stamp.write_text(str(root) + '\n')
            MODULE['repair_cache'](root)
            self.assertTrue(output.is_file(), 'an alias of the same root must keep warm outputs')
            self.assertEqual(output.read_text(), expected)
            self.assertEqual(stamp.read_text().strip(), str(root.resolve()))
            MODULE['repair_cache'](root.resolve())
            self.assertEqual(output.read_text(), expected)

    def test_fetch_failure_trap_uploads_once_without_checkout_helper(self):
        self.fetch_failure_trap()

    def test_fetch_failure_trap_with_missing_bootstrap_stage_log(self):
        self.fetch_failure_trap(missing_stage_log=True)

    def fetch_failure_trap(self, missing_stage_log=False):
        with tempfile.TemporaryDirectory() as directory, endpoint() as (url, requests):
            home = Path(directory)
            root = home / 'sprint'
            (root / 'src').mkdir(parents=True)
            checkout = home / 'okilum'
            (checkout / '.git').mkdir(parents=True)
            commands = home / 'bin'
            commands.mkdir()
            temporary = home / 'tmp'
            temporary.mkdir()
            ready = home / 'tee-ready'
            if missing_stage_log:
                os.mkfifo(ready)
            mocks = {
                'uname': '#!/bin/bash\nif [[ $1 == -s ]]; then echo Darwin; else echo arm64; fi\n',
                'cargo': '#!/bin/bash\nexit 0\n',
                'git': f'''#!/bin/bash
if [[ $1 == -C ]]; then shift 2; fi
case "$1 $2" in
 'check-ref-format --branch')
   if [[ $SPRINT_TEST_MISSING_LOG == 1 ]]; then
     read -r signal < "$SPRINT_TEST_TEE_READY"
     [[ $signal == ready ]] || exit 90
     for log in "$TMPDIR"/okilum-sprint.*/stages.log; do
       [[ ! -e $log ]] || exit 91
     done
   fi
   exit 0;;
 'rev-parse --show-toplevel') echo '{root}/src';;
 'remote get-url') echo https://git.oklabs.uk/BeFeast/okilum.git;;
 'status --porcelain') exit 0;;
 'fetch origin') echo 'secret=private-fetch-token' >&2; exit 42;;
 *) exit 0;;
esac
''',
            }
            if missing_stage_log:
                # Deliberately omit tee's output file. The FIFO proves the first
                # tee is running before the script reaches the log handoff.
                mocks['tee'] = '''#!/bin/bash
case "$2" in
 "$TMPDIR"/okilum-sprint.*/stages.log) printf 'ready\n' > "$SPRINT_TEST_TEE_READY";;
esac
exec cat
'''
            for name, text in mocks.items():
                path = commands / name
                path.write_text(text)
                path.chmod(0o700)
            script = home / 'sprint.sh'
            script.write_text(SCRIPT.read_text().replace(MODULE['ENDPOINT'], url))
            env = dict(os.environ, HOME=str(home), TMPDIR=str(temporary), SPRINT_TEST_MISSING_LOG=str(int(missing_stage_log)), SPRINT_TEST_TEE_READY=str(ready), OKILUM_SPRINT_HOME=str(root), OKILUM_CHECKOUT=str(checkout), PATH=str(commands) + os.pathsep + os.environ['PATH'])
            result = subprocess.run(['bash', str(script), 'fix/test'], env=env, capture_output=True, text=True, timeout=40)
            self.assertEqual(result.returncode, 42, result.stdout + result.stderr)
            self.assertEqual(len(requests), 1)
            self.assertIn('fetch FAILED', requests[0].decode())
            self.assertNotIn('private-fetch-token', requests[0].decode())
            self.assertIn('https://git.oklabs.uk/BeFeast/okilum/issues/42', result.stdout)
            self.assertFalse((root / 'build.lock').exists())

    def test_rejected_installation_restores_previous_app(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            app = root / 'Sprint.app'
            backup = root / 'previous.app'
            app.mkdir()
            backup.mkdir()
            (app / 'invalid').write_text('rejected')
            (backup / 'working').write_text('previous')
            restore = SCRIPT.read_text().split('restore_sprint_app() {', 1)[1].split('\nsprint_exit() {', 1)[0]
            source = 'restore_sprint_app() {' + restore
            command = source + '\napp=$1; backup=$2; restore_on_failure=1; restore_sprint_app\n'
            subprocess.run(['bash', '-c', command, 'test', str(app), str(backup)], check=True, capture_output=True)
            self.assertFalse((app / 'invalid').exists())
            self.assertEqual((app / 'working').read_text(), 'previous')
            self.assertFalse(backup.exists())

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
            # Isolate the GPUI reset contract from the independent canonical
            # parser setup. Record that dependency invocation without networking
            # or substituting fake parser sources into a production build.
            (scripts / 'comrak-setup.py').write_text(
                'from pathlib import Path\nimport json, sys\n'
                'with Path(__file__).with_name("parser-calls.jsonl").open("a") as log:\n'
                '    log.write(json.dumps(sys.argv[1:]) + "\\n")\n')
            # Velopack is another independent pinned dependency, unrelated to
            # the generated GPUI clone reset exercised by this fixture.
            (scripts / 'velopack-setup.py').write_text('pass\n')
            # The independently pinned Windows backend participates in setup
            # and verification without changing this GPUI-clone reset fixture.
            (scripts / 'gpui-pre-windows-setup.py').write_text(
                'from pathlib import Path\nimport json, sys\n'
                'with Path(__file__).with_name("atlas-calls.jsonl").open("a") as log:\n'
                '    log.write(json.dumps(sys.argv[1:]) + "\\n")\n')
            # The pinned macOS first-window crates (#2825) are likewise independent.
            (scripts / 'gpui-pre-apple-setup.py').write_text('pass\n')
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
            self.assertEqual(
                [json.loads(line) for line in (scripts / 'parser-calls.jsonl').read_text().splitlines()],
                [[], ['--verify'], [], ['--verify'], []])
            self.assertEqual(
                [json.loads(line) for line in (scripts / 'atlas-calls.jsonl').read_text().splitlines()],
                [[], ['--verify'], [], ['--verify'], []])


class SprintTargetTests(unittest.TestCase):
    @contextlib.contextmanager
    def tree(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = base / 'sprint'
            (root / 'build.lock').mkdir(parents=True)
            (root / 'build.lock/owner').write_text(f'{os.getpid()} test parent\n')
            for relative in ['src/note.rs', 'staging.noindex/Okilum.app/binary']:
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b'keep canonical source and staged app\n')
            outside = base / 'Applications/Okilum.app/binary'
            outside.parent.mkdir(parents=True)
            outside.write_bytes(b'keep installed app\n')
            helper = base / 'reporter.py'
            helper.write_text(SOURCE)
            commands = base / 'bin'
            commands.mkdir()
            # ZFS can defer st_blocks accounting even after fsync. Model du's
            # KiB census deterministically; deletion still uses the real tree.
            du = commands / 'du'
            du.write_text('#!/usr/bin/env python3\nimport os, sys\nfrom pathlib import Path\n'
                          'root=Path(sys.argv[-1]); total=0\n'
                          'for parent, dirs, files in os.walk(root, followlinks=False):\n'
                          '    for name in files:\n'
                          '        p=Path(parent)/name\n'
                          '        if not p.is_symlink(): total += (p.stat().st_size+1023)//1024\n'
                          'print(str(total)+"\\t"+str(root))\n')
            du.chmod(0o755)
            yield root, helper, outside

    def budget(self, root, helper, cap='0.0002'):
        env = dict(os.environ, OKILUM_SPRINT_TARGET_CAP_GIB=cap)
        env['PATH'] = str(root.parent / 'bin') + os.pathsep + env['PATH']
        return subprocess.run(['python3', str(helper), '--trim-target', str(root)],
                              env=env, text=True, capture_output=True)

    def fill(self, root, relative, kib):
        path = root / 'target' / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        # Incompressible bytes keep du meaningful on compressed APFS/ZFS too.
        path.write_bytes(os.urandom(kib * 1024))
        return path

    def assert_kept(self, root, outside):
        self.assertEqual((root / 'src/note.rs').read_bytes(), b'keep canonical source and staged app\n')
        self.assertEqual((root / 'staging.noindex/Okilum.app/binary').read_bytes(),
                         b'keep canonical source and staged app\n')
        self.assertEqual(outside.read_bytes(), b'keep installed app\n')

    def test_cli_trims_incremental_first_without_touching_warm_deps_or_external_link(self):
        with self.tree() as (root, helper, outside):
            inc = self.fill(root, 'aarch64-apple-darwin/sprint/incremental/old', 256)
            deps = self.fill(root, 'aarch64-apple-darwin/sprint/deps/warm', 16)
            (inc.parent / 'external').symlink_to(outside.parent, target_is_directory=True)
            result = self.budget(root, helper)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(inc.parent.exists())
            self.assertTrue(deps.is_file())
            self.assertIn('removed=aarch64-apple-darwin/sprint/incremental\n', result.stdout)
            self.assertIn('Sprint target volume: free=', result.stdout)
            self.assert_kept(root, outside)

    def test_cli_removes_only_mac_sprint_profile_if_incremental_is_insufficient(self):
        with self.tree() as (root, helper, outside):
            self.fill(root, 'aarch64-apple-darwin/sprint/incremental/old', 256)
            self.fill(root, 'aarch64-apple-darwin/sprint/deps/old', 256)
            other = self.fill(root, 'other-triple/debug/keep', 16)
            result = self.budget(root, helper)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse((root / 'target/aarch64-apple-darwin/sprint').exists())
            self.assertTrue(other.is_file())
            self.assertIn('removed=aarch64-apple-darwin/sprint/incremental,aarch64-apple-darwin/sprint',
                          result.stdout)
            self.assert_kept(root, outside)

    def test_cli_below_cap_retains_outputs_and_default_is_twenty_gib(self):
        with self.tree() as (root, helper, outside):
            inc = self.fill(root, 'aarch64-apple-darwin/sprint/incremental/warm', 16)
            env = dict(os.environ)
            env.pop('OKILUM_SPRINT_TARGET_CAP_GIB', None)
            result = subprocess.run(['python3', str(helper), '--trim-target', str(root)],
                                    env=env, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('cap_gib=20 removed=none', result.stdout)
            self.assertTrue(inc.exists())
            self.assert_kept(root, outside)

    def test_disabled_incremental_removes_only_its_empty_cargo_scaffold(self):
        for setting in ['0', '1', None]:
            with self.subTest(setting=setting), self.tree() as (root, helper, outside):
                inc = root / 'target/aarch64-apple-darwin/sprint/incremental'
                inc.mkdir(parents=True)
                other = root / 'target/other-triple/debug/incremental'
                other.mkdir(parents=True)
                deps = self.fill(root, 'aarch64-apple-darwin/sprint/deps/warm', 16)
                env = dict(os.environ)
                env.pop('CARGO_INCREMENTAL', None)
                env.pop('OKILUM_SPRINT_TARGET_CAP_GIB', None)
                if setting is not None:
                    env['CARGO_INCREMENTAL'] = setting
                result = subprocess.run(['python3', str(helper), '--trim-target', str(root)],
                                        env=env, text=True, capture_output=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(inc.exists(), setting != '0')
                if setting == '0':
                    self.assertIn('removed=aarch64-apple-darwin/sprint/incremental(empty)', result.stdout)
                self.assertTrue(other.is_dir())
                self.assertEqual(deps.stat().st_size, 16 * 1024)
                self.assert_kept(root, outside)

    def test_disabled_incremental_keeps_existing_nonempty_cache_below_cap(self):
        with self.tree() as (root, helper, outside):
            inc = self.fill(root, 'aarch64-apple-darwin/sprint/incremental/warm', 16)
            before = inc.read_bytes()
            env = dict(os.environ, CARGO_INCREMENTAL='0', OKILUM_SPRINT_TARGET_CAP_GIB='20')
            result = subprocess.run(['python3', str(helper), '--trim-target', str(root)],
                                    env=env, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(inc.read_bytes(), before)
            self.assertIn('removed=none', result.stdout)
            self.assert_kept(root, outside)

    def test_cli_refuses_oversized_other_outputs_without_deleting_them(self):
        with self.tree() as (root, helper, outside):
            other = self.fill(root, 'other-triple/debug/keep', 256)
            result = self.budget(root, helper)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('outside the allowed Mac Sprint profile', result.stderr)
            self.assertEqual(other.stat().st_size, 256 * 1024)
            self.assert_kept(root, outside)

    def test_cli_refuses_symlinked_cleanup_paths_and_missing_parent_lock(self):
        for relative in ['target', 'target/aarch64-apple-darwin',
                         'target/aarch64-apple-darwin/sprint',
                         'target/aarch64-apple-darwin/sprint/incremental']:
            with self.subTest(relative=relative), self.tree() as (root, helper, outside):
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.symlink_to(outside.parent, target_is_directory=True)
                result = self.budget(root, helper)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('Refusing Sprint cache symlink', result.stderr)
                self.assert_kept(root, outside)
        with self.tree() as (root, helper, outside):
            inc = self.fill(root, 'aarch64-apple-darwin/sprint/incremental/keep', 256)
            (root / 'build.lock/owner').write_text('0 not the parent\n')
            result = self.budget(root, helper)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('parent build.lock', result.stderr)
            self.assertTrue(inc.exists())
            self.assert_kept(root, outside)

    def test_cli_invalid_caps_never_delete_outputs(self):
        for cap in ['0', '-1', 'NaN', 'Infinity', 'invalid', '0.00000001']:
            with self.subTest(cap=cap), self.tree() as (root, helper, outside):
                inc = self.fill(root, 'aarch64-apple-darwin/sprint/incremental/keep', 256)
                result = self.budget(root, helper, cap)
                self.assertNotEqual(result.returncode, 0)
                self.assertTrue(inc.exists())
                self.assert_kept(root, outside)

    @unittest.skipUnless(sys.platform == 'darwin', 'native Mac du accounting control')
    def test_native_mac_du_trims_real_allocated_cache(self):
        with self.tree() as (root, helper, outside):
            inc = self.fill(root, 'aarch64-apple-darwin/sprint/incremental/old', 8192)
            deps = self.fill(root, 'aarch64-apple-darwin/sprint/deps/warm', 512)
            before = int(subprocess.check_output(['du', '-sk', str(root / 'target')], text=True).split()[0])
            self.assertGreater(before, 2048, 'real allocated-size control must fire')
            result = subprocess.run(['python3', str(helper), '--trim-target', str(root)],
                                    env=dict(os.environ, OKILUM_SPRINT_TARGET_CAP_GIB='0.001953125'),
                                    text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(inc.parent.exists())
            self.assertTrue(deps.exists())
            self.assert_kept(root, outside)

    def test_low_space_warning_uses_target_volume_or_root_before_first_build(self):
        for free_gib in [49, 51]:
            with self.subTest(free_gib=free_gib), self.tree() as (root, helper, outside):
                (root / 'build.lock/owner').write_text(f'{os.getppid()} test parent\n')
                volume = root
                if free_gib == 49:
                    (root / 'target').mkdir()
                    volume = root / 'target'
                usage = shutil.disk_usage(root)._replace(free=free_gib * 1024**3)
                output = io.StringIO()
                with patch.object(MODULE['shutil'], 'disk_usage', return_value=usage) as disk, \
                     contextlib.redirect_stdout(output):
                    MODULE['trim_target'](root)
                disk.assert_called_once_with(volume)
                self.assertEqual('WARNING below 50 GiB' in output.getvalue(), free_gib < 50)
                self.assert_kept(root, outside)

    def test_shell_respects_incremental_override_and_bounds_before_build_and_after_staging(self):
        script = SCRIPT.read_text()
        line = next(line for line in script.splitlines() if line.startswith('export CARGO_TARGET_DIR='))
        for setting, expected in [(None, '1'), ('0', '0'), ('1', '1'), ('', '')]:
            with self.subTest(setting=setting), tempfile.TemporaryDirectory() as root:
                env = dict(os.environ)
                env.pop('CARGO_INCREMENTAL', None)
                if setting is not None:
                    env['CARGO_INCREMENTAL'] = setting
                result = subprocess.run(['bash', '-c', 'root="$1"\n' + line +
                                         '\nprintf "%s" "$CARGO_INCREMENTAL"', 'export-test', root],
                                        env=env, check=True, text=True, capture_output=True)
                self.assertEqual(result.stdout, expected)
        calls = [m.start() for m in re.finditer(
            re.escape('python3 "$reporter" --trim-target "$root"'), script)]
        self.assertEqual(len(calls), 2)
        self.assertLess(script.index('lock_owned=1'), calls[0])
        self.assertLess(calls[0], script.index('if ! cargo build'))
        self.assertLess(script.index('codesign --verify --deep --strict "$bundle_stage"'), calls[1])
        self.assertLess(calls[1], script.index('unregister "$bundle_stage"\n# Target only'))


if __name__ == '__main__':
    unittest.main()
