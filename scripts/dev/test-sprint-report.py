#!/usr/bin/env python3
"""Isolated Sprint regressions. HTTP is loopback; git resets touch temp clones only."""
import contextlib
import io
from http.server import BaseHTTPRequestHandler, HTTPServer
import importlib.util
import json
import os
import re
import shlex
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


QUIT_MOCKS = r'''
state=$MOCK_STATE
app=/Applications/Okilum.app
ps() { sed "s/UID/$(id -u)/" "$state/processes"; }
drop_pid() { grep -v "^$1 " "$state/processes" > "$state/p" || true; mv "$state/p" "$state/processes"; }
osascript() {
    local script="$*"
    echo "$script" >> "$state/apple-events"
    if [[ $script == *'to quit'* ]]; then
        echo quit >> "$state/calls"
        echo 0 > "$state/since_quit"
        case $MOCK_QUIT in
            cancel*) echo "execution error: Okilum got an error: User canceled. (-128)" >&2; return 1 ;;
            *) return 0 ;;
        esac
    fi
}
# MOCK_IDLE: idle seconds per ioreg call, space separated; the last one repeats.
ioreg() {
    local calls=0 values=($MOCK_IDLE)
    [[ -f $state/ioreg ]] && calls=$(cat "$state/ioreg")
    echo $((calls + 1)) > "$state/ioreg"
    ((calls < ${#values[@]})) || calls=$((${#values[@]} - 1))
    printf '    |   "HIDIdleTime" = %s\n' "$((values[calls] * 1000000000))"
}
# Events keyed to sleeps: MOCK_EXIT_AFTER = app exits that many sleeps after a
# cancel-then-exit Quit; MOCK_EXIT_AT / MOCK_REUSE_AT = total sleeps after which
# the app exits / its pid is reused by another process (new start time).
sleep() {
    local total=0 since
    echo "$1" >> "$state/sleeps"
    [[ -f $state/total ]] && total=$(cat "$state/total")
    total=$((total + 1)); echo "$total" > "$state/total"
    if [[ -f $state/since_quit ]]; then
        since=$(( $(cat "$state/since_quit") + 1 )); echo "$since" > "$state/since_quit"
        if [[ $MOCK_QUIT == cancel-then-exit ]] && (( since >= MOCK_EXIT_AFTER )); then drop_pid 501; fi
    fi
    if [[ -n ${MOCK_EXIT_AT:-} ]] && (( total >= MOCK_EXIT_AT )); then drop_pid 501; fi
    if [[ -n ${MOCK_REUSE_AT:-} ]] && (( total == MOCK_REUSE_AT )); then
        sed -i.bak 's/^501 UID Sat Nov 14 08:00:00 2026 /501 UID Sat Nov 14 10:59:59 2026 /' "$state/processes"
    fi
}
kill() {
    # kill -0 / plain kill only reach the bounded launchctl job; report it gone.
    [[ $1 == -* && $1 != -0 ]] || return 1
    local signal=$1 pid; shift
    grep -q "^$1 " "$state/processes" || return 0
    echo "$signal $*" >> "$state/calls"
    if [[ $signal == -TERM && -n ${MOCK_SPAWN_ON_TERM:-} ]]; then
        echo "$MOCK_SPAWN_ON_TERM UID Sat Nov 14 11:00:00 2026 /Applications/Okilum.app/Contents/MacOS/okilum" >> "$state/processes"
    fi
    if [[ $MOCK_STOPS_ON == *"${signal#-}"* ]]; then
        for pid in "$@"; do drop_pid "$pid"; done
    fi
}
launchctl() {
    echo "launchctl $*" >> "$state/calls"
    if (( ${MOCK_LAUNCHCTL_RC:-0} )); then
        echo "Could not find service \"com.befeast.okilum.sync\" in domain for user gui: $(id -u)" >&2
    fi
    return "${MOCK_LAUNCHCTL_RC:-0}"
}
open() { echo "open $*" >> "$state/calls"; }
'''


def quit_block():
    script = SCRIPT.read_text()
    return 'SPRINT_DEFERRED=' + script.split('\nSPRINT_DEFERRED=', 1)[1].split('\nsprint_exit() {', 1)[0]


