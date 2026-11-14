#!/usr/bin/env python3
"""okilum-beta-deliver.py (Oleg 14.11): updater-equivalent checks, idleness over
the Wayland wire protocol, the install transaction, quit/reopen and rollback.
HTTP is loopback, the compositor and /proc are fakes, no process is signalled."""
import contextlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import struct
import subprocess
import tempfile
import threading
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts/dev/okilum-beta-deliver.py'
SPEC = importlib.util.spec_from_file_location('okilum_beta_deliver', SCRIPT)
D = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(D)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def have_gpg():
    return shutil.which('gpg') and shutil.which('gpgv')


class TestKey:
    """Disposable ed25519 signing key, like crates/okilum-appimage tests."""

    def __init__(self, directory):
        self.home = Path(directory) / 'g'
        self.home.mkdir(mode=0o700)
        subprocess.run(['gpg', '--homedir', self.home, '--batch', '--pinentry-mode', 'loopback', '--passphrase', '',
                        '--quick-generate-key', 'Deliver Test <deliver@example.invalid>', 'ed25519', 'sign', 'never'],
                       check=True, capture_output=True)
        listing = subprocess.run(['gpg', '--homedir', self.home, '--with-colons', '--fingerprint'],
                                 check=True, capture_output=True, text=True).stdout
        self.fingerprint = next(line.split(':')[9] for line in listing.splitlines() if line.startswith('fpr:'))
        self.armored = subprocess.run(['gpg', '--homedir', self.home, '--armor', '--export'],
                                      check=True, capture_output=True, text=True).stdout

    def sign(self, path):
        subprocess.run(['gpg', '--homedir', self.home, '--batch', '--yes', '--pinentry-mode', 'loopback',
                        '--passphrase', '', '--detach-sign', '-o', f'{path}.sig', path], check=True, capture_output=True)

    def close(self):
        subprocess.run(['gpgconf', '--homedir', self.home, '--kill', 'all'], capture_output=True)


def release_for(build, payload, provenance, fingerprint=D.FINGERPRINT):
    return {'schema': 1, 'platform': 'linux-appimage', 'channel': 'beta', 'build': build,
            'version': f'0.1.{build}', 'source': 'a' * 40, 'filename': D.IMAGE, 'sha256': sha(payload),
            'size': len(payload), 'key': f'okilum/linux/builds/{build}/{D.IMAGE}', 'fingerprint': fingerprint,
            'provenance_sha256': sha(provenance), 'github': {'run': 1}, 'publication_build': 9}


class Contract(unittest.TestCase):
    def test_authority_matches_the_in_app_updater(self):
        self.assertEqual(D.KEY, (ROOT / 'scripts/arch/okilum-signing-key.asc').read_text())
        lib = (ROOT / 'crates/okilum-appimage/src/lib.rs').read_text()
        self.assertIn(f'pub const FINGERPRINT: &str = "{D.FINGERPRINT}";', lib)
        self.assertIn(f'const ORIGIN: &str = "{D.ORIGIN}";', lib)
        self.assertIn(f'const IMAGE: &str = "{D.IMAGE}";', lib)
        fields = re.search(r'pub struct Release \{(.*?)\}', lib, re.S).group(1)
        self.assertEqual(tuple(re.findall(r'pub (\w+):', fields)), D.RELEASE_FIELDS)
        self.assertIn(D.OWNER.decode().strip(), (ROOT / 'crates/okilum-appimage/src/installation.rs').read_text())

    def test_descriptor_validation(self):
        good = release_for(7760, b'x', b'p')
        self.assertEqual(set(D.validate_release(dict(good))), set(D.RELEASE_FIELDS))
        for change in ({'channel': 'stable'}, {'platform': 'linux'}, {'build': True}, {'build': 4000},
                       {'version': '0.1.1'}, {'key': 'okilum/linux/builds/1/' + D.IMAGE}, {'fingerprint': 'B' * 40},
                       {'sha256': 'Z' * 64}, {'size': 0}, {'filename': 'x.AppImage'}, {'schema': 2}):
            with self.subTest(change=change), self.assertRaises(D.Refused):
                D.validate_release({**good, **change})
        with self.assertRaises(D.Refused):
            D.validate_release({k: v for k, v in good.items() if k != 'source'})

    def test_dearmor_reads_the_pinned_key(self):
        self.assertEqual(D.dearmor(D.KEY)[0], 0x98)   # an OpenPGP public-key packet


