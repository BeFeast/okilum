#!/usr/bin/env python3
"""Dev build scripts (#2589): build numbers, origins, locks, LaunchServices
cleanup, Linux install and restart. HTTP is loopback; processes are our own."""
import contextlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import shutil
import signal
import stat
import subprocess
import tempfile
import threading
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
MAC = ROOT / 'scripts/dev/okilum-sprint.sh'
LINUX = ROOT / 'scripts/dev/okilum-dev-linux.sh'


def embedded(script, marker):
    text = script.read_text()
    source = text.split(f"<<'{marker}'\n", 1)[1].split(f'\n{marker}\n', 1)[0]
    module = {'__name__': 'dev_build_test_' + marker.lower()}
    exec(compile(source, f'<{marker}>', 'exec'), module)
    return module


def bash_function(script, name):
    text = script.read_text()
    body = text.split(f'\n{name}() {{\n', 1)[1].split('\n}\n', 1)[0]
    return f'{name}() {{\n{body}\n}}\n'


MAC_HELPER = embedded(MAC, 'SPRINT_REPORTER')
LINUX_HELPER = embedded(LINUX, 'DEV_HELPER')

APPCAST = b'''<?xml version='1.0' encoding='utf-8'?>
<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" version="2.0"><channel>
<item><title>Okilum 0.1.28468</title><sparkle:version>28468</sparkle:version>
  <sparkle:channel>beta</sparkle:channel><enclosure url="https://example.invalid/a.zip" /></item>
<item><title>Okilum 0.1.28450</title><sparkle:version>28450</sparkle:version>
  <sparkle:channel>beta</sparkle:channel></item>
<item><title>Okilum 0.1.28100</title><sparkle:version>28100</sparkle:version></item>
</channel></rss>'''

DUMP = '''Checking data integrity......done.
--------------------------------------------------------------------------------
bundle id:                  Okilum (0x1a01)
path:                       /Applications/Okilum.app (0x2b02)
identifier:                 com.befeast.okilum
--------------------------------------------------------------------------------
bundle id:                  Okilum (0x1a02)
path:                       /Users/me/Developer/personal/okilum-sprint/previous-Okilum-Sprint.app (0x2b03)
identifier:                 com.befeast.okilum
--------------------------------------------------------------------------------
bundle id:                  Okilum (0x1a03)
path:                       /Users/me/Developer/personal/okilum-sprint/staging.noindex/Okilum.app
identifier:                 com.befeast.okilum
--------------------------------------------------------------------------------
bundle id:                  Sync helper (0x1a04)
path:                       /Applications/Okilum.app/Contents/Library/Helpers/Okilum Sync.app (0x2b05)
identifier:                 com.befeast.okilum
--------------------------------------------------------------------------------
bundle id:                  Other (0x1a05)
path:                       /Applications/Other.app (0x2b06)
identifier:                 com.befeast.okilum.other
--------------------------------------------------------------------------------
claim id:                   Markdown (0x1a06)
identifier:                 com.befeast.okilum
--------------------------------------------------------------------------------
bundle id:                  Okilum (0x1a07)
path:                       /Volumes/Okilum/Okilum.app (0x2b08)
identifier:                 com.befeast.okilum
'''


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


def git_checkout(path, url):
    subprocess.run(['git', 'init', '-q', str(path)], check=True)
    subprocess.run(['git', '-C', str(path), 'remote', 'add', 'origin', url], check=True)