class SprintQuitTests(unittest.TestCase):
    '''Okilum is closed only while the user is idle >= 60 s, by identity-bound
    signals, never SIGKILLed; the old Sync helper is restarted (14.11 reviews).'''
    START = 'Fri Nov 13 15:56:48 2026'
    APP = '501 UID Sat Nov 14 08:00:00 2026 /Applications/Okilum.app/Contents/MacOS/okilum\n'
    HELPER = '502 UID Fri Nov 13 15:56:48 2026 /Applications/Okilum.app/Contents/MacOS/okilum-sync-supervisor\n'
    QA_COPY = '503 UID Sat Nov 14 08:00:00 2026 /Users/qa/Okilum QA/Okilum.app/Contents/MacOS/okilum\n'
    STAGED = ('504 UID Sat Nov 14 08:00:00 2026 /Users/o/Developer/worktrees/okilum/sprint/staging.noindex/'
              'Okilum.app/Contents/MacOS/okilum\n')
    OTHER_USER = '505 0 Sat Nov 14 08:00:00 2026 /Applications/Okilum.app/Contents/MacOS/okilum\n'

    def run_call(self, processes, call='stop_installed_app', quit='ignore', idle='120', stops_on='', **mock):
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            (state / 'processes').write_text(processes)
            env = dict(os.environ, MOCK_STATE=str(state), MOCK_QUIT=quit, MOCK_IDLE=idle, MOCK_STOPS_ON=stops_on,
                       MOCK_EXIT_AFTER=str(mock.pop('exit_after', 3)), TMPDIR=str(state))
            env.update({'MOCK_' + key.upper(): str(value) for key, value in mock.items()})
            command = ('set -Eeuo pipefail\n' + QUIT_MOCKS + quit_block() +
                       '\nstatus=0; ' + call + ' || status=$?; echo RESULT=$status\n')
            result = subprocess.run(['bash', '-c', command], env=env, capture_output=True, text=True, timeout=60)
            read = lambda name: (state / name).read_text().split('\n') if (state / name).exists() else []
            self.assertEqual(sorted(p.name for p in state.glob('okilum-launchctl.*')), [], 'temp file removed')
            return (result, [c for c in read('calls') if c], [s for s in read('sleeps') if s],
                    (state / 'processes').read_text(), ''.join(read('apple-events')))

    def signals(self, calls):
        return [c for c in calls if c.startswith('-')]

    # Finding 1: nothing, not even Quit, before >= 60 s idle; idle re-read before the replacement.
    def test_active_user_gets_no_quit_and_no_signal_and_the_sprint_defers(self):
        result, calls, sleeps, processes, events = self.run_call(self.APP, idle='0', stops_on='TERM KILL')
        self.assertIn('RESULT=75', result.stdout, result.stderr)
        self.assertIn('Okilum was not sent Quit', result.stdout)
        self.assertEqual(events, '', 'no AppleEvent at all')
        self.assertEqual(calls, [])
        self.assertEqual(sleeps, ['5'] * 24)
        self.assertEqual(processes, self.APP)

    def test_quit_is_sent_only_once_the_user_is_idle(self):
        result, calls, sleeps, _, _ = self.run_call(self.APP, quit='cancel-then-exit', idle='5 5 61', exit_after=2)
        self.assertIn('RESULT=0', result.stdout, result.stderr)
        self.assertEqual(sleeps[:2], ['5', '5'])
        self.assertIn('User idle 61s: asking Okilum to quit', result.stdout)
        self.assertEqual(calls, ['quit'])

    def test_self_exit_after_quit_still_rechecks_idle_before_replacement(self):
        # -128, then Okilum exits by itself 20 s later; the user is back at the replacement.
        result, calls, sleeps, _, _ = self.run_call(
            self.APP, call='close_for_replacement', quit='cancel-then-exit', exit_after=20, idle='120 0')
        self.assertIn('RESULT=75', result.stdout, result.stderr)
        self.assertIn('right before the replacement; reopening the installed Okilum unchanged', result.stdout)
        self.assertEqual(calls, ['quit', 'open /Applications/Okilum.app'])
        self.assertEqual(sleeps, ['1'] * 20)

    def test_self_exit_after_20_s_with_idle_user_proceeds_without_signal(self):
        result, calls, sleeps, processes, _ = self.run_call(
            self.APP + self.HELPER, call='close_for_replacement', quit='cancel-then-exit', exit_after=20,
            stops_on='TERM KILL')
        self.assertIn('RESULT=0', result.stdout, result.stderr)
        self.assertIn('User canceled. (-128)', result.stdout)
        self.assertEqual(calls, ['quit'])
        self.assertEqual(sleeps, ['1'] * 20)
        self.assertEqual(processes, self.HELPER)

    def test_app_closed_by_the_user_before_any_quit_is_not_reopened(self):
        result, calls, _, _, events = self.run_call(self.APP, call='close_for_replacement', idle='0 0 0', exit_at=2)
        self.assertIn('RESULT=0', result.stdout, result.stderr)
        self.assertIn('Okilum quit by itself', result.stdout)
        self.assertEqual(calls, [])
        self.assertEqual(events, '')

    def test_quit_wait_for_self_exit_is_60_seconds(self):
        result, calls, sleeps, _, _ = self.run_call(self.APP, quit='cancel', stops_on='TERM')
        self.assertIn('RESULT=0', result.stdout, result.stderr)
        self.assertEqual(sleeps.count('1'), 60)
        self.assertEqual(self.signals(calls), ['-TERM 501'])

    def test_user_back_before_sigterm_gets_no_signal(self):
        result, calls, _, processes, _ = self.run_call(self.APP, idle='120 0', stops_on='TERM')
        self.assertIn('RESULT=75', result.stdout, result.stderr)
        self.assertIn('idle 0s < 60s); Okilum was not sent SIGTERM', result.stdout)
        self.assertEqual(calls, ['quit'])
        self.assertEqual(processes, self.APP)

    def test_the_signal_logs_the_idle_read_that_allowed_it(self):
        result, _, _, _, _ = self.run_call(self.APP, idle='61 75', stops_on='TERM')
        self.assertIn('SIGTERM pid 501 (user idle 75s)', result.stdout)

    # Finding 2: only captured identities, re-validated right before the signal.
    def test_a_pid_reused_during_the_quit_wait_is_never_signalled(self):
        result, calls, _, _, _ = self.run_call(self.APP, reuse_at=30, stops_on='TERM')
        self.assertIn('RESULT=0', result.stdout, result.stderr)
        self.assertEqual(self.signals(calls), [])

    def test_a_new_instance_started_after_sigterm_is_never_signalled(self):
        # Review 4 reproduced TERM 501 -> KILL 601 on e4a730a2.
        result, calls, _, processes, _ = self.run_call(self.APP, spawn_on_term=601)
        self.assertIn('RESULT=75', result.stdout, result.stderr)
        self.assertEqual(self.signals(calls), ['-TERM 501'])
        self.assertIn('601 UID', processes)

    def test_only_this_users_installed_app_is_quit_or_signalled(self):
        others = self.QA_COPY + self.STAGED + self.OTHER_USER + self.HELPER
        result, calls, _, _, events = self.run_call(self.APP + others, stops_on='TERM')
        self.assertIn('RESULT=0', result.stdout, result.stderr)
        self.assertEqual(self.signals(calls), ['-TERM 501'])
        self.assertIn('application "/Applications/Okilum.app"', events)
        self.assertNotIn('application id', events)
        result, calls, _, _, events = self.run_call(others)
        self.assertIn('RESULT=0', result.stdout, result.stderr)
        self.assertEqual(calls, [], 'copies alone need no quit and get no signal')
        self.assertEqual(events, '')

    # Finding 3: no SIGKILL; still open 30 s after SIGTERM -> left running, deferred.
    def test_app_still_open_after_sigterm_is_left_running_and_never_killed(self):
        result, calls, sleeps, processes, _ = self.run_call(self.APP + self.HELPER)
        self.assertIn('RESULT=75', result.stdout, result.stderr)
        self.assertIn('still runs 30s after SIGTERM; left running, no SIGKILL', result.stdout)
        self.assertEqual(self.signals(calls), ['-TERM 501'])
        self.assertEqual(sleeps, ['1'] * 90)
        self.assertEqual(processes, self.APP + self.HELPER)
        self.assertNotIn('-KILL', SCRIPT.read_text())

    # Finding 4: helper restart reports launchctl's real exit status; bounded wait.
    def restart_helper(self, processes, snapshot, stops_on='TERM', **mock):
        return self.run_call(processes, call='restart_old_sync_helper ' + shlex.quote(snapshot),
                             stops_on=stops_on, **mock)

    def test_old_sync_helper_is_terminated_and_kickstarted_with_exit_0_reported(self):
        result, calls, _, processes, _ = self.restart_helper(self.HELPER + self.QA_COPY, '502 ' + self.START)
        self.assertIn('RESULT=0', result.stdout, result.stderr)
        self.assertEqual(self.signals(calls), ['-TERM 502'])
        self.assertTrue(any(c.startswith('launchctl kickstart gui/') and c.endswith('/com.befeast.okilum.sync')
                            for c in calls), calls)
        self.assertIn('/com.befeast.okilum.sync exit 0.', result.stdout)
        self.assertEqual(processes, self.QA_COPY)

    def test_failed_kickstart_is_a_warning_with_its_exit_code_not_success(self):
        result, _, _, _, _ = self.restart_helper(self.HELPER, '502 ' + self.START, launchctl_rc=113)
        self.assertIn('RESULT=0', result.stdout, result.stderr)  # fail-soft
        self.assertIn('WARNING: Sync helper restart request failed: launchctl kickstart gui/', result.stdout)
        self.assertIn('exit 113: Could not find service', result.stdout)
        self.assertNotIn('exit 0.', result.stdout)

    def test_helper_that_ignores_sigterm_gets_a_bounded_wait_and_a_warning(self):
        result, calls, sleeps, processes, _ = self.restart_helper(self.HELPER, '502 ' + self.START, stops_on='')
        self.assertIn('RESULT=0', result.stdout, result.stderr)
        self.assertIn('WARNING: Sync helper (pid 502) still runs 15s after SIGTERM; left running, no SIGKILL', result.stdout)
        self.assertEqual(self.signals(calls), ['-TERM 502'])
        self.assertEqual(sleeps, ['1'] * 15)
        self.assertEqual(processes, self.HELPER)

    def test_new_helper_with_another_pid_or_start_time_is_left_alone(self):
        new_pid = '602 UID Sat Nov 14 10:40:00 2026 /Applications/Okilum.app/Contents/MacOS/okilum-sync-supervisor\n'
        reused = '502 UID Sat Nov 14 10:40:00 2026 /Applications/Okilum.app/Contents/MacOS/okilum-sync-supervisor\n'
        for running in (new_pid, reused):
            with self.subTest(running=running):
                result, calls, _, processes, _ = self.restart_helper(running, '502 ' + self.START)
                self.assertIn('RESULT=0', result.stdout, result.stderr)
                self.assertEqual(self.signals(calls), [])
                self.assertEqual(processes, running)

    def test_helper_of_another_user_or_bundle_is_not_snapshotted(self):
        others = ('702 0 Fri Nov 13 15:56:48 2026 /Applications/Okilum.app/Contents/MacOS/okilum-sync-supervisor\n'
                  '703 UID Fri Nov 13 15:56:48 2026 /Users/qa/Okilum.app/Contents/MacOS/okilum-sync-supervisor\n')
        result, _, _, _, _ = self.run_call(self.HELPER + others, call='bundle_processes okilum-sync-supervisor')
        self.assertEqual(result.stdout.splitlines(), ['502 ' + self.START, 'RESULT=0'])

    def test_deferral_is_not_reported_as_a_failure(self):
        script = SCRIPT.read_text()
        trap = script.split('sprint_exit() {', 1)[1].split('\ntrap sprint_exit EXIT', 1)[0]
        deferred = trap.split('if (( result == SPRINT_DEFERRED )); then', 1)[1].split('elif (( result != 0 )); then', 1)[0]
        self.assertNotIn('reporter', deferred)
        self.assertNotIn('FAILED', deferred)
        self.assertIn('if ((closed == SPRINT_DEFERRED)); then exit "$SPRINT_DEFERRED"; fi', script)
        self.assertLess(script.index('\nSPRINT_DEFERRED=75\n'), script.index('\ntrap sprint_exit EXIT\n'))

    def test_sprint_order_close_snapshot_replace_restart_launch(self):
        script = SCRIPT.read_text()
        close = script.index('closed=0; close_for_replacement || closed=$?')
        snapshot = script.index('old_sync_helper=$(bundle_processes "$SYNC_HELPER")')
        self.assertLess(close, snapshot)
        self.assertLess(snapshot, script.index('mv "$app" "$backup"'))
        restart = script.index('restart_old_sync_helper "$old_sync_helper"')
        self.assertLess(script.index('ditto "$bundle_stage" "$app"'), restart)
        self.assertLess(script.index('keep_one_registration\nrestart_old_sync_helper'), script.index('set_stage launch'))
        self.assertLess(restart, script.index('open "$app"\nrestore_on_failure=0'))

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS ps/ioreg semantics')
    def test_native_mac_finds_and_stops_only_the_exact_bundle_executable(self):
        def alive(pid):
            try:
                os.kill(pid, 0)
                return True
            except ProcessLookupError:
                return False

        with tempfile.TemporaryDirectory() as directory:
            app = Path(directory) / 'Okilum.app'
            qa = Path(directory) / 'QA/Okilum.app'
            pids = []
            for executable in (app / 'Contents/MacOS/okilum', app / 'Contents/MacOS/okilum-sync-supervisor',
                               qa / 'Contents/MacOS/okilum'):
                executable.parent.mkdir(parents=True, exist_ok=True)
                # copy, not copy2: /bin/sleep carries SIP flags that chflags refuses.
                shutil.copy('/bin/sleep', executable)
                # Orphaned to launchd, which reaps it: no zombie left in ps.
                started = subprocess.run(['/bin/bash', '-c', '"$0" 120 >/dev/null 2>&1 & echo $!', str(executable)],
                                         check=True, capture_output=True, text=True)
                pids.append(int(started.stdout))
            target, helper, copy = pids
            try:
                # IDLE_NEEDED=0: a hosted runner's HID idle time is not controllable.
                env = dict(os.environ, OKILUM_SPRINT_QUIT_WAIT='1', OKILUM_SPRINT_IDLE_WAIT='0',
                           OKILUM_SPRINT_IDLE_NEEDED='0', OKILUM_SPRINT_TERM_GRACE='5')
                command = ('set -Eeuo pipefail\napp="$1"\n' + quit_block() +
                           '\nprintf "pids=%s\\n" "$(installed_app_pids | tr "\\n" " ")"'
                           '\nprintf "idle=%s\\n" "$(user_idle_seconds)"'
                           '\nstatus=0; stop_installed_app || status=$?; echo RESULT=$status\n')
                result = subprocess.run(['/bin/bash', '-c', command, 'native', str(app)], env=env,
                                        capture_output=True, text=True, timeout=60)
                self.assertIn(f'pids={target} \n', result.stdout, result.stderr)
                self.assertRegex(result.stdout, r'idle=\d+\n')
                self.assertIn('RESULT=0', result.stdout, result.stderr)
                self.assertIn('SIGTERM', result.stdout)
                self.assertFalse(alive(target))
                self.assertTrue(alive(helper), 'the Sync helper keeps running')
                self.assertTrue(alive(copy), 'a copy elsewhere keeps running')
            finally:
                for pid in pids:
                    if alive(pid):
                        os.kill(pid, 9)

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS ps semantics')
    def test_native_mac_restarts_only_the_snapshotted_sync_helper(self):
        def alive(pid):
            try:
                os.kill(pid, 0)
                return True
            except ProcessLookupError:
                return False

        def start(executable):
            # Orphaned to launchd, which reaps it: no zombie left in ps.
            started = subprocess.run(['/bin/bash', '-c', '"$0" 120 >/dev/null 2>&1 & echo $!', str(executable)],
                                     check=True, capture_output=True, text=True)
            return int(started.stdout)

        with tempfile.TemporaryDirectory() as directory:
            app = Path(directory) / 'Okilum.app'
            helper = app / 'Contents/MacOS/okilum-sync-supervisor'
            helper.parent.mkdir(parents=True)
            # copy, not copy2: /bin/sleep carries SIP flags that chflags refuses.
            shutil.copy('/bin/sleep', helper)
            old = start(helper)
            pids = [old]
            try:
                block = quit_block()
                snapshot = subprocess.run(['/bin/bash', '-c', 'set -Eeuo pipefail\napp="$1"\n' + block +
                                           '\nbundle_processes okilum-sync-supervisor', 'native', str(app)],
                                          check=True, capture_output=True, text=True).stdout
                self.assertRegex(snapshot, rf'^{old} \w{{3}} \w{{3}} +\d+ \d\d:\d\d:\d\d \d{{4}}\n$')
                new = start(helper)  # e.g. launchd already started the new bundle's helper
                pids.append(new)
                env = dict(os.environ, OKILUM_SPRINT_SYNC_HELPER_WAIT='5')
                result = subprocess.run(['/bin/bash', '-c', 'set -Eeuo pipefail\napp="$1"\n' + block +
                                         '\nrestart_old_sync_helper "$2"; echo RESULT=$?', 'native', str(app), snapshot],
                                        env=env, capture_output=True, text=True, timeout=60)
                self.assertIn('RESULT=0', result.stdout, result.stderr)
                self.assertIn(f'(pid {old})', result.stdout)
                self.assertFalse(alive(old))
                self.assertTrue(alive(new), 'a helper started after the snapshot is not signalled')
            finally:
                for pid in pids:
                    if alive(pid):
                        os.kill(pid, 9)


if __name__ == '__main__':
    unittest.main()