@unittest.skipUnless(have_gpg(), 'gpg/gpgv not installed')
class Signatures(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp()
        self.key = TestKey(self.dir)
        self.settings = D.Settings({'HOME': self.dir})

    def tearDown(self):
        self.key.close()
        shutil.rmtree(self.dir)

    def test_valid_foreign_and_tampered(self):
        data = Path(self.dir, 'data')
        data.write_bytes(b'payload')
        self.key.sign(data)
        D.verify_signature(self.settings, data, f'{data}.sig', self.key.armored, self.key.fingerprint)
        with self.assertRaises(D.Refused):   # good signature, other authority
            D.verify_signature(self.settings, data, f'{data}.sig', self.key.armored, 'C' * 40)
        with self.assertRaises(D.Refused):   # the pinned production key did not sign it
            D.verify_signature(self.settings, data, f'{data}.sig')
        data.write_bytes(b'payload!')
        with self.assertRaises(D.Refused):
            D.verify_signature(self.settings, data, f'{data}.sig', self.key.armored, self.key.fingerprint)


class FakeCompositor(threading.Thread):
    """Answers the registry with wl_seat + ext_idle_notifier_v1 and sends `idled`
    for the notification when `idle` is set. Records every request."""

    def __init__(self, path, notifier_version=2, idle=True, offer=True, burst=None):
        super().__init__(daemon=True)
        self.server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.server.bind(path)
        self.server.listen(1)
        self.version, self.idle, self.offer, self.requests = notifier_version, idle, offer, []
        # Opcodes sent in ONE write right after the notification is created
        # (0 idled, 1 resumed); replaces `idle` when given.
        self.burst = burst
        self.subscribed = threading.Event()
        self.start()

    def send_event(self, opcode):
        """A later idled/resumed, e.g. input during the quit."""
        self.conn.sendall(D.wl_message(self.notification[0], opcode))

    def run(self):
        with self.server:
            conn, _ = self.server.accept()
            self.serve(conn)

    def serve(self, conn):
        self.conn = conn
        buffer, objects = b'', {}
        with conn:
            while True:
                chunk = conn.recv(4096)
                if not chunk:
                    return
                buffer += chunk
                while len(buffer) >= 8 and len(buffer) >= struct.unpack_from('<I', buffer, 4)[0] >> 16:
                    obj, header = struct.unpack_from('<II', buffer)
                    size, opcode = header >> 16, header & 0xFFFF
                    payload, buffer = buffer[8:size], buffer[size:]
                    self.requests.append((obj, opcode, payload))
                    out = b''
                    if obj == 1 and opcode == 1:
                        registry = struct.unpack('<I', payload)[0]
                        objects['registry'] = registry
                        out += D.wl_message(registry, 0, struct.pack('<I', 7) + D.wl_string('wl_seat') + struct.pack('<I', 9))
                        if self.offer:
                            out += D.wl_message(registry, 0, struct.pack('<I', 8) + D.wl_string('ext_idle_notifier_v1')
                                                + struct.pack('<I', self.version))
                    elif obj == 1 and opcode == 0:
                        out += D.wl_message(struct.unpack('<I', payload)[0], 0, struct.pack('<I', 0))
                    elif obj == objects.get('registry') and opcode == 0:
                        name, length = struct.unpack_from('<II', payload)
                        offset = 8 + length + (-length % 4)
                        version, new = struct.unpack_from('<II', payload, offset)
                        objects[name] = (new, version)
                    elif objects.get(8) and obj == objects[8][0] and opcode in (1, 2):
                        self.notification = struct.unpack_from('<III', payload)
                        self.opcode = opcode
                        if self.burst is not None:
                            out += b''.join(D.wl_message(self.notification[0], code) for code in self.burst)
                        elif self.idle:
                            out += D.wl_message(self.notification[0], 0)
                        self.subscribed.set()
                    if out:
                        conn.sendall(out)


class Idleness(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp()
        self.path = os.path.join(self.dir, 'wayland-9')

    def tearDown(self):
        shutil.rmtree(self.dir)

    def test_input_idleness_on_notifier_v2(self):
        fake = FakeCompositor(self.path)
        self.assertTrue(D.wait_idle(self.path, 60000, 5))
        self.assertEqual(fake.opcode, 2)                 # get_input_idle_notification
        self.assertEqual(fake.notification[1], 60000)
        seat = struct.unpack('<I', fake.requests[3][2][-4:])[0]   # wl_registry.bind(wl_seat) new_id
        self.assertIn(b'wl_seat', fake.requests[3][2])
        self.assertEqual(fake.notification[2], seat)

    def test_notifier_v1_and_active_user(self):
        fake = FakeCompositor(self.path, notifier_version=1, idle=False)
        self.assertFalse(D.wait_idle(self.path, 60000, 0.5))
        self.assertEqual(fake.opcode, 1)                 # get_idle_notification

    def test_idled_then_resumed_in_one_packet_is_not_idle(self):
        # Review P1 (15760a27): the old client returned on the first `idled`.
        FakeCompositor(self.path, burst=[0, 1])
        self.assertFalse(D.wait_idle(self.path, 60000, 0.5))

    def test_resumed_idled_in_one_packet_is_idle(self):
        FakeCompositor(self.path, burst=[0, 1, 0])
        self.assertTrue(D.wait_idle(self.path, 60000, 2))

    def test_input_after_idle_is_seen_before_the_signal(self):
        fake = FakeCompositor(self.path)
        with D.IdleWatch(self.path, 60000) as watch:
            self.assertTrue(watch.wait(5))
            fake.send_event(1)                           # resumed
            deadline = time.monotonic() + 5
            while watch.still_idle() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertFalse(watch.still_idle())
            fake.send_event(0)
            self.assertTrue(watch.wait(5))

    def test_no_idle_protocol(self):
        FakeCompositor(self.path, offer=False)
        with self.assertRaises(ConnectionError):
            D.wait_idle(self.path, 60000, 1)

    def test_socket_from_environment(self):
        self.assertEqual(D.wayland_socket({'WAYLAND_DISPLAY': 'wayland-1', 'XDG_RUNTIME_DIR': '/run/user/1'}),
                         '/run/user/1/wayland-1')
        self.assertEqual(D.wayland_socket({'WAYLAND_DISPLAY': '/x/w'}), '/x/w')
        self.assertIsNone(D.wayland_socket({}))


class FakeProc:
    """A /proc with Okilum windows: exe link, lock fd, stat and environ."""

    def __init__(self, root):
        self.root = Path(root)
        self.root.mkdir()

    def add(self, pid, exe, lock, appimage, start=100):
        base = self.root / str(pid)
        (base / 'fd').mkdir(parents=True)
        os.symlink(exe, base / 'exe')
        os.symlink(lock, base / 'fd' / '7')
        self.state(pid, 'S', start)
        (base / 'environ').write_bytes(f'APPIMAGE={appimage}\0HOME=/h\0'.encode())

    def state(self, pid, state, start=100):
        fields = [state] + ['0'] * 18 + [str(start)] + ['0'] * 5
        (self.root / str(pid) / 'stat').write_text(f'{pid} (okilum main) ' + ' '.join(fields))

    def exit(self, pid):
        shutil.rmtree(self.root / str(pid))


class FakeWatch:
    def __init__(self, idle=True):
        self.idle = idle

    def still_idle(self):
        return self.idle


class Kernel:
    """Stands in for pidfd_open / pidfd_send_signal / poll on a FakeProc."""

    def __init__(self, test, proc, on_term='exit', on_open=None):
        self.proc, self.on_term, self.on_open, self.sent, self.pinned = proc, on_term, on_open, [], {}
        test.patches = getattr(test, 'patches', {})
        for name in ('open_pidfd', 'close_pidfd', 'send_signal', 'wait_exit'):
            test.patches.setdefault(name, getattr(D, name))
            setattr(D, name, getattr(self, name))

    def open_pidfd(self, pid):
        if not (self.proc.root / str(pid)).exists():
            raise ProcessLookupError(pid)
        if self.on_open:
            self.on_open(pid)
        fd = 1000 + pid
        self.pinned[fd] = pid
        return fd

    def close_pidfd(self, fd):
        self.pinned.pop(fd, None)

    def send_signal(self, fd, sig):
        pid = self.pinned[fd]
        self.sent.append((pid, sig))
        if sig == signal.SIGTERM and self.on_term == 'exit':
            self.proc.exit(pid)

    def wait_exit(self, fd, seconds):
        return not (self.proc.root / str(self.pinned[fd])).exists()


def restore(test):
    for name, value in getattr(test, 'patches', {}).items():
        setattr(D, name, value)


class Quit(unittest.TestCase):
    IMAGE = '/h/.local/share/okilum/Okilum.AppImage'

    def setUp(self):
        self.dir = tempfile.mkdtemp()
        self.proc = FakeProc(os.path.join(self.dir, 'proc'))
        self.lock = os.path.join(self.dir, 'state/okilum/reader-instance.lock')
        self.proc.add(41, '/tmp/.mount_x/usr/bin/okilum', self.lock, self.IMAGE)
        self.proc.add(42, '/tmp/.mount_x/usr/bin/okilum', '/h/.local/state/okilum-sprint/reader-instance.lock', '/s')
        self.proc.add(43, '/usr/bin/okilum-sprint', self.lock, '/s')

    def tearDown(self):
        restore(self)
        shutil.rmtree(self.dir)

    def windows(self):
        return D.gui_processes(str(self.proc.root), self.lock)

    def quit(self, idle=True):
        return D.quit_app(str(self.proc.root), self.lock, self.IMAGE, FakeWatch(idle), 1)

    def test_only_the_window_of_this_state_directory(self):
        self.assertEqual([(w['pid'], w['appimage']) for w in self.windows()], [(41, self.IMAGE)])

    def test_term_is_enough(self):
        kernel = Kernel(self, self.proc)
        self.assertEqual((self.quit(), kernel.sent), ('TERM', [(41, signal.SIGTERM)]))

    def test_ignored_term_is_never_followed_by_kill(self):
        # Review P1 (15760a27): no SIGKILL without proof that everything is saved.
        kernel = Kernel(self, self.proc, on_term='ignore')
        self.assertEqual((self.quit(), kernel.sent), ('still running', [(41, signal.SIGTERM)]))
        self.assertTrue((self.proc.root / '41').exists())

    def test_input_before_the_signal_sends_nothing(self):
        kernel = Kernel(self, self.proc)
        self.assertEqual((self.quit(idle=False), kernel.sent), ('user active', []))

    def test_pid_reused_during_the_wait_is_never_signalled(self):
        # Review P1 (15760a27): the window seen before the 25-min wait is gone and
        # its pid now names another program; the old code signalled the stale pid.
        kernel = Kernel(self, self.proc)
        self.proc.exit(41)
        self.proc.add(41, '/usr/bin/foot', '/dev/null', '')
        self.assertEqual((self.quit(), kernel.sent), ('closed', []))

    def test_restarted_window_gets_the_signal_under_its_new_identity(self):
        kernel = Kernel(self, self.proc)
        self.proc.exit(41)
        self.proc.add(57, '/tmp/.mount_y/usr/bin/okilum', self.lock, self.IMAGE, start=900)
        self.assertEqual((self.quit(), kernel.sent), ('TERM', [(57, signal.SIGTERM)]))

    def test_window_restarted_between_scan_and_pidfd_defers(self):
        # Re-review P1 (ea19f7d8): a rejected identity was taken as «closed», so
        # the install went ahead under a GUI that kept running.
        kernel = Kernel(self, self.proc, on_open=lambda pid: self.proc.state(pid, 'S', start=999))
        self.assertEqual((self.quit(), kernel.sent), ('changed', []))
        self.assertEqual(kernel.pinned, {})

    def test_window_that_exited_between_scan_and_pidfd_is_closed(self):
        kernel = Kernel(self, self.proc, on_open=lambda pid: self.proc.state(pid, 'Z'))
        self.assertEqual((self.quit(), kernel.sent), ('closed', []))

    def test_zombie_is_not_the_window(self):
        window = self.windows()[0]
        self.proc.state(41, 'Z')
        self.assertFalse(D.same_process(str(self.proc.root), window))

    def test_real_pidfd_on_a_child(self):
        child = subprocess.Popen(['sleep', '30'])
        try:
            fd = D.open_pidfd(child.pid)
            D.send_signal(fd, signal.SIGTERM)
            self.assertTrue(D.wait_exit(fd, 5))
            D.close_pidfd(fd)
        finally:
            child.kill()
            child.wait()


class Feed(BaseHTTPRequestHandler):
    files = {}

    def do_GET(self):
        body = self.files.get(self.path.lstrip('/'))
        if body is None:
            self.send_response(404)
            self.end_headers()
            return
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Last-Modified', 'Sat, 14 Nov 2026 08:07:20 GMT')
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


@unittest.skipUnless(have_gpg(), 'gpg/gpgv not installed')
class Delivery(unittest.TestCase):
    """deliver() against a signed loopback feed and a fake managed install."""

    @classmethod
    def setUpClass(cls):
        cls.keydir = tempfile.mkdtemp()
        cls.key = TestKey(cls.keydir)
        cls.saved = D.KEY, D.FINGERPRINT
        D.KEY, D.FINGERPRINT = cls.key.armored, cls.key.fingerprint
        cls.server = HTTPServer(('127.0.0.1', 0), Feed)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        D.KEY, D.FINGERPRINT = cls.saved
        cls.key.close()
        shutil.rmtree(cls.keydir)

    def setUp(self):
        self.dir = tempfile.mkdtemp()
        self.home = Path(self.dir, 'home')
        self.image = self.home / '.local/share/okilum/Okilum.AppImage'
        self.image.parent.mkdir(parents=True)
        self.old = b'#!/bin/sh\necho old 7746\n'
        self.image.write_bytes(self.old)
        self.image.chmod(0o755)
        (self.image.parent / '.appimage-owner').write_bytes(D.OWNER)
        D.write_build(self.image, 7746, self.image)
        self.payload = b'#!/bin/sh\necho new 7760\n' * 100
        self.publish(7760, self.payload)
        self.settings = D.Settings({'HOME': str(self.home), 'WAYLAND_DISPLAY': 'wayland-test',
                                    'XDG_RUNTIME_DIR': self.dir,
                                    'OKILUM_DELIVER_ORIGIN': f'http://127.0.0.1:{self.server.server_port}/',
                                    'OKILUM_DELIVER_START_SECONDS': '2'})
        self.proc = FakeProc(os.path.join(self.dir, 'proc'))
        self.launches, self.signals = [], []
        self.patches = {}
        self.idle_result, self.on_wait = True, None
        test = self

        class Watch:
            def __init__(self, *_):
                pass

            def __enter__(self):
                return self

            def __exit__(self, *_):
                pass

            def wait(self, seconds):
                if test.on_wait:
                    test.on_wait()
                return test.idle_result

            def still_idle(self):
                return test.idle_result
        self.patch('IdleWatch', Watch)
        self.patch('quit_app', self.fake_quit)
        self.patch('launch', self.fake_launch)
        self.patch('unit_active', lambda unit: self.app_runs)
        self.app_confirms, self.app_runs = True, True
        self.on_quit = None

    def tearDown(self):
        restore(self)
        shutil.rmtree(self.dir)

    def patch(self, name, value):
        self.patches.setdefault(name, getattr(D, name))
        setattr(D, name, value)

    def publish(self, build, payload, provenance_sha=None, source='a' * 40):
        folder = Path(self.dir, 'feed')
        shutil.rmtree(folder, ignore_errors=True)
        folder.mkdir(parents=True)
        provenance = json.dumps({'source_sha': source, 'build_version': str(build),
                                 'release_version': f'0.1.{build}', 'sha256': provenance_sha or sha(payload)}).encode()
        release = release_for(build, payload, provenance, self.key.fingerprint)
        release['source'] = source
        files = {'okilum/linux/beta/latest.json': json.dumps(release).encode(),
                 release['key']: payload, release['key'].replace(D.IMAGE, 'provenance.json'): provenance}
        for key, body in list(files.items()):
            path = folder / key.replace('/', '_')
            path.write_bytes(body)
            if not key.endswith('provenance.json'):
                self.key.sign(path)
                files[key + '.sig'] = Path(f'{path}.sig').read_bytes()
        Feed.files = files

    def open_window(self):
        lock = str(self.settings.app_state / D.INSTANCE_LOCK)
        self.proc.add(51058, '/tmp/x/usr/bin/okilum', lock, str(self.image))

    def fake_quit(self, proc, lock, image, watch, term_wait):
        windows = D.gui_processes(proc, lock)
        self.signals.append([w['pid'] for w in windows])
        for window in windows:
            self.proc.exit(window['pid'])
        if self.on_quit:
            self.on_quit()
        return 'TERM'

    def lock_free(self, name):
        import fcntl
        try:
            fd = os.open(self.image.parent / name, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        except OSError:
            return None
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return True
        except BlockingIOError:
            return False
        finally:
            os.close(fd)

    def fake_launch(self, image, receipt=None, **_):
        # An ordinary launch blocks on .install-lock (installation::install), so
        # only the update lock may be held while the new build starts (APPLY).
        self.locks_at_launch = (self.lock_free('.install-lock'), self.lock_free('.Okilum.AppImage.update-lock'))
        self.launches.append((Path(image).read_bytes(), receipt))
        if receipt and self.app_confirms:
            Path(receipt, 'started').write_text('startup confirmed\n')
        return f'app-test-{len(self.launches)}.service'

    def deliver(self):
        with contextlib.redirect_stdout(open(os.devnull, 'w')) as quiet, quiet:
            return D.deliver(self.settings, proc=str(self.proc.root))

    def log(self):
        return self.settings.log.read_text() if self.settings.log.exists() else ''

    def stages(self):
        return sorted(p for p in self.image.parent.iterdir() if p.name.startswith('.okilum-update-'))

    def test_open_app_is_quit_installed_and_reopened_with_receipt(self):
        self.open_window()
        self.assertEqual(self.deliver(), 'installed 7760')
        self.assertEqual(self.image.read_bytes(), self.payload)
        self.assertEqual(self.signals, [[51058]])
        self.assertEqual(self.locks_at_launch, (True, False))
        (content, receipt), = self.launches
        self.assertEqual(content, self.payload)
        stage = Path(receipt)
        self.assertEqual(stage.parent, self.image.parent)
        self.assertEqual(stage.stat().st_mode & 0o777, 0o700)
        self.assertEqual((stage / 'previous.AppImage').read_bytes(), self.old)
        self.assertEqual((stage / 'completed').read_text(), 'committed\n')
        expected = json.loads((stage / 'expected.json').read_text())
        self.assertEqual(tuple(expected), D.RELEASE_FIELDS)
        self.assertEqual((expected['build'], expected['sha256']), (7760, sha(self.payload)))
        self.assertEqual(D.recorded_build(self.image), 7760)
        self.assertEqual(os.lstat(self.image).st_nlink, 1)
        self.assertEqual(self.image.stat().st_mode & 0o777, 0o755)
        self.assertRegex(self.log(), r'installed build 7760 source aaaaaaaaa · beta published \d\d:07:20 .*quit TERM'
                                     r'.*start confirmed by the app')
        self.assertEqual(self.deliver(), 'up to date (7760)')
        self.assertEqual(len(self.launches), 1)
        self.assertEqual(json.loads((self.settings.own_state / 'last-check.json').read_text())['installed'], 7760)

    def test_install_never_writes_into_the_running_inode(self):
        # 14.11 13:41: a QA instance on idunn, running from the managed AppImage
        # through its FUSE runtime, died with SIGBUS. Delivery is safe for such
        # processes only while it never writes into the file they run.
        self.open_window()
        held = os.open(self.image, os.O_RDONLY)          # what the AppImage runtime holds
        try:
            before = os.fstat(held)
            self.assertEqual(self.deliver(), 'installed 7760')
            after = os.fstat(held)
            self.assertEqual((after.st_ino, after.st_size, after.st_mtime_ns),
                             (before.st_ino, before.st_size, before.st_mtime_ns))
            self.assertGreaterEqual(after.st_nlink, 1)   # still linked as previous.AppImage
            self.assertEqual(os.pread(held, len(self.old), 0), self.old)
            self.assertNotEqual(os.stat(self.image).st_ino, before.st_ino)
            for stage in self.stages():                  # pruning only unlinks
                shutil.rmtree(stage)
            self.assertEqual(os.pread(held, len(self.old), 0), self.old)
        finally:
            os.close(held)

    def test_closed_app_is_installed_but_not_started(self):
        self.assertEqual(self.deliver(), 'installed 7760')
        self.assertEqual((self.image.read_bytes(), self.launches, self.signals), (self.payload, [], []))
        self.assertIn('Okilum was not open, not started', self.log())

    def absent(self):
        self.settings.assume_absent = True

        def no_idle_watch(*_):
            raise AssertionError('idleness must not be measured when the user is assumed absent')
        self.patch('IdleWatch', no_idle_watch)

    def test_assumed_absent_user_closes_without_idle_wait(self):
        # Oleg 14.11: on idunn agents' synthetic input keeps Wayland "active".
        self.open_window()
        self.absent()
        kernel = self.real_quit()
        self.assertEqual(self.deliver(), 'installed 7760')
        self.assertEqual(kernel.sent, [(51058, signal.SIGTERM)])
        self.assertEqual(self.image.read_bytes(), self.payload)
        self.assertEqual(len(self.launches), 1)
        self.assertIn('user assumed absent (OKILUM_DELIVER_ASSUME_USER_ABSENT)', self.log())
        self.assertRegex(self.log(), r'installed build 7760 .* no idle wait \(user assumed absent\) · quit TERM')

    def test_assumed_absent_still_never_kills_and_stays_identity_bound(self):
        self.open_window()
        self.absent()
        kernel = self.real_quit(on_term='ignore')
        self.settings.term_wait = 0.2
        self.assertEqual(self.deliver(), 'Okilum did not quit')
        self.assertEqual(kernel.sent, [(51058, signal.SIGTERM)])
        self.assertEqual((self.image.read_bytes(), self.launches), (self.old, []))
        restore(self)
        self.patches = {}
        self.patch('IdleWatch', lambda *_: (_ for _ in ()).throw(AssertionError('no idle watch')))
        self.patch('launch', self.fake_launch)
        self.patch('unit_active', lambda unit: True)
        kernel = Kernel(self, self.proc, on_open=lambda pid: self.proc.state(pid, 'S', start=999))
        self.assertEqual(self.deliver(), 'Okilum changed')
        self.assertEqual((kernel.sent, self.image.read_bytes()), ([], self.old))

    def test_assume_absent_is_off_by_default_and_parsed(self):
        self.assertFalse(D.Settings({'HOME': '/h'}).assume_absent)
        for value, expected in (('1', True), ('true', True), ('yes', True), ('0', False), ('', False)):
            self.assertEqual(D.Settings({'HOME': '/h', 'OKILUM_DELIVER_ASSUME_USER_ABSENT': value}).assume_absent,
                             expected, value)

    def test_active_user_waits_and_logs_once(self):
        self.open_window()
        self.idle_result = False
        self.assertEqual(self.deliver(), 'user active')
        self.assertEqual(self.deliver(), 'user active')
        self.assertEqual((self.image.read_bytes(), self.signals, self.launches), (self.old, [], []))
        self.assertEqual(self.log().count('build 7760 waits: user active'), 1)

    def test_failed_start_rolls_back_reopens_and_gives_up_after_two(self):
        self.open_window()
        self.app_confirms, self.app_runs = False, False
        self.assertEqual(self.deliver(), 'rolled back')
        self.assertEqual(self.image.read_bytes(), self.old)
        self.assertEqual(D.recorded_build(self.image), 7746)
        self.assertEqual([(c == self.payload, r is not None) for c, r in self.launches], [(True, True), (False, False)])
        self.assertIn('previous AppImage restored, reopened', self.log())
        self.open_window()
        self.assertEqual(self.deliver(), 'rolled back')
        self.assertEqual(self.deliver(), 'gave up on this build')
        self.assertEqual(len(self.launches), 4)
        self.publish(7761, self.payload + b'#')
        self.open_window()
        self.app_confirms, self.app_runs = True, True
        self.assertEqual(self.deliver(), 'installed 7761')
        self.assertEqual(sorted(p.name for p in self.settings.own_state.glob('*-*')), ['last-check.json', 'last-log-line'])

    def test_unconfirmed_start_keeps_the_new_build(self):
        self.open_window()
        self.app_confirms = False
        self.assertEqual(self.deliver(), 'unconfirmed')
        self.assertEqual(self.image.read_bytes(), self.payload)
        self.assertIn('no start confirmation', self.log())

    def test_bad_payload_refuses_before_quitting(self):
        self.open_window()
        self.publish(7760, self.payload, provenance_sha='b' * 64)
        with self.assertRaises(D.Refused):
            self.deliver()
        self.assertEqual((self.image.read_bytes(), self.signals, self.launches), (self.old, [], []))

    def test_install_failure_after_quit_reopens_the_old_build(self):
        self.open_window()
        os.symlink('/nonexistent', self.image.parent / '.Okilum.AppImage.update-lock')
        with self.assertRaises(D.Refused):
            self.deliver()
        self.assertEqual(self.image.read_bytes(), self.old)
        self.assertEqual([(c, r) for c, r in self.launches], [(self.old, None)])

    def real_quit(self, **kernel):
        D.quit_app = self.patches['quit_app']           # the real one; restore() keeps the original
        return Kernel(self, self.proc, **kernel)

    def test_input_right_before_the_signal_cancels_the_run(self):
        self.open_window()
        kernel = self.real_quit()
        self.on_wait = lambda: setattr(self, 'idle_result', True)
        Watch = D.IdleWatch
        Watch.still_idle = lambda _: False                # `resumed` after `idled`
        self.assertEqual(self.deliver(), 'user active')
        self.assertEqual((kernel.sent, self.image.read_bytes(), self.launches), ([], self.old, []))
        self.assertIn('input arrived just before quitting; nothing sent', self.log())

    def test_window_that_ignores_term_keeps_running_and_nothing_is_installed(self):
        self.open_window()
        kernel = self.real_quit(on_term='ignore')
        self.settings.term_wait = 0.2
        self.assertEqual(self.deliver(), 'Okilum did not quit')
        self.assertEqual(self.deliver(), 'Okilum did not quit')
        self.assertEqual(kernel.sent, [(51058, signal.SIGTERM)] * 2)
        self.assertEqual((self.image.read_bytes(), self.launches), (self.old, []))
        self.assertEqual(self.log().count('never killed'), 1)

    def newer_in_app_update(self):
        self.image.write_bytes(b'in-app 7761')
        self.image.chmod(0o755)
        D.write_build(self.image, 7761, self.image)

    def test_in_app_update_during_the_quit_is_not_downgraded(self):
        # Review P1 (15760a27): re-check under both locks right before installing.
        self.open_window()
        self.on_quit = self.newer_in_app_update
        self.assertEqual(self.deliver(), 'up to date (7761)')
        self.assertEqual(self.image.read_bytes(), b'in-app 7761')
        self.assertEqual(D.recorded_build(self.image), 7761)
        self.assertEqual(self.launches, [(b'in-app 7761', None)])   # reopened unchanged
        self.assertIn('build 7760 not installed: build 7761 is already installed, reopened', self.log())
        self.assertEqual(self.stages(), [])

    def test_newer_build_after_the_first_check_is_kept_with_the_app_closed(self):
        original = self.patches.setdefault('download_payload', D.download_payload)

        def download_then_update(settings, release):
            payload = original(settings, release)
            self.newer_in_app_update()                   # lands before the locks are taken
            return payload
        D.download_payload = download_then_update
        self.assertEqual(self.deliver(), 'up to date (7761)')
        self.assertEqual((self.image.read_bytes(), self.launches), (b'in-app 7761', []))

    def test_in_app_update_during_the_idle_wait_skips_the_quit(self):
        self.open_window()
        self.on_wait = self.newer_in_app_update
        self.assertEqual(self.deliver(), 'up to date (7761)')
        self.assertEqual((self.signals, self.launches, self.image.read_bytes()), ([], [], b'in-app 7761'))

    def test_restart_during_the_quit_installs_nothing(self):
        self.open_window()
        self.real_quit(on_open=lambda pid: self.proc.state(pid, 'S', start=999))
        self.assertEqual(self.deliver(), 'Okilum changed')
        self.assertEqual((self.image.read_bytes(), self.launches), (self.old, []))
        self.assertIn('Okilum restarted while it was being quit; nothing sent', self.log())

    def test_window_reopened_after_the_quit_is_not_installed_under(self):
        self.open_window()
        self.on_quit = lambda: self.proc.add(61000, '/tmp/z/usr/bin/okilum',
                                             str(self.settings.app_state / D.INSTANCE_LOCK), str(self.image), start=5)
        self.assertEqual(self.deliver(), 'Okilum running')
        self.assertEqual((self.image.read_bytes(), self.launches), (self.old, []))
        self.assertIn('is running again; not installed under it', self.log())

    def test_held_updater_lock_is_respected(self):
        import fcntl
        self.settings.lock_wait = 0.5
        fd = os.open(self.image.parent / '.Okilum.AppImage.update-lock', os.O_RDWR | os.O_CREAT, 0o600)
        fcntl.flock(fd, fcntl.LOCK_EX)
        try:
            with self.assertRaises(D.Refused):
                self.deliver()
        finally:
            os.close(fd)
        self.assertEqual(self.image.read_bytes(), self.old)

    def test_unknown_record_with_same_bytes_counts_as_installed(self):
        self.image.write_bytes(self.payload)
        self.assertEqual(self.deliver(), 'up to date (7760)')

    def test_stages_are_pruned_like_the_updater(self):
        parent = self.image.parent
        made = []
        for index, kind in enumerate(['completed', 'completed', 'previous.AppImage', 'previous.AppImage', 'new.AppImage']):
            stage = Path(tempfile.mkdtemp(prefix='.okilum-update-', dir=parent))
            (stage / kind).write_text('x')
            os.utime(stage / kind, ns=(index * 10**9, index * 10**9))
            os.utime(stage, ns=(index * 10**9, index * 10**9))
            made.append(stage)
        D.prune_stages(parent)
        self.assertEqual(sorted(self.stages()), sorted([made[1], made[3]]))


class Locks(unittest.TestCase):
    """Re-review P2 (ea19f7d8): holding .install-lock while waiting for the update
    lock deadlocks with APPLY, which holds the update lock until the new GUI has
    started, and that start takes .install-lock."""

    def test_no_cycle_with_the_apply_helper(self):
        import fcntl
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory, 'Okilum.AppImage')
            image.write_bytes(b'x')
            update = os.open(Path(directory, '.Okilum.AppImage.update-lock'), os.O_RDWR | os.O_CREAT, 0o600)
            fcntl.flock(update, fcntl.LOCK_EX)
            started = threading.Event()

            def apply_helper():
                # The GUI APPLY waits for: it needs .install-lock, then APPLY ends.
                install = os.open(Path(directory, '.install-lock'), os.O_RDWR | os.O_CREAT, 0o644)
                try:
                    time.sleep(0.3)                      # the script takes .install-lock first
                    deadline = time.monotonic() + 5
                    while time.monotonic() < deadline:
                        try:
                            fcntl.flock(install, fcntl.LOCK_EX | fcntl.LOCK_NB)
                            started.set()
                            break
                        except BlockingIOError:
                            time.sleep(0.05)
                finally:
                    os.close(install)
                    os.close(update)
            helper = threading.Thread(target=apply_helper)
            helper.start()
            try:
                with D.install_locks(image, 4):          # ea19f7d8: Refused after 4 s
                    pass
            finally:
                helper.join()
            self.assertTrue(started.is_set())


class Log(unittest.TestCase):
    def test_repeated_lines_once_and_rotation(self):
        # Re-review P2 (ea19f7d8): the same refusal was appended on every tick.
        with tempfile.TemporaryDirectory() as home, contextlib.redirect_stdout(open(os.devnull, 'w')) as quiet, quiet:
            settings = D.Settings({'HOME': home, 'OKILUM_DELIVER_LOG_BYTES': '400'})
            for _ in range(3):
                D.log_line(settings, 'refused: an Okilum install or update holds its lock')
            self.assertEqual(settings.log.read_text().count('refused:'), 1)
            D.log_line(settings, 'installed build 7761')
            D.log_line(settings, 'refused: an Okilum install or update holds its lock')
            self.assertEqual(settings.log.read_text().count('refused:'), 2)
            for index in range(20):
                D.log_line(settings, f'installed build {7800 + index} · ' + 'x' * 40)
            old = settings.log.with_name('okilum-deliver.log.1')
            self.assertTrue(old.is_file())
            self.assertLessEqual(settings.log.stat().st_size, 400)
            self.assertLessEqual(old.stat().st_size, 400)
            self.assertEqual(sorted(p.name for p in settings.log.parent.glob('okilum-deliver.log*')),
                             ['okilum-deliver.log', 'okilum-deliver.log.1'])
            self.assertIn('installed build 7819', settings.log.read_text())


class Units(unittest.TestCase):
    def test_idunn_drop_in_assumes_the_user_absent(self):
        drop_in = (ROOT / 'scripts/dev/systemd/okilum-beta-deliver.service.d/assume-user-absent.conf').read_text()
        self.assertIn('[Service]\nEnvironment=OKILUM_DELIVER_ASSUME_USER_ABSENT=1', drop_in)
        self.assertNotIn('ASSUME_USER_ABSENT', (ROOT / 'scripts/dev/systemd/okilum-beta-deliver.service').read_text())

    def test_units_run_the_installed_script_in_the_session(self):
        service = (ROOT / 'scripts/dev/systemd/okilum-beta-deliver.service').read_text()
        timer = (ROOT / 'scripts/dev/systemd/okilum-beta-deliver.timer').read_text()
        self.assertIn('ExecStart=/usr/bin/python3 -I %h/.local/bin/okilum-beta-deliver', service)
        self.assertIn('Type=oneshot', service)
        self.assertIn('PartOf=graphical-session.target', service)
        self.assertIn('WantedBy=graphical-session.target', timer)
        self.assertIn('OnUnitInactiveSec=150s', timer)
        minutes = int(re.search(r'TimeoutStartSec=(\d+)min', service).group(1))
        self.assertGreater(minutes * 60, D.Settings({'HOME': '/h'}).idle_wait + 300)

    def test_launch_command(self):
        calls = []

        def run(command, **_):
            calls.append(command)
            return subprocess.CompletedProcess(command, 0, '', '')
        unit = D.launch(Path('/h/Okilum.AppImage'), receipt='/h/.okilum-update-x', run=run, suffix='1')
        self.assertEqual(unit, 'app-okilum-beta-deliver-1.service')
        command = calls[0]
        self.assertEqual(command[:3], ['systemd-run', '--user', '--quiet'])
        self.assertIn('app-graphical.slice', command)
        self.assertIn('OKILUM_APPIMAGE_UPDATE_RECEIPT=/h/.okilum-update-x', command)
        unset = next(part for part in command if part.startswith('UnsetEnvironment='))
        # systemd applies UnsetEnvironment= after --setenv (14.11 on idunn: the
        # receipt vanished and the new build could not confirm its start).
        self.assertNotIn('OKILUM_APPIMAGE_UPDATE_RECEIPT', unset)
        self.assertIn('APPIMAGE', unset.split('=', 1)[1].split())
        self.assertEqual(command[-2:], ['--', '/h/Okilum.AppImage'])
        D.launch(Path('/h/Okilum.AppImage'), run=run, suffix='2')
        self.assertFalse(any('RECEIPT=/' in part for part in calls[1]))
        unset = next(part for part in calls[1] if part.startswith('UnsetEnvironment='))
        self.assertIn('OKILUM_APPIMAGE_UPDATE_RECEIPT', unset)


if __name__ == '__main__':
    unittest.main()