class BuildNumbers(unittest.TestCase):
    def test_mac_build_is_one_above_every_published_build(self):
        self.assertEqual(MAC_HELPER['next_build'](APPCAST), (28469, 28468, 28468))
        # A stable build never outranks the dev build either.
        higher_stable = APPCAST.replace(b'<sparkle:version>28100<', b'<sparkle:version>28500<')
        self.assertEqual(MAC_HELPER['next_build'](higher_stable), (28501, 28500, 28468))

    def test_mac_build_reads_enclosure_version_and_refuses_an_empty_feed(self):
        legacy = (b'<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle"><channel>'
                  b'<item><enclosure sparkle:version="27000" url="x" /></item></channel></rss>')
        self.assertEqual(MAC_HELPER['next_build'](legacy), (27001, 27000, 0))
        with self.assertRaises(ValueError):
            MAC_HELPER['next_build'](b'<rss><channel></channel></rss>')

    def test_mac_next_build_command_reads_the_appcast_from_stdin(self):
        with tempfile.TemporaryDirectory() as directory:
            reporter = Path(directory) / 'reporter.py'
            reporter.write_text(MAC.read_text().split("<<'SPRINT_REPORTER'\n", 1)[1].split('\nSPRINT_REPORTER\n', 1)[0])
            result = subprocess.run(['python3', str(reporter), '--next-build'], input=APPCAST,
                                    capture_output=True, check=True)
        self.assertEqual(result.stdout.decode().split(), ['28469', '28468', '28468'])

    def test_linux_build_is_one_above_every_published_build(self):
        beta = json.dumps({'build': 7602, 'channel': 'beta', 'publication_build': 28505})
        stable = json.dumps({'build': 7400})
        self.assertEqual(LINUX_HELPER['next_build']([beta, stable]), (7603, 7602))
        self.assertEqual(LINUX_HELPER['next_build']([beta, '']), (7603, 7602))
        with self.assertRaises(ValueError):
            LINUX_HELPER['next_build'](['{"build": true}', ''])


class Origins(unittest.TestCase):
    # The Mac builds in a worktree of Oleg's one checkout (#2635): its origin is used.
    def test_linux_checkout_explicit_then_the_one_around_the_script(self):
        checkout = LINUX_HELPER['checkout']
        with tempfile.TemporaryDirectory() as directory:
            repository = Path(directory).resolve() / 'okilum'
            scripts = repository / 'scripts/dev'
            scripts.mkdir(parents=True)
            self.assertEqual(checkout({}, scripts)[0], '')
            git_checkout(repository, 'ssh://git@10.10.0.27:2222/BeFeast/okilum.git')
            self.assertEqual(checkout({}, scripts), (str(repository), f'Okilum checkout around {scripts}'))
            # bash <(git show …): the script is /dev/fd/N, the working directory is the checkout.
            self.assertEqual(checkout({}, '/dev/fd', repository)[0], str(repository))
            self.assertEqual(checkout({'OKILUM_CHECKOUT': '/srv/okilum'}, scripts), ('/srv/okilum', 'OKILUM_CHECKOUT'))
            subprocess.run(['git', '-C', str(repository), 'remote', 'set-url', 'origin', 'https://example.invalid/other.git'], check=True)
            self.assertEqual(checkout({}, scripts)[0], '')

    def test_linux_script_reads_an_empty_checkout_as_empty(self):
        # A tab-separated read once swallowed the empty path and took the reason as the checkout.
        text = LINUX.read_text()
        parse = text[text.index('checkout_lines=$('):].split('\n', 2)
        with tempfile.TemporaryDirectory() as directory:
            helper = Path(directory) / 'helper.py'
            helper.write_text(text.split("<<'DEV_HELPER'\n", 1)[1].split('\nDEV_HELPER\n', 1)[0])
            script = (f'helper={helper}; script_dir={directory}; start_dir={directory}\n' + parse[0] + '\n' + parse[1]
                      + '\nprintf "[%s][%s]" "$checkout" "$checkout_source"')
            env = {k: v for k, v in os.environ.items() if k != 'OKILUM_CHECKOUT'}
            output = subprocess.run(['bash', '-c', script], capture_output=True, text=True, check=True, env=env).stdout
        self.assertTrue(output.startswith('[][no Okilum checkout around '), output)

    def test_linux_checkout_of_a_worktree_is_its_main_checkout(self):
        with tempfile.TemporaryDirectory() as directory:
            repository = Path(directory).resolve() / 'okilum'
            git_checkout(repository, 'https://git.oklabs.uk/BeFeast/okilum.git')
            (repository / 'f').write_text('x')
            for command in (['add', 'f'], ['-c', 'user.name=t', '-c', 'user.email=t@example.invalid', 'commit', '-qm', 'x'],
                            ['worktree', 'add', '-q', '--detach', str(Path(directory).resolve() / 'wt')]):
                subprocess.run(['git', '-C', str(repository), *command], check=True)
            self.assertEqual(LINUX_HELPER['checkout']({}, Path(directory).resolve() / 'wt')[0], str(repository))

    def test_only_okilum_remotes_are_rewritten(self):
        okilum_remote = LINUX_HELPER['okilum_remote']
        for url in ('https://git.oklabs.uk/BeFeast/okilum.git', 'ssh://git@10.10.0.27:2222/BeFeast/okilum.git',
                    'git@github.com:BeFeast/okilum', 'http://10.10.0.27:3000/befeast/okilum/'):
            self.assertTrue(okilum_remote(url), url)
        for url in ('https://git.oklabs.uk/BeFeast/okilum-web.git', 'https://example.invalid/x/notokilum.git', ''):
            self.assertFalse(okilum_remote(url), url)


class LaunchServices(unittest.TestCase):
    def test_only_the_installed_app_stays_registered(self):
        copies = MAC_HELPER['registered_copies'](DUMP, 'com.befeast.okilum', '/Applications/Okilum.app')
        self.assertEqual(copies, [
            '/Users/me/Developer/personal/okilum-sprint/previous-Okilum-Sprint.app',
            '/Users/me/Developer/personal/okilum-sprint/staging.noindex/Okilum.app',
            '/Volumes/Okilum/Okilum.app',
        ])

    def test_staged_and_previous_bundles_live_in_a_noindex_directory(self):
        text = MAC.read_text()
        self.assertIn('STAGE_DIR=staging.noindex', text)
        self.assertIn('bundle_stage="$root/$STAGE_DIR/Okilum.app"', text)
        self.assertIn('backup="$root/$STAGE_DIR/previous-Okilum.app"', text)
        self.assertEqual(MAC_HELPER['STAGE_DIR'], 'staging.noindex')
        self.assertNotIn('27850', text)


class Locks(unittest.TestCase):
    def run_lock(self, lock):
        text = LINUX.read_text()
        start = text.split('\nprocess_start() ', 1)[1].split('\n', 1)[0]
        script = 'process_start() ' + start + '\n' + bash_function(LINUX, 'acquire_lock')
        return subprocess.run(['bash', '-c', script + 'acquire_lock "$1"', 'test', str(lock)],
                              capture_output=True, text=True)

    def test_both_scripts_share_one_lock_implementation(self):
        self.assertEqual(bash_function(MAC, 'acquire_lock'), bash_function(LINUX, 'acquire_lock'))

    def test_stale_lock_is_taken_over_and_live_lock_is_kept(self):
        with tempfile.TemporaryDirectory() as directory:
            lock = Path(directory) / 'build.lock'
            result = self.run_lock(lock)
            self.assertEqual(result.returncode, 0, result.stderr)
            pid, since = (lock / 'owner').read_text().strip().split(' ', 1)
            self.assertTrue(pid.isdigit() and since)

            sleeper = subprocess.Popen(['sleep', '60'])
            try:
                started = subprocess.run(['ps', '-o', 'lstart=', '-p', str(sleeper.pid)],
                                         capture_output=True, text=True, check=True).stdout.strip()
                (lock / 'owner').write_text(f'{sleeper.pid} {started}\n')
                result = self.run_lock(lock)
                self.assertEqual(result.returncode, 1)
                self.assertIn(f'already running (pid {sleeper.pid}', result.stderr)
                # The same pid with another start time is a reused pid: stale.
                (lock / 'owner').write_text(f'{sleeper.pid} Mon Jan  1 00:00:00 2001\n')
                result = self.run_lock(lock)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn('Removed a stale build lock', result.stdout)
                self.assertNotEqual((lock / 'owner').read_text().split()[0], str(sleeper.pid))
            finally:
                sleeper.kill()
                sleeper.wait()
            # A dead owner is stale.
            (lock / 'owner').write_text(f'{sleeper.pid} {started}\n')
            self.assertEqual(self.run_lock(lock).returncode, 0)
            self.assertEqual(list(Path(directory).iterdir()), [lock])

    def test_lock_without_owner_is_stale_only_when_old(self):
        with tempfile.TemporaryDirectory() as directory:
            lock = Path(directory) / 'build.lock'
            lock.mkdir()
            result = self.run_lock(lock)
            self.assertEqual(result.returncode, 1)
            self.assertIn('without an owner', result.stderr)
            old = time.time() - 3 * 3600
            os.utime(lock, (old, old))
            result = self.run_lock(lock)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue((lock / 'owner').is_file())


class MacSprintFlow(unittest.TestCase):
    def test_dead_build_locks_are_cleared_and_the_old_clone_is_removed(self):
        with tempfile.TemporaryDirectory() as directory, endpoint() as (url, requests):
            home = Path(directory)
            root = home / 'sprint'
            (root / 'src').mkdir(parents=True)
            (root / 'build.lock').mkdir()
            (root / 'build.lock/owner').write_text('999999999 Mon Jan  1 00:00:00 2001\n')
            checkout = home / 'okilum'
            (checkout / '.git').mkdir(parents=True)
            # The 04.11 clone: an owner-less lock left by a dead build, plus its warm cache.
            legacy = home / 'Developer/personal/okilum-sprint'
            (legacy / 'build.lock').mkdir(parents=True)
            (legacy / 'target').mkdir()
            old = time.time() - 3 * 3600
            os.utime(legacy / 'build.lock', (old, old))
            commands = home / 'bin'
            commands.mkdir()
            mocks = {
                'uname': '#!/bin/bash\nif [[ $1 == -s ]]; then echo Darwin; else echo arm64; fi\n',
                'cargo': '#!/bin/bash\nexit 0\n',
                'git': '''#!/bin/bash
if [[ $1 == -C ]]; then shift 2; fi
case "$1 $2" in
 'remote get-url') echo ssh://git@10.10.0.27:2222/BeFeast/okilum.git;;
 'fetch origin') exit 42;;
 *) exit 0;;
esac
''',
            }
            for name, text in mocks.items():
                path = commands / name
                path.write_text(text)
                path.chmod(0o700)
            script = home / 'sprint.sh'
            script.write_text(MAC.read_text().replace(MAC_HELPER['ENDPOINT'], url))
            env = dict(os.environ, HOME=str(home), TMPDIR=directory, OKILUM_SPRINT_HOME=str(root),
                       OKILUM_CHECKOUT=str(checkout), PATH=str(commands) + os.pathsep + os.environ['PATH'])
            result = subprocess.run(['bash', str(script), 'fix/test'], env=env, capture_output=True,
                                    text=True, timeout=60)
            output = result.stdout + result.stderr
            self.assertEqual(result.returncode, 42, output)
            self.assertEqual(output.count('Removed a stale build lock'), 2, output)
            self.assertFalse(legacy.exists())
            self.assertTrue((root / 'target').is_dir())
            self.assertEqual(len(requests), 1)
            self.assertFalse((root / 'build.lock').exists())


class LinuxInstall(unittest.TestCase):
    def install(self, home, new, env=None):
        script = bash_function(LINUX, 'install_image')
        command = ('set -Eeuo pipefail\ncreated=()\ndir=$1; image=$1/Okilum.AppImage\n' + script
                   + 'install_image "$2"\nprintf "%s\\n" "${created[@]}"\n')
        return subprocess.run(['bash', '-c', command, 'test', str(home), str(new)],
                              capture_output=True, text=True, env=dict(os.environ, **(env or {})), timeout=30)

    def test_install_replaces_atomically_and_keeps_the_previous_file(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / 'share/okilum'
            target.mkdir(parents=True)
            (target / 'Okilum.AppImage').write_bytes(b'old build')
            new = Path(directory) / 'Okilum-x86_64.AppImage'
            new.write_bytes(b'new build')
            result = self.install(target, new)
            self.assertEqual(result.returncode, 0, result.stderr)
            image = target / 'Okilum.AppImage'
            self.assertEqual(image.read_bytes(), b'new build')
            self.assertEqual(stat.S_IMODE(image.stat().st_mode), 0o755)
            self.assertEqual(image.stat().st_nlink, 1)
            self.assertEqual((target / 'Okilum.AppImage.previous').read_bytes(), b'old build')
            lock = target / '.Okilum.AppImage.update-lock'
            self.assertEqual(stat.S_IMODE(lock.stat().st_mode), 0o600)
            self.assertEqual(sorted(p.name for p in target.iterdir()),
                             ['.Okilum.AppImage.update-lock', '.install-lock', 'Okilum.AppImage', 'Okilum.AppImage.previous'])

    def test_an_active_update_blocks_the_install(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / 'share/okilum'
            target.mkdir(parents=True)
            (target / 'Okilum.AppImage').write_bytes(b'old build')
            new = Path(directory) / 'new.AppImage'
            new.write_bytes(b'new build')
            lock = target / '.Okilum.AppImage.update-lock'
            lock.touch(mode=0o600)
            holder = subprocess.Popen(['flock', str(lock), 'sleep', '30'])
            try:
                deadline = time.monotonic() + 10
                while subprocess.run(['flock', '-n', str(lock), 'true']).returncode == 0:
                    self.assertLess(time.monotonic(), deadline)
                    time.sleep(0.05)
                result = self.install(target, new, {'OKILUM_DEV_LOCK_SECONDS': '1'})
            finally:
                holder.kill()
                holder.wait()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('holds its lock', result.stderr)
            self.assertEqual((target / 'Okilum.AppImage').read_bytes(), b'old build')

    def test_build_record_uses_okilum_installation_format(self):
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / 'Okilum.AppImage'
            image.write_bytes(b'bytes')
            self.assertFalse(LINUX_HELPER['record_build'](image, 7603))
            self.assertFalse((Path(directory) / '.appimage-build').exists())
            (Path(directory) / '.appimage-owner').write_bytes(b'Okilum managed AppImage v1\n')
            self.assertTrue(LINUX_HELPER['record_build'](image, 7603))
            st = os.lstat(image)
            self.assertEqual((Path(directory) / '.appimage-build').read_text(),
                             f'7603 {st.st_size} {st.st_mtime_ns // 10**9} {st.st_mtime_ns % 10**9} {st.st_ino}')


class LinuxRestart(unittest.TestCase):
    def fake_proc(self, root, pid, exe, fds, env):
        base = root / str(pid)
        (base / 'fd').mkdir(parents=True)
        os.symlink(exe, base / 'exe')
        for number, target in enumerate(fds):
            os.symlink(target, base / 'fd' / str(number))
        (base / 'environ').write_bytes(b'\0'.join(f'{k}={v}'.encode() for k, v in env.items()) + b'\0')
        fields = ['S'] + ['0'] * 18 + [str(1000 + pid)]
        (base / 'stat').write_text(f'{pid} (okilum (x)) ' + ' '.join(fields) + ' 0 0\n')

    def test_windows_are_found_by_executable_and_instance_lock_not_argv(self):
        with tempfile.TemporaryDirectory() as directory:
            proc = Path(directory)
            lock = '/home/u/.local/state/okilum/reader-instance.lock'
            mount = '/tmp/.mount_OkilumAB12/usr/bin/okilum'
            self.fake_proc(proc, 100, mount + ' (deleted)', ['/dev/null', lock], {'APPIMAGE': '/home/u/Okilum.AppImage'})
            self.fake_proc(proc, 101, mount, ['/dev/null'], {'APPIMAGE': '/home/u/Okilum.AppImage'})  # sync serve
            self.fake_proc(proc, 102, '/usr/bin/bash', [lock], {})  # argv may say okilum; exe does not
            self.fake_proc(proc, 103, '/home/u/src/target/release/okilum', [lock + ' (deleted)'], {})
            (proc / 'self').mkdir()
            found = LINUX_HELPER['gui_processes'](str(proc))
            self.assertEqual([(p['pid'], p['appimage'], p['start']) for p in found],
                             [(100, '/home/u/Okilum.AppImage', 1100), (103, None, 1103)])
            self.assertEqual(found[0]['exe'], mount)
            self.assertEqual(LINUX_HELPER['gui_processes'](str(proc), lock='/elsewhere/reader-instance.lock'), [])

    def test_launch_environment_drops_appimage_runtime_variables(self):
        env = {'APPIMAGE': 'a', 'APPDIR': 'b', 'ARGV0': 'c', 'OWD': 'd', 'XDG_ACTIVATION_TOKEN': 'e',
               'WAYLAND_DISPLAY': 'wayland-1', 'HOME': '/home/u'}
        self.assertEqual(LINUX_HELPER['launch_env'](env), {'WAYLAND_DISPLAY': 'wayland-1', 'HOME': '/home/u'})

    def test_quit_and_relaunch_a_running_window(self):
        with tempfile.TemporaryDirectory() as directory:
            # /proc shows resolved paths; TMPDIR may contain a symlink.
            work = Path(directory).resolve()
            # A window stand-in: bash named okilum (multi-call coreutils refuse
            # another name) holding the instance lock, blocked in a builtin read.
            binary = work / '.mount_OkilumT/usr/bin/okilum'
            binary.parent.mkdir(parents=True)
            shutil.copy2(shutil.which('bash'), binary)
            os.mkfifo(work / 'fifo')
            state = work / 'state'
            state.mkdir()
            lock = state / 'reader-instance.lock'
            body = 'exec 9<>"$LOCK" 8<>"$FIFO"; read -r -t 300 -u 8'
            image = work / 'Okilum.AppImage'
            image.write_text(f'#!/bin/bash\nenv > "$OUT/relaunch.env"\nexec "$BINARY" -c \'{body}\'\n')
            image.chmod(0o755)
            env = dict(os.environ, OUT=str(work), LOCK=str(lock), FIFO=str(work / 'fifo'), BINARY=str(binary),
                       MARKER='kept', APPIMAGE=str(image), APPDIR=str(binary.parents[2]))
            window = subprocess.Popen([str(binary), '-c', body], env=env, start_new_session=True)
            relaunched = None
            try:
                deadline = time.monotonic() + 10
                while not LINUX_HELPER['gui_processes'](lock=str(lock)):
                    self.assertLess(time.monotonic(), deadline)
                    time.sleep(0.05)
                env_file = work / 'env.json'
                status = LINUX_HELPER['quit_gui'](str(env_file), 10, lock=str(lock))
                self.assertEqual(status, 0)
                window.wait(timeout=5)
                self.assertEqual(window.returncode, -signal.SIGTERM)
                saved = json.loads(env_file.read_text())
                self.assertEqual(saved['MARKER'], 'kept')
                self.assertNotIn('APPIMAGE', saved)
                self.assertNotIn('APPDIR', saved)
                status = LINUX_HELPER['launch'](str(image), str(env_file), str(work / 'app.log'), 10, lock=str(lock))
                self.assertEqual(status, 0, (work / 'app.log').read_text())
                found = LINUX_HELPER['gui_processes'](lock=str(lock))
                self.assertEqual(len(found), 1)
                relaunched = found[0]['pid']
                self.assertIn('MARKER=kept', (work / 'relaunch.env').read_text())
                self.assertEqual(LINUX_HELPER['quit_gui'](str(work / 'none.json'), 1,
                                                          lock=str(work / 'other.lock')), 3)
            finally:
                for pid in (window.pid, relaunched):
                    if pid:
                        with contextlib.suppress(ProcessLookupError):
                            os.kill(pid, signal.SIGKILL)
                if window.poll() is None:
                    window.wait()


class LinuxScript(unittest.TestCase):
    def test_build_sh_local_dev_mode_is_explicit(self):
        build = (ROOT / 'scripts/appimage/build.sh').read_text()
        self.assertIn('local_dev=${OKILUM_LOCAL_DEV:-0}', build)
        self.assertIn('export NO_STRIP=1', build)
        self.assertIn('[[ $local_dev == 1 ]] || python3 ../../scripts/appimage/provenance.py', build)
        # CI keeps its Ubuntu 22.04 guard and run-number build.
        ci = build.split('\nelse\n', 1)[1].split('\nfi\n', 1)[0]
        self.assertIn('= 22.04', ci)
        self.assertIn('OKILUM_BUILD_VERSION="$((5000 + GITHUB_RUN_NUMBER))"', ci)

    def test_dev_script_uses_build_sh_without_editing_it(self):
        text = LINUX.read_text()
        self.assertIn('OKILUM_LOCAL_DEV=1', text)
        self.assertNotIn('sed -i', text)
        self.assertNotIn('pkill', text)
        self.assertNotIn('pgrep', text)

    def test_usage_and_syntax(self):
        subprocess.run(['bash', '-n', str(LINUX)], check=True)
        subprocess.run(['bash', '-n', str(MAC)], check=True)
        result = subprocess.run(['bash', str(LINUX), '--help'], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0)
        self.assertIn('Usage: okilum-dev-linux.sh', result.stdout)
        self.assertEqual(subprocess.run(['bash', str(LINUX)], capture_output=True).returncode, 2)


if __name__ == '__main__':
    unittest.main()
